use anyhow::{anyhow, Result};
use api::Project;
use clap::Parser as _;
use futures::{StreamExt as _, TryStreamExt};
use log::debug;
use reqwest_eventsource::EventSource;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Command;
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

    dbg!(auth_url
        .join(&format!("/git/{}", project.hash))?
        .to_string());

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
        "Go to the following URL to authenticate: \x1b[1;34m{}\x1b[0m",
        oidc_url(api_url).join(&format!("auth?client_id=cli&redirect_uri=http://localhost:{}/&scope=openid&response_type=code&response_mode=query&prompt=login", port)).unwrap()
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

#[tokio::main]
async fn main() -> Result<()> {
    let args = Cli::parse();
    env_logger::Builder::new()
        .filter_level(args.global.verbose.log_level_filter())
        .init();

    if let cli::Command::Login = args.command {
        debug!("Starting login flow");

        let token = oidc_server(&args.global.api_url).await?;

        let client = APIClient::new(&args.global.api_url, &token)?;
        let organizations: Vec<api::Organization> =
            client.get("/organizations").send().await?.json().await?;

        println!("Select an organization:");
        for (i, org) in organizations.iter().enumerate() {
            println!("{}: {}", i + 1, org.name);
        }
        print!("> ");
        std::io::stdout().flush()?;

        let mut org_selector = String::new();
        std::io::stdin().read_line(&mut org_selector)?;
        let organization = if let Ok(org_idx) = org_selector.trim().parse::<usize>() {
            if org_idx > organizations.len() {
                return Err(anyhow!("Invalid organization index"));
            }
            &organizations[org_idx - 1]
        } else {
            organizations
                .iter()
                .find(|org| org.name == org_selector.trim())
                .ok_or_else(|| anyhow!("No such organization"))?
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
                let project: Project = client
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
            cli::FeatureCommand::Config {
                project,
                feature,
                command,
            } => {
                let project = resolve_project_id(&client, project).await?;
                let feature = resolve_feature_id(&client, &project, feature).await?;

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
            cli::FeatureCommand::Deploy { project, feature } => {
                let project = resolve_project_id(&client, project).await?;
                let feature = resolve_feature_id(&client, &project, feature).await?;

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
            cli::FeatureCommand::Teardown { project, feature } => {
                let project = resolve_project_id(&client, project).await?;
                let feature = resolve_feature_id(&client, &project, feature).await?;

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
            cli::FeatureCommand::GetInvokeURL { project, feature } => {
                let project = resolve_project_id(&client, project).await?;
                let feature = resolve_feature_id(&client, &project, feature).await?;

                let resp = client
                    .get(&format!(
                        "/projects/{}/features/{}/invoke_url",
                        project.id, feature.id
                    ))
                    .send()
                    .await?
                    .error_body_for_status()
                    .await?;
                println!("{}", resp.text().await?);
                Ok(())
            }
            cli::FeatureCommand::Logs {
                project,
                feature,
                follow,
            } => {
                let project = resolve_project_id(&client, project).await?;
                let feature = resolve_feature_id(&client, &project, feature).await?;

                feature_logs(&project, &feature, *follow, &client).await
            }
        },
        cli::Command::KV {
            project,
            feature,
            command,
        } => {
            let project = resolve_project_id(&client, project).await?;
            let feature = resolve_feature_id(&client, &project, feature).await?;
            match command {
                cli::KVCommand::Get { key } => {
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
                cli::KVCommand::Set { key, value } => {
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
                cli::KVCommand::Delete { key } => {
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
            }
        }
        cli::Command::Blob {
            project,
            feature,
            command,
        } => {
            let project = resolve_project_id(&client, project).await?;
            let feature = resolve_feature_id(&client, &project, feature).await?;
            match command {
                cli::BlobCommand::List => {
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
                cli::BlobCommand::Create { key, value } => {
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
                cli::BlobCommand::Get { key, output } => {
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
                cli::BlobCommand::Set { key, value } => {
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
                cli::BlobCommand::Delete { key } => {
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
            }
        }
        cli::Command::Login => unreachable!(),
    }
}
