//! `sip-faxd`: the SIP fax server executable.
//!
//! ```text
//! sip-faxd serve [--config PATH]               run the fax-in controller
//! sip-faxd config check [--config PATH]        validate a configuration file
//! sip-faxd db migrate [--config PATH]          create / upgrade the database schema
//! sip-faxd db backfill [--config PATH]         restore calls from logs/calls/*/result.json
//! sip-faxd purge [--dry-run] [--config PATH]   run the retention job now
//! sip-faxd config import [--force] [--file PATH]  runtime sections of a file -> database
//! sip-faxd config export [--include-secrets]  runtime configuration of the database as TOML
//! sip-faxd user add|passwd|disable|enable|list   accounts of the web interface
//! sip-faxd fax list|show|read|note|redeliver     received faxes
//! ```
//!
//! Without `--config` the file is `conf/sip-faxd.toml` next to the executable. Relative
//! paths inside the configuration (the capture directory, the data directory, a SQLite
//! database) are relative to the executable's directory too, so an unpacked tarball is
//! self-contained.

mod admin_cli;
mod commands;
mod db;
mod delivery_service;
mod runtime;
mod schedule;
mod settings;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use fax_in::recorder::{Recorder, RecorderConfig};
use fax_in::{CallEvent, FaxIn, StartOptions};

use crate::db::{Db, Startup};
use crate::delivery_service::DeliveryService;
use crate::settings::AppConfig;

/// SIP fax server (fax-in over T.38 / G.711).
#[derive(Parser)]
#[command(name = "sip-faxd", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Args)]
struct ConfigArg {
    /// Configuration file (default: conf/sip-faxd.toml next to the executable).
    #[arg(long, short = 'c', value_name = "PATH")]
    config: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the fax server daemon.
    Serve(ConfigArg),
    /// Configuration tools.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Database tools.
    Db {
        #[command(subcommand)]
        action: DbAction,
    },
    /// Accounts of the web interface.
    User {
        #[command(subcommand)]
        action: admin_cli::UserAction,
    },
    /// Received faxes.
    Fax {
        #[command(subcommand)]
        action: admin_cli::FaxAction,
    },
    /// Delete expired files and records now (what the daily job does).
    Purge {
        /// Only count what would be deleted.
        #[arg(long)]
        dry_run: bool,
        #[command(flatten)]
        config: ConfigArg,
    },
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Validate a configuration file (and the runtime configuration in the database) and print
    /// what it means.
    Check(ConfigArg),
    /// Write the runtime sections of a configuration file into the database. Refuses when the
    /// database already has runtime configuration, unless `--force`.
    Import {
        /// Replace what the database holds (sections the file does not have are removed).
        #[arg(long)]
        force: bool,
        /// Import this file's runtime sections (default: the `--config` file).
        #[arg(long, value_name = "PATH")]
        file: Option<PathBuf>,
        #[command(flatten)]
        config: ConfigArg,
    },
    /// Print the runtime configuration of the database as TOML (a backup, and a way to look at
    /// it). Secrets are left out unless `--include-secrets`.
    Export {
        /// Include passwords and HMAC keys in clear text.
        #[arg(long)]
        include_secrets: bool,
        #[command(flatten)]
        config: ConfigArg,
    },
}

#[derive(Subcommand)]
enum DbAction {
    /// Create the schema or upgrade it to this version.
    Migrate(ConfigArg),
    /// Insert the calls of `logs/calls/*/result.json` that are missing from the database.
    Backfill {
        /// Directory of the per-call captures (default: `[capture] dir`).
        #[arg(long, value_name = "DIR")]
        dir: Option<PathBuf>,
        #[command(flatten)]
        config: ConfigArg,
    },
}

fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn config_path(arg: &ConfigArg) -> PathBuf {
    arg.config
        .clone()
        .unwrap_or_else(|| exe_dir().join("conf").join("sip-faxd.toml"))
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        None => {
            println!("sip-faxd {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some(Command::Config {
            action: ConfigAction::Check(arg),
        }) => with_runtime(&config_path(&arg), |cfg| async move {
            commands::config_check_full(&cfg, &config_path(&arg), &exe_dir()).await
        }),
        Some(Command::Config {
            action:
                ConfigAction::Import {
                    force,
                    file,
                    config,
                },
        }) => with_runtime(&config_path(&config), |cfg| async move {
            admin_cli::config_import(&cfg, &exe_dir(), force, file).await
        }),
        Some(Command::Config {
            action:
                ConfigAction::Export {
                    include_secrets,
                    config,
                },
        }) => with_runtime(&config_path(&config), |cfg| async move {
            admin_cli::config_export(&cfg, &exe_dir(), include_secrets).await
        }),
        Some(Command::User { action }) => admin_cli::user(action),
        Some(Command::Fax { action }) => admin_cli::fax(action),
        Some(Command::Serve(arg)) => serve(&config_path(&arg)),
        Some(Command::Db {
            action: DbAction::Migrate(arg),
        }) => with_runtime(&config_path(&arg), |cfg| async move {
            commands::db_migrate(&cfg, &exe_dir()).await
        }),
        Some(Command::Db {
            action: DbAction::Backfill { dir, config },
        }) => with_runtime(&config_path(&config), |cfg| async move {
            commands::db_backfill(&cfg, &exe_dir(), dir).await
        }),
        Some(Command::Purge { dry_run, config }) => {
            with_runtime(&config_path(&config), |cfg| async move {
                commands::purge(&cfg, &exe_dir(), dry_run).await
            })
        }
    }
}

fn init_logging(cfg: &AppConfig) {
    let level = if cfg.log.level.is_empty() {
        "info".to_string()
    } else {
        cfg.log.level.clone()
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&level));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();
}

fn runtime() -> Result<tokio::runtime::Runtime, ExitCode> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| {
            eprintln!("sip-faxd: error: cannot start the async runtime: {e}");
            ExitCode::from(1)
        })
}

/// Loads the configuration and runs a one-shot command on a runtime.
fn with_runtime<F, Fut>(path: &Path, f: F) -> ExitCode
where
    F: FnOnce(AppConfig) -> Fut,
    Fut: std::future::Future<Output = ExitCode>,
{
    let cfg = match AppConfig::load(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("sip-faxd: error: {e}");
            return ExitCode::from(2);
        }
    };
    init_logging(&cfg);
    match runtime() {
        Ok(rt) => rt.block_on(f(cfg)),
        Err(c) => c,
    }
}

fn serve(path: &Path) -> ExitCode {
    with_runtime(path, run)
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match (
            signal(SignalKind::terminate()),
            signal(SignalKind::interrupt()),
        ) {
            (Ok(mut term), Ok(mut int)) => {
                tokio::select! {
                    _ = term.recv() => {}
                    _ = int.recv() => {}
                }
            }
            _ => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// How long the shutdown waits for the database write queue.
const RECORDER_GRACE: Duration = Duration::from_secs(10);

async fn run(cfg: AppConfig) -> ExitCode {
    let base = exe_dir();
    // The database first: a schema this binary cannot work with stops the start here, before a
    // single call is answered. An unreachable database does not: the calls are received and
    // written to the database as soon as it is back.
    let db = Arc::new(Db::new(&cfg, &base));
    match db.start().await {
        Ok(Startup::Ready { stale_fixed }) => {
            if stale_fixed > 0 {
                tracing::warn!(
                    rows = stale_fixed,
                    "records still `receiving` from a previous run were closed (shutdown)"
                );
            }
            tracing::info!(url = %settings::mask_url(&cfg.database.url), "database ready");
        }
        Ok(Startup::Unreachable(why)) => {
            tracing::error!(url = %settings::mask_url(&cfg.database.url),
                "database not reachable, receiving faxes anyway and retrying the writes: {why}");
        }
        Err(e) => {
            eprintln!("sip-faxd: error: database: {e}");
            return ExitCode::from(1);
        }
    }
    // The runtime configuration (sip, media, call, capture, trunks, routes, delivery, retention)
    // lives in the database from the first start on (ADR-051); the file only seeds it.
    let rt = runtime::Runtime::start(&cfg, db.clone()).await;
    let live = rt.current();
    let fax_cfg = live.to_fax_in(&cfg.fax_in.storage);
    for w in fax_cfg.warnings() {
        tracing::warn!("{w}");
    }
    let data_dir = cfg.data_dir(&base);
    let mut rcfg = RecorderConfig::new(data_dir.clone());
    rcfg.spill_dir = Some(cfg.calls_dir(&base));
    // Delivery: the recorder announces every fax row it has written; a database that is down or a
    // failing delivery never reaches the call path. It runs without routes too: routes can be
    // added while the server runs.
    if live.routes.routes.is_empty() {
        tracing::info!("no [[routes]] configured: received faxes are stored, not delivered");
    }
    let delivery = DeliveryService::start_default(
        db.clone(),
        live.routes.clone(),
        data_dir.clone(),
        live.delivery.clone(),
    );
    rcfg.on_finalized = Some(delivery.hook());
    let recorder = Arc::new(Recorder::start(db.clone(), rcfg));
    let purge_task = tokio::spawn(commands::purge_daily(db.clone(), data_dir));

    let opts = StartOptions {
        session_factory: None,
        base_dir: Some(base),
    };
    let (fax, mut events) = match FaxIn::start(fax_cfg, opts).await {
        Ok((fax, events)) => (Arc::new(fax), events),
        Err(e) => {
            eprintln!("sip-faxd: error: {e}");
            rt.detach();
            purge_task.abort();
            recorder.shutdown(RECORDER_GRACE).await;
            delivery.shutdown().await;
            return ExitCode::from(1);
        }
    };
    // from now on a configuration change reaches the running server
    rt.attach(fax.clone(), delivery.reconfigurer());
    tracing::info!(udp = ?fax.udp_addr(), tcp = ?fax.tcp_addr(), "sip-faxd ready");
    let rec = recorder.clone();
    let logger = tokio::spawn(async move {
        while let Some(ev) = events.recv().await {
            rec.record(&ev);
            if let CallEvent::Finished(r) = ev {
                match serde_json::to_string(&*r) {
                    Ok(json) => tracing::info!(target: "call_result", "{json}"),
                    Err(e) => tracing::error!("cannot serialise a call result: {e}"),
                }
            }
        }
    });
    shutdown_signal().await;
    tracing::info!("shutting down");
    rt.detach();
    // the applier's reference is gone; an apply that was just running may still hold one
    let mut fax = fax;
    let fax = loop {
        match Arc::try_unwrap(fax) {
            Ok(f) => break f,
            Err(shared) => {
                fax = shared;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    };
    fax.shutdown().await;
    let _ = logger.await;
    purge_task.abort();
    recorder.shutdown(RECORDER_GRACE).await;
    delivery.shutdown().await;
    db.close().await;
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }
}
