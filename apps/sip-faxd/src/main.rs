//! `sip-faxd`: the SIP fax server executable.
//!
//! ```text
//! sip-faxd serve [--config PATH]               run the fax-in controller (foreground)
//! sip-faxd start [-d] [--config PATH]          start it in the background, then show its log
//! sip-faxd stop [--force] [--timeout SECS]     stop it, after the running calls (Linux)
//! sip-faxd restart | status | logs [-f] [-n N] ...
//! sip-faxd tui [--config PATH]                 terminal screen: faxes, calls, status
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
mod service;
mod settings;
#[cfg(unix)]
mod tui;
mod web_service;

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
    /// Accounts of the web interface.
    User {
        #[command(subcommand)]
        action: admin_cli::UserAction,
    },
    /// API keys for other systems that read the faxes (create, list, revoke).
    Apikey {
        #[command(subcommand)]
        action: admin_cli::ApiKeyAction,
    },
    /// Received faxes.
    Fax {
        #[command(subcommand)]
        action: admin_cli::FaxAction,
    },
    /// Terminal screen: faxes, calls in progress, status (needs the running service for live data).
    Tui(ConfigArg),
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
    /// Copy every row of this database (`[database] url`) into another, empty database, e.g. from
    /// SQLite to SQL Server. The service must be stopped. Afterwards set `[database] url` to the
    /// target and start the service again.
    Copy {
        /// The target database URL (same forms as `[database] url`).
        #[arg(
            long,
            value_name = "URL",
            conflicts_with = "to_config",
            required_unless_present = "to_config"
        )]
        to: Option<String>,
        /// Read the password of the --to URL from stdin (one line, as it is: no percent-encoding).
        /// The URL then has a user but no password (`mssql://sa@host:1433/faxdb`).
        #[arg(long, requires = "to")]
        to_password_stdin: bool,
        /// Take the target from the [database] table of another configuration file (any of its
        /// forms: url, or type / host / name / user / password / password_file).
        #[arg(long, value_name = "FILE")]
        to_config: Option<PathBuf>,
        #[command(flatten)]
        config: ConfigArg,
    },
    /// Test the connection: connect, show the server and schema versions, change nothing.
    Check {
        /// Test this URL instead of [database] of the configuration file.
        #[arg(long, value_name = "URL")]
        url: Option<String>,
        /// Read the password of --url from stdin (one line, as it is: no percent-encoding).
        #[arg(long, requires = "url")]
        password_stdin: bool,
        #[command(flatten)]
        config: ConfigArg,
    },
}

/// The directory of the executable: relative paths of the configuration are relative to it.
pub(crate) fn exe_dir() -> PathBuf {
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
        Some(Command::Apikey { action }) => admin_cli::apikey(action),
        Some(Command::Fax { action }) => admin_cli::fax(action),
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
        Some(Command::Tui(arg)) => service_cmd::tui(&config_path(&arg)),
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
        Some(Command::Db {
            action:
                DbAction::Copy {
                    to,
                    to_password_stdin,
                    to_config,
                    config,
                },
        }) => with_runtime(&config_path(&config), |cfg| async move {
            let target = match commands::copy_target(to, to_password_stdin, to_config) {
                Ok(t) => t,
                Err(code) => return code,
            };
            commands::db_copy(&cfg, &exe_dir(), &target).await
        }),
        Some(Command::Db {
            action:
                DbAction::Check {
                    url,
                    password_stdin,
                    config,
                },
        }) => {
            let path = config_path(&config);
            match url {
                // another database: the configuration file is not needed
                Some(u) => commands::db_check_url(u, password_stdin),
                None => with_runtime(&path, |cfg| async move {
                    commands::db_check(&cfg, &exe_dir()).await
                }),
            }
        }
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

    #[cfg(unix)]
    pub fn tui(cfg: &Path) -> ExitCode {
        tui::run(cfg)
    }
    #[cfg(not(unix))]
    pub fn tui(_: &Path) -> ExitCode {
        service::unsupported("tui")
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

/// The trunks of the running configuration and their registration state, for `status`.
fn trunk_status(fax: &std::sync::Weak<FaxIn>, regs: &fax_in::RegistrationView) -> Vec<TrunkStatus> {
    let states = regs.snapshot();
    let state_of = |name: &str| {
        states
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, s)| s.clone())
    };
    match fax.upgrade() {
        Some(f) => f
            .snapshot()
            .table
            .trunks()
            .iter()
            .map(|t| TrunkStatus {
                name: t.name().to_string(),
                registration: if t.cfg.register.enabled {
                    state_of(t.name()).unwrap_or_else(|| "unknown".to_string())
                } else {
                    "none".to_string()
                },
            })
            .collect(),
        // shutting down: only the registrations are still known
        None => states
            .into_iter()
            .map(|(name, registration)| TrunkStatus { name, registration })
            .collect(),
    }
}

/// How long the shutdown waits for the database write queue.
const RECORDER_GRACE: Duration = Duration::from_secs(10);

/// How long the web interface's requests in progress may take at the shutdown.
const WEB_GRACE: Duration = Duration::from_secs(5);

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
    // The database first: a schema this binary cannot work with stops the start here, before a
    // single call is answered. An unreachable database does not: the calls are received and
    // written to the database as soon as it is back.
    let db = Arc::new(Db::new(&cfg, &base));
    let mut db_ready = false;
    match db.start().await {
        Ok(Startup::Ready { stale_fixed }) => {
            db_ready = true;
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
    // The runtime configuration (sip, media, call, capture, trunks, routes, delivery, retention)
    // lives in the database from the first start on (ADR-051); the file only seeds it.
    let rt = runtime::Runtime::start(&cfg, db.clone(), db_ready).await;
    let live = rt.current();
    if live.trunks.is_empty() {
        tracing::warn!("{}", commands::NO_TRUNK);
    }
    let fax_cfg = live.to_fax_in(&cfg.fax_in.storage);
    let advertised_missing = fax_cfg.advertises_wildcard();
    for w in fax_cfg
        .warnings()
        .into_iter()
        .chain(cfg.database.warnings.iter().cloned())
    {
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
    // the web interface's events: what reached the database, and the delivery changes
    let web_events = Arc::new(web::events::EventBus::new());
    rcfg.on_written = Some(web_service::recorder_events(web_events.clone()));
    let bus = web_events.clone();
    delivery.set_on_change(Arc::new(move |id| bus.delivery_updated(id)));
    let recorder = Arc::new(Recorder::start(db.clone(), rcfg));
    let purge_task = tokio::spawn(commands::purge_daily(db.clone(), data_dir));

    let opts = StartOptions {
        session_factory: None,
        base_dir: Some(base.clone()),
    };
    let service_cfg = cfg.service.clone();
    let (fax, mut events) = match FaxIn::start(fax_cfg, opts).await {
        Ok((fax, events)) => (Arc::new(fax), events),
        Err(e) => {
            tracing::error!("{e}");
            eprintln!("sip-faxd: error: {e}");
            rt.detach();
            purge_task.abort();
            recorder.shutdown(RECORDER_GRACE).await;
            delivery.shutdown().await;
            return ExitCode::from(1);
        }
    };
    // The control socket comes up only now, with the SIP sockets open: a `start` that sees it
    // answer knows the service is ready.
    let regs = fax.registration_view();
    let weak_fax = Arc::downgrade(&fax);
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
        move || trunk_status(&weak_fax, &regs),
    );
    hub.set_reloader(Arc::new(runtime::ConfigReloader(rt.config.clone())));
    // the web interface before the control socket: a service that answers `status` serves it
    let web = match web_service::start(
        &cfg,
        &base,
        rt.auth.clone(),
        rt.config.clone(),
        hub.clone(),
        web_events,
        db.clone(),
        advertised_missing,
    )
    .await
    {
        Ok(w) => w,
        Err(e) => {
            tracing::error!("{e}");
            eprintln!("sip-faxd: error: {e}");
            purge_task.abort();
            rt.detach();
            if let Ok(f) = Arc::try_unwrap(fax) {
                f.shutdown().await;
            }
            recorder.shutdown(RECORDER_GRACE).await;
            delivery.shutdown().await;
            db.close().await;
            return ExitCode::from(1);
        }
    };
    let control = match service::Control::bind(&service_cfg.socket_path(&base), hub.clone()) {
        Ok(c) => Some(c),
        Err(e) if daemonized => {
            tracing::error!("{e}");
            eprintln!("sip-faxd: error: {e}");
            purge_task.abort();
            if let Ok(f) = Arc::try_unwrap(fax) {
                f.shutdown().await;
            }
            recorder.shutdown(RECORDER_GRACE).await;
            rt.detach();
            delivery.shutdown().await;
            db.close().await;
            return ExitCode::from(1);
        }
        Err(e) => {
            tracing::warn!("no control socket (`status` and `stop` will not work): {e}");
            None
        }
    };
    // from now on a configuration change reaches the running server
    rt.attach(fax.clone(), delivery.reconfigurer());
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
    if let Some(w) = web {
        w.shutdown(WEB_GRACE).await;
    }
    // fax-in first (ends what is left, then the sockets), then the recorder's queue, then the
    // delivery service (M3f), then the database
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
