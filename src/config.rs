use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::appdata;
use crate::error::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_output")]
    pub output_dir: PathBuf,
    #[serde(default = "default_rpm")]
    pub requests_per_minute: u32,
    #[serde(default = "default_concurrency")]
    pub concurrent_downloads: usize,
    #[serde(default = "default_user_agent")]
    pub user_agent: String,
}

fn default_output() -> PathBuf {
    PathBuf::from("./downloads")
}

fn default_rpm() -> u32 {
    30
}

fn default_concurrency() -> usize {
    3
}

fn default_user_agent() -> String {
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36".into()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            output_dir: default_output(),
            requests_per_minute: default_rpm(),
            concurrent_downloads: default_concurrency(),
            user_agent: default_user_agent(),
        }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = appdata::config_file_path()?;
        if path.exists() {
            let raw = std::fs::read_to_string(&path)?;
            let cfg: Config = serde_yaml::from_str(&raw)
                .map_err(|e| crate::error::ImagoError::Parse(e.to_string()))?;
            return Ok(cfg);
        }
        Ok(Self::default())
    }
}
