use anyhow::{anyhow, Result};
use clap::Parser as _;
use colored::Colorize;
use futures::{StreamExt as _, TryStreamExt};
use log::debug;
use once_cell::sync::OnceCell;
use reqwest_eventsource::EventSource;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Command;
use std::time::Duration;
use tokio::fs::File;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio_util::io::StreamReader;
use url::Url;

mod api;
mod cli;
use cli::{Cli, IdOrName};
mod chat;
use chat::start_chat;
mod bismuth_toml;

static GLOBAL_OPTS: OnceCell<cli::GlobalOpts> = OnceCell::new();

#[derive(Debug, Serialize, Deserialize)]
struct Config {
    organization_id: u64,
    token: String,
}

#[derive(Clone)]
struct APIClient {
    client: reqwest::Client,
    pub base_url: Url,
    pub token: String,
}

impl APIClient {
    fn new(api_url: &Url, token: &str) -> Result<Self> {
        let mut base_url = api_url.clone();
        base_url.set_password(Some(token)).unwrap();
        Ok(Self {
            client: reqwest::ClientBuilder::new()
                .user_agent("bismuthcloud-cli")
                .build()?,
            base_url,
            token: token.to_string(),
        })
    }
    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.client
            .get(self.base_url.join(path.trim_start_matches('/')).unwrap())
    }
    fn post(&self, path: &str) -> reqwest::RequestBuilder {
        self.client
            .post(self.base_url.join(path.trim_start_matches('/')).unwrap())
    }
    fn put(&self, path: &str) -> reqwest::RequestBuilder {
        self.client
            .put(self.base_url.join(path.trim_start_matches('/')).unwrap())
    }
    fn delete(&self, path: &str) -> reqwest::RequestBuilder {
        self.client
            .delete(self.base_url.join(path.trim_start_matches('/')).unwrap())
    }
}

trait ResponseErrorExt {
    async fn error_body_for_status(self) -> Result<reqwest::Response>;
}

impl ResponseErrorExt for reqwest::Response {
    async fn error_body_for_status(self) -> Result<reqwest::Response> {
        let status = self.status();
        if status.is_success() {
            Ok(self)
        } else if status == reqwest::StatusCode::UNAUTHORIZED {
            Err(anyhow!("Unauthorized - maybe you need to login?",))
        } else {
            let body = self.text().await?;
            Err(anyhow!("{} ({})", body, status))
        }
    }
}

async fn choice<'a, 'b, T>(things: &'a [T], name: &'b str) -> Result<&'a T>
where
    T: ToString,
{
    loop {
        println!("Select a {}:", name);
        for (i, thing) in things.iter().enumerate() {
            println!("{}: {}", i + 1, thing.to_string());
        }
        print!("> ");
        std::io::stdout().flush()?;
        let selector = std::io::stdin()
            .lines()
            .next()
            .unwrap_or(Ok("".to_string()))?;

        if let Ok(idx) = selector.trim().parse::<usize>() {
            if idx > things.len() {
                eprintln!("Invalid index");
                continue;
            }
            break Ok(&things[idx - 1]);
        } else {
            match things
                .iter()
                .find(|thing| thing.to_string() == selector.trim())
            {
                Some(thing) => break Ok(thing),
                None => {
                    eprintln!("No such {}", name);
                    continue;
                }
            }
        }
    }
}

async fn confirm(prompt: impl Into<String>, default: bool) -> Result<bool> {
    print!(
        "{} [{}/{}] ",
        prompt.into(),
        if default { "Y" } else { "y" },
        if default { "n" } else { "N" }
    );
    std::io::stdout().flush()?;
    let confirm = std::io::stdin()
        .lines()
        .next()
        .unwrap_or(Ok("".to_string()))?
        .to_lowercase();
    if confirm.is_empty() || (confirm != "y" && confirm != "n") {
        return Ok(default);
    }
    Ok(confirm == "y")
}

async fn press_any_key(msg: &str) -> Result<()> {
    println!("{}", msg);
    std::io::stdout().flush()?;
    let termios = termios::Termios::from_fd(0).unwrap();
    let mut new_termios = termios.clone();
    new_termios.c_lflag &= !(termios::ICANON | termios::ECHO);
    termios::tcsetattr(0, termios::TCSANOW, &new_termios).unwrap();
    std::io::stdin().read(&mut [0])?;
    termios::tcsetattr(0, termios::TCSANOW, &termios).unwrap();
    Ok(())
}

fn github_app_url(api_url: &Url) -> &'static str {
    match api_url.host_str() {
        Some("localhost") => "https://github.com/apps/bismuthdev-dev/installations/new",
        Some("api-staging.bismuth.cloud") => {
            "https://github.com/apps/bismuthdev-staging/installations/new"
        }
        _ => "https://github.com/apps/bismuthdev/installations/new",
    }
}

async fn resolve_project_id(client: &APIClient, id: &IdOrName) -> Result<api::Project> {
    let project_id = match id {
        cli::IdOrName::Name(name) => {
            let projects: api::ListProjectsResponse = client
                .get("/projects/list")
                .send()
                .await?
                .error_body_for_status()
                .await?
                .json()
                .await?;
            let project = projects
                .projects
                .iter()
                .find(|p| p.name == *name)
                .ok_or_else(|| anyhow!("No such project"))?;
            project.id
        }
        cli::IdOrName::Id(id) => *id,
    };
    let get_project = client
        .get(&format!("projects/{}", project_id))
        .send()
        .await?
        .error_body_for_status()
        .await?;
    Ok(get_project.json().await?)
}

async fn resolve_feature_id(
    client: &APIClient,
    project: &api::Project,
    feature: &IdOrName,
) -> Result<api::Feature> {
    let feature_id = match feature {
        cli::IdOrName::Name(name) => {
            let feature = project
                .features
                .iter()
                .find(|f| f.name == *name)
                .ok_or_else(|| anyhow!("No such feature"))?;
            feature.id
        }
        cli::IdOrName::Id(id) => *id,
    };
    let get_feature = client
        .get(&format!("/projects/{}/features/{}", project.id, feature_id))
        .send()
        .await?
        .error_body_for_status()
        .await?;
    Ok(get_feature.json().await?)
}

async fn resolve_chat_session(
    client: &APIClient,
    project: &api::Project,
    feature: &api::Feature,
    session_name: &str,
) -> Result<api::ChatSession> {
    let sessions: Vec<api::ChatSession> = client
        .get(&format!(
            "/projects/{}/features/{}/chat/sessions",
            project.id, feature.id
        ))
        .send()
        .await?
        .error_body_for_status()
        .await?
        .json()
        .await?;
    sessions
        .iter()
        .find(|s| s.name() == session_name)
        .cloned()
        .ok_or_else(|| {
            anyhow!(
                "No such chat session. Available sessions: {}",
                sessions
                    .iter()
                    .map(|s| s.name())
                    .collect::<Vec<String>>()
                    .join(", ")
            )
        })
}

fn set_bismuth_remote(repo: &Path, project: &api::Project) -> Result<()> {
    let mut git_url = GLOBAL_OPTS
        .get()
        .unwrap()
        .api_url
        .clone()
        .join(&format!("/git/{}", project.hash))?;
    git_url.set_username("git").unwrap();
    git_url.set_password(Some(&project.clone_token)).unwrap();

    let git_repo = git2::Repository::discover(repo)?;
    match git_repo.find_remote("bismuth") {
        Ok(_) => {
            debug!("Updating existing bismuth remote URL");
            git_repo.remote_set_url("bismuth", git_url.as_ref())?;
        }
        Err(_) => {
            debug!("Adding new bismuth remote");
            git_repo.remote("bismuth", git_url.as_ref())?;
        }
    }
    Ok(())
}

async fn get_project_and_feature_for_repo(
    client: &APIClient,
    repo: &Path,
) -> Result<(api::Project, api::Feature)> {
    let repo = git2::Repository::discover(repo).map_err(|_| {
        anyhow!("Unable to determine project and feature (path is not a git repository)")
    })?;
    let remote_url = repo
        .find_remote("bismuth")
        .map_err(|e| {
            anyhow!(
                "You must import this repository to Bismuth before using it ({})",
                e
            )
        })?
        .url()
        .unwrap()
        .to_string();
    let branch_name = repo.head()?.shorthand().unwrap().to_string();

    for project in &client
        .get("/projects/list")
        .send()
        .await?
        .error_body_for_status()
        .await?
        .json::<api::ListProjectsResponse>()
        .await?
        .projects
    {
        if remote_url.contains(&project.clone_token) {
            for feature in &project.features {
                if branch_name == feature.name {
                    return Ok((project.clone(), feature.clone()));
                }
            }
            if !project.has_pushed {
                let new_feature: api::Feature = client
                    .post(&format!("/projects/{}/features", project.id))
                    .json(&json!({ "name": branch_name }))
                    .send()
                    .await?
                    .error_body_for_status()
                    .await?
                    .json()
                    .await?;
                return Ok((project.clone(), new_feature));
            }
            // TODO: this repo has been pushed before, do we just want to implicitly do it again?
            return Err(anyhow!(
                "Unable to determine feature (current branch is not pushed?)"
            ));
        }
    }
    Err(anyhow!(
        "Unable to determine project (no matching projects found)"
    ))
}

async fn project_import(args: &cli::ImportArgs, client: &APIClient) -> Result<()> {
    let repo = args.source.repo.clone().unwrap_or(PathBuf::from("."));
    if !repo.exists() {
        return Err(anyhow!("Repo does not exist"));
    }
    let repo = std::fs::canonicalize(repo)?;

    let git_repo = git2::Repository::discover(repo.as_path())
        .map_err(|_| anyhow!("Directory is not a git repository"))?;

    let project: api::Project = client
        .post("/projects")
        .json(&api::CreateProjectRequest::Name(api::CreateProjectRepo {
            name: repo.file_name().unwrap().to_string_lossy().to_string(),
        }))
        .send()
        .await?
        .error_body_for_status()
        .await?
        .json()
        .await?;

    if git_repo.head().is_err() {
        let mut index = git_repo.index()?;
        let tree_id = index.write_tree()?;
        let tree = git_repo.find_tree(tree_id)?;
        let signature = git2::Signature::now(
            "bismuthdev[bot]",
            "bismuthdev[bot]@users.noreply.github.com",
        )?;
        git_repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            "Initial commit",
            &tree,
            &[],
        )?;
    }
    set_bismuth_remote(&repo, &project)?;

    if args.upload || confirm(
            "Would you like to upload your code to Bismuth Cloud for analysis?\nThis will improve the accuracy and intelligence of Bismuth on your code (but will not be used for training).",
            true,
        )
        .await?
        {
            if !Command::new("git")
                .arg("-C")
                .arg(repo.as_path())
                .arg("push")
                .arg("--force")
                .arg("bismuth")
                .arg("--all")
                //.arg("refs/remotes/origin/*")
                //.arg("refs/heads/*")
                .stdout(std::process::Stdio::inherit())
                .stderr(std::process::Stdio::inherit())
                .output()
                .map_err(|e| anyhow!(e))?.status.success() {
                    if confirm("Failed to push to Bismuth. Would you like to continue without pushing?", true).await? {
                        println!(
                            "{}",
                            format!(
                                "🎉 Successfully created project {}",
                                project.name
                            )
                            .green()
                        );
                        return Ok(());
                    } else {
                        println!("Cleaning up project...");
                        client
                            .delete(&format!("/projects/{}", project.id))
                            .send()
                            .await?
                            .error_body_for_status()
                            .await?;
                        return Err(anyhow!("Failed to push! Hint: you may need to temporarily disable git pre-push hooks."));
                    }
                }
        }
    println!(
        "{}",
        format!(
            "🎉 Successfully imported {} to project {}",
            repo.as_path().display(),
            project.name
        )
        .green()
    );
    Ok(())
}

fn project_clone(project: &api::Project, outdir: Option<&Path>) -> Result<PathBuf> {
    let mut auth_url = GLOBAL_OPTS.get().unwrap().api_url.clone();
    auth_url.set_username("git").unwrap();
    auth_url.set_password(Some(&project.clone_token)).unwrap();

    let outdir = outdir
        .map(|p| p.to_owned())
        .unwrap_or(PathBuf::from(&project.name));
    debug!("Cloning project to {:?}", outdir);

    let bismuth_remote_url = auth_url
        .join(&format!("/git/{}", project.hash))?
        .to_string();

    let clone_url = match &project.github_app_install {
        Some(_) => {
            format!(
                "git@github.com:{}.git",
                project.github_repo.as_ref().unwrap()
            )
        }
        None => bismuth_remote_url.clone(),
    };

    Command::new("git")
        .arg("clone")
        .arg(&clone_url)
        .arg(&outdir)
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .output()
        .map_err(|e| anyhow!(e))
        .and_then(|o| {
            if o.status.success() {
                Ok(())
            } else {
                Err(anyhow!("Failed to clone ({})", o.status))
            }
        })?;

    let repo = git2::Repository::open(&outdir)?;
    repo.remote("bismuth", &bismuth_remote_url)?;

    Ok(outdir)
}

/// Returns true if the specified repository has changes in the checked out branch
/// that have not been pushed to a Bismuth remote.
fn check_not_pushed(repo: &Path, project: &api::Project, feature: &api::Feature) -> Result<bool> {
    let repo = git2::Repository::discover(repo)?;
    let origin_url = repo.find_remote("origin")?.url().unwrap().to_string();
    if origin_url.contains(&project.clone_token) {
        return Ok(false);
    }

    let remote_url = repo.find_remote("bismuth")?.url().unwrap().to_string();
    let branch_name = repo.head()?.shorthand().unwrap().to_string();

    if !remote_url.contains(&project.clone_token) {
        return Err(anyhow!("Repository does not correspond to project"));
    }

    if branch_name != feature.name {
        return Err(anyhow!("Current branch does not match feature name"));
    }

    let origin_commit = repo
        .find_branch(
            &format!("origin/{}", &branch_name),
            git2::BranchType::Remote,
        )?
        .get()
        .target()
        .ok_or(anyhow!("No such branch in origin remote?"))?;
    let bismuth_commit = repo
        .find_branch(
            &format!("bismuth/{}", &branch_name),
            git2::BranchType::Remote,
        )?
        .get()
        .target()
        .ok_or(anyhow!("No such branch in bismuth remote?"))?;

    Ok(origin_commit != bismuth_commit)
}

async fn feature_deploy(
    project: &api::Project,
    feature: &api::Feature,
    client: &APIClient,
    timeout: Option<Duration>,
) -> Result<()> {
    if let Ok(true) = check_not_pushed(&std::env::current_dir()?, project, feature) {
        println!(
            "{}",
            "Repository has commits not pushed to Bismuth - you may be deploying an old version."
                .yellow()
        );
        if confirm("Would you like to push changes now?", true).await? {
            Command::new("git")
                .arg("push")
                .arg("--force")
                .arg("bismuth")
                .stdout(std::process::Stdio::inherit())
                .stderr(std::process::Stdio::inherit())
                .output()
                .map_err(|e| anyhow!(e))
                .and_then(|o| {
                    if o.status.success() {
                        Ok(())
                    } else {
                        Err(anyhow!("Failed to push to Bismuth"))
                    }
                })?;
        }
    }

    client
        .post(&format!(
            "/projects/{}/features/{}/deploy",
            project.id, feature.id
        ))
        .send()
        .await?
        .error_body_for_status()
        .await?;

    if timeout.is_none() {
        return Ok(());
    }

    print!("Waiting for deployment to be healthy");
    std::io::stdout().flush()?;
    for _ in 0..timeout.unwrap().as_secs() {
        tokio::time::sleep(Duration::from_secs(1)).await;
        print!(".");
        std::io::stdout().flush()?;

        let status: api::DeployStatusResponse = client
            .get(&format!(
                "/projects/{}/features/{}/deploy/status",
                project.id, feature.id
            ))
            .send()
            .await?
            .error_body_for_status()
            .await?
            .json()
            .await?;

        match status.status {
            api::ContainerState::Running => {
                let url = feature_get_url(project, feature, client).await?;
                println!("\nDeployed to {}", url);
                return Ok(());
            }
            api::ContainerState::Failed => {
                // TODO: print logs?
                return Err(anyhow!("Deployment failed"));
            }
            _ => {}
        }
    }

    println!("");

    Err(anyhow!("Timed out waiting for deployment"))
}

async fn feature_deploy_status(
    project: &api::Project,
    feature: &api::Feature,
    client: &APIClient,
) -> Result<()> {
    let resp = client
        .get(&format!(
            "/projects/{}/features/{}/deploy/status",
            project.id, feature.id
        ))
        .send()
        .await?;
    if resp.status().as_u16() == 404 {
        println!("Status: Not Deployed");
        return Ok(());
    }
    let status: api::DeployStatusResponse = resp.error_body_for_status().await?.json().await?;
    println!("Status: {:?}", status.status);
    println!("Deployed Commit: {}", status.commit);
    Ok(())
}

async fn feature_teardown(
    project: &api::Project,
    feature: &api::Feature,
    client: &APIClient,
) -> Result<()> {
    client
        .delete(&format!(
            "/projects/{}/features/{}/deploy",
            project.id, feature.id
        ))
        .send()
        .await?
        .error_body_for_status()
        .await?;
    Ok(())
}

async fn feature_get_url(
    project: &api::Project,
    feature: &api::Feature,
    client: &APIClient,
) -> Result<String> {
    let resp: api::InvokeURLResponse = client
        .get(&format!(
            "/projects/{}/features/{}/invoke_url",
            project.id, feature.id
        ))
        .send()
        .await?
        .error_body_for_status()
        .await?
        .json()
        .await?;
    Ok(resp.url)
}

async fn feature_logs(
    project: &api::Project,
    feature: &api::Feature,
    follow: bool,
    client: &APIClient,
) -> Result<()> {
    if follow {
        let mut es = EventSource::new(client.get(&format!(
            "/projects/{}/features/{}/logs/streaming",
            project.id, feature.id
        )))?;

        while let Some(event) = es.next().await {
            match event {
                Ok(reqwest_eventsource::Event::Open) => {}
                Ok(reqwest_eventsource::Event::Message(message)) => {
                    print!("{}", message.data);
                    std::io::stdout().flush()?;
                }
                Err(err) => {
                    eprintln!("Error streaming logs: {}", err);
                    es.close();
                }
            }
        }

        Ok(())
    } else {
        let logs = client
            .get(&format!(
                "/projects/{}/features/{}/logs",
                project.id, feature.id
            ))
            .send()
            .await?
            .error_body_for_status()
            .await?
            .text()
            .await?;
        println!("{}", logs);

        Ok(())
    }
}

fn oidc_url(api_url: &Url) -> Url {
    let base = match api_url.host_str() {
        Some("localhost") => Url::parse("http://localhost:8543/").unwrap(),
        Some("api-staging.bismuth.cloud") => {
            Url::parse("https://auth-staging.bismuth.cloud/").unwrap()
        }
        _ => Url::parse("https://auth.bismuth.cloud/").unwrap(),
    };
    base.join("/realms/bismuth/protocol/openid-connect/")
        .unwrap()
}

#[derive(Debug, Serialize, Deserialize)]
struct Tokens {
    access_token: String,
    // Don't care about the rest
}

async fn oidc_server(api_url: &Url) -> Result<String> {
    let server = tiny_http::Server::http("localhost:0").map_err(|e| anyhow!(e))?;
    let port = server.server_addr().to_ip().unwrap().port();
    press_any_key("Press any key to open the login page.").await?;
    open::that_detached(
        api_url
            .join(&format!("auth/cli?port={}", port))
            .unwrap()
            .as_str(),
    )?;
    let request = tokio::task::spawn_blocking(move || {
        server
            .incoming_requests()
            .next()
            .ok_or_else(|| anyhow!("No request"))
    })
    .await??;
    let code = request
        .url()
        .split("code=")
        .last()
        .expect("No code")
        .split("&")
        .next()
        .unwrap()
        .to_string();
    debug!("Got code: {}", code);
    let client = reqwest::Client::new();
    let tokens: Tokens = client
        .post(oidc_url(api_url).join("token").unwrap())
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!(
            "grant_type=authorization_code&client_id=cli&code={}&redirect_uri=http://localhost:{}/",
            code, port
        ))
        .send()
        .await?
        .error_body_for_status()
        .await?
        .json()
        .await?;
    let api_key = client
        .post(api_url.join("/auth/apikey").unwrap())
        .header("Authorization", format!("Bearer {}", tokens.access_token))
        .send()
        .await?
        .error_body_for_status()
        .await?
        .text()
        .await?;

    request.respond(
        tiny_http::Response::from_string(
            "<html><body>Authentication successful. You may now close this window</body></html>",
        )
        .with_header(
            "Content-type: text/html"
                .parse::<tiny_http::Header>()
                .unwrap(),
        ),
    )?;
    Ok(api_key)
}

async fn check_version() -> Result<()> {
    let client = reqwest::Client::new();
    let resp = client
        .get("https://bismuthcloud.github.io/cli/LATEST")
        .timeout(Duration::from_secs(1))
        .send()
        .await?;
    if let [latest_maj, latest_min, latest_patch] = resp
        .text()
        .await?
        .trim()
        .split('.')
        .map(|s| -> Result<u64> { Ok(s.split('-').next().unwrap().parse::<u64>()?) })
        .collect::<Result<Vec<u64>, _>>()?
        .as_slice()
    {
        if let [this_maj, this_min, this_patch] = env!("CARGO_PKG_VERSION")
            .split('.')
            .map(|s| -> Result<u64> { Ok(s.split('-').next().unwrap().parse::<u64>()?) })
            .collect::<Result<Vec<u64>, _>>()?
            .as_slice()
        {
            if latest_maj > this_maj || latest_min > this_min || latest_patch > this_patch {
                eprintln!("{}", "A newer version of the CLI is available!".yellow());
                eprintln!(
                    "{}",
                    "Get it at https://github.com/BismuthCloud/cli/releases".yellow()
                );
            }
        }
    }
    Ok(())
}

async fn _main() -> Result<()> {
    let args = Cli::parse();

    if args.markdown_help {
        clap_markdown::print_help_markdown::<Cli>();

        return Ok(());
    }

    GLOBAL_OPTS.set(args.global.clone()).unwrap();

    env_logger::Builder::new()
        .filter_level(args.global.verbose.log_level_filter())
        .init();

    if std::env::var("BISMUTH_NO_VERSION_CHECK").is_err() {
        let _ = check_version().await;
    }

    if let cli::Command::Version = args.command {
        println!(
            "Bismuth CLI {} ({})",
            env!("CARGO_PKG_VERSION"),
            git_version::git_version!()
        );
        return Ok(());
    }

    if let cli::Command::Login = args.command {
        debug!("Starting login flow");

        let token = oidc_server(&args.global.api_url).await?;

        let client = APIClient::new(&args.global.api_url, &token)?;
        let user = client
            .get("/auth/me")
            .send()
            .await?
            .error_body_for_status()
            .await?
            .json::<api::User>()
            .await?;

        let organization = choice(&user.organizations, "organization").await?;

        let config = Config {
            token: token.to_string(),
            organization_id: organization.id,
        };
        let config_str = serde_json::to_string(&config)?;
        let mut config_file = File::create(&args.global.config_file).await?;
        config_file.write_all(config_str.as_bytes()).await?;
        return Ok(());
    }

    let mut config_file = File::open(&args.global.config_file).await.map_err(|_| {
        anyhow!("Failed to open auth token. Maybe you need to `bismuth login` first?")
    })?;
    let mut config_str: String = String::new();
    config_file.read_to_string(&mut config_str).await?;
    let config: Config = serde_json::from_str(&config_str)?;

    debug!("Organization ID: {}", config.organization_id);

    let client = APIClient::new(
        &args
            .global
            .api_url
            .join(&format!("/organizations/{}/", config.organization_id))?,
        &config.token,
    )?;

    match &args.command {
        cli::Command::Configure { command } => match command {
            cli::ConfigureCommand::OpenRouter {} => {
                let server = tiny_http::Server::http("localhost:0").map_err(|e| anyhow!(e))?;
                let port = server.server_addr().to_ip().unwrap().port();
                let mut url = Url::parse("https://openrouter.ai/auth").unwrap();
                url.query_pairs_mut()
                    .append_pair("callback_url", &format!("http://localhost:{}/", port));
                println!(
                    "Go to the following URL to authenticate: {}",
                    url.to_string().blue().bold()
                );
                let request = tokio::task::spawn_blocking(move || {
                    server
                        .incoming_requests()
                        .next()
                        .ok_or_else(|| anyhow!("No request"))
                })
                .await??;
                let code = request
                    .url()
                    .split("code=")
                    .last()
                    .expect("No code")
                    .split("&")
                    .next()
                    .unwrap()
                    .to_string();
                debug!("Got code: {}", code);
                let resp: serde_json::Value = reqwest::Client::new()
                    .post("https://openrouter.ai/api/v1/auth/keys")
                    .json(&json!({"code": code}))
                    .send()
                    .await?
                    .error_body_for_status()
                    .await?
                    .json()
                    .await?;
                let key = resp.get("key").unwrap().as_str().unwrap().to_string();
                debug!("Got key: {}", key);

                client
                    .post("/llm-configuration")
                    .json(&api::LLMConfigurationRequest { key })
                    .send()
                    .await?
                    .error_body_for_status()
                    .await?;

                request.respond(
                    tiny_http::Response::from_string(
                        "<html><body>Authentication successful. You may now close this window</body></html>",
                    )
                    .with_header(
                        "Content-type: text/html"
                            .parse::<tiny_http::Header>()
                            .unwrap(),
                    ),
                )?;
                Ok(())
            }
        },
        cli::Command::Project { command } => match command {
            cli::ProjectCommand::List => {
                let get_projects = client
                    .get("/projects/list")
                    .send()
                    .await?
                    .error_body_for_status()
                    .await?;
                let projects: api::ListProjectsResponse = get_projects.json().await?;
                for project in &projects.projects {
                    println!("{}", project.name);
                }
                Ok(())
            }
            cli::ProjectCommand::Create { name } => {
                let project: api::Project = client
                    .post("/projects")
                    .json(&api::CreateProjectRequest::Name(api::CreateProjectRepo {
                        name: name.clone(),
                    }))
                    .send()
                    .await?
                    .error_body_for_status()
                    .await?
                    .json()
                    .await?;
                project_clone(&project, None)?;
                Ok(())
            }
            cli::ProjectCommand::Import(args) => project_import(args, &client).await,
            cli::ProjectCommand::AddRemote { project, repo } => {
                let project = resolve_project_id(&client, project).await?;
                let repo = std::fs::canonicalize(repo.clone().unwrap_or(std::env::current_dir()?))?;
                set_bismuth_remote(&repo, &project)?;
                Ok(())
            }
            cli::ProjectCommand::Upload { project, repo } => {
                let project = resolve_project_id(&client, project).await?;
                let repo = std::fs::canonicalize(repo.clone().unwrap_or(std::env::current_dir()?))?;
                set_bismuth_remote(&repo, &project)?;
                Command::new("git")
                    .arg("-C")
                    .arg(repo.as_path())
                    .arg("push")
                    .arg("--force")
                    .arg("bismuth")
                    .arg("--all")
                    //.arg("refs/remotes/origin/*")
                    //.arg("refs/heads/*")
                    .stdout(std::process::Stdio::inherit())
                    .stderr(std::process::Stdio::inherit())
                    .output()
                    .map_err(|e| anyhow!(e))
                    .and_then(|o| {
                        if o.status.success() {
                            Ok(())
                        } else {
                            Err(anyhow!("Failed to push to Bismuth"))
                        }
                    })?;
                Ok(())
            }
            cli::ProjectCommand::Clone { project, outdir } => {
                let project = resolve_project_id(&client, project).await?;
                project_clone(&project, outdir.as_deref())?;
                Ok(())
            }
            cli::ProjectCommand::Link { project } => {
                let project = resolve_project_id(&client, project).await?;
                let mut gh_orgs: Vec<api::GitHubAppInstall> = client
                    .get("/projects/connect/github/organizations")
                    .send()
                    .await?
                    .error_body_for_status()
                    .await?
                    .json()
                    .await?;
                if gh_orgs.is_empty() {
                    println!("You'll need to install the GitHub app first.");
                    press_any_key("Press any key to open the installation page.").await?;
                    open::that_detached(github_app_url(&client.base_url))?;
                    print!("Waiting for app install");
                    std::io::stdout().flush()?;
                    loop {
                        tokio::time::sleep(Duration::from_secs(2)).await;
                        print!(".");
                        std::io::stdout().flush()?;
                        gh_orgs = client
                            .get("/projects/connect/github/organizations")
                            .send()
                            .await?
                            .error_body_for_status()
                            .await?
                            .json::<Vec<api::GitHubAppInstall>>()
                            .await?;
                        if !gh_orgs.is_empty() {
                            break;
                        }
                    }
                }
                let gh_org = choice(&gh_orgs, "organization").await?;
                let updated_project: api::Project = client
                    .post(&format!("/projects/{}/connect/github", project.id))
                    .json(&gh_org)
                    .send()
                    .await?
                    .error_body_for_status()
                    .await?
                    .json()
                    .await?;
                println!(
                    "{}",
                    format!(
                        "🎉 Successfully linked {} to https://github.com/{}",
                        updated_project.name,
                        updated_project.github_repo.unwrap(),
                    )
                    .green()
                );
                Ok(())
            }
            cli::ProjectCommand::Delete { project } => {
                let project = resolve_project_id(&client, project).await?;
                if confirm(
                    format!("Are you sure you want to delete project {}?", project.name),
                    false,
                )
                .await?
                {
                    client
                        .delete(&format!("/projects/{}", project.id))
                        .send()
                        .await?
                        .error_body_for_status()
                        .await?;
                }
                Ok(())
            }
        },
        cli::Command::Feature { command } => match command {
            cli::FeatureCommand::List { project } => {
                let project = resolve_project_id(&client, project).await?;
                for feature in &project.features {
                    println!("{}", feature.name);
                }
                Ok(())
            }
            cli::FeatureCommand::Config { feature, command } => {
                let (project_name, feature_name) = feature.split();
                let project = resolve_project_id(&client, &project_name).await?;
                let feature = resolve_feature_id(&client, &project, &feature_name).await?;

                match command {
                    cli::FeatureConfigCommand::Get { key } => {
                        let resp = client
                            .get(&format!(
                                "/projects/{}/features/{}/config",
                                project.id, feature.id
                            ))
                            .send()
                            .await?
                            .error_body_for_status()
                            .await?;
                        let feature_config: Vec<api::FeatureConfig> = resp.json().await?;
                        match key {
                            Some(key) => {
                                let config = feature_config
                                    .iter()
                                    .find(|c| c.key == *key)
                                    .ok_or_else(|| anyhow!("No such key"))?;
                                println!("{}", config.value);
                            }
                            None => {
                                for c in feature_config {
                                    println!("{}={}", c.key, c.value);
                                }
                            }
                        }
                        Ok(())
                    }
                    cli::FeatureConfigCommand::Set { key, value } => {
                        let resp = client
                            .get(&format!(
                                "/projects/{}/features/{}/config",
                                project.id, feature.id
                            ))
                            .send()
                            .await?
                            .error_body_for_status()
                            .await?;
                        let mut feature_config: Vec<api::FeatureConfig> = resp.json().await?;
                        feature_config.retain(|c| c.key != *key);
                        feature_config.push(api::FeatureConfig {
                            key: key.clone(),
                            value: value.clone(),
                        });
                        client
                            .post(&format!(
                                "/projects/{}/features/{}/config",
                                project.id, feature.id
                            ))
                            .json(&feature_config)
                            .send()
                            .await?
                            .error_body_for_status()
                            .await?;
                        Ok(())
                    }
                }
            }
            cli::FeatureCommand::Deploy {
                feature,
                no_wait,
                timeout,
            } => {
                let (project_name, feature_name) = feature.split();
                let project = resolve_project_id(&client, &project_name).await?;
                let feature = resolve_feature_id(&client, &project, &feature_name).await?;
                feature_deploy(
                    &project,
                    &feature,
                    &client,
                    if *no_wait {
                        None
                    } else {
                        Some(Duration::from_secs(*timeout))
                    },
                )
                .await
            }
            cli::FeatureCommand::DeployStatus { feature } => {
                let (project_name, feature_name) = feature.split();
                let project = resolve_project_id(&client, &project_name).await?;
                let feature = resolve_feature_id(&client, &project, &feature_name).await?;
                feature_deploy_status(&project, &feature, &client).await
            }
            cli::FeatureCommand::Teardown { feature } => {
                let (project_name, feature_name) = feature.split();
                let project = resolve_project_id(&client, &project_name).await?;
                let feature = resolve_feature_id(&client, &project, &feature_name).await?;
                feature_teardown(&project, &feature, &client).await
            }
            cli::FeatureCommand::GetURL { feature } => {
                let (project_name, feature_name) = feature.split();
                let project = resolve_project_id(&client, &project_name).await?;
                let feature = resolve_feature_id(&client, &project, &feature_name).await?;
                let url = feature_get_url(&project, &feature, &client).await?;
                println!("{}", url);
                Ok(())
            }
            cli::FeatureCommand::Logs { feature, follow } => {
                let (project_name, feature_name) = feature.split();
                let project = resolve_project_id(&client, &project_name).await?;
                let feature = resolve_feature_id(&client, &project, &feature_name).await?;
                feature_logs(&project, &feature, *follow, &client).await
            }
        },
        cli::Command::KV { command } => match command {
            cli::KVCommand::Get { feature, key } => {
                let (project_name, feature_name) = feature.split();
                let project = resolve_project_id(&client, &project_name).await?;
                let feature = resolve_feature_id(&client, &project, &feature_name).await?;

                let resp = client
                    .get(&format!(
                        "/projects/{}/features/{}/svcprovider/kv/v1/{}",
                        project.id, feature.id, key
                    ))
                    .send()
                    .await?
                    .error_body_for_status()
                    .await?;
                tokio::io::copy(
                    &mut StreamReader::new(
                        resp.bytes_stream()
                            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e)),
                    ),
                    &mut tokio::io::stdout(),
                )
                .await?;
                Ok(())
            }
            cli::KVCommand::Set {
                feature,
                key,
                value,
            } => {
                let (project_name, feature_name) = feature.split();
                let project = resolve_project_id(&client, &project_name).await?;
                let feature = resolve_feature_id(&client, &project, &feature_name).await?;

                client
                    .post(&format!(
                        "/projects/{}/features/{}/svcprovider/kv/v1/{}",
                        project.id, feature.id, key
                    ))
                    .body(value.clone())
                    .send()
                    .await?
                    .error_body_for_status()
                    .await?;
                Ok(())
            }
            cli::KVCommand::Delete { feature, key } => {
                let (project_name, feature_name) = feature.split();
                let project = resolve_project_id(&client, &project_name).await?;
                let feature = resolve_feature_id(&client, &project, &feature_name).await?;

                client
                    .delete(&format!(
                        "/projects/{}/features/{}/svcprovider/kv/v1/{}",
                        project.id, feature.id, key
                    ))
                    .send()
                    .await?
                    .error_body_for_status()
                    .await?;
                Ok(())
            }
        },
        cli::Command::Blob { command } => match command {
            cli::BlobCommand::List { feature } => {
                let (project_name, feature_name) = feature.split();
                let project = resolve_project_id(&client, &project_name).await?;
                let feature = resolve_feature_id(&client, &project, &feature_name).await?;

                let resp = client
                    .get(&format!(
                        "/projects/{}/features/{}/svcprovider/blob/v1/",
                        project.id, feature.id
                    ))
                    .send()
                    .await?;
                let blobs: HashMap<String, Vec<u8>> = resp.json().await?;
                for blob in blobs.keys() {
                    println!("{}", blob);
                }
                Ok(())
            }
            cli::BlobCommand::Create {
                feature,
                key,
                value,
            } => {
                let (project_name, feature_name) = feature.split();
                let project = resolve_project_id(&client, &project_name).await?;
                let feature = resolve_feature_id(&client, &project, &feature_name).await?;

                client
                    .post(&format!(
                        "/projects/{}/features/{}/svcprovider/blob/v1/{}",
                        project.id, feature.id, key
                    ))
                    .body(if let Some(literal) = &value.literal {
                        reqwest::Body::from(literal.clone())
                    } else {
                        reqwest::Body::from(File::open(value.file.as_ref().unwrap()).await?)
                    })
                    .send()
                    .await?
                    .error_body_for_status()
                    .await?;
                Ok(())
            }
            cli::BlobCommand::Get {
                feature,
                key,
                output,
            } => {
                let (project_name, feature_name) = feature.split();
                let project = resolve_project_id(&client, &project_name).await?;
                let feature = resolve_feature_id(&client, &project, &feature_name).await?;

                let resp = client
                    .get(&format!(
                        "/projects/{}/features/{}/svcprovider/blob/v1/{}",
                        project.id, feature.id, key
                    ))
                    .send()
                    .await?
                    .error_body_for_status()
                    .await?;
                let mut output: Pin<Box<dyn tokio::io::AsyncWrite>> = match output {
                    Some(output) => Box::pin(File::create(output).await?),
                    None => Box::pin(tokio::io::stdout()),
                };
                tokio::io::copy(
                    &mut StreamReader::new(
                        resp.bytes_stream()
                            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e)),
                    ),
                    &mut output,
                )
                .await?;
                Ok(())
            }
            cli::BlobCommand::Set {
                feature,
                key,
                value,
            } => {
                let (project_name, feature_name) = feature.split();
                let project = resolve_project_id(&client, &project_name).await?;
                let feature = resolve_feature_id(&client, &project, &feature_name).await?;

                client
                    .put(&format!(
                        "/projects/{}/features/{}/svcprovider/blob/v1/{}",
                        project.id, feature.id, key
                    ))
                    .body(if let Some(literal) = &value.literal {
                        reqwest::Body::from(literal.clone())
                    } else {
                        reqwest::Body::from(File::open(value.file.as_ref().unwrap()).await?)
                    })
                    .send()
                    .await?
                    .error_body_for_status()
                    .await?;
                Ok(())
            }
            cli::BlobCommand::Delete { feature, key } => {
                let (project_name, feature_name) = feature.split();
                let project = resolve_project_id(&client, &project_name).await?;
                let feature = resolve_feature_id(&client, &project, &feature_name).await?;

                client
                    .delete(&format!(
                        "/projects/{}/features/{}/svcprovider/blob/v1/{}",
                        project.id, feature.id, key
                    ))
                    .send()
                    .await?
                    .error_body_for_status()
                    .await?;
                Ok(())
            }
        },
        cli::Command::SQL { command } => match command {
            cli::SQLCommand::Query { feature, query } => {
                let (project_name, feature_name) = feature.split();
                let project = resolve_project_id(&client, &project_name).await?;
                let feature = resolve_feature_id(&client, &project, &feature_name).await?;

                let resp = client
                    .post(&format!(
                        "/projects/{}/features/{}/svcprovider/sql",
                        project.id, feature.id
                    ))
                    .body(if let Some(literal) = &query.literal {
                        reqwest::Body::from(literal.clone())
                    } else {
                        reqwest::Body::from(File::open(query.file.as_ref().unwrap()).await?)
                    })
                    .send()
                    .await?
                    .error_body_for_status()
                    .await?;
                tokio::io::copy(
                    &mut StreamReader::new(
                        resp.bytes_stream()
                            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e)),
                    ),
                    &mut tokio::io::stdout(),
                )
                .await?;
                Ok(())
            }
        },
        cli::Command::Billing { command } => match command {
            cli::BillingCommand::ManageSubscription => {
                let org = client
                    .get("")
                    .send()
                    .await?
                    .error_body_for_status()
                    .await?
                    .json::<api::Organization>()
                    .await?;
                let url = if org.subscription.r#type == api::SubscriptionType::Individual {
                    println!("Opening subscription upgrade page");
                    client
                        .get("/billing/upgrade")
                        .query(&[("tier", "PROFESSIONAL")])
                        .send()
                        .await?
                        .error_body_for_status()
                        .await?
                        .text()
                        .await?
                } else {
                    println!("Opening subscription management page");
                    client
                        .get("/billing/manage")
                        .send()
                        .await?
                        .error_body_for_status()
                        .await?
                        .text()
                        .await?
                };
                open::that_detached(url)?;
                Ok(())
            }
            cli::BillingCommand::CreditsRemaining => {
                let credits: api::CreditUsage = client
                    .get("/billing/credits/usage")
                    .send()
                    .await?
                    .error_body_for_status()
                    .await?
                    .json()
                    .await?;
                println!(
                    "{}",
                    credits.plan_included - credits.plan_used + credits.purchased_remaining
                );
                Ok(())
            }
            cli::BillingCommand::Refill => {
                let url = client
                    .get("/billing/credits/buy")
                    .send()
                    .await?
                    .error_body_for_status()
                    .await?
                    .text()
                    .await?;
                println!("Opening checkout page");
                open::that_detached(url)?;
                Ok(())
            }
        },
        // Convenience aliases
        cli::Command::Import(args) => project_import(args, &client).await,
        cli::Command::Deploy {
            feature,
            no_wait,
            timeout,
        } => {
            let (project_name, feature_name) = feature.split();
            let project = resolve_project_id(&client, &project_name).await?;
            let feature = resolve_feature_id(&client, &project, &feature_name).await?;
            feature_deploy(
                &project,
                &feature,
                &client,
                if *no_wait {
                    None
                } else {
                    Some(Duration::from_secs(*timeout))
                },
            )
            .await
        }
        cli::Command::DeployStatus { feature } => {
            let (project_name, feature_name) = feature.split();
            let project = resolve_project_id(&client, &project_name).await?;
            let feature = resolve_feature_id(&client, &project, &feature_name).await?;
            feature_deploy_status(&project, &feature, &client).await
        }
        cli::Command::Teardown { feature } => {
            let (project_name, feature_name) = feature.split();
            let project = resolve_project_id(&client, &project_name).await?;
            let feature = resolve_feature_id(&client, &project, &feature_name).await?;
            feature_teardown(&project, &feature, &client).await
        }
        cli::Command::GetURL { feature } => {
            let (project_name, feature_name) = feature.split();
            let project = resolve_project_id(&client, &project_name).await?;
            let feature = resolve_feature_id(&client, &project, &feature_name).await?;
            let url = feature_get_url(&project, &feature, &client).await?;
            println!("{}", url);
            Ok(())
        }
        cli::Command::Logs { feature, follow } => {
            let (project_name, feature_name) = feature.split();
            let project = resolve_project_id(&client, &project_name).await?;
            let feature = resolve_feature_id(&client, &project, &feature_name).await?;
            feature_logs(&project, &feature, *follow, &client).await
        }
        cli::Command::Chat {
            repo,
            session_name,
            command,
        } => {
            let current_user: api::User = client
                .get("/../../auth/me")
                .send()
                .await?
                .error_body_for_status()
                .await?
                .json()
                .await?;

            let repo_path = match repo {
                Some(repo) => {
                    if repo.exists() {
                        repo.to_path_buf()
                    } else {
                        return Err(anyhow!("Repo does not exist"));
                    }
                }
                _ => std::env::current_dir()?,
            };
            let (project, feature) = get_project_and_feature_for_repo(&client, &repo_path).await?;

            match command {
                None => {
                    let repo_path = match repo {
                        Some(repo) => {
                            if repo.exists() {
                                repo.to_path_buf()
                            } else {
                                project_clone(&project, Some(repo))?
                            }
                        }
                        None => {
                            // Check if CWD is a git repo which has the correct remote
                            let repo = git2::Repository::open_from_env()?;
                            let remote_url = repo
                                .find_remote("bismuth")
                                .map(|r| r.url().unwrap().to_string())
                                .unwrap_or("".to_string());
                            if remote_url.contains(&project.clone_token) {
                                repo.workdir().unwrap().to_path_buf()
                            } else {
                                project_clone(&project, None)?
                            }
                        }
                    };
                    Command::new("git")
                        .arg("-C")
                        .arg(&repo_path)
                        .arg("fetch")
                        .arg("bismuth")
                        .output()
                        .map_err(|e| anyhow!(e))
                        .and_then(|o| {
                            if o.status.success() {
                                Ok(())
                            } else {
                                Err(anyhow!("Failed to `git fetch` ({})", o.status))
                            }
                        })?;

                    let sessions: Vec<api::ChatSession> = client
                        .get(&format!(
                            "/projects/{}/features/{}/chat/sessions",
                            project.id, feature.id
                        ))
                        .send()
                        .await?
                        .error_body_for_status()
                        .await?
                        .json()
                        .await?;

                    let existing_session = match session_name {
                        Some(session_name) => {
                            resolve_chat_session(&client, &project, &feature, session_name)
                                .await
                                .ok()
                        }
                        None => None,
                    };
                    let session = match existing_session {
                        Some(session) => session,
                        None => {
                            client
                                .post(&format!(
                                    "/projects/{}/features/{}/chat/sessions",
                                    project.id, feature.id
                                ))
                                .json(&json!({ "name": session_name }))
                                .send()
                                .await?
                                .error_body_for_status()
                                .await?
                                .json()
                                .await?
                        }
                    };

                    if let Err(e) = bismuth_toml::parse_config(&repo_path) {
                        return Err(anyhow!("Invalid bismuth.toml: {}", e));
                    }

                    start_chat(
                        &current_user,
                        &project,
                        &feature,
                        sessions,
                        &session,
                        &repo_path,
                        &client,
                    )
                    .await
                }
                Some(cli::ChatSubcommand::ListSessions) => {
                    let sessions: Vec<api::ChatSession> = client
                        .get(&format!(
                            "/projects/{}/features/{}/chat/sessions",
                            project.id, feature.id
                        ))
                        .send()
                        .await?
                        .error_body_for_status()
                        .await?
                        .json()
                        .await?;
                    for session in sessions {
                        println!("{}", session.name());
                    }
                    Ok(())
                }
                Some(cli::ChatSubcommand::RenameSession { old_name, new_name }) => {
                    let session =
                        resolve_chat_session(&client, &project, &feature, old_name).await?;
                    client
                        .put(&format!(
                            "/projects/{}/features/{}/chat/sessions/{}",
                            project.id, feature.id, session.id
                        ))
                        .json(&json!({ "name": new_name }))
                        .send()
                        .await?
                        .error_body_for_status()
                        .await?;

                    Ok(())
                }
                Some(cli::ChatSubcommand::DeleteSession { name }) => {
                    let session = resolve_chat_session(&client, &project, &feature, name).await?;
                    client
                        .delete(&format!(
                            "/projects/{}/features/{}/chat/sessions/{}",
                            project.id, feature.id, session.id
                        ))
                        .send()
                        .await?
                        .error_body_for_status()
                        .await?;

                    Ok(())
                }
            }
        }
        cli::Command::Version => unreachable!(),
        cli::Command::Login => unreachable!(),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    match _main().await {
        Ok(_) => Ok(()),
        Err(e) => {
            eprintln!("{}", e.to_string().red());
            if std::env::var("RUST_BACKTRACE").is_ok() {
                return Err(e);
            }
            std::process::exit(1);
        }
    }
}
