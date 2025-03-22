use anyhow::{anyhow, Result};
use clap::Parser as _;
use colored::Colorize;
use futures::{StreamExt as _, TryStreamExt};
use log::debug;
use once_cell::sync::OnceCell;
use serde::{Deserialize, Serialize};
use serde_json::json;
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
mod tree;
use cli::{Cli, IdOrName};
mod chat;
use chat::start_chat;
mod bismuth_toml;

static GLOBAL_OPTS: OnceCell<cli::GlobalOpts> = OnceCell::new();

#[derive(Debug, Serialize, Deserialize)]
struct Config {
    api_url: String,
    oidc_url: String,
    daneel_url: String,
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
        debug!("GET {}", path);
        self.client
            .get(self.base_url.join(path.trim_start_matches('/')).unwrap())
    }
    fn post(&self, path: &str) -> reqwest::RequestBuilder {
        debug!("POST {}", path);
        self.client
            .post(self.base_url.join(path.trim_start_matches('/')).unwrap())
    }
    fn put(&self, path: &str) -> reqwest::RequestBuilder {
        debug!("PUT {}", path);
        self.client
            .put(self.base_url.join(path.trim_start_matches('/')).unwrap())
    }
    fn delete(&self, path: &str) -> reqwest::RequestBuilder {
        debug!("DELETE {}", path);
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

macro_rules! can_launch_browser {
    () => {
        cfg!(target_os = "macos") || cfg!(target_os = "windows")
    };
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

async fn enter_with_default(prompt: &str, default: &str) -> Result<String> {
    print!("{} [{}]: ", prompt, default);
    std::io::stdout().flush()?;
    let input = std::io::stdin()
        .lines()
        .next()
        .unwrap_or(Ok("".to_string()))?;
    if input.trim().is_empty() {
        Ok(default.to_string())
    } else {
        Ok(input)
    }
}

#[cfg(not(target_os = "windows"))]
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

#[cfg(target_os = "windows")]
async fn press_any_key(msg: &str) -> Result<()> {
    use windows::Win32::System::Console::{
        GetConsoleMode, GetStdHandle, SetConsoleMode, CONSOLE_MODE, ENABLE_ECHO_INPUT,
        ENABLE_LINE_INPUT, STD_INPUT_HANDLE,
    };

    println!("{}", msg);
    std::io::stdout().flush()?;

    let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) }?;

    let mut original_mode = CONSOLE_MODE::default();
    unsafe { GetConsoleMode(handle, &mut original_mode) };

    let new_mode = CONSOLE_MODE(original_mode.0 & !(ENABLE_LINE_INPUT.0 | ENABLE_ECHO_INPUT.0));
    unsafe { SetConsoleMode(handle, new_mode) };

    let mut buffer = [0u8; 1];
    std::io::stdin().read_exact(&mut buffer)?;

    unsafe { SetConsoleMode(handle, original_mode) };

    Ok(())
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

fn set_bismuth_remote(repo: &Path, base_url: &Url, project: &api::Project) -> Result<()> {
    let mut git_url = base_url.join(&format!("/git/{}", project.hash))?;
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
    set_bismuth_remote(&repo, &client.base_url, &project)?;

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
        .map_err(|e| anyhow!(e))?
        .status
        .success()
    {
        if confirm(
            "Failed to push to Bismuth. Would you like to continue without pushing?",
            true,
        )
        .await?
        {
            println!(
                "{}",
                format!("🎉 Successfully created project {}", project.name).green()
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
            return Err(anyhow!(
                "Failed to push! Hint: you may need to temporarily disable git pre-push hooks."
            ));
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

fn project_clone(base_url: &Url, project: &api::Project, outdir: Option<&Path>) -> Result<PathBuf> {
    let mut auth_url = base_url.clone();
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

#[derive(Debug, Serialize, Deserialize)]
struct Tokens {
    access_token: String,
    // Don't care about the rest
}

async fn oidc_server(api_url: &Url, oidc_url: &Url) -> Result<String> {
    let server = tiny_http::Server::http("localhost:0").map_err(|e| anyhow!(e))?;
    let port = server.server_addr().to_ip().unwrap().port();
    let url = api_url
        .join(&format!("auth/cli?port={}", port))
        .unwrap()
        .to_string();

    if can_launch_browser!() {
        press_any_key("Press any key to open the login page.").await?;
        open::that_detached(url)?;
    } else {
        println!(
            "Go to the following URL to authenticate: {}",
            url.blue().bold()
        );
    }

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
        .post(
            oidc_url
                .join("/realms/bismuth/protocol/openid-connect/token")
                .unwrap(),
        )
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
        .json(&json!({}))
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

    if !cfg!(debug_assertions) && std::env::var("BISMUTH_NO_VERSION_CHECK").is_err() {
        let _ = check_version().await;
    }

    if let cli::Command::Version = args.command {
        println!("Bismuth CLI {}", env!("CARGO_PKG_VERSION"),);
        return Ok(());
    }

    if let cli::Command::Login = args.command {
        debug!("Starting login flow");

        let api_url: Url = enter_with_default("Enter API URL", "http://localhost:8080")
            .await?
            .parse()?;

        let oidc_url: Url = enter_with_default("Enter Keycloak URL", "http://localhost:8543")
            .await?
            .parse()?;

        let daneel_url: Url = enter_with_default("Enter Daneel URL", "ws://localhost:8765")
            .await?
            .parse()?;

        let token = oidc_server(&api_url, &oidc_url).await?;

        let client = APIClient::new(&api_url, &token)?;
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
            api_url: api_url.to_string(),
            oidc_url: oidc_url.to_string(),
            daneel_url: daneel_url.to_string(),
            token: token.to_string(),
            organization_id: organization.id,
        };
        let config_str = serde_json::to_string(&config)?;
        let mut config_file = File::create(&args.global.config_file).await?;
        config_file.write_all(config_str.as_bytes()).await?;
        return Ok(());
    }

    let mut config_file = File::open(&args.global.config_file)
        .await
        .map_err(|_| anyhow!("Failed to open config. Maybe you need to `bismuth login` first?"))?;
    let mut config_str: String = String::new();
    config_file.read_to_string(&mut config_str).await?;
    let config: Config = serde_json::from_str(&config_str)?;

    debug!("Organization ID: {}", config.organization_id);

    let client = APIClient::new(
        &config
            .api_url
            .parse::<Url>()?
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
                project_clone(&client.base_url, &project, None)?;
                Ok(())
            }
            cli::ProjectCommand::Import(args) => project_import(args, &client).await,
            cli::ProjectCommand::AddRemote { project, repo } => {
                let project = resolve_project_id(&client, project).await?;
                let repo = std::fs::canonicalize(repo.clone().unwrap_or(std::env::current_dir()?))?;
                set_bismuth_remote(&repo, &client.base_url, &project)?;
                Ok(())
            }
            cli::ProjectCommand::Upload { project, repo } => {
                let project = resolve_project_id(&client, project).await?;
                let repo = std::fs::canonicalize(repo.clone().unwrap_or(std::env::current_dir()?))?;
                set_bismuth_remote(&repo, &client.base_url, &project)?;
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
                project_clone(&client.base_url, &project, outdir.as_deref())?;
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
        },
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
                                project_clone(&client.base_url, &project, Some(repo))?
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
                                project_clone(&client.base_url, &project, None)?
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
                        &config.daneel_url,
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
        // Convenience aliases
        cli::Command::Import(args) => project_import(args, &client).await,
        // Handled above
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
