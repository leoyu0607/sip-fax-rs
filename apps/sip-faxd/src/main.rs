//! `sip-faxd`: the SIP fax server executable.
//!
//! ```text
//! sip-faxd serve [--config PATH]               run the fax-in controller
//! sip-faxd config check [--config PATH]        validate a configuration file
//! sip-faxd db migrate [--config PATH]          create / upgrade the database schema
//! sip-faxd db backfill [--config PATH]         restore calls from logs/calls/*/result.json
//! sip-faxd purge [--dry-run] [--config PATH]   run the retention job now
//! ```
//!
//! Without `--config` the file is `conf/sip-faxd.toml` next to the executable. Relative
//! paths inside the configuration (the capture directory, the data directory, a SQLite
//! database) are relative to the executable's directory too, so an unpacked tarball is
//! self-contained.

mod commands;
mod db;
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
    /// Validate a configuration file and print what it means.
    Check(ConfigArg),
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
        }) => commands::config_check(&config_path(&arg)),
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
    for w in cfg.fax_in.warnings() {
        tracing::warn!("{w}");
    }
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
    let data_dir = cfg.data_dir(&base);
    let mut rcfg = RecorderConfig::new(data_dir.clone());
    rcfg.spill_dir = Some(cfg.calls_dir(&base));
    let recorder = Arc::new(Recorder::start(db.clone(), rcfg));
    let purge_task = tokio::spawn(commands::purge_daily(db.clone(), data_dir));

    let opts = StartOptions {
        session_factory: None,
        base_dir: Some(base),
    };
    let (fax, mut events) = match FaxIn::start(cfg.fax_in, opts).await {
        Ok(x) => x,
        Err(e) => {
            eprintln!("sip-faxd: error: {e}");
            purge_task.abort();
            recorder.shutdown(RECORDER_GRACE).await;
            return ExitCode::from(1);
        }
    };
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
    fax.shutdown().await;
    let _ = logger.await;
    purge_task.abort();
    recorder.shutdown(RECORDER_GRACE).await;
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
