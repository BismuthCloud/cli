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
            .map(|i| IdOrName::Id(i))
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
            // tODO: nice error message
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
    #[arg(long, default_value = std::env::var("BISMUTH_API").unwrap_or("https://api.bismuth.cloud".to_string()))]
    pub api_url: Url,

    #[arg(long, default_value = default_config_file().into_os_string())]
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
    KV {
        #[clap(subcommand)]
        command: KVCommand,
    },
    /// Interact with blob (file) storage
    Blob {
        #[clap(subcommand)]
        command: BlobCommand,
    },
    /// Run SQL queries against a feature's database
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
    Deploy {
        #[clap(flatten)]
        feature: FeatureRef,
        #[clap(long, default_value = "false")]
        no_wait: bool,
        #[clap(long, default_value = "15")]
        timeout: u64,
    },
    /// Get the status of a deployment. Alias of `feature deploy-status`.
    DeployStatus {
        #[clap(flatten)]
        feature: FeatureRef,
    },
    /// Teardown a feature. Alias of `feature teardown`.
    Teardown {
        #[clap(flatten)]
        feature: FeatureRef,
    },
    /// Get the URL for a deployed feature. Alias of `feature get-url`.
    GetURL {
        #[clap(flatten)]
        feature: FeatureRef,
    },
    /// Get logs from a deployment. Alias of `feature logs`.
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
        /// The project/feature to work on.
        /// If not specified, attempt to find based on the provided (or current) directory.
        feature: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConfigureCommand {
    #[clap(name = "openrouter")]
    /// OAuth via OpenRouter.
    /// Required to use chat on free tier.
    OpenRouter {},
}

#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
pub struct ImportSource {
    /// The path to the Git repository to import. Defaults to the current directory.
    pub repo: Option<PathBuf>,

    /// Import a repository from GitHub.
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
    /// Link a project to a GitHub repository.
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
    Config {
        #[clap(flatten)]
        feature: FeatureRef,
        #[clap(subcommand)]
        command: FeatureConfigCommand,
    },
    /// Deploy project/feature to the cloud
    Deploy {
        #[clap(flatten)]
        feature: FeatureRef,
        #[clap(long, default_value = "false")]
        no_wait: bool,
        #[clap(long, default_value = "15")]
        timeout: u64,
    },
    /// Get the status of a deployment
    DeployStatus {
        #[clap(flatten)]
        feature: FeatureRef,
    },
    /// Teardown a feature
    Teardown {
        #[clap(flatten)]
        feature: FeatureRef,
    },
    /// Get the URL for a deployed feature
    GetURL {
        #[clap(flatten)]
        feature: FeatureRef,
    },
    /// Get logs from a deployment
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
