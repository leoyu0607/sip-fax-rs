//! `sip-faxd`: the SIP fax server executable.
//!
//! ```text
//! sip-faxd serve [--config PATH]          run the fax-in controller
//! sip-faxd config check [--config PATH]   validate a configuration file
//! ```
//!
//! Without `--config` the file is `conf/sip-faxd.toml` next to the executable. Relative
//! paths inside the configuration (the capture directory) are relative to the executable's
//! directory too, so an unpacked tarball is self-contained.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use fax_in::{CallEvent, FaxIn, FaxInConfig, StartOptions};
use serde::Deserialize;

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
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Validate a configuration file and print what it means.
    Check(ConfigArg),
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct LogSection {
    /// Default log filter (overridden by `RUST_LOG`).
    level: String,
}

struct AppConfig {
    log: LogSection,
    fax_in: FaxInConfig,
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

fn load(path: &Path) -> Result<AppConfig, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read configuration {}: {e}", path.display()))?;
    let mut table: toml::Table = text.parse().map_err(|e: toml::de::Error| {
        format!("{}: invalid TOML: {}", path.display(), e.message())
    })?;
    let log = match table.remove("log") {
        None => LogSection::default(),
        Some(v) => v
            .try_into()
            .map_err(|e: toml::de::Error| format!("{}: [log]: {}", path.display(), e.message()))?,
    };
    let fax_in = FaxInConfig::from_table(table).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(AppConfig { log, fax_in })
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
        }) => config_check(&config_path(&arg)),
        Some(Command::Serve(arg)) => serve(&config_path(&arg)),
    }
}

fn config_check(path: &Path) -> ExitCode {
    let cfg = match load(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("sip-faxd: error: {e}");
            return ExitCode::from(2);
        }
    };
    let f = &cfg.fax_in;
    println!("configuration OK: {}", path.display());
    println!(
        "  sip: bind {} ({}), unknown_source = {:?}",
        f.sip.bind,
        f.transports()
            .iter()
            .map(|t| format!("{t:?}").to_lowercase())
            .collect::<Vec<_>>()
            .join("+"),
        f.sip.unknown_source
    );
    println!(
        "  capture: {} ({:?}) -> {}",
        if f.capture.enabled { "on" } else { "off" },
        f.capture.keep,
        f.capture.dir.display()
    );
    println!(
        "  fax session: {:?}, received faxes -> {}/faxes/<YYYY>/<MM>/<DD>/<call-id>.tif|pdf",
        f.call.fax_session,
        f.storage.data_dir.display()
    );
    for t in &f.trunks {
        println!(
            "  trunk {}: preset {}, peer {}, allowed {:?}, {:?}, t38_policy {:?}, register {}",
            t.name,
            t.preset,
            t.peer.as_deref().unwrap_or("-"),
            t.allowed_sources,
            t.transport,
            t.t38_policy,
            if t.register.enabled { "on" } else { "off" }
        );
    }
    for w in f.warnings() {
        println!("warning: {w}");
    }
    ExitCode::SUCCESS
}

fn serve(path: &Path) -> ExitCode {
    let cfg = match load(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("sip-faxd: error: {e}");
            return ExitCode::from(2);
        }
    };
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
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("sip-faxd: error: cannot start the async runtime: {e}");
            return ExitCode::from(1);
        }
    };
    rt.block_on(run(cfg.fax_in))
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

async fn run(cfg: FaxInConfig) -> ExitCode {
    for w in cfg.warnings() {
        tracing::warn!("{w}");
    }
    let opts = StartOptions {
        session_factory: None,
        base_dir: Some(exe_dir()),
    };
    let (fax, mut events) = match FaxIn::start(cfg, opts).await {
        Ok(x) => x,
        Err(e) => {
            eprintln!("sip-faxd: error: {e}");
            return ExitCode::from(1);
        }
    };
    tracing::info!(udp = ?fax.udp_addr(), tcp = ?fax.tcp_addr(), "sip-faxd ready");
    let logger = tokio::spawn(async move {
        while let Some(ev) = events.recv().await {
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
