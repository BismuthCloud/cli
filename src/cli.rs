use std::{path::PathBuf, str::FromStr};

use clap::{Args, Parser, Subcommand};
use url::Url;

/// The CLI for Bismuth Cloud
#[derive(Debug, Parser)]
pub struct Cli {
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

#[derive(Debug, Args)]
pub struct GlobalOpts {
    #[arg(long, default_value = "https://api.bismuth.cloud")]
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
        #[clap(short, long)]
        project: IdOrName,
        #[clap(short, long)]
        feature: IdOrName,
        #[clap(subcommand)]
        command: KVCommand,
    },
    /// Interact with blob (file) storage
    Blob {
        #[clap(short, long)]
        project: IdOrName,
        #[clap(short, long)]
        feature: IdOrName,
        #[clap(subcommand)]
        command: BlobCommand,
    },
    /// Run SQL queries against a feature's database
    SQL {
        #[clap(short, long)]
        project: IdOrName,
        #[clap(short, long)]
        feature: IdOrName,
        #[clap(subcommand)]
        command: SQLCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum ProjectCommand {
    /// List all projects
    List,
    /// Create a new project
    Create {
        #[clap(short, long)]
        name: String,
    },
    /// Create a new Bismuth project, and import an existing Git repository into it
    Import {
        /// The name of the new project
        #[clap(short, long)]
        name: String,
        /// The path to the Git repository to import. Defaults to the current directory.
        #[clap(short, long)]
        repo: Option<PathBuf>,
    },
    /// Clone the project for local development
    Clone {
        #[clap(short, long)]
        project: IdOrName,
        /// The target directory to clone the project into. Defaults to the project name.
        outdir: Option<PathBuf>,
    },
    /// Delete a project
    Delete {
        #[clap(short, long)]
        project: IdOrName,
    },
}

#[derive(Debug, Subcommand)]
pub enum FeatureCommand {
    /// List all features in a project
    List {
        #[clap(short, long)]
        project: IdOrName,
    },
    /// Manage feature configuration
    Config {
        #[clap(short, long)]
        project: IdOrName,
        #[clap(short, long)]
        feature: IdOrName,
        #[clap(subcommand)]
        command: FeatureConfigCommand,
    },
    /// Deploy a feature to the cloud
    Deploy {
        #[clap(short, long)]
        project: IdOrName,
        #[clap(short, long)]
        feature: IdOrName,
    },
    /// Teardown a feature
    Teardown {
        #[clap(short, long)]
        project: IdOrName,
        #[clap(short, long)]
        feature: IdOrName,
    },
    /// Get the URL for a deployed feature
    GetInvokeURL {
        #[clap(short, long)]
        project: IdOrName,
        #[clap(short, long)]
        feature: IdOrName,
    },
    /// Get logs from a deployment
    Logs {
        #[clap(short, long)]
        project: IdOrName,
        #[clap(short, long)]
        feature: IdOrName,
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
    Get { key: String },
    Set { key: String, value: String },
    Delete { key: String },
}

#[derive(Debug, Subcommand)]
pub enum BlobCommand {
    List,
    Create {
        key: String,
        #[clap(flatten)]
        value: LiteralOrFile,
    },
    Get {
        key: String,
        /// The path to write the blob to. Defaults to writing to stdout.
        output: Option<PathBuf>,
    },
    Set {
        key: String,
        #[clap(flatten)]
        value: LiteralOrFile,
    },
    Delete {
        key: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum SQLCommand {
    Query {
        #[clap(flatten)]
        query: LiteralOrFile,
    },
}
