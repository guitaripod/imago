use std::fs;
use std::path::PathBuf;

use crate::error::{ImagoError, Result};

const APP: &str = "imago";

pub fn config_dir() -> Result<PathBuf> {
    let dir = dirs::config_dir()
        .ok_or_else(|| ImagoError::Other("cannot resolve config directory".into()))?
        .join(APP);
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn data_dir() -> Result<PathBuf> {
    let dir = dirs::data_local_dir()
        .ok_or_else(|| ImagoError::Other("cannot resolve data directory".into()))?
        .join(APP);
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn logs_dir() -> Result<PathBuf> {
    let dir = data_dir()?.join("logs");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn jobs_dir() -> Result<PathBuf> {
    let dir = data_dir()?.join("jobs");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn watchlist_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("watchlist.json"))
}

pub fn credentials_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("credentials.json"))
}

pub fn config_file_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.yaml"))
}

pub fn job_path(username: &str) -> Result<PathBuf> {
    Ok(jobs_dir()?.join(format!("{username}.json")))
}

pub fn log_file_path() -> Result<PathBuf> {
    Ok(logs_dir()?.join("imago.log"))
}

/// Atomic write: temp file in same dir + rename.
pub fn atomic_write(path: &std::path::Path, data: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, data)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

pub fn atomic_write_json<T: serde::Serialize>(path: &std::path::Path, value: &T) -> Result<()> {
    let data = serde_json::to_vec_pretty(value)?;
    atomic_write(path, &data)
}
