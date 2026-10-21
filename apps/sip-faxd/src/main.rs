//! `sip-faxd`: the SIP fax server executable.
//!
//! ```text
//! sip-faxd serve [--config PATH]               run the fax-in controller (foreground)
//! sip-faxd start [-d] [--config PATH]          start it in the background, then show its log
//! sip-faxd stop [--force] [--timeout SECS]     stop it, after the running calls (Linux)
//! sip-faxd restart | status | logs [-f] [-n N] ...
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
mod delivery_service;
mod schedule;
mod service;
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
use crate::service::hub::{Hub, HubInfo, Progress};
use crate::service::protocol::TrunkStatus;
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

#[derive(Args)]
struct ServeArgs {
    #[command(flatten)]
    config: ConfigArg,
    /// Used by `start`: detach from the terminal and log to the file only.
    #[arg(long, hide = true)]
    daemonized: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Run the fax server in the foreground (what systemd runs).
    Serve(ServeArgs),
    /// Start the service in the background and show its log (Ctrl+C only stops the display).
    Start {
        /// Start only; do not show the log.
        #[arg(short = 'd', long)]
        detach: bool,
        /// Start it even though systemd manages it.
        #[arg(long)]
        ignore_systemd: bool,
        #[command(flatten)]
        config: ConfigArg,
    },
    /// Stop the service: no new calls, wait for the running ones.
    Stop {
        /// End the running calls now (the pages received so far are kept).
        #[arg(long)]
        force: bool,
        /// Seconds to wait for the calls (default: `[service] stop_timeout_secs`).
        #[arg(long, value_name = "SECS")]
        timeout: Option<u64>,
        /// Stop it even though systemd manages it.
        #[arg(long)]
        ignore_systemd: bool,
        #[command(flatten)]
        config: ConfigArg,
    },
    /// Stop, then start.
    Restart {
        /// Start only; do not show the log.
        #[arg(short = 'd', long)]
        detach: bool,
        /// End the running calls now.
        #[arg(long)]
        force: bool,
        /// Seconds to wait for the calls (default: `[service] stop_timeout_secs`).
        #[arg(long, value_name = "SECS")]
        timeout: Option<u64>,
        /// Restart it even though systemd manages it.
        #[arg(long)]
        ignore_systemd: bool,
        #[command(flatten)]
        config: ConfigArg,
    },
    /// Show whether the service runs (exit code 3 when it does not).
    Status {
        /// Print the status as JSON.
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        config: ConfigArg,
    },
    /// Show the service log.
    Logs {
        /// Keep showing new lines (also across log rotation).
        #[arg(short = 'f', long)]
        follow: bool,
        /// How many of the last lines to show first.
        #[arg(short = 'n', long, default_value_t = 50, value_name = "N")]
        lines: usize,
        #[command(flatten)]
        config: ConfigArg,
    },
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
        Some(Command::Serve(arg)) => serve(&config_path(&arg.config), arg.daemonized),
        Some(Command::Start {
            detach,
            ignore_systemd,
            config,
        }) => service_cmd::start(&config_path(&config), detach, ignore_systemd),
        Some(Command::Stop {
            force,
            timeout,
            ignore_systemd,
            config,
        }) => service_cmd::stop(&config_path(&config), force, timeout, ignore_systemd),
        Some(Command::Restart {
            detach,
            force,
            timeout,
            ignore_systemd,
            config,
        }) => service_cmd::restart(
            &config_path(&config),
            detach,
            force,
            timeout,
            ignore_systemd,
        ),
        Some(Command::Status { json, config }) => service_cmd::status(&config_path(&config), json),
        Some(Command::Logs {
            follow,
            lines,
            config,
        }) => service_cmd::logs(&config_path(&config), follow, lines),
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

/// The service commands: Linux only, they need flock, setsid and a unix socket.
mod service_cmd {
    use super::*;

    #[cfg(unix)]
    pub fn start(cfg: &Path, detach: bool, ignore_systemd: bool) -> ExitCode {
        service::cli::start(cfg, &exe_dir(), detach, ignore_systemd)
    }
    #[cfg(unix)]
    pub fn stop(cfg: &Path, force: bool, timeout: Option<u64>, ignore_systemd: bool) -> ExitCode {
        service::cli::stop(cfg, &exe_dir(), force, timeout, ignore_systemd)
    }
    #[cfg(unix)]
    pub fn restart(
        cfg: &Path,
        detach: bool,
        force: bool,
        timeout: Option<u64>,
        ignore_systemd: bool,
    ) -> ExitCode {
        service::cli::restart(cfg, &exe_dir(), detach, force, timeout, ignore_systemd)
    }
    #[cfg(unix)]
    pub fn status(cfg: &Path, json: bool) -> ExitCode {
        service::cli::status(cfg, &exe_dir(), json)
    }
    #[cfg(unix)]
    pub fn logs(cfg: &Path, follow: bool, lines: usize) -> ExitCode {
        service::cli::logs(cfg, &exe_dir(), follow, lines)
    }

    #[cfg(not(unix))]
    pub fn start(_: &Path, _: bool, _: bool) -> ExitCode {
        service::unsupported("start")
    }
    #[cfg(not(unix))]
    pub fn stop(_: &Path, _: bool, _: Option<u64>, _: bool) -> ExitCode {
        service::unsupported("stop")
    }
    #[cfg(not(unix))]
    pub fn restart(_: &Path, _: bool, _: bool, _: Option<u64>, _: bool) -> ExitCode {
        service::unsupported("restart")
    }
    #[cfg(not(unix))]
    pub fn status(_: &Path, _: bool) -> ExitCode {
        service::unsupported("status")
    }
    #[cfg(not(unix))]
    pub fn logs(_: &Path, _: bool, _: usize) -> ExitCode {
        service::unsupported("logs")
    }
}

fn serve(path: &Path, daemonized: bool) -> ExitCode {
    let cfg = match AppConfig::load(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("sip-faxd: error: {e}");
            return ExitCode::from(2);
        }
    };
    if daemonized {
        service::detach_from_terminal();
    }
    let sink = match service::init_logging(&cfg.log, &exe_dir(), daemonized) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("sip-faxd: error: {e}");
            return ExitCode::from(1);
        }
    };
    let code = match runtime() {
        Ok(rt) => rt.block_on(run(cfg, daemonized)),
        Err(c) => c,
    };
    // lets the compression of a log file rotated just now finish
    sink.finish();
    code
}

/// How long the shutdown waits for the database write queue.
const RECORDER_GRACE: Duration = Duration::from_secs(10);

/// Seconds between the "waiting for N calls" lines of a stop.
const DRAIN_REPORT_EVERY: Duration = Duration::from_secs(5);

async fn run(cfg: AppConfig, daemonized: bool) -> ExitCode {
    let base = exe_dir();
    // Signals first: a SIGTERM during the start-up is answered by the stop sequence below, not
    // by the default action.
    let mut signals = service::Signals::install();
    // One instance only. The PID file is locked for as long as this function runs.
    let _pid_lock = match service::PidLock::acquire(&cfg.service.pid_path(&base)) {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("{e}");
            eprintln!("sip-faxd: error: {e}");
            return ExitCode::from(1);
        }
    };
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
            // a detached service has no stderr: the log file is where `start` looks
            tracing::error!("database: {e}");
            eprintln!("sip-faxd: error: database: {e}");
            return ExitCode::from(1);
        }
    }
    let data_dir = cfg.data_dir(&base);
    let mut rcfg = RecorderConfig::new(data_dir.clone());
    rcfg.spill_dir = Some(cfg.calls_dir(&base));
    // Delivery: only when routes are configured. The recorder announces every fax row it has
    // written; a database that is down or a failing delivery never reaches the call path.
    let delivery = if cfg.routes.routes.is_empty() {
        tracing::info!("no [[routes]] configured: received faxes are stored, not delivered");
        None
    } else {
        Some(DeliveryService::start_default(
            db.clone(),
            cfg.routes.clone(),
            data_dir.clone(),
            cfg.delivery.clone(),
        ))
    };
    rcfg.on_finalized = delivery.as_ref().map(DeliveryService::hook);
    let recorder = Arc::new(Recorder::start(db.clone(), rcfg));
    let purge_task = tokio::spawn(commands::purge_daily(db.clone(), data_dir));

    let opts = StartOptions {
        session_factory: None,
        base_dir: Some(base.clone()),
    };
    let trunk_list: Vec<(String, bool)> = cfg
        .fax_in
        .trunks
        .iter()
        .map(|t| (t.name.clone(), t.register.enabled))
        .collect();
    let service_cfg = cfg.service.clone();
    let (fax, mut events) = match FaxIn::start(cfg.fax_in, opts).await {
        Ok(x) => x,
        Err(e) => {
            tracing::error!("{e}");
            eprintln!("sip-faxd: error: {e}");
            purge_task.abort();
            recorder.shutdown(RECORDER_GRACE).await;
            if let Some(d) = delivery {
                d.shutdown().await;
            }
            return ExitCode::from(1);
        }
    };
    // The control socket comes up only now, with the SIP sockets open: a `start` that sees it
    // answer knows the service is ready.
    let regs = fax.registration_view();
    let listen: Vec<String> = fax
        .udp_addr()
        .map(|a| format!("udp {a}"))
        .into_iter()
        .chain(fax.tcp_addr().map(|a| format!("tcp {a}")))
        .collect();
    let hub = Hub::new(
        HubInfo {
            pid: std::process::id(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            listen,
        },
        Duration::from_secs(service_cfg.stop_timeout_secs),
        move || {
            let states = regs.snapshot();
            trunk_list
                .iter()
                .map(|(name, registers)| TrunkStatus {
                    name: name.clone(),
                    registration: if *registers {
                        states
                            .iter()
                            .find(|(n, _)| n == name)
                            .map_or_else(|| "unknown".to_string(), |(_, s)| s.clone())
                    } else {
                        "none".to_string()
                    },
                })
                .collect()
        },
    );
    let control = match service::Control::bind(&service_cfg.socket_path(&base), hub.clone()) {
        Ok(c) => Some(c),
        Err(e) if daemonized => {
            tracing::error!("{e}");
            eprintln!("sip-faxd: error: {e}");
            purge_task.abort();
            fax.shutdown().await;
            recorder.shutdown(RECORDER_GRACE).await;
            if let Some(d) = delivery {
                d.shutdown().await;
            }
            db.close().await;
            return ExitCode::from(1);
        }
        Err(e) => {
            tracing::warn!("no control socket (`status` and `stop` will not work): {e}");
            None
        }
    };
    tracing::info!(udp = ?fax.udp_addr(), tcp = ?fax.tcp_addr(), "sip-faxd ready");
    let rec = recorder.clone();
    let hub_events = hub.clone();
    let logger = tokio::spawn(async move {
        while let Some(ev) = events.recv().await {
            hub_events.on_event(&ev);
            rec.record(&ev);
            if let CallEvent::Finished(r) = ev {
                match serde_json::to_string(&*r) {
                    Ok(json) => tracing::info!(target: "call_result", "{json}"),
                    Err(e) => tracing::error!("cannot serialise a call result: {e}"),
                }
            }
        }
    });
    // Stop: SIGTERM / Ctrl+C or a `stop` on the control socket. Same sequence for all.
    let request = tokio::select! {
        () = signals.next() => {
            hub.request_stop(false, None)
        }
        r = hub.stop_requested() => r,
    };
    tracing::info!(
        force = request.force,
        timeout_secs = request.timeout.as_secs(),
        "stopping: no new calls are accepted, {} call(s) in progress",
        fax.active_calls()
    );
    // a second signal while waiting means "now"
    let force_hub = hub.clone();
    tokio::spawn(async move {
        loop {
            signals.next().await;
            tracing::warn!("second stop signal: ending the calls now");
            force_hub.force_now();
        }
    });
    let hub_progress = hub.clone();
    hub.set_progress(Progress::Draining {
        active: fax.active_calls(),
        remaining_secs: request.timeout.as_secs(),
    });
    let left = fax
        .drain(
            request.timeout,
            hub.force_receiver(),
            DRAIN_REPORT_EVERY,
            move |p| {
                tracing::info!(
                    active = p.active,
                    remaining_secs = p.remaining.as_secs(),
                    "waiting for the calls to end"
                );
                hub_progress.set_progress(Progress::Draining {
                    active: p.active,
                    remaining_secs: p.remaining.as_secs(),
                });
            },
        )
        .await;
    if left > 0 {
        tracing::warn!(
            calls = left,
            "ending the calls that are still running; the pages received so far are kept"
        );
    }
    tracing::info!("shutting down");
    // fax-in first (ends what is left, then the sockets), then the recorder's queue, then the
    // delivery service (M3f), then the database
    fax.shutdown().await;
    let _ = logger.await;
    purge_task.abort();
    recorder.shutdown(RECORDER_GRACE).await;
    if let Some(d) = delivery {
        d.shutdown().await;
    }
    db.close().await;
    hub.set_progress(Progress::Stopped);
    if let Some(c) = control {
        c.shutdown().await;
    }
    tracing::info!("sip-faxd stopped");
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
