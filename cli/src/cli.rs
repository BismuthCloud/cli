use std::{path::PathBuf, str::FromStr};

use clap::{Args, Parser, Subcommand};

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
    #[clap(hide = true)]
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
    /// Create a new Bismuth project, and import an existing Git repository into it.
    /// Alias of `project import`.
    Import(ImportArgs),
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
    #[clap(hide = true)]
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
    #[arg(group = "source")]
    pub repo: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct ImportArgs {
    #[clap(flatten)]
    pub source: ImportSource,

    /// Implicitly upload to Bismuth Cloud
    #[arg(long)]
    pub upload: bool,
}

#[derive(Debug, Subcommand)]
pub enum ProjectCommand {
    /// List all projects
    List,
    /// Create a new project
    Create { name: String },
    /// Create a new Bismuth project, and import an existing Git repository into it
    Import(ImportArgs),
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
}
