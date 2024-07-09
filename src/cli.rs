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
    Login,
    Project {
        #[clap(subcommand)]
        command: ProjectCommand,
    },
    Feature {
        #[clap(subcommand)]
        command: FeatureCommand,
    },
    KV {
        #[clap(short, long)]
        project: IdOrName,
        #[clap(short, long)]
        feature: IdOrName,
        #[clap(subcommand)]
        command: KVCommand,
    },
    Blob {
        #[clap(short, long)]
        project: IdOrName,
        #[clap(short, long)]
        feature: IdOrName,
        #[clap(subcommand)]
        command: BlobCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum ProjectCommand {
    List,
    Create {
        name: String,
    },
    Clone {
        #[clap(short, long)]
        project: IdOrName,
        outdir: Option<PathBuf>,
    },
    Delete {
        #[clap(short, long)]
        project: IdOrName,
    },
}

#[derive(Debug, Subcommand)]
pub enum FeatureCommand {
    List {
        #[clap(short, long)]
        project: IdOrName,
    },
    Config {
        #[clap(short, long)]
        project: IdOrName,
        #[clap(short, long)]
        feature: IdOrName,
        #[clap(subcommand)]
        command: FeatureConfigCommand,
    },
    Deploy {
        #[clap(short, long)]
        project: IdOrName,
        #[clap(short, long)]
        feature: IdOrName,
    },
    Teardown {
        #[clap(short, long)]
        project: IdOrName,
        #[clap(short, long)]
        feature: IdOrName,
    },
    GetInvokeURL {
        #[clap(short, long)]
        project: IdOrName,
        #[clap(short, long)]
        feature: IdOrName,
    },
    Logs {
        #[clap(short, long)]
        project: IdOrName,
        #[clap(short, long)]
        feature: IdOrName,
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

#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
pub struct BlobValue {
    #[clap(short, long)]
    pub literal: Option<String>,

    #[clap(short, long)]
    pub file: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub enum BlobCommand {
    List,
    Create {
        key: String,
        #[clap(flatten)]
        value: BlobValue,
    },
    Get {
        key: String,
        output: Option<PathBuf>,
    },
    Set {
        key: String,
        #[clap(flatten)]
        value: BlobValue,
    },
    Delete {
        key: String,
    },
}
