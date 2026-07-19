use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::appdata;
use crate::error::Result;
use crate::media::Asset;

const MEDIA_EXTS: &[&str] = &["jpg", "jpeg", "png", "mp4", "webp"];

#[derive(Debug, Clone)]
pub struct Store {
    pub dir: PathBuf,
    present: HashSet<String>,
}

impl Store {
    pub fn open(base: &Path, username: &str) -> Result<Self> {
        let modern = base.join(username);
        let legacy = base.join(format!("{username}_photos"));
        let dir = if modern.exists() {
            modern
        } else if legacy.exists() {
            legacy
        } else {
            modern
        };
        fs::create_dir_all(&dir)?;
        let present = scan_keys(&dir)?;
        Ok(Self { dir, present })
    }

    pub fn has(&self, key: &str) -> bool {
        self.present.contains(key)
    }

    pub fn path_for(&self, asset: &Asset) -> PathBuf {
        self.dir.join(format!("{}.{}", asset.key, asset.ext))
    }

    pub fn save(&mut self, asset: &Asset, bytes: &[u8]) -> Result<PathBuf> {
        let path = self.path_for(asset);
        let tmp = path.with_extension(format!("{}.tmp", asset.ext));
        fs::write(&tmp, bytes)?;
        fs::rename(&tmp, &path)?;
        self.present.insert(asset.key.clone());
        Ok(path)
    }

    pub fn metadata_path(&self) -> PathBuf {
        self.dir.join("metadata.json")
    }
}

fn scan_keys(dir: &Path) -> Result<HashSet<String>> {
    let mut set = HashSet::new();
    if !dir.exists() {
        return Ok(set);
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Some((stem, ext)) = name.rsplit_once('.') {
            if MEDIA_EXTS.contains(&ext.to_ascii_lowercase().as_str()) {
                set.insert(stem.to_string());
            }
        }
    }
    Ok(set)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobState {
    pub version: u32,
    pub username: String,
    pub user_id: String,
    pub end_cursor: Option<String>,
    pub pages_done: u32,
    pub assets_downloaded: u64,
    pub assets_skipped: u64,
    pub completed: bool,
    pub updated_at: String,
}

impl JobState {
    pub fn load(username: &str) -> Result<Option<Self>> {
        let path = appdata::job_path(username)?;
        if !path.exists() {
            return Ok(None);
        }
        let raw = fs::read_to_string(&path)?;
        Ok(Some(serde_json::from_str(&raw)?))
    }

    pub fn save(&self) -> Result<()> {
        let path = appdata::job_path(&self.username)?;
        appdata::atomic_write_json(&path, self)
    }

    pub fn delete(username: &str) -> Result<()> {
        let path = appdata::job_path(username)?;
        if path.exists() {
            fs::remove_file(path)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ArchiveMetadata {
    pub version: u32,
    pub username: String,
    pub user_id: String,
    pub media_count: Option<u64>,
    pub downloaded: u64,
    pub assets: Vec<AssetRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetRecord {
    pub key: String,
    pub shortcode: String,
    pub is_video: bool,
    pub file: String,
    pub taken_at: Option<i64>,
    pub caption: Option<String>,
    /// Instagram's stable media id (`pk`), for cross-referencing. Absent in
    /// archives written before this field existed.
    #[serde(default)]
    pub media_id: Option<String>,
}

impl ArchiveMetadata {
    pub fn load_or_new(path: &Path, username: &str, user_id: &str) -> Self {
        if path.exists() {
            if let Ok(raw) = fs::read_to_string(path) {
                if let Ok(m) = serde_json::from_str::<ArchiveMetadata>(&raw) {
                    return m;
                }
            }
        }
        Self {
            version: 1,
            username: username.to_string(),
            user_id: user_id.to_string(),
            media_count: None,
            downloaded: 0,
            assets: Vec::new(),
        }
    }

    pub fn record(&mut self, asset: &Asset, file: &str) {
        if self.assets.iter().any(|a| a.key == asset.key) {
            return;
        }
        self.assets.push(AssetRecord {
            key: asset.key.clone(),
            shortcode: asset.shortcode.clone(),
            is_video: asset.is_video,
            file: file.to_string(),
            taken_at: asset.taken_at,
            caption: asset.caption.clone(),
            media_id: asset.media_id.clone(),
        });
        self.downloaded = self.assets.len() as u64;
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        appdata::atomic_write_json(path, self)
    }
}
