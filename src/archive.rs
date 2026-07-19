use std::path::PathBuf;
use std::time::Duration;

use chrono::Utc;
use futures::stream::{self, StreamExt};
use indicatif::{ProgressBar, ProgressStyle};
use serde::Serialize;
use tracing::{info, warn};

use crate::auth::Credentials;
use crate::config::Config;
use crate::error::{ImagoError, Result};
use crate::ig::IgClient;
use crate::store::{ArchiveMetadata, JobState, Store};

#[derive(Debug, Clone, Serialize)]
pub struct ArchiveReport {
    pub ok: bool,
    pub command: &'static str,
    pub username: String,
    pub user_id: String,
    pub assets_downloaded: u64,
    pub assets_skipped: u64,
    pub assets_failed: u64,
    pub output_dir: String,
    pub duration_ms: u64,
    pub early_stopped: bool,
}

pub struct ArchiveOpts {
    pub force: bool,
    pub json: bool,
    pub output: Option<PathBuf>,
    /// Stop after this many consecutive already-known posts (0 = never).
    pub early_stop_known_posts: u32,
    pub max_pages: Option<u32>,
}

impl Default for ArchiveOpts {
    fn default() -> Self {
        Self {
            force: false,
            json: false,
            output: None,
            early_stop_known_posts: 0,
            max_pages: None,
        }
    }
}

pub async fn run(
    username: &str,
    creds: &Credentials,
    cfg: &Config,
    opts: ArchiveOpts,
) -> Result<ArchiveReport> {
    let started = std::time::Instant::now();
    let client = IgClient::new(creds, &cfg.user_agent)?;
    let base = opts
        .output
        .clone()
        .unwrap_or_else(|| cfg.output_dir.clone());

    let mut job = if opts.force {
        let _ = JobState::delete(username);
        None
    } else {
        JobState::load(username)?
    };

    // Seed / resume
    let (user_id, mut cursor, mut pages_done, mut downloaded, mut skipped) =
        if let Some(ref j) = job {
            if j.completed && !opts.force {
                // Fresh incremental: start from newest without old cursor
                info!(username, "previous job completed; starting incremental from head");
                (j.user_id.clone(), None, 0u32, 0u64, 0u64)
            } else {
                info!(
                    username,
                    cursor = ?j.end_cursor,
                    "resuming job"
                );
                (
                    j.user_id.clone(),
                    j.end_cursor.clone(),
                    j.pages_done,
                    j.assets_downloaded,
                    j.assets_skipped,
                )
            }
        } else {
            (String::new(), None, 0u32, 0u64, 0u64)
        };

    let mut store = Store::open(&base, username)?;
    let mut user_id = user_id;
    let mut failed = 0u64;
    let mut early_stopped = false;
    let mut consecutive_known_posts = 0u32;

    let pb = if opts.json {
        None
    } else {
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::with_template("{spinner:.green} {msg}")
                .unwrap()
                .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"),
        );
        pb.enable_steady_tick(Duration::from_millis(80));
        Some(pb)
    };

    // First page: profile seed when no cursor
    loop {
        if let Some(max) = opts.max_pages {
            if pages_done >= max {
                break;
            }
        }

        if let Some(ref pb) = pb {
            pb.set_message(format!(
                "{username}  page {}  +{downloaded} new  ~{skipped} skip",
                pages_done + 1
            ));
        }

        let page = match fetch_with_backoff(
            &client,
            username,
            &user_id,
            cursor.as_deref(),
            pages_done == 0 && cursor.is_none(),
        )
        .await
        {
            Ok(p) => p,
            Err(ImagoError::SessionDead) => return Err(ImagoError::SessionDead),
            Err(e) => return Err(e),
        };

        if user_id.is_empty() {
            user_id = page.user_id.clone();
        }

        // Early-stop heuristic (watch sync)
        if opts.early_stop_known_posts > 0 {
            let mut page_all_known = !page.post_keys.is_empty();
            for pk in &page.post_keys {
                // post known if any asset with that shortcode exists OR key itself
                let known = store.has(pk)
                    || store.has(&format!("{pk}_00"))
                    || page
                        .assets
                        .iter()
                        .filter(|a| a.shortcode == *pk)
                        .all(|a| store.has(&a.key));
                if known {
                    consecutive_known_posts += 1;
                } else {
                    consecutive_known_posts = 0;
                    page_all_known = false;
                }
            }
            if page_all_known && consecutive_known_posts >= opts.early_stop_known_posts {
                info!(username, "early stop — page fully known");
                early_stopped = true;
                // still download any missing assets on this page
            }
        }

        // Download assets concurrently
        let to_fetch: Vec<_> = page
            .assets
            .into_iter()
            .filter(|a| {
                if store.has(&a.key) {
                    skipped += 1;
                    false
                } else {
                    true
                }
            })
            .collect();

        let concurrency = cfg.concurrent_downloads.max(1);
        let results: Vec<_> = stream::iter(to_fetch)
            .map(|asset| {
                let client = &client;
                async move {
                    let bytes = client.download_bytes(&asset.url).await;
                    (asset, bytes)
                }
            })
            .buffer_unordered(concurrency)
            .collect()
            .await;

        let mut meta =
            ArchiveMetadata::load_or_new(&store.metadata_path(), username, &user_id);
        if page.media_count.is_some() {
            meta.media_count = page.media_count;
        }

        for (asset, bytes) in results {
            match bytes {
                Ok(data) if !data.is_empty() => match store.save(&asset, &data) {
                    Ok(path) => {
                        downloaded += 1;
                        let file = path
                            .file_name()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_default();
                        meta.record(&asset, &file);
                        info!(key = %asset.key, bytes = data.len(), "saved");
                    }
                    Err(e) => {
                        warn!(key = %asset.key, error = %e, "save failed");
                        failed += 1;
                    }
                },
                Ok(_) => {
                    warn!(key = %asset.key, "empty body");
                    failed += 1;
                }
                Err(e) => {
                    warn!(key = %asset.key, error = %e, "download failed");
                    failed += 1;
                }
            }
        }
        let _ = meta.save(&store.metadata_path());

        pages_done += 1;
        cursor = page.end_cursor.clone();

        // Persist job
        let state = JobState {
            version: 1,
            username: username.to_string(),
            user_id: user_id.clone(),
            end_cursor: cursor.clone(),
            pages_done,
            assets_downloaded: downloaded,
            assets_skipped: skipped,
            completed: false,
            updated_at: Utc::now().to_rfc3339(),
        };
        let _ = state.save();
        job = Some(state);

        if early_stopped || !page.has_next || cursor.is_none() {
            break;
        }

        // polite pacing between pages
        tokio::time::sleep(page_delay(cfg.requests_per_minute)).await;
    }

    // mark complete
    if let Some(mut j) = job {
        j.completed = true;
        j.assets_downloaded = downloaded;
        j.assets_skipped = skipped;
        j.updated_at = Utc::now().to_rfc3339();
        let _ = j.save();
    }

    if let Some(pb) = pb {
        pb.finish_and_clear();
    }

    Ok(ArchiveReport {
        ok: failed == 0,
        command: "get",
        username: username.to_string(),
        user_id,
        assets_downloaded: downloaded,
        assets_skipped: skipped,
        assets_failed: failed,
        output_dir: store.dir.display().to_string(),
        duration_ms: started.elapsed().as_millis() as u64,
        early_stopped,
    })
}

async fn fetch_with_backoff(
    client: &IgClient,
    username: &str,
    user_id: &str,
    cursor: Option<&str>,
    seed_profile: bool,
) -> Result<crate::media::Page> {
    let mut attempt = 0u32;
    loop {
        let res = if seed_profile && cursor.is_none() {
            client.fetch_profile_page(username).await
        } else {
            let uid = if user_id.is_empty() {
                // need profile first
                let p = client.fetch_profile_page(username).await?;
                return Ok(if cursor.is_none() {
                    p
                } else {
                    client
                        .fetch_media_page(&p.user_id, username, cursor)
                        .await?
                });
            } else {
                user_id
            };
            client.fetch_media_page(uid, username, cursor).await
        };

        match res {
            Ok(p) => return Ok(p),
            Err(ImagoError::RateLimited(msg)) => {
                attempt += 1;
                let wait = Duration::from_secs(60 * attempt.min(10) as u64);
                warn!(%msg, ?wait, attempt, "rate limited — waiting");
                tokio::time::sleep(wait).await;
            }
            Err(ImagoError::SessionDead) => return Err(ImagoError::SessionDead),
            Err(e) if attempt < 5 => {
                attempt += 1;
                let wait = Duration::from_secs(5 * attempt as u64);
                warn!(error = %e, ?wait, "retrying page fetch");
                tokio::time::sleep(wait).await;
            }
            Err(e) => return Err(e),
        }
    }
}

fn page_delay(rpm: u32) -> Duration {
    let rpm = rpm.max(1);
    Duration::from_millis((60_000 / rpm as u64).max(500))
}
