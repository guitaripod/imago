use std::fs::OpenOptions;
use std::sync::OnceLock;

use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use crate::appdata;
use crate::error::Result;

static INIT: OnceLock<()> = OnceLock::new();

pub fn init(verbose: bool) -> Result<()> {
    if INIT.get().is_some() {
        return Ok(());
    }

    let level = if verbose { "debug" } else { "info" };
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("imago={level},info")));

    let log_path = appdata::log_file_path()?;
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;

    // stderr: human; file: full trail for agents
    let stderr_layer = fmt::layer()
        .with_writer(std::io::stderr)
        .with_target(false)
        .with_level(true)
        .compact();

    let file_layer = fmt::layer()
        .with_writer(file)
        .with_ansi(false)
        .with_target(true)
        .with_level(true);

    tracing_subscriber::registry()
        .with(filter)
        .with(stderr_layer)
        .with(file_layer)
        .init();

    let _ = INIT.set(());
    tracing::debug!(path = %log_path.display(), "logging initialized");
    Ok(())
}
