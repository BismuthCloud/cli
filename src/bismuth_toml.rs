use anyhow::Result;
use globset::Glob;
use serde::Deserialize;
use std::{fs, path::Path};

/// The root configuration for Bismuth.
#[derive(Default, Deserialize, Debug)]
#[serde(default)]
pub struct BismuthTOML {
    pub chat: ChatConfig,
}

/// Configuration options for interactive chat.
#[derive(Deserialize, Debug)]
#[serde(default)]
pub struct ChatConfig {
    /// Timeout in seconds for commands run by the agent. Deafult 60s.
    pub command_timeout: u64,

    /// Additional files to be sent to the agent that are normally excluded by .gitignore.
    /// Defaults to .env, .env.local, .env.development.
    pub additional_files: Vec<String>,

    /// File globs that should not be sent to the agent after command running, even if they would be tracked by git.
    /// Defaults to **/node_modules/**, **/target/**, **/dist/**, **/build/**.
    pub block_globs: Vec<Glob>,
}

impl Default for ChatConfig {
    fn default() -> Self {
        ChatConfig {
            command_timeout: 60,
            additional_files: vec![
                ".env".to_string(),
                ".env.local".to_string(),
                ".env.development".to_string(),
            ],
            block_globs: vec![
                Glob::new("**/node_modules/**").unwrap(),
                Glob::new("**/target/**").unwrap(),
                Glob::new("**/dist/**").unwrap(),
                Glob::new("**/build/**").unwrap(),
            ],
        }
    }
}

pub fn parse_config(repo_root: &Path) -> Result<BismuthTOML> {
    let config_path = repo_root.join("bismuth.toml");
    if fs::metadata(&config_path).is_err() {
        return Ok(BismuthTOML::default());
    }
    let config_str = fs::read_to_string(config_path)?;
    let config: BismuthTOML = toml::from_str(&config_str)?;
    Ok(config)
}
