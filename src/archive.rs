use std::collections::HashSet;
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
    /// Distinct posts on disk after this run.
    pub posts_archived: u64,
    /// Post count the profile reports (often higher: pinned/hidden/removed).
    pub posts_reported: Option<u64>,
}

#[derive(Default)]
pub struct ArchiveOpts {
    pub force: bool,
    pub json: bool,
    pub output: Option<PathBuf>,
    /// Stop after this many consecutive already-known posts (0 = never).
    pub early_stop_known_posts: u32,
    pub max_pages: Option<u32>,
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
                info!(
                    username,
                    "previous job completed; starting incremental from head"
                );
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
    let mut posts_seen: HashSet<String> = HashSet::new();
    let mut media_count: Option<u64> = None;
    // Adaptive pacing: tighten after a blocked page, ease off on clean ones.
    let mut throttle = Duration::ZERO;

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
            let progress = match media_count {
                Some(c) => format!("{}/{c} posts", posts_seen.len()),
                None => format!("{} posts", posts_seen.len()),
            };
            pb.set_message(format!(
                "{username}  {progress}  +{downloaded} new  ~{skipped} skip"
            ));
        }

        let (page, attempts) = match fetch_with_backoff(
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
        if page.media_count.is_some() {
            media_count = page.media_count;
        }
        for pk in &page.post_keys {
            posts_seen.insert(pk.clone());
        }
        if attempts > 0 {
            throttle = (throttle + Duration::from_secs(5)).min(Duration::from_secs(60));
        } else {
            throttle = throttle.saturating_sub(Duration::from_secs(2));
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
                    let bytes = download_with_backoff(client, &asset.url, &asset.key).await;
                    (asset, bytes)
                }
            })
            .buffer_unordered(concurrency)
            .collect()
            .await;

        let mut meta = ArchiveMetadata::load_or_new(&store.metadata_path(), username, &user_id);
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
                    warn!(key = %asset.key, "empty body after retries");
                    failed += 1;
                }
                Err(ImagoError::SessionDead) => return Err(ImagoError::SessionDead),
                Err(e) => {
                    // Should be rare — download_with_backoff only exits on SessionDead
                    // or empty responses converted above. Count and continue the archive.
                    warn!(key = %asset.key, error = %e, "download gave up");
                    failed += 1;
                }
            }
        }
        let _ = meta.save(&store.metadata_path());

        pages_done += 1;

        // Prefer feed max_id pagination. Profile seed returns a GraphQL cursor that
        // Instagram often soft-blocks; switch to feed stream (empty after = page 1).
        cursor = match page.end_cursor {
            Some(c) if crate::media::is_feed_cursor(&c) && !c.is_empty() => Some(c),
            _ if page.has_next => {
                info!("switching pagination to feed API");
                Some(String::new())
            }
            _ => None,
        };

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

        if early_stopped || !page.has_next {
            break;
        }
        // has_next with empty cursor still means "continue via feed bootstrap"
        if cursor.is_none() {
            break;
        }

        // polite pacing between pages (extra pause after large download batches,
        // plus any adaptive throttle accrued from recent soft blocks)
        let mut delay = page_delay(cfg.requests_per_minute);
        if downloaded > 0 {
            delay = delay.saturating_mul(2).max(Duration::from_secs(3));
        }
        tokio::time::sleep(delay + throttle).await;
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

    let final_meta = ArchiveMetadata::load_or_new(&store.metadata_path(), username, &user_id);
    let posts_archived = final_meta
        .assets
        .iter()
        .map(|a| a.shortcode.as_str())
        .collect::<HashSet<_>>()
        .len() as u64;

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
        posts_archived,
        posts_reported: media_count.or(final_meta.media_count),
    })
}

/// Keep trying until the page succeeds or the session is truly dead.
/// Rate limits and transient network/API errors never abort the archive.
/// Returns the page plus how many backoff retries it took (0 = first try),
/// which drives adaptive pacing.
async fn fetch_with_backoff(
    client: &IgClient,
    username: &str,
    user_id: &str,
    cursor: Option<&str>,
    seed_profile: bool,
) -> Result<(crate::media::Page, u32)> {
    let mut attempt = 0u32;
    loop {
        let res = if seed_profile && cursor.is_none() {
            client.fetch_profile_page(username).await
        } else if user_id.is_empty() {
            match client.fetch_profile_page(username).await {
                Ok(p) if cursor.is_none() => Ok(p),
                Ok(p) => client.fetch_media_page(&p.user_id, username, cursor).await,
                Err(e) => Err(e),
            }
        } else {
            client.fetch_media_page(user_id, username, cursor).await
        };

        match res {
            Ok(p) => return Ok((p, attempt)),
            Err(ImagoError::SessionDead) => return Err(ImagoError::SessionDead),
            Err(ImagoError::NotFound(u)) => return Err(ImagoError::NotFound(u)),
            Err(ImagoError::Usage(m)) => return Err(ImagoError::Usage(m)),
            Err(e) => {
                attempt = attempt.saturating_add(1);
                let wait = backoff_delay(&e, attempt);
                warn!(
                    error = %e,
                    ?wait,
                    attempt,
                    "page fetch blocked — waiting, will not stop until complete"
                );
                tokio::time::sleep(wait).await;
            }
        }
    }
}

/// Download one asset. Rate limits wait forever; other failures retry a while
/// then surface so the archive can skip that file and continue the profile.
async fn download_with_backoff(client: &IgClient, url: &str, key: &str) -> Result<Vec<u8>> {
    let mut attempt = 0u32;
    let mut non_rate_failures = 0u32;
    loop {
        match client.download_bytes(url).await {
            Ok(data) if !data.is_empty() => return Ok(data),
            Ok(_) => {
                non_rate_failures += 1;
                if non_rate_failures >= 15 {
                    return Err(ImagoError::Network(format!(
                        "empty body for {key} after {non_rate_failures} tries"
                    )));
                }
                warn!(%key, non_rate_failures, "empty download body — retrying");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            Err(ImagoError::SessionDead) => return Err(ImagoError::SessionDead),
            Err(e @ ImagoError::RateLimited(_)) | Err(e @ ImagoError::Auth(_)) => {
                // Soft blocks: never give up on this asset until it downloads
                attempt = attempt.saturating_add(1);
                let wait = backoff_delay(&e, attempt);
                warn!(%key, error = %e, ?wait, attempt, "download rate-limited — waiting");
                tokio::time::sleep(wait).await;
            }
            Err(e) => {
                non_rate_failures += 1;
                if non_rate_failures >= 8 {
                    return Err(e);
                }
                let wait = Duration::from_secs(5);
                warn!(%key, error = %e, ?wait, non_rate_failures, "download error — retrying");
                tokio::time::sleep(wait).await;
            }
        }
    }
}

/// Exponential-ish backoff, capped at 5 min so recovery is quick once a soft
/// block lifts; never stops retrying. A checkpoint/dead session is surfaced as
/// `SessionDead` upstream instead of being waited out here.
fn backoff_delay(err: &ImagoError, attempt: u32) -> Duration {
    let (base_secs, cap_secs) = match err {
        ImagoError::RateLimited(_) | ImagoError::Auth(_) => (120u64, 5 * 60),
        ImagoError::Network(_) => (15, 5 * 60),
        _ => (20, 5 * 60),
    };
    // 1,2,4,8… capped
    let exp = 1u64 << attempt.min(6);
    let secs = (base_secs.saturating_mul(exp)).min(cap_secs);
    Duration::from_secs_f64(secs as f64 * jitter())
}

/// ±15% multiplicative jitter so concurrent retries don't realign into bursts.
fn jitter() -> f64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    0.85 + (nanos % 300) as f64 / 1000.0
}

fn page_delay(rpm: u32) -> Duration {
    let rpm = rpm.max(1);
    Duration::from_millis((60_000 / rpm as u64).max(500))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_is_capped_near_five_minutes() {
        // Even at high attempt counts, a rate-limit wait never blows past
        // ~5 min (+ jitter headroom) — recovery stays quick once a block lifts.
        for attempt in 0..20 {
            let d = backoff_delay(&ImagoError::RateLimited("x".into()), attempt);
            assert!(
                d <= Duration::from_secs(6 * 60),
                "attempt {attempt} waited {d:?}"
            );
        }
    }

    #[test]
    fn jitter_stays_in_band() {
        let j = jitter();
        assert!((0.85..1.15).contains(&j), "jitter {j}");
    }
}
