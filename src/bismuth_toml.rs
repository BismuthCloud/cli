use anyhow::Result;
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
}

impl Default for ChatConfig {
    fn default() -> Self {
        ChatConfig {
            command_timeout: 60,
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
