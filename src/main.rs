mod appdata;
mod archive;
mod auth;
mod config;
mod error;
mod guide;
mod ig;
mod logx;
mod media;
mod store;
mod watch;

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use serde::Serialize;

use crate::error::{ImagoError, EXIT_AUTH, EXIT_OK, EXIT_PARTIAL};

#[derive(Parser, Debug)]
#[command(
    name = "imago",
    version,
    about = "Agent-native Instagram profile archive",
    long_about = "Drop a profile URL, archive every photo/video/carousel slide, watch for more.\nRun `imago guide` for the full playbook."
)]
struct Cli {
    #[arg(long, global = true, help = "Machine-readable JSON on stdout")]
    json: bool,

    #[arg(long, short, global = true, help = "Debug logging")]
    verbose: bool,

    #[arg(long, global = true, env = "IMAGO_SESSION_ID")]
    session_id: Option<String>,

    #[arg(long, global = true, env = "IMAGO_CSRF_TOKEN")]
    csrf_token: Option<String>,

    #[arg(long, global = true, help = "Output base directory")]
    output: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Commands>,

    /// Profile URL / @user / username (shorthand for `get`)
    #[arg(value_name = "TARGET")]
    target: Option<String>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Print the agent playbook
    Guide,
    /// Credential management
    Auth {
        #[command(subcommand)]
        action: AuthCmd,
    },
    /// Full archive of a profile
    Get {
        target: String,
        #[arg(long, help = "Reset job state (still skips existing files)")]
        force: bool,
    },
    /// Track profiles and incremental sync
    Watch {
        #[command(subcommand)]
        action: WatchCmd,
    },
    /// Print version
    Version,
}

#[derive(Subcommand, Debug)]
enum AuthCmd {
    Login {
        #[arg(long)]
        session_id: Option<String>,
        #[arg(long)]
        csrf_token: Option<String>,
    },
    Status,
    Logout,
}

#[derive(Subcommand, Debug)]
enum WatchCmd {
    Add {
        target: String,
        #[arg(long)]
        note: Option<String>,
        #[arg(long, help = "Only register, do not archive yet")]
        no_fetch: bool,
    },
    List,
    Remove {
        target: String,
    },
    Sync {
        users: Vec<String>,
        #[arg(long, help = "Disable early-stop; full re-scan")]
        full: bool,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Err(e) = logx::init(cli.verbose) {
        eprintln!("log init: {e}");
    }

    match run(cli).await {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("error: {e}");
            e.to_exit_code()
        }
    }
}

async fn run(cli: Cli) -> Result<u8, ImagoError> {
    let json = cli.json;

    match cli.command {
        Some(Commands::Guide) => {
            guide::print_guide();
            Ok(EXIT_OK)
        }
        Some(Commands::Version) => {
            emit(
                json,
                &serde_json::json!({
                    "ok": true,
                    "command": "version",
                    "name": "imago",
                    "version": env!("CARGO_PKG_VERSION"),
                    "homepage": "https://midgarcorp.cc/imago",
                }),
            );
            if !json {
                println!("imago {}", env!("CARGO_PKG_VERSION"));
            }
            Ok(EXIT_OK)
        }
        Some(Commands::Auth { action }) => match action {
            AuthCmd::Login {
                session_id,
                csrf_token,
            } => {
                let sid = session_id
                    .or(cli.session_id.clone())
                    .or_else(|| read_tty_line("sessionid"));
                let csrf = csrf_token
                    .or(cli.csrf_token.clone())
                    .or_else(|| read_tty_line("csrftoken"));
                let (Some(session_id), Some(csrf_token)) = (sid, csrf) else {
                    return Err(ImagoError::Usage(
                        "provide --session-id and --csrf-token (or env / TTY)".into(),
                    ));
                };
                let creds = auth::Credentials {
                    session_id,
                    csrf_token,
                    user_agent: None,
                };
                auth::store(&creds)?;
                emit(
                    json,
                    &serde_json::json!({"ok": true, "command": "auth login"}),
                );
                if !json {
                    println!("credentials stored");
                }
                Ok(EXIT_OK)
            }
            AuthCmd::Logout => {
                auth::clear()?;
                emit(
                    json,
                    &serde_json::json!({"ok": true, "command": "auth logout"}),
                );
                if !json {
                    println!("credentials cleared");
                }
                Ok(EXIT_OK)
            }
            AuthCmd::Status => {
                let creds = auth::load(cli.session_id.as_deref(), cli.csrf_token.as_deref())?;
                let cfg = config::Config::load()?;
                let client = ig::IgClient::new(&creds, &cfg.user_agent)?;
                match client.probe_session().await {
                    Ok(marker) => {
                        emit(
                            json,
                            &serde_json::json!({
                                "ok": true,
                                "command": "auth status",
                                "alive": true,
                                "probe": marker,
                            }),
                        );
                        if !json {
                            println!("session alive (probe={marker})");
                        }
                        Ok(EXIT_OK)
                    }
                    Err(ImagoError::SessionDead) | Err(ImagoError::Auth(_)) => {
                        emit(
                            json,
                            &serde_json::json!({
                                "ok": false,
                                "command": "auth status",
                                "alive": false,
                            }),
                        );
                        if !json {
                            eprintln!("session dead — imago auth login");
                        }
                        Ok(EXIT_AUTH)
                    }
                    Err(e) => Err(e),
                }
            }
        },
        Some(Commands::Get { target, force }) => {
            do_get(
                &target,
                force,
                json,
                cli.session_id.as_deref(),
                cli.csrf_token.as_deref(),
                cli.output.clone(),
            )
            .await
        }
        Some(Commands::Watch { action }) => {
            match action {
                WatchCmd::Add {
                    target,
                    note,
                    no_fetch,
                } => {
                    let username = ig::parse_profile_input(&target)?;
                    let mut list = watch::Watchlist::load()?;
                    let created = list.add(&username, note.as_deref().unwrap_or(""));
                    list.save()?;
                    emit(
                        json,
                        &serde_json::json!({
                            "ok": true,
                            "command": "watch add",
                            "username": username,
                            "created": created,
                        }),
                    );
                    if !json {
                        println!(
                            "{} {username}",
                            if created { "watching" } else { "updated" }
                        );
                    }
                    if !no_fetch {
                        return do_get(
                            &username,
                            false,
                            json,
                            cli.session_id.as_deref(),
                            cli.csrf_token.as_deref(),
                            cli.output.clone(),
                        )
                        .await;
                    }
                    Ok(EXIT_OK)
                }
                WatchCmd::List => {
                    let list = watch::Watchlist::load()?;
                    emit(json, &list);
                    if !json {
                        if list.entries.is_empty() {
                            println!("(empty)");
                        }
                        for e in &list.entries {
                            println!(
                                "{} {:12}  last={}  new={}  {}",
                                if e.enabled { "on " } else { "off" },
                                e.username,
                                e.last_synced_at.as_deref().unwrap_or("-"),
                                e.last_new_count,
                                e.last_status.as_deref().unwrap_or("-"),
                            );
                        }
                    }
                    Ok(EXIT_OK)
                }
                WatchCmd::Remove { target } => {
                    let username = ig::parse_profile_input(&target)?;
                    let mut list = watch::Watchlist::load()?;
                    let removed = list.remove(&username);
                    list.save()?;
                    emit(
                        json,
                        &serde_json::json!({
                            "ok": true,
                            "command": "watch remove",
                            "username": username,
                            "removed": removed,
                        }),
                    );
                    if !json {
                        println!(
                            "{}",
                            if removed {
                                format!("removed {username}")
                            } else {
                                format!("not watching {username}")
                            }
                        );
                    }
                    Ok(EXIT_OK)
                }
                WatchCmd::Sync { users, full } => {
                    let creds =
                        auth::load(cli.session_id.as_deref(), cli.csrf_token.as_deref())?;
                    let cfg = config::Config::load()?;
                    let users = if users.is_empty() {
                        None
                    } else {
                        Some(users)
                    };
                    let report = watch::sync(
                        users,
                        &creds,
                        &cfg,
                        json,
                        full,
                        cli.output.clone(),
                    )
                    .await?;
                    emit(json, &report);
                    if !json {
                        for r in &report.results {
                            println!(
                                "{}  +{} new  ~{} skip  fail={}  {}",
                                r.username,
                                r.assets_downloaded,
                                r.assets_skipped,
                                r.assets_failed,
                                r.output_dir
                            );
                        }
                        if !report.failed.is_empty() {
                            eprintln!("failed: {}", report.failed.join(", "));
                        }
                    }
                    Ok(if report.ok { EXIT_OK } else { EXIT_PARTIAL })
                }
            }
        }
        None => {
            let Some(target) = cli.target else {
                if std::io::stdout().is_terminal() {
                    guide::print_guide();
                    return Ok(EXIT_OK);
                }
                return Err(ImagoError::Usage(
                    "missing target — imago get <url|user>  (see imago guide)".into(),
                ));
            };
            // Reject known subcommand names used without subcommand parser confusion
            do_get(
                &target,
                false,
                json,
                cli.session_id.as_deref(),
                cli.csrf_token.as_deref(),
                cli.output.clone(),
            )
            .await
        }
    }
}

async fn do_get(
    target: &str,
    force: bool,
    json: bool,
    session: Option<&str>,
    csrf: Option<&str>,
    output: Option<PathBuf>,
) -> Result<u8, ImagoError> {
    let username = ig::parse_profile_input(target)?;
    let creds = auth::load(session, csrf)?;
    let cfg = config::Config::load()?;
    let opts = archive::ArchiveOpts {
        force,
        json,
        output,
        early_stop_known_posts: 0,
        max_pages: None,
    };
    let report = archive::run(&username, &creds, &cfg, opts).await?;
    emit(json, &report);
    if !json {
        println!(
            "done  {}  +{} new  ~{} skip  fail={}  {}ms  → {}",
            report.username,
            report.assets_downloaded,
            report.assets_skipped,
            report.assets_failed,
            report.duration_ms,
            report.output_dir
        );
    }
    Ok(if report.assets_failed > 0 {
        EXIT_PARTIAL
    } else {
        EXIT_OK
    })
}

fn emit<T: Serialize>(json: bool, value: &T) {
    if json {
        if let Ok(s) = serde_json::to_string_pretty(value) {
            println!("{s}");
        }
    }
}

fn read_tty_line(label: &str) -> Option<String> {
    if !std::io::stdin().is_terminal() {
        return None;
    }
    eprint!("{label}: ");
    let mut buf = String::new();
    if std::io::stdin().read_line(&mut buf).is_ok() {
        let t = buf.trim().to_string();
        if !t.is_empty() {
            return Some(t);
        }
    }
    None
}
