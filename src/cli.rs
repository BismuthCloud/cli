use std::{path::PathBuf, str::FromStr};

use clap::{Args, Parser, Subcommand};
use url::Url;

/// The CLI for Bismuth Cloud
#[derive(Debug, Parser)]
pub struct Cli {
    #[arg(long, hide = true)]
    pub markdown_help: bool,

    #[clap(flatten)]
    pub global: GlobalOpts,

    #[clap(subcommand)]
    pub command: Command,
}

pub fn default_config_file() -> PathBuf {
    if let Some(config_dir) = dirs::config_dir() {
        config_dir.join("bismuth.json")
    } else {
        dirs::home_dir().unwrap().join(".config/bismuth.json")
    }
}

#[derive(Debug, Clone)]
pub enum IdOrName {
    Id(u64),
    Name(String),
}

impl FromStr for IdOrName {
    type Err = &'static str;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(s.parse::<u64>()
            .map(IdOrName::Id)
            .unwrap_or_else(|_| IdOrName::Name(s.to_string())))
    }
}

#[derive(Clone, Debug, Args)]
pub struct FeatureRef {
    #[clap(help = "The feature to operate on, specified as 'project/feature'")]
    pub feature: String,
}

impl FromStr for FeatureRef {
    type Err = &'static str;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(FeatureRef {
            feature: s.to_string(),
        })
    }
}

impl FeatureRef {
    pub fn as_str(&self) -> &str {
        &self.feature
    }
    pub fn split(&self) -> (IdOrName, IdOrName) {
        let parts: Vec<&str> = self.feature.splitn(2, '/').collect();
        if parts.len() != 2 {
            // TODO: nice error message
            panic!(
                "Invalid feature reference (use `project/feature`): {}",
                self.feature
            );
        }
        (
            IdOrName::Name(parts[0].to_string()),
            IdOrName::Name(parts[1].to_string()),
        )
    }
}

#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
pub struct LiteralOrFile {
    /// A literal value to use
    #[clap(long)]
    pub literal: Option<String>,

    /// The path to a file to use
    #[clap(long)]
    pub file: Option<PathBuf>,
}

#[derive(Debug, Clone, Args)]
pub struct GlobalOpts {
    #[arg(long, hide = true, default_value = std::env::var("BISMUTH_API").unwrap_or("https://api.bismuth.cloud".to_string()))]
    pub api_url: Url,

    #[arg(long, hide = true, default_value = default_config_file().into_os_string())]
    pub config_file: PathBuf,

    #[command(flatten)]
    pub verbose: clap_verbosity_flag::Verbosity,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Login to Bismuth Cloud
    Login,
    /// Show the CLI version
    Version,
    /// Configure the CLI
    Configure {
        #[clap(subcommand)]
        command: ConfigureCommand,
    },
    /// Manage projects
    Project {
        #[clap(subcommand)]
        command: ProjectCommand,
    },
    /// Manage features
    Feature {
        #[clap(subcommand)]
        command: FeatureCommand,
    },
    /// Interact with key-value storage
    #[clap(hide = true)]
    KV {
        #[clap(subcommand)]
        command: KVCommand,
    },
    /// Interact with blob (file) storage
    #[clap(hide = true)]
    Blob {
        #[clap(subcommand)]
        command: BlobCommand,
    },
    /// Run SQL queries against a feature's database
    #[clap(hide = true)]
    SQL {
        #[clap(subcommand)]
        command: SQLCommand,
    },
    Billing {
        #[clap(subcommand)]
        command: BillingCommand,
    },
    /// Create a new Bismuth project, and import an existing Git repository into it.
    /// Alias of `project import`.
    Import(ImportSource),
    /// Deploy a feature to the cloud. Alias of `feature deploy`.
    #[clap(hide = true)]
    Deploy {
        #[clap(flatten)]
        feature: FeatureRef,
        #[clap(long, default_value = "false")]
        no_wait: bool,
        #[clap(long, default_value = "15")]
        timeout: u64,
    },
    /// Get the status of a deployment. Alias of `feature deploy-status`.
    #[clap(hide = true)]
    DeployStatus {
        #[clap(flatten)]
        feature: FeatureRef,
    },
    /// Teardown a feature. Alias of `feature teardown`.
    #[clap(hide = true)]
    Teardown {
        #[clap(flatten)]
        feature: FeatureRef,
    },
    /// Get the URL for a deployed feature. Alias of `feature get-url`.
    #[clap(hide = true)]
    GetURL {
        #[clap(flatten)]
        feature: FeatureRef,
    },
    /// Get logs from a deployment. Alias of `feature logs`.
    #[clap(hide = true)]
    Logs {
        #[clap(flatten)]
        feature: FeatureRef,
        /// Continuously tail the log stream. Equivalent to `tail -f`.
        #[clap(short, long, default_value_t = false)]
        follow: bool,
    },
    /// Interact with the Bismuth AI
    Chat {
        /// The cloned repository.
        /// If not specified, checks if the current directory is a clone of the project.
        /// If not, the repo is automatically cloned into a new folder in the current directory.
        #[clap(long)]
        repo: Option<PathBuf>,
        /// Specify a chat session name to use.
        #[clap(short, long = "session")]
        session_name: Option<String>,
        #[clap(subcommand)]
        command: Option<ChatSubcommand>,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConfigureCommand {
    #[clap(name = "openrouter")]
    /// OAuth via OpenRouter.
    /// Required to use chat on free tier.
    OpenRouter {},
}

#[derive(Debug, Subcommand)]
pub enum ChatSubcommand {
    ListSessions,
    RenameSession { old_name: String, new_name: String },
    DeleteSession { name: String },
}

#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
pub struct ImportSource {
    /// The path to the Git repository to import. Defaults to the current directory.
    pub repo: Option<PathBuf>,

    /// Import a repository from GitHub
    #[arg(long)]
    pub github: bool,
}

#[derive(Debug, Subcommand)]
pub enum ProjectCommand {
    /// List all projects
    List,
    /// Create a new project
    Create { name: String },
    /// Create a new Bismuth project, and import an existing Git repository into it
    Import(ImportSource),
    /// Add the bismuth git remote to an existing repository
    #[clap(hide = true)]
    AddRemote {
        project: IdOrName,
        /// The path to the Git repository to set the bismuth remote on
        repo: Option<PathBuf>,
    },
    Upload {
        project: IdOrName,
        /// The path to the Git repository to upload
        repo: Option<PathBuf>,
    },
    /// Link a project to a GitHub repository
    #[clap(hide = true)]
    Link {
        /// The project to link
        project: IdOrName,
    },
    /// Clone the project for local development
    Clone {
        project: IdOrName,
        /// The target directory to clone the project into. Defaults to the project name.
        outdir: Option<PathBuf>,
    },
    /// Delete a project
    Delete { project: IdOrName },
}

#[derive(Debug, Subcommand)]
pub enum FeatureCommand {
    /// List all features in a project
    List { project: IdOrName },
    /// Manage feature configuration
    #[clap(hide = true)]
    Config {
        #[clap(flatten)]
        feature: FeatureRef,
        #[clap(subcommand)]
        command: FeatureConfigCommand,
    },
    /// Deploy project/feature to the cloud
    #[clap(hide = true)]
    Deploy {
        #[clap(flatten)]
        feature: FeatureRef,
        #[clap(long, default_value = "false")]
        no_wait: bool,
        #[clap(long, default_value = "15")]
        timeout: u64,
    },
    /// Get the status of a deployment
    #[clap(hide = true)]
    DeployStatus {
        #[clap(flatten)]
        feature: FeatureRef,
    },
    /// Teardown a feature
    #[clap(hide = true)]
    Teardown {
        #[clap(flatten)]
        feature: FeatureRef,
    },
    /// Get the URL for a deployed feature
    #[clap(hide = true)]
    GetURL {
        #[clap(flatten)]
        feature: FeatureRef,
    },
    /// Get logs from a deployment
    #[clap(hide = true)]
    Logs {
        #[clap(flatten)]
        feature: FeatureRef,
        /// Continuously tail the log stream. Equivalent to `tail -f`.
        #[clap(short, long, default_value_t = false)]
        follow: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum FeatureConfigCommand {
    Get { key: Option<String> },
    Set { key: String, value: String },
}

#[derive(Debug, Subcommand)]
#[clap(hide = true)]
pub enum KVCommand {
    Get {
        #[clap(flatten)]
        feature: FeatureRef,
        key: String,
    },
    Set {
        #[clap(flatten)]
        feature: FeatureRef,
        key: String,
        value: String,
    },
    Delete {
        #[clap(flatten)]
        feature: FeatureRef,
        key: String,
    },
}

#[derive(Debug, Subcommand)]
#[clap(hide = true)]
pub enum BlobCommand {
    List {
        #[clap(flatten)]
        feature: FeatureRef,
    },
    Create {
        #[clap(flatten)]
        feature: FeatureRef,
        key: String,
        #[clap(flatten)]
        value: LiteralOrFile,
    },
    Get {
        #[clap(flatten)]
        feature: FeatureRef,
        key: String,
        /// The path to write the blob to. Defaults to writing to stdout.
        output: Option<PathBuf>,
    },
    Set {
        #[clap(flatten)]
        feature: FeatureRef,
        key: String,
        #[clap(flatten)]
        value: LiteralOrFile,
    },
    Delete {
        #[clap(flatten)]
        feature: FeatureRef,
        key: String,
    },
}

#[derive(Debug, Subcommand)]
#[clap(hide = true)]
pub enum SQLCommand {
    Query {
        #[clap(flatten)]
        feature: FeatureRef,
        #[clap(flatten)]
        query: LiteralOrFile,
    },
}

#[derive(Debug, Subcommand)]
pub enum BillingCommand {
    /// Open Stripe subscription management page
    ManageSubscription,
}
