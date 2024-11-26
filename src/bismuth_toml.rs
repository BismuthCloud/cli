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

    /// File globs that should not be sent to the agent, even if they would be tracked by git.
    /// Mainly used to avoid accidentally sending large directories like node_modules in the case of a missing or misconfigured .gitignore.
    /// Defaults to **/.*/**, venv/**, **/__pycache__/**, *.pyc, **/node_modules/**, **/target/**, **/dist/**, **/build/**
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
                Glob::new("**/.*/**").unwrap(),
                Glob::new("venv/**").unwrap(),
                Glob::new("**/__pycache__/**").unwrap(),
                Glob::new("*.pyc").unwrap(),
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

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_block_globs() {
        let config = BismuthTOML::default();
        let globset = {
            let mut builder = globset::GlobSetBuilder::new();
            for glob in &config.chat.block_globs {
                builder.add(glob.clone());
            }
            builder.build().unwrap()
        };

        assert!(globset.is_match(".venv/bin/activate"));
        assert!(globset.is_match("venv/bin/activate"));
        assert!(globset.is_match("src/__pycache__/foo"));
        assert!(globset.is_match("src/main.pyc"));

        assert!(!globset.is_match("src/main.py"));

        assert!(globset.is_match("node_modules/foo/foo.js"));
        assert!(globset.is_match("foo/node_modules/foo/foo.js"));
        assert!(globset.is_match("target/debug/cli"));
        assert!(globset.is_match("dist/thing.whl"));
        assert!(globset.is_match("build/out.o"));
    }
}
