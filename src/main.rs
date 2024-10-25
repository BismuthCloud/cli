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
use std::io::{BufRead, BufReader, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Command;
use std::time::Duration;
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _};
use tokio_util::io::StreamReader;
use url::Url;

mod api;
mod cli;
use cli::{Cli, IdOrName};
mod chat;
use chat::start_chat;

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
    return Ok(confirm == "y");
}

async fn press_any_key() -> Result<()> {
    std::io::stdout().flush()?;
    let termios = termios::Termios::from_fd(0).unwrap();
    let mut new_termios = termios.clone();
    new_termios.c_lflag &= !(termios::ICANON | termios::ECHO);
    termios::tcsetattr(0, termios::TCSANOW, &mut new_termios).unwrap();
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

fn set_bismuth_remote(repo: &Path, project: &api::Project) -> Result<()> {
    let mut git_url = GLOBAL_OPTS
        .get()
        .unwrap()
        .api_url
        .clone()
        .join(&format!("/git/{}", project.hash))?;
    git_url.set_username("git").unwrap();
    git_url.set_password(Some(&project.clone_token)).unwrap();

    let git_repo = git2::Repository::open(repo)?;
    match git_repo.find_remote("bismuth") {
        Ok(_) => {
            debug!("Updating existing bismuth remote URL");
            git_repo.remote_set_url("bismuth", &git_url.to_string())?;
        }
        Err(_) => {
            debug!("Adding new bismuth remote");
            git_repo.remote("bismuth", &git_url.to_string())?;
        }
    }
    Ok(())
}

async fn get_project_and_feature_for_repo(
    client: &APIClient,
    repo: &Path,
) -> Result<(api::Project, api::Feature)> {
    if !repo.join(".git").is_dir() {
        return Err(anyhow!(
            "Unable to determine project and feature (path is not a git repository)"
        ));
    }
    let repo = git2::Repository::open(repo)?;
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
            return Err(anyhow!(
                "Unable to determine feature (current branch is not pushed?)"
            ));
        }
    }
    return Err(anyhow!(
        "Unable to determine project (no matching projects found)"
    ));
}

async fn project_import(source: &cli::ImportSource, client: &APIClient) -> Result<()> {
    let mut gh_repos = client
        .get("/projects/connect/github/repo")
        .send()
        .await?
        .error_body_for_status()
        .await?
        .json::<Vec<api::GitHubRepo>>()
        .await?;

    if !source.github {
        let repo = source.repo.clone().unwrap_or(PathBuf::from("."));
        if !repo.exists() {
            return Err(anyhow!("Repo does not exist"));
        }
        let repo = std::fs::canonicalize(repo)?;
        if !repo.join(".git").is_dir() {
            return Err(anyhow!("Directory is not a git repository"));
        }

        let git_repo = git2::Repository::open(repo.as_path())?;
        if let Ok(remote) = git_repo.find_remote("origin") {
            let remote_url = remote.url().unwrap().to_string();
            if remote_url.contains("github.com") {
                // org/repo
                // http will have github.com/... and ssh will have github.com:...
                // and trim off any .git
                let gh_repo_name =
                    remote_url.split("github.com").last().unwrap()[1..].trim_end_matches(".git");

                println!("This repository is from GitHub.");
                println!(
                    "If you own this repository, it's recommended to use the Bismuth GitHub App to link it."
                );
                if confirm("Would you like to do this?", true).await? {
                    if gh_repos.is_empty()
                        || !gh_repos
                            .iter()
                            .any(|r| r.repo.to_lowercase() == gh_repo_name)
                    {
                        let orig_gh_repos = gh_repos.clone();
                        println!("Press any key to open the installation page.");
                        press_any_key().await?;
                        open::that_detached(github_app_url(&client.base_url))?;
                        print!("Waiting for app install");
                        std::io::stdout().flush()?;
                        loop {
                            tokio::time::sleep(Duration::from_secs(2)).await;
                            print!(".");
                            std::io::stdout().flush()?;
                            gh_repos = client
                                .get("/projects/connect/github/repo")
                                .send()
                                .await?
                                .error_body_for_status()
                                .await?
                                .json::<Vec<api::GitHubRepo>>()
                                .await?;
                            if gh_repos != orig_gh_repos {
                                break;
                            }
                        }
                        println!();
                    }
                    let mut gh_repo = gh_repos
                        .iter()
                        .find(|r| r.repo.to_lowercase() == gh_repo_name);
                    if gh_repo.is_none() {
                        println!("I can't find this repository in the list of repositories the GitHub App has access to.");
                        gh_repo = Some(choice(&gh_repos, "repository").await?);
                    }
                    let project: api::Project = client
                        .post("/projects")
                        .json(&api::CreateProjectRequest::Repo(gh_repo.unwrap().clone()))
                        .send()
                        .await?
                        .error_body_for_status()
                        .await?
                        .json()
                        .await?;
                    set_bismuth_remote(&repo, &project)?;
                    println!("Successfully imported {}!", gh_repo.unwrap().repo);
                    return Ok(());
                }
            }
        }

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

        set_bismuth_remote(&repo, &project)?;
        let branch_name = git_repo.head()?.shorthand().unwrap().to_string();

        Command::new("git")
            .arg("-C")
            .arg(repo.as_path())
            .arg("push")
            .arg("--force")
            .arg("bismuth")
            .arg("--all")
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
        println!(
            "Successfully imported {} to project {}!",
            repo.canonicalize()?.as_path().display(),
            project.name
        );
        println!("You can now push to Bismuth with `git push bismuth` in this repository.");
        println!(
                    "You can also deploy this project with `bismuth deploy '{}/{}'` after creating an entrypoint.",
                    project.name, branch_name
                );
        Ok(())
    } else {
        if gh_repos.is_empty() {
            println!("You'll need to install the GitHub App first.");
            println!("Press any key to open the installation page.");
            press_any_key().await?;
            open::that_detached(github_app_url(&client.base_url))?;
            print!("Waiting for app install");
            std::io::stdout().flush()?;
            loop {
                tokio::time::sleep(Duration::from_secs(2)).await;
                print!(".");
                std::io::stdout().flush()?;
                gh_repos = client
                    .get("/projects/connect/github/repo")
                    .send()
                    .await?
                    .error_body_for_status()
                    .await?
                    .json::<Vec<api::GitHubRepo>>()
                    .await?;
                if !gh_repos.is_empty() {
                    break;
                }
            }
        }
        let repo = choice(&gh_repos, "repository").await?;
        client
            .post("/projects")
            .json(&api::CreateProjectRequest::Repo(repo.clone()))
            .send()
            .await?
            .error_body_for_status()
            .await?;
        Ok(())
    }
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
    let repo = git2::Repository::open(repo)?;
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
        .ok_or(anyhow!("No branch in origin?"))?;
    let bismuth_commit = repo
        .find_branch(
            &format!("bismuth/{}", &branch_name),
            git2::BranchType::Remote,
        )?
        .get()
        .target()
        .ok_or(anyhow!("No branch in bismuth?"))?;

    return Ok(origin_commit != bismuth_commit);
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
    println!(
        "Go to the following URL to authenticate: {}",
        api_url
            .join(&format!("auth/cli?port={}", port))
            .unwrap()
            .to_string()
            .blue()
            .bold()
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
                println!("{}", "A newer version of the CLI is available!".yellow());
                println!(
                    "{}",
                    "Get it at https://github.com/BismuthCloud/cli/releases".yellow()
                );
            }
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
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
        let organizations: Vec<api::Organization> =
            client.get("/organizations").send().await?.json().await?;

        let organization = choice(&organizations, "organization").await?;

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
            cli::ProjectCommand::Import(source) => project_import(source, &client).await,
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
                    println!("Press any key to open the installation page.");
                    press_any_key().await?;
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
                    "Successfully linked {} to https://github.com/{}",
                    updated_project.name,
                    updated_project.github_repo.unwrap(),
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
                let url = client
                    .get("/billing/manage")
                    .send()
                    .await?
                    .error_body_for_status()
                    .await?
                    .text()
                    .await?;
                println!("Opening Stripe subscription management page");
                open::that_detached(url)?;
                Ok(())
            }
        },
        // Convenience aliases
        cli::Command::Import(source) => project_import(source, &client).await,
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
            feature,
            repo,
            session,
        } => {
            let current_user: api::User = client
                .get("/../../auth/me")
                .send()
                .await?
                .error_body_for_status()
                .await?
                .json()
                .await?;

            let (project, feature) = match feature {
                Some(feature) => {
                    let (project_name, feature_name) = cli::FeatureRef {
                        feature: feature.to_string(),
                    }
                    .split();
                    let project = resolve_project_id(&client, &project_name).await?;
                    let feature = resolve_feature_id(&client, &project, &feature_name).await?;
                    (project, feature)
                }
                None => {
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
                    get_project_and_feature_for_repo(&client, &repo_path).await?
                }
            };
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
                        std::env::current_dir().unwrap()
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
                        Err(anyhow!("Failed to fetch ({})", o.status))
                    }
                })?;

            start_chat(
                &current_user,
                &project,
                &feature,
                &session,
                &repo_path,
                &client,
            )
            .await
        }
        cli::Command::Version => unreachable!(),
        cli::Command::Login => unreachable!(),
    }
}
