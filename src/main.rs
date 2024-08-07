use anyhow::{anyhow, Result};
use clap::Parser as _;
use colored::Colorize;
use futures::{StreamExt as _, TryStreamExt};
use log::debug;
use reqwest_eventsource::EventSource;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write as _;
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

#[derive(Debug, Serialize, Deserialize)]
struct Config {
    organization_id: u64,
    token: String,
}

struct APIClient {
    client: reqwest::Client,
    base_url: Url,
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

async fn resolve_project_id(client: &APIClient, id: &IdOrName) -> Result<api::Project> {
    let project_id = match id {
        cli::IdOrName::Name(name) => {
            let get_projects = client
                .get("/projects/list")
                .send()
                .await?
                .error_body_for_status()
                .await?;
            let projects: api::ListProjectsResponse = get_projects.json().await?;
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

fn project_clone(project: &api::Project, api_url: &Url, outdir: Option<&Path>) -> Result<PathBuf> {
    let mut auth_url = api_url.clone();
    auth_url.set_password(Some(&project.clone_token)).unwrap();

    let outdir = outdir
        .map(|p| p.to_owned())
        .unwrap_or(PathBuf::from(&project.name));
    debug!("Cloning project to {:?}", outdir);

    Command::new("git")
        .arg("clone")
        .arg(
            auth_url
                .join(&format!("/git/{}", project.hash))?
                .to_string(),
        )
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

    Ok(outdir)
}

async fn feature_deploy(
    project: &api::Project,
    feature: &api::Feature,
    client: &APIClient,
) -> Result<()> {
    client
        .post(&format!(
            "/projects/{}/features/{}/deploy",
            project.id, feature.id
        ))
        .send()
        .await?
        .error_body_for_status()
        .await?;
    Ok(())
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
    println!("Status: {}", status.status);
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
) -> Result<()> {
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
    println!("{}", resp.url);
    Ok(())
}

async fn feature_logs(
    project: &api::Project,
    feature: &api::Feature,
    follow: bool,
    client: &APIClient,
) -> Result<()> {
    let mut es = EventSource::new(client.get(&format!(
        "/projects/{}/features/{}/logs",
        project.id, feature.id
    )))?;

    while let Some(event) = es.next().await {
        match event {
            Ok(reqwest_eventsource::Event::Open) => {}
            Ok(reqwest_eventsource::Event::Message(message)) => print!("{}", message.data),
            Err(err) => {
                eprintln!("Error streaming logs: {}", err);
                es.close();
            }
        }
    }
    Ok(())
}

fn oidc_url(api_url: &Url) -> Url {
    let base = match api_url.host_str() {
        Some("api-staging.bismuth.cloud") => {
            Url::parse("https://auth-staging.bismuth.cloud/").unwrap()
        }
        Some("localhost") => Url::parse("http://localhost:8543/").unwrap(),
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
        oidc_url(api_url)
            .join(&format!("auth?client_id=cli&redirect_uri=http://localhost:{}/&scope=openid&response_type=code&response_mode=query&prompt=login", port))
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

        let organization = loop {
            println!("Select an organization:");
            for (i, org) in organizations.iter().enumerate() {
                println!("{}: {}", i + 1, org.name);
            }
            print!("> ");
            std::io::stdout().flush()?;

            let mut org_selector = String::new();
            std::io::stdin().read_line(&mut org_selector)?;

            if let Ok(org_idx) = org_selector.trim().parse::<usize>() {
                if org_idx > organizations.len() {
                    eprintln!("Invalid organization index");
                    continue;
                }
                break &organizations[org_idx - 1];
            } else {
                match organizations
                    .iter()
                    .find(|org| org.name == org_selector.trim())
                {
                    Some(org) => break org,
                    None => {
                        eprintln!("No such organization");
                        continue;
                    }
                }
            };
        };

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
                client
                    .post("/projects/upsert")
                    .json(&api::CreateProjectRequest { name: name.clone() })
                    .send()
                    .await?
                    .error_body_for_status()
                    .await?;
                Ok(())
            }
            cli::ProjectCommand::Import { name, repo } => {
                let repo = repo.clone().unwrap_or_else(|| PathBuf::from("."));
                if !repo.exists() {
                    return Err(anyhow!("Repo does not exist"));
                }
                if !repo.join(".git").is_dir() {
                    return Err(anyhow!("Directory is not a git repository"));
                }
                let project: api::Project = client
                    .post("/projects/upsert")
                    .json(&api::CreateProjectRequest { name: name.clone() })
                    .send()
                    .await?
                    .error_body_for_status()
                    .await?
                    .json()
                    .await?;
                let mut git_url = args
                    .global
                    .api_url
                    .clone()
                    .join(&format!("/git/{}", project.hash))?;
                git_url.set_password(Some(&project.clone_token)).unwrap();
                Command::new("git")
                    .arg("-C")
                    .arg(repo.as_path())
                    .arg("remote")
                    .arg("add")
                    .arg("bismuth")
                    .arg(git_url.to_string())
                    .output()
                    .map_err(|e| anyhow!(e))
                    .and_then(|o| {
                        if o.status.success() {
                            Ok(())
                        } else {
                            Err(anyhow!("Failed to add bismuth remote"))
                        }
                    })?;
                Command::new("git")
                    .arg("-C")
                    .arg(repo.as_path())
                    .arg("push")
                    .arg("--force")
                    .arg("--set-upstream")
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
                    "You can also deploy this project with `bismuth feature deploy --project {} --feature main` after creating an entrypoint.",
                    project.id
                );
                Ok(())
            }
            cli::ProjectCommand::Clone { project, outdir } => {
                let project = resolve_project_id(&client, project).await?;
                project_clone(&project, &args.global.api_url, outdir.as_deref())?;
                Ok(())
            }
            cli::ProjectCommand::Delete { project } => {
                let project = resolve_project_id(&client, project).await?;
                print!(
                    "Are you sure you want to delete project {}? [y/N] ",
                    project.name
                );
                std::io::stdout().flush()?;
                let mut confirm = String::new();
                std::io::stdin().read_line(&mut confirm)?;
                if confirm.trim().to_lowercase() != "y" {
                    return Ok(());
                }
                client
                    .delete(&format!("/projects/{}", project.id))
                    .send()
                    .await?
                    .error_body_for_status()
                    .await?;
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
            cli::FeatureCommand::Deploy { feature } => {
                let (project_name, feature_name) = feature.split();
                let project = resolve_project_id(&client, &project_name).await?;
                let feature = resolve_feature_id(&client, &project, &feature_name).await?;
                feature_deploy(&project, &feature, &client).await
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
                feature_get_url(&project, &feature, &client).await
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
        // Convenience aliases
        cli::Command::Deploy { feature } => {
            let (project_name, feature_name) = feature.split();
            let project = resolve_project_id(&client, &project_name).await?;
            let feature = resolve_feature_id(&client, &project, &feature_name).await?;
            feature_deploy(&project, &feature, &client).await
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
            feature_get_url(&project, &feature, &client).await
        }
        cli::Command::Logs { feature, follow } => {
            let (project_name, feature_name) = feature.split();
            let project = resolve_project_id(&client, &project_name).await?;
            let feature = resolve_feature_id(&client, &project, &feature_name).await?;
            feature_logs(&project, &feature, *follow, &client).await
        }
        cli::Command::Version => unreachable!(),
        cli::Command::Login => unreachable!(),
    }
}
