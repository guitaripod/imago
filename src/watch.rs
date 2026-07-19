use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::appdata;
use crate::archive::{self, ArchiveOpts, ArchiveReport};
use crate::auth::Credentials;
use crate::config::Config;
use crate::error::{ImagoError, Result};
use crate::ig;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Watchlist {
    pub version: u32,
    pub updated_at: String,
    pub entries: Vec<WatchEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchEntry {
    pub username: String,
    pub enabled: bool,
    pub added_at: String,
    #[serde(default)]
    pub last_synced_at: Option<String>,
    #[serde(default)]
    pub last_status: Option<String>,
    #[serde(default)]
    pub last_new_count: u64,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub note: String,
}

impl Default for Watchlist {
    fn default() -> Self {
        Self {
            version: 1,
            updated_at: Utc::now().to_rfc3339(),
            entries: Vec::new(),
        }
    }
}

impl Watchlist {
    pub fn load() -> Result<Self> {
        let path = appdata::watchlist_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(&path)?;
        Ok(serde_json::from_str(&raw)?)
    }

    pub fn save(&mut self) -> Result<()> {
        self.updated_at = Utc::now().to_rfc3339();
        let path = appdata::watchlist_path()?;
        appdata::atomic_write_json(&path, self)
    }

    pub fn add(&mut self, username: &str, note: &str) -> bool {
        if let Some(e) = self.entries.iter_mut().find(|e| e.username == username) {
            e.enabled = true;
            if !note.is_empty() {
                e.note = note.to_string();
            }
            return false;
        }
        self.entries.push(WatchEntry {
            username: username.to_string(),
            enabled: true,
            added_at: Utc::now().to_rfc3339(),
            last_synced_at: None,
            last_status: None,
            last_new_count: 0,
            last_error: None,
            note: note.to_string(),
        });
        true
    }

    pub fn remove(&mut self, username: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| e.username != username);
        self.entries.len() != before
    }
}

#[derive(Debug, Serialize)]
pub struct SyncReport {
    pub ok: bool,
    pub command: &'static str,
    pub results: Vec<ArchiveReport>,
    pub failed: Vec<String>,
}

pub async fn sync(
    usernames: Option<Vec<String>>,
    creds: &Credentials,
    cfg: &Config,
    json: bool,
    full: bool,
    output: Option<std::path::PathBuf>,
) -> Result<SyncReport> {
    let mut list = Watchlist::load()?;
    let targets: Vec<String> = if let Some(u) = usernames {
        u
    } else {
        list.entries
            .iter()
            .filter(|e| e.enabled)
            .map(|e| e.username.clone())
            .collect()
    };

    if targets.is_empty() {
        return Err(ImagoError::Usage(
            "watchlist empty — imago watch add <user>".into(),
        ));
    }

    let mut results = Vec::new();
    let mut failed = Vec::new();

    for username in targets {
        let username = ig::parse_profile_input(&username).unwrap_or(username);
        let opts = ArchiveOpts {
            force: false,
            json,
            output: output.clone(),
            early_stop_known_posts: if full { 0 } else { 12 },
            max_pages: None,
        };
        match archive::run(&username, creds, cfg, opts).await {
            Ok(report) => {
                if let Some(e) = list.entries.iter_mut().find(|e| e.username == username) {
                    e.last_synced_at = Some(Utc::now().to_rfc3339());
                    e.last_status = Some(if report.ok { "ok".into() } else { "partial".into() });
                    e.last_new_count = report.assets_downloaded;
                    e.last_error = None;
                }
                if report.assets_failed > 0 {
                    failed.push(username.clone());
                }
                results.push(report);
            }
            Err(ImagoError::SessionDead) => {
                if let Some(e) = list.entries.iter_mut().find(|e| e.username == username) {
                    e.last_status = Some("auth".into());
                    e.last_error = Some("session dead".into());
                }
                let _ = list.save();
                return Err(ImagoError::SessionDead);
            }
            Err(e) => {
                if let Some(ent) = list.entries.iter_mut().find(|e| e.username == username) {
                    ent.last_status = Some("error".into());
                    ent.last_error = Some(e.to_string());
                }
                failed.push(username);
            }
        }
        let _ = list.save();
    }

    Ok(SyncReport {
        ok: failed.is_empty(),
        command: "watch sync",
        results,
        failed,
    })
}
