//! The administration subcommands (M4a, docs/design/m4-web.md section 7.4): `user ...`,
//! `fax ...` and `config import / export`. They talk to the database directly (the service does
//! not have to run) and record `cli:<operating system user>` as the operator.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use accounts::{AuthConfig, AuthService, NewAccount, Role};
use chrono::{DateTime, Utc};
use clap::Subcommand;
use runtime_config::{ConfigService, ImportError, SecretBox, StoreSource};
use store::{FaxRecord, FaxStatus, FaxStore, ListFilter, PageRequest, SystemClock};

use crate::ConfigArg;
use crate::db::Db;
use crate::runtime::cli_operator;
use crate::settings::{AppConfig, mask_url};

fn fail(msg: impl std::fmt::Display) -> ExitCode {
    eprintln!("sip-faxd: error: {msg}");
    ExitCode::from(1)
}

fn ts(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%d %H:%M:%S").to_string()
}

fn opt(s: &Option<String>) -> &str {
    s.as_deref().unwrap_or("-")
}

// ------------------------------------------------------------------------ passwords

/// Reads a password: one line from stdin with `--password-stdin`, else a prompt (asked twice;
/// the terminal echo is switched off on Unix).
fn read_password(from_stdin: bool, who: &str) -> Result<String, String> {
    let stdin = std::io::stdin();
    if from_stdin {
        let mut line = String::new();
        stdin
            .lock()
            .read_line(&mut line)
            .map_err(|e| format!("cannot read the password: {e}"))?;
        let pw = line.trim_end_matches(['\r', '\n']).to_string();
        if pw.is_empty() {
            return Err("no password on stdin".into());
        }
        return Ok(pw);
    }
    let ask = |prompt: &str| -> Result<String, String> {
        eprint!("{prompt}");
        let _ = std::io::stderr().flush();
        #[cfg(unix)]
        let _ = std::process::Command::new("stty").arg("-echo").status();
        let mut line = String::new();
        let r = stdin.lock().read_line(&mut line);
        #[cfg(unix)]
        {
            let _ = std::process::Command::new("stty").arg("echo").status();
            eprintln!();
        }
        r.map_err(|e| format!("cannot read the password: {e}"))?;
        Ok(line.trim_end_matches(['\r', '\n']).to_string())
    };
    let a = ask(&format!("Password for {who}: "))?;
    let b = ask("Again: ")?;
    if a != b {
        return Err("the two passwords differ".into());
    }
    Ok(a)
}

// ------------------------------------------------------------------------ user

/// `sip-faxd user ...`
#[derive(Subcommand)]
pub enum UserAction {
    /// Create an account.
    Add {
        /// Login name (a-z, 0-9, `.`, `_`, `-`).
        name: String,
        /// `admin` or `agent`.
        #[arg(long, default_value = "agent")]
        role: String,
        /// Read the password from stdin (one line) instead of asking for it.
        #[arg(long)]
        password_stdin: bool,
        /// Name shown in the interface.
        #[arg(long)]
        display_name: Option<String>,
        /// Interface language (`zh-TW` or `en`).
        #[arg(long)]
        locale: Option<String>,
        /// The first login has to choose a new password.
        #[arg(long)]
        must_change: bool,
        #[command(flatten)]
        config: ConfigArg,
    },
    /// Set a new password (ends all sessions of the account).
    Passwd {
        /// Login name.
        name: String,
        /// Read the password from stdin (one line) instead of asking for it.
        #[arg(long)]
        password_stdin: bool,
        /// The next login has to choose another password.
        #[arg(long)]
        must_change: bool,
        #[command(flatten)]
        config: ConfigArg,
    },
    /// Disable an account (ends its sessions).
    Disable {
        /// Login name.
        name: String,
        #[command(flatten)]
        config: ConfigArg,
    },
    /// Enable an account again.
    Enable {
        /// Login name.
        name: String,
        #[command(flatten)]
        config: ConfigArg,
    },
    /// List the accounts.
    List {
        #[command(flatten)]
        config: ConfigArg,
    },
}

fn auth_service(db: &Arc<Db>) -> AuthService {
    let src: Arc<dyn StoreSource> = db.clone();
    AuthService::new(src, Arc::new(SystemClock), AuthConfig::default())
}

/// Entry point of `sip-faxd user ...`.
pub fn user(action: UserAction) -> ExitCode {
    let arg = match &action {
        UserAction::Add { config, .. }
        | UserAction::Passwd { config, .. }
        | UserAction::Disable { config, .. }
        | UserAction::Enable { config, .. }
        | UserAction::List { config } => ConfigArg {
            config: config.config.clone(),
        },
    };
    crate::with_runtime(&crate::config_path(&arg), |cfg| async move {
        let db = Arc::new(Db::new(&cfg, &crate::exe_dir()));
        let code = run_user(&cfg, &db, action).await;
        db.close().await;
        code
    })
}

async fn run_user(cfg: &AppConfig, db: &Arc<Db>, action: UserAction) -> ExitCode {
    if let Err(e) = db.store().await {
        return fail(format!("database {}: {e}", mask_url(&cfg.database.url)));
    }
    let auth = auth_service(db);
    let by = cli_operator();
    match action {
        UserAction::Add {
            name,
            role,
            password_stdin,
            display_name,
            locale,
            must_change,
            ..
        } => {
            let Some(role) = Role::parse(&role) else {
                return fail("the role must be admin or agent");
            };
            let pw = match read_password(password_stdin, &name) {
                Ok(p) => p,
                Err(e) => return fail(e),
            };
            let account = NewAccount {
                username: &name,
                role,
                password: &pw,
                display_name: display_name.as_deref(),
                locale: locale.as_deref(),
                must_change,
            };
            match auth.create_user(&account, &by).await {
                Ok(u) => {
                    println!("user {} ({}) created", u.username, u.role);
                    ExitCode::SUCCESS
                }
                Err(e) => fail(format!("{e} ({})", e.code())),
            }
        }
        UserAction::Passwd {
            name,
            password_stdin,
            must_change,
            ..
        } => {
            let pw = match read_password(password_stdin, &name) {
                Ok(p) => p,
                Err(e) => return fail(e),
            };
            match auth.set_password(&name, &pw, must_change, &by).await {
                Ok(()) => {
                    println!("password of {name} changed; its sessions were ended");
                    ExitCode::SUCCESS
                }
                Err(e) => fail(format!("{e} ({})", e.code())),
            }
        }
        UserAction::Disable { name, .. } => {
            match auth.update_user(&name, None, Some(true), None, None, &by).await {
                Ok(_) => {
                    println!("user {name} disabled; its sessions were ended");
                    ExitCode::SUCCESS
                }
                Err(e) => fail(format!("{e} ({})", e.code())),
            }
        }
        UserAction::Enable { name, .. } => {
            match auth.update_user(&name, None, Some(false), None, None, &by).await {
                Ok(_) => {
                    println!("user {name} enabled");
                    ExitCode::SUCCESS
                }
                Err(e) => fail(format!("{e} ({})", e.code())),
            }
        }
        UserAction::List { .. } => match auth.list_users().await {
            Ok(list) => {
                println!(
                    "{:<20} {:<6} {:<9} {:<19} NAME",
                    "USERNAME", "ROLE", "STATUS", "LAST LOGIN"
                );
                for u in list {
                    println!(
                        "{:<20} {:<6} {:<9} {:<19} {}",
                        u.username,
                        u.role,
                        if u.disabled {
                            "disabled"
                        } else if u.must_change_password {
                            "must-change"
                        } else {
                            "enabled"
                        },
                        u.last_login_at.map_or("-".to_string(), ts),
                        u.display_name.as_deref().unwrap_or("")
                    );
                }
                ExitCode::SUCCESS
            }
            Err(e) => fail(e),
        },
    }
}

// ------------------------------------------------------------------------ fax

/// `sip-faxd fax ...`
#[derive(Subcommand)]
pub enum FaxAction {
    /// List received faxes, newest first.
    List {
        /// Only unread faxes.
        #[arg(long)]
        unread: bool,
        /// Only this status (`receiving`, `received`, `partial`, `failed`, `no_fax`).
        #[arg(long)]
        status: Option<String>,
        /// How many to show.
        #[arg(long, default_value_t = 20)]
        limit: u32,
        #[command(flatten)]
        config: ConfigArg,
    },
    /// Show one fax with its delivery attempts.
    Show {
        /// Fax id.
        id: i64,
        #[command(flatten)]
        config: ConfigArg,
    },
    /// Mark a fax as read (or unread).
    Read {
        /// Fax id.
        id: i64,
        /// Mark it unread instead.
        #[arg(long)]
        unread: bool,
        #[command(flatten)]
        config: ConfigArg,
    },
    /// Replace the note of a fax.
    Note {
        /// Fax id.
        id: i64,
        /// The note (empty text clears it).
        text: String,
        #[command(flatten)]
        config: ConfigArg,
    },
    /// Queue the failed or abandoned deliveries of a fax again.
    Redeliver {
        /// Fax id.
        id: i64,
        #[command(flatten)]
        config: ConfigArg,
    },
}

/// Entry point of `sip-faxd fax ...`.
pub fn fax(action: FaxAction) -> ExitCode {
    let arg = match &action {
        FaxAction::List { config, .. }
        | FaxAction::Show { config, .. }
        | FaxAction::Read { config, .. }
        | FaxAction::Note { config, .. }
        | FaxAction::Redeliver { config, .. } => ConfigArg {
            config: config.config.clone(),
        },
    };
    crate::with_runtime(&crate::config_path(&arg), |cfg| async move {
        let db = Db::new(&cfg, &crate::exe_dir());
        let code = match db.store().await {
            Ok(st) => run_fax(&*st, action, &cli_operator()).await,
            Err(e) => fail(format!("database {}: {e}", mask_url(&cfg.database.url))),
        };
        db.close().await;
        code
    })
}

fn line(r: &FaxRecord) -> String {
    format!(
        "{:<7} {:<19} {:<12} {:<14} {:<14} {:<9} {:>3} {}",
        r.id,
        ts(r.started_at),
        r.trunk,
        opt(&r.ani),
        opt(&r.dnis),
        r.fax_status.to_string(),
        r.pages,
        if r.is_read { "read" } else { "UNREAD" }
    )
}

async fn run_fax(st: &dyn FaxStore, action: FaxAction, by: &str) -> ExitCode {
    match action {
        FaxAction::List {
            unread,
            status,
            limit,
            ..
        } => {
            let fax_status = match status.as_deref().map(str::parse::<FaxStatus>) {
                None => None,
                Some(Ok(s)) => Some(s),
                Some(Err(_)) => {
                    return fail("status must be receiving, received, partial, failed or no_fax");
                }
            };
            let filter = ListFilter {
                is_read: unread.then_some(false),
                fax_status,
                ..ListFilter::default()
            };
            match st
                .list(
                    &filter,
                    PageRequest {
                        cursor: None,
                        limit,
                    },
                )
                .await
            {
                Ok(p) => {
                    println!(
                        "{:<7} {:<19} {:<12} {:<14} {:<14} {:<9} {:>3} STATE",
                        "ID", "STARTED (UTC)", "TRUNK", "ANI", "DNIS", "STATUS", "PG"
                    );
                    for r in &p.items {
                        println!("{}", line(r));
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => fail(e),
            }
        }
        FaxAction::Show { id, .. } => {
            let r = match st.get(id).await {
                Ok(Some(r)) => r,
                Ok(None) => return fail(format!("no fax with id {id}")),
                Err(e) => return fail(e),
            };
            let kv = |k: &str, v: String| println!("{k:<16}{v}");
            kv("id", r.id.to_string());
            kv("call id", r.call_id.clone());
            kv("trunk", r.trunk.clone());
            kv("ani", format!("{} ({})", opt(&r.ani), opt(&r.ani_source)));
            kv("dnis", format!("{} ({})", opt(&r.dnis), opt(&r.dnis_source)));
            kv("started", ts(r.started_at));
            kv("ended", r.ended_at.map_or("-".into(), ts));
            kv("duration", r.duration_sec.map_or("-".into(), |d| format!("{d} s")));
            kv("status", r.fax_status.to_string());
            kv("pages", r.pages.to_string());
            kv(
                "hangup",
                format!(
                    "{} ({})",
                    r.hangup_code.as_deref().unwrap_or("-"),
                    r.hangup_direction.map_or("-".to_string(), |d| d.to_string())
                ),
            );
            kv("remote ident", opt(&r.remote_ident).to_string());
            kv("tiff", opt(&r.tiff_path).to_string());
            kv("pdf", opt(&r.pdf_path).to_string());
            kv(
                "files purged",
                r.files_purged_at.map_or("no".into(), ts),
            );
            kv(
                "read",
                match (r.is_read, &r.read_changed_by) {
                    (true, Some(b)) => format!("read (by {b})"),
                    (true, None) => "read".into(),
                    (false, _) => "unread".into(),
                },
            );
            kv(
                "note",
                match &r.note {
                    Some(n) if !n.is_empty() => format!(
                        "{n} (by {}, {})",
                        opt(&r.note_updated_by),
                        r.note_updated_at.map_or("-".into(), ts)
                    ),
                    _ => "-".into(),
                },
            );
            match st.list_deliveries(id).await {
                Ok(list) if list.is_empty() => println!("deliveries      none"),
                Ok(list) => {
                    println!("deliveries:");
                    for d in list {
                        println!(
                            "  #{:<5} attempt {:<2} {:<9} {} {}",
                            d.id,
                            d.attempt,
                            d.status,
                            d.target,
                            d.last_error.map(|e| format!("({e})")).unwrap_or_default()
                        );
                    }
                }
                Err(e) => return fail(e),
            }
            ExitCode::SUCCESS
        }
        FaxAction::Read { id, unread, .. } => match st.set_read(id, !unread, by).await {
            Ok(r) => {
                println!(
                    "fax {} marked {} by {by}",
                    r.id,
                    if r.is_read { "read" } else { "unread" }
                );
                ExitCode::SUCCESS
            }
            Err(store::Error::NotFound(_)) => fail(format!("no fax with id {id}")),
            Err(e) => fail(e),
        },
        FaxAction::Note { id, text, .. } => {
            let cur = match st.get(id).await {
                Ok(Some(r)) => r,
                Ok(None) => return fail(format!("no fax with id {id}")),
                Err(e) => return fail(e),
            };
            match st
                .set_note(id, &text, by, cur.note_updated_at)
                .await
            {
                Ok(_) => {
                    println!("note of fax {id} saved by {by}");
                    ExitCode::SUCCESS
                }
                Err(e) => fail(e),
            }
        }
        FaxAction::Redeliver { id, .. } => {
            match st.get(id).await {
                Ok(Some(_)) => {}
                Ok(None) => return fail(format!("no fax with id {id}")),
                Err(e) => return fail(e),
            }
            match st.redeliver(id, Utc::now()).await {
                Ok(ids) if ids.is_empty() => {
                    println!(
                        "fax {id}: nothing to redeliver (no failed or abandoned delivery; or one is still waiting)"
                    );
                    ExitCode::SUCCESS
                }
                Ok(ids) => {
                    println!(
                        "fax {id}: {} delivery attempt(s) queued by {by}; a running sip-faxd picks them up within a few seconds",
                        ids.len()
                    );
                    ExitCode::SUCCESS
                }
                Err(e) => fail(e),
            }
        }
    }
}

// ------------------------------------------------------------------------ config import / export

fn config_service(cfg: &AppConfig, db: &Arc<Db>, file: runtime_config::RuntimeConfig) -> ConfigService {
    let src: Arc<dyn StoreSource> = db.clone();
    let secrets = Arc::new(SecretBox::load_or_create(&cfg.conf_dir.join("secret.key")));
    ConfigService::new(src, secrets, file, Vec::new())
}

/// `sip-faxd config import [--force] [--file PATH]`: writes the runtime sections of a
/// configuration file into the database.
pub async fn config_import(
    cfg: &AppConfig,
    base: &Path,
    force: bool,
    file: Option<PathBuf>,
) -> ExitCode {
    let runtime = match &file {
        None => cfg.runtime.clone(),
        // a complete configuration file or an exported one (no `[database]`): both parse
        Some(p) => match AppConfig::load(p) {
            Ok(f) => f.runtime,
            Err(e) => return fail(e),
        },
    };
    let db = Arc::new(Db::new(cfg, base));
    let svc = config_service(cfg, &db, runtime.clone());
    let by = cli_operator();
    let code = match svc.import(&runtime, force, &by).await {
        Ok(rep) => {
            println!(
                "imported {} section(s) into the database{}",
                rep.written.len(),
                if rep.removed.is_empty() {
                    String::new()
                } else {
                    format!(", removed {}", rep.removed.join(", "))
                }
            );
            println!("a running sip-faxd applies it on `reload` (or at the next start)");
            ExitCode::SUCCESS
        }
        Err(ImportError::AlreadyConfigured(n)) => fail(format!(
            "the database already holds runtime configuration ({n} section(s)); nothing was changed. Use --force to replace it"
        )),
        Err(e) => fail(e),
    };
    db.close().await;
    code
}

/// `sip-faxd config export [--include-secrets]`: the database's runtime configuration as TOML.
pub async fn config_export(cfg: &AppConfig, base: &Path, include_secrets: bool) -> ExitCode {
    let db = Arc::new(Db::new(cfg, base));
    let svc = config_service(cfg, &db, cfg.runtime.clone());
    let code = match svc.export(include_secrets).await {
        Ok(text) => {
            print!("{text}");
            ExitCode::SUCCESS
        }
        Err(e) => fail(e),
    };
    db.close().await;
    code
}
