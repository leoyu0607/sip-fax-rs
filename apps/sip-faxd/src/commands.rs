//! The one-shot subcommands (`config check`, `db migrate`, `db backfill`, `purge`) and the daily
//! purge task of `serve`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use store::{ConnectOptions, Purger, RetentionSettings, SystemClock};

use crate::db::Db;
use crate::schedule;
use crate::settings::{AppConfig, mask_http_url, mask_url};

fn fail(msg: impl std::fmt::Display) -> ExitCode {
    eprintln!("sip-faxd: error: {msg}");
    ExitCode::from(1)
}

fn onoff(b: bool) -> &'static str {
    if b { "on" } else { "off" }
}

fn or_dash(s: &Option<String>) -> &str {
    s.as_deref().unwrap_or("-")
}

/// One line per route and per delivery for `config check`. Secrets never appear: passwords and
/// HMAC keys are shown as `***`, URLs go through [`mask_http_url`].
pub fn routes_summary(routes: &delivery::Routes) -> Vec<String> {
    use delivery::DeliveryConfig;
    let mut out = Vec::new();
    for (i, r) in routes.routes.iter().enumerate() {
        let mut when = Vec::new();
        if let Some(d) = &r.dnis {
            when.push(format!("dnis = {d}"));
        }
        if let Some(p) = &r.dnis_prefix {
            when.push(format!("dnis_prefix = {p}"));
        }
        if let Some(t) = &r.trunk {
            when.push(format!("trunk = {t}"));
        }
        if r.default {
            when.push("default (everything else)".into());
        }
        out.push(format!(
            "    route {}{}: {}; partial faxes {}",
            i + 1,
            r.name
                .as_deref()
                .map(|n| format!(" ({n})"))
                .unwrap_or_default(),
            when.join(" and "),
            if r.deliver_partial {
                "delivered"
            } else {
                "not delivered"
            }
        ));
        for d in &r.deliveries {
            out.push(match d {
                DeliveryConfig::Folder(f) => format!(
                    "      folder {} (filename {}, {})",
                    f.dir,
                    f.filename,
                    f.formats
                        .iter()
                        .map(|x| x.ext())
                        .collect::<Vec<_>>()
                        .join("+")
                ),
                DeliveryConfig::Email(e) => format!(
                    "      email {}:{} {:?}{}, from {}, to {}",
                    e.host,
                    e.effective_port(),
                    e.security,
                    match (&e.username, &e.password) {
                        (Some(u), Some(_)) => format!(", user {u} password ***"),
                        _ => String::new(),
                    },
                    e.from,
                    e.to.join(", ")
                ),
                DeliveryConfig::Webhook(w) => format!(
                    "      webhook {}{}{}",
                    mask_http_url(&w.url),
                    if w.secret.is_some() {
                        format!(", HMAC {} secret ***", w.signature_header)
                    } else {
                        ", unsigned".into()
                    },
                    if w.attach_pdf { ", with PDF" } else { "" }
                ),
            });
        }
    }
    out
}

/// What `config check` prints about the configuration file.
fn print_file_check(cfg: &AppConfig, path: &Path) {
    let f = &cfg.fax_in;
    println!("configuration OK: {}", path.display());
    let backend = store::backend_for_url(&cfg.database.url).map_or("?", |k| k.name());
    println!(
        "  database: {} ({backend}), auto_migrate = {}, max_connections = {}",
        mask_url(&cfg.database.url),
        cfg.database.auto_migrate,
        cfg.database.max_connections
    );
    for w in &cfg.database.warnings {
        println!("  warning: {w}");
    }
    println!(
        "  received faxes -> {}/faxes/<YYYY>/<MM>/<DD>/<call-id>.tif|pdf",
        f.storage.data_dir.display()
    );
    // the runtime settings live in the database (ADR-051, ADR-060); a file may still carry them
    if cfg.file_sections.is_empty() {
        println!(
            "  runtime settings (SIP, trunks, routes, ...): none in this file, they live in the database (web interface: Settings)"
        );
        return;
    }
    println!(
        "  runtime sections in this file: {} (imported only while the database has none; after that they are not used and can be removed)",
        cfg.file_sections.join(", ")
    );
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
    println!("  fax session: {:?}", f.call.fax_session);
    let r = &cfg.retention;
    let days = |d: u32| {
        if d == 0 {
            "never".to_string()
        } else {
            format!("after {d} days")
        }
    };
    println!(
        "  retention: files {}, records {}, daily run at {} (server local time); imported into the database on the first start, after that the database decides",
        days(r.files_days),
        days(r.records_days),
        r.run_at
    );
    if cfg.routes.routes.is_empty() {
        println!("  routes: none (received faxes are stored, nothing is delivered)");
    } else {
        println!(
            "  routes: {} (first match wins; catch-up at start: {}; up to {} attempts per delivery)",
            cfg.routes.routes.len(),
            if cfg.delivery.catch_up_hours == 0 {
                "off".to_string()
            } else {
                format!("last {} hours", cfg.delivery.catch_up_hours)
            },
            cfg.delivery.max_attempts
        );
        for l in routes_summary(&cfg.routes) {
            println!("{l}");
        }
    }
    for t in &f.trunks {
        println!(
            "  trunk {}: preset {}, peer {}, allowed {:?}, {:?}, t38_policy {:?}, register {}",
            t.name,
            t.preset,
            t.peer.as_deref().unwrap_or("-"),
            t.allowed_sources,
            t.transport,
            t.t38_policy,
            onoff(t.register.enabled)
        );
        println!(
            "    fax: resolutions {}, ecm {} (and MMR with it)",
            t.fax.resolutions.join("+"),
            onoff(t.fax.ecm)
        );
        let n = &t.numbers;
        println!(
            "    numbers: ani from {}, dnis from {}, strip_plus {}, ani_strip_prefix {}, dnis_strip_prefix {}",
            n.ani_sources.join(">"),
            n.dnis_sources.join(">"),
            onoff(n.strip_plus),
            or_dash(&n.ani_strip_prefix),
            or_dash(&n.dnis_strip_prefix)
        );
    }
    for w in f.warnings() {
        println!("warning: {w}");
    }
}

/// `sip-faxd config check`: the configuration file, and the runtime configuration in the
/// database when it can be reached (nothing is written to the database: no migration, no
/// import, no key file).
pub async fn config_check_full(cfg: &AppConfig, path: &Path, base: &Path) -> ExitCode {
    print_file_check(cfg, path);
    println!("database runtime configuration:");
    let opts = ConnectOptions {
        max_connections: 1,
        auto_migrate: false,
        ..ConnectOptions::default()
    };
    let st = match store::connect_with(&cfg.database_url(base), opts).await {
        Ok(s) => s,
        Err(e) if e.is_transient() => {
            println!(
                "  the database {} is not reachable ({e}): only the configuration file was checked",
                mask_url(&cfg.database.url)
            );
            return ExitCode::SUCCESS;
        }
        Err(store::Error::SchemaOutdated { .. }) => {
            println!(
                "  the database schema is older than this version: run `sip-faxd db migrate` (or start `serve`); only the configuration file was checked"
            );
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!(
                "sip-faxd: error: database {}: {e}",
                mask_url(&cfg.database.url)
            );
            return ExitCode::from(1);
        }
    };
    let key = cfg.conf_dir.join("secret.key");
    let secrets = if key.exists() {
        runtime_config::SecretBox::load_or_create(&key)
    } else {
        runtime_config::SecretBox::unavailable("conf/secret.key does not exist yet")
    };
    let src: Arc<dyn store::FaxStore> = st.clone();
    let svc = runtime_config::ConfigService::new(
        Arc::new(src),
        Arc::new(secrets),
        cfg.runtime.clone(),
        cfg.file_sections.clone(),
    );
    let code = match svc.inspect().await {
        Err(e) => {
            eprintln!("sip-faxd: error: cannot read the runtime configuration: {e}");
            ExitCode::from(1)
        }
        Ok(ins) if ins.sections == 0 => {
            println!(
                "  empty: the first start stores {} (then set the rest in the web interface)",
                if cfg.file_sections.is_empty() {
                    "the defaults, without any trunk"
                } else {
                    "the runtime sections of this file"
                }
            );
            ExitCode::SUCCESS
        }
        Ok(ins) => {
            let rt = &ins.config;
            println!("  {} stored section(s)", ins.sections);
            if !cfg.file_sections.is_empty() {
                println!(
                    "warning: the runtime sections of the configuration file are not used (the database has its own): remove them from the file: {}",
                    cfg.file_sections.join(", ")
                );
            }
            println!(
                "  sip: bind {}, unknown_source = {:?}; media: port_range {}; call: max_duration {} s, station_id {:?}",
                rt.sip.bind,
                rt.sip.unknown_source,
                rt.media.port_range,
                rt.call.max_duration_secs,
                rt.call.station_id
            );
            println!(
                "  retention: files {} days, records {} days (0 = never), daily run at {}",
                rt.retention.files_days, rt.retention.records_days, rt.retention.run_at
            );
            println!(
                "  trunks (match order): {}",
                if rt.trunks.is_empty() {
                    "none".to_string()
                } else {
                    rt.trunks
                        .iter()
                        .map(|t| t.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            );
            for l in routes_summary(&rt.routes) {
                println!("{l}");
            }
            for w in runtime_config::validate::warnings(rt) {
                println!("warning: {w}");
            }
            if rt.trunks.is_empty() {
                println!("warning: {NO_TRUNK}");
            }
            let mut bad = false;
            for p in &ins.problems {
                bad = true;
                eprintln!("error: {p}");
            }
            for u in &ins.undecryptable {
                bad = true;
                eprintln!(
                    "error: the secret {u} cannot be decrypted with conf/secret.key: enter it again"
                );
            }
            for p in runtime_config::validate::cross_check(rt) {
                bad = true;
                eprintln!("error: {p}");
            }
            if bad {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            }
        }
    };
    st.close().await;
    code
}

/// `sip-faxd db migrate`
pub async fn db_migrate(cfg: &AppConfig, base: &Path) -> ExitCode {
    let url = cfg.database_url(base);
    let opts = ConnectOptions {
        max_connections: cfg.database.max_connections,
        ..ConnectOptions::default()
    };
    match store::migrate_url(&url, opts).await {
        Ok((kind, rep)) if rep.applied.is_empty() => {
            println!(
                "database {}: already at schema version {}, nothing to do",
                kind.name(),
                rep.to
            );
            ExitCode::SUCCESS
        }
        Ok((kind, rep)) => {
            let list: Vec<String> = rep.applied.iter().map(|v| format!("{v:04}")).collect();
            println!(
                "database {}: schema version {} -> {} (applied {})",
                kind.name(),
                rep.from,
                rep.to,
                list.join(", ")
            );
            ExitCode::SUCCESS
        }
        Err(e) => fail(format!("database {}: {e}", mask_url(&cfg.database.url))),
    }
}

/// `sip-faxd db copy --to URL` (ADR-059): every row into another, empty database.
pub async fn db_copy(cfg: &AppConfig, base: &Path, to: &str) -> ExitCode {
    // nothing may change in the source while it is copied
    #[cfg(unix)]
    if let Some(pid) = crate::service::pidfile::running_pid(&cfg.service.pid_path(base)) {
        return fail(format!(
            "the service is running (pid {pid}): stop it first (`sip-faxd stop` or `systemctl stop`)"
        ));
    }
    let from = cfg.database_url(base);
    // a relative SQLite path of the target is relative to the executable, like [database] url
    let to_url = crate::settings::resolve_database_url(to, base);
    if let Err(e) = store::backend_for_url(&to_url) {
        return fail(format!("--to: {e}"));
    }
    println!(
        "copying {} -> {}",
        mask_url(&cfg.database.url),
        mask_url(to)
    );
    let mut progress = |table: &str, rows: u64| println!("  {table:<18} {rows:>9} rows");
    match store::copy_database(&from, &to_url, &mut progress).await {
        Ok(r) => {
            let total: u64 = r.tables.iter().map(|(_, n)| n).sum();
            println!(
                "done: {total} rows from {} into {} (schema version {}) in {:.1} s, verified",
                r.from.name(),
                r.to.name(),
                r.schema,
                r.elapsed.as_secs_f64()
            );
            println!(
                "next: point [database] of the configuration file at {} (url, or type / host / name / user / password), keep conf/secret.key, then start the service",
                mask_url(to)
            );
            ExitCode::SUCCESS
        }
        Err(e) => fail(format!("db copy: {e}")),
    }
}

/// The target of `db copy`: `--to URL` (with `--to-password-stdin`, the password from stdin) or
/// the `[database]` of `--to-config FILE`.
pub fn copy_target(
    to: Option<String>,
    password_stdin: bool,
    to_config: Option<PathBuf>,
) -> Result<String, ExitCode> {
    match (to, to_config) {
        (Some(url), None) if password_stdin => {
            let pw = read_password_line().map_err(fail)?;
            with_password(&url, &pw).map_err(|e| fail(format!("--to: {e}")))
        }
        (Some(url), None) => Ok(url),
        (None, Some(_)) if password_stdin => Err(fail(
            "--to-password-stdin goes with --to URL; a --to-config file has its own password",
        )),
        (None, Some(file)) => match AppConfig::load(&file) {
            Ok(c) => Ok(c.database.url),
            Err(e) => Err(fail(format!("--to-config: {e}"))),
        },
        // (clap refuses the other combinations)
        _ => Err(fail("give --to URL or --to-config FILE")),
    }
}

/// One line of stdin, as it is (the line end removed).
fn read_password_line() -> Result<String, String> {
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|e| format!("cannot read the password from stdin: {e}"))?;
    let pw = line.trim_end_matches(['\n', '\r']).to_string();
    if pw.is_empty() {
        return Err("the password from stdin is empty".into());
    }
    Ok(pw)
}

/// `scheme://user@host/...` with `password` put in (percent-encoded).
pub fn with_password(url: &str, password: &str) -> Result<String, String> {
    let (scheme, rest) = url.split_once("://").ok_or("not a URL with scheme://")?;
    let authority_end = rest.find(['/', '?']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    let Some((userinfo, host)) = authority.rsplit_once('@') else {
        return Err("the URL needs a user (scheme://user@host/...) to go with the password".into());
    };
    if userinfo.contains(':') {
        return Err(
            "the URL already has a password: leave it out when the password comes from stdin"
                .into(),
        );
    }
    let pw = percent_encoding::utf8_percent_encode(password, percent_encoding::NON_ALPHANUMERIC);
    Ok(format!("{scheme}://{userinfo}:{pw}@{host}{tail}"))
}

/// `sip-faxd db check` on `[database]` of the configuration.
pub async fn db_check(cfg: &AppConfig, base: &Path) -> ExitCode {
    for w in &cfg.database.warnings {
        println!("warning: {w}");
    }
    check_and_report(&cfg.database_url(base), &cfg.database.url).await
}

/// `sip-faxd db check --url URL [--password-stdin]`.
pub fn db_check_url(url: String, password_stdin: bool) -> ExitCode {
    let url = if password_stdin {
        match read_password_line().and_then(|pw| with_password(&url, &pw)) {
            Ok(u) => u,
            Err(e) => return fail(format!("--url: {e}")),
        }
    } else {
        url
    };
    let resolved = crate::settings::resolve_database_url(&url, &crate::exe_dir());
    match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt.block_on(check_and_report(&resolved, &url)),
        Err(e) => fail(format!("cannot start the runtime: {e}")),
    }
}

/// Seconds `db check` waits for the database.
pub const CHECK_LIMIT: std::time::Duration = std::time::Duration::from_secs(15);

async fn check_and_report(url: &str, shown: &str) -> ExitCode {
    let kind = store::backend_for_url(url).map_or("?", |k| k.name());
    println!("database: {} ({kind})", mask_url(shown));
    match store::check_url(url, CHECK_LIMIT).await {
        Ok(r) => {
            println!("  connected in {:.2} s", r.elapsed.as_secs_f64());
            println!("  server version: {}", r.server_version);
            let (state, ok) = match r.schema.cmp(&r.required) {
                std::cmp::Ordering::Equal => ("up to date".to_string(), true),
                std::cmp::Ordering::Less if r.schema == 0 => (
                    "no tables yet: they are created at the first start (or run `sip-faxd db migrate`)".into(),
                    true,
                ),
                std::cmp::Ordering::Less => (
                    "older: the next start upgrades it (auto_migrate), or run `sip-faxd db migrate`".into(),
                    true,
                ),
                std::cmp::Ordering::Greater => (
                    "NEWER than this sip-faxd: install the newer sip-faxd".into(),
                    false,
                ),
            };
            println!(
                "  schema version: {} (this sip-faxd needs {}): {state}",
                r.schema, r.required
            );
            if ok {
                println!("ok");
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
        // a new installation: the file comes with the first start
        Err(store::Error::NotFound(m)) if kind == "sqlite" => {
            println!("  {m}");
            println!("ok");
            ExitCode::SUCCESS
        }
        Err(e) => {
            let msg = e.to_string();
            eprintln!(
                "sip-faxd: error: cannot use the database: {}",
                mask_secrets(&msg, url)
            );
            if let Some(h) = connection_hint(&msg) {
                eprintln!("hint: {h}");
            }
            ExitCode::from(1)
        }
    }
}

/// An error message with the URL's password replaced: drivers sometimes echo the URL, and a
/// message could also carry the password alone (as written in the URL or decoded).
fn mask_secrets(msg: &str, url: &str) -> String {
    let mut out = msg.replace(url, &mask_url(url));
    let password = url
        .split_once("://")
        .map(|(_, rest)| rest.split(['/', '?']).next().unwrap_or(rest))
        .and_then(|authority| authority.rsplit_once('@'))
        .and_then(|(userinfo, _)| userinfo.split_once(':'))
        .map(|(_, pw)| pw);
    if let Some(pw) = password {
        let decoded = percent_encoding::percent_decode_str(pw).decode_utf8_lossy();
        // very short ones would mask unrelated text; they are not worth protecting anyway
        for secret in [pw, decoded.as_ref()] {
            if secret.len() >= 4 {
                out = out.replace(secret, "***");
            }
        }
    }
    out
}

/// What a connection error usually means, from the drivers' messages.
pub fn connection_hint(msg: &str) -> Option<&'static str> {
    let m = msg.to_ascii_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| m.contains(n));
    if has(&["saslprep", "prohibitedcharacter"]) {
        Some(
            "PostgreSQL's password check (SCRAM, SASLprep) refuses a character of the password, such as an emoji: use letters, digits, punctuation or CJK characters in the password",
        )
    } else if has(&["cannot open database", "unknown database"]) {
        // (before the password: SQL Server adds "The login failed" to a missing database)
        Some(
            "the database does not exist: create an empty database first (sip-faxd creates the tables, not the database)",
        )
    } else if has(&[
        "password authentication failed",
        "login failed",
        "access denied",
        "authentication failed",
    ]) {
        Some(
            "the user name or the password is wrong (with the split [database] fields, write the password as it is)",
        )
    } else if has(&["does not exist"]) {
        Some(
            "the database does not exist: create an empty database first (sip-faxd creates the tables, not the database)",
        )
    } else if has(&["certificate", "tls", "ssl"]) {
        Some(
            "the TLS certificate of the server is not trusted: for a self-signed SQL Server certificate set trust_server_certificate = true",
        )
    } else if has(&["connection refused", "actively refused"]) {
        Some(
            "nothing listens on that host and port: check the address, the port and that the database server runs",
        )
    } else if has(&[
        "failed to lookup",
        "nodename nor servname",
        "name or service not known",
        "no such host",
        "dns",
    ]) {
        Some("the host name cannot be resolved: check the host")
    } else if has(&["no answer within", "timed out", "timeout", "pool timed out"]) {
        Some(
            "no connection: nothing listens on that host and port, a firewall drops it, or the host is wrong",
        )
    } else {
        None
    }
}

/// What is said when no trunk is configured.
pub const NO_TRUNK: &str = "no trunk is configured: every call is refused (or dropped) until one is added in the web interface (Settings -> Trunks)";

/// `sip-faxd db backfill`
pub async fn db_backfill(cfg: &AppConfig, base: &Path, dir: Option<PathBuf>) -> ExitCode {
    let db = Db::new(cfg, base);
    let st = match db.store().await {
        Ok(s) => s,
        Err(e) => return fail(format!("database {}: {e}", mask_url(&cfg.database.url))),
    };
    let calls = dir.unwrap_or_else(|| cfg.calls_dir(base));
    let rep = fax_in::recorder::backfill(&*st, &calls, &cfg.data_dir(base)).await;
    println!(
        "backfill {}: {} result.json found, {} inserted, {} finished (were still receiving), {} already recorded, {} not recordable, {} error(s)",
        calls.display(),
        rep.scanned,
        rep.inserted,
        rep.updated,
        rep.already_recorded,
        rep.not_recordable,
        rep.errors.len()
    );
    for e in &rep.errors {
        eprintln!("sip-faxd: backfill: {e}");
    }
    db.close().await;
    if rep.errors.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// `sip-faxd purge [--dry-run]`
pub async fn purge(cfg: &AppConfig, base: &Path, dry_run: bool) -> ExitCode {
    let db = Db::new(cfg, base);
    let st = match db.store().await {
        Ok(s) => s,
        Err(e) => return fail(format!("database {}: {e}", mask_url(&cfg.database.url))),
    };
    // the file's [retention] is the starting point until the database has its own
    if let Err(e) = store::purge::seed_retention(&*st, &cfg.retention.settings()).await {
        return fail(format!("retention settings: {e}"));
    }
    let settings = match store::purge::load_retention(&*st).await {
        Ok(s) => s,
        Err(e) => return fail(format!("retention settings: {e}")),
    };
    let purger = Purger::new(st, cfg.data_dir(base), Arc::new(SystemClock));
    let code = match purger.run(settings.policy, dry_run).await {
        Ok(rep) => {
            println!(
                "retention (from the database): files {} days, records {} days (0 = never)",
                settings.policy.files_days, settings.policy.records_days
            );
            if rep.dry_run {
                println!(
                    "purge (dry run): files of {} record(s) are due, {} record(s) are due for deletion; nothing was deleted",
                    rep.files_due, rep.records_due
                );
            } else {
                println!(
                    "purge: files of {} record(s) removed ({} file(s)), {} record(s) deleted ({} + {} batch(es)), {} skipped",
                    rep.files_purged,
                    rep.files_removed,
                    rep.records_deleted,
                    rep.file_batches,
                    rep.record_batches,
                    rep.skipped
                );
            }
            for e in &rep.errors {
                eprintln!("sip-faxd: purge: {e}");
            }
            if rep.errors.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
        Err(e) => fail(format!("purge: {e}")),
    };
    db.close().await;
    code
}

/// The daily purge of `serve` at `retention.run_at` (read from the database before every wait, so
/// a change made there applies from the next day). Never returns; the caller aborts it.
pub async fn purge_daily(db: Arc<Db>, data_dir: PathBuf) {
    let run_at = || {
        let db = db.clone();
        async move {
            let configured = RetentionSettings::default().run_at;
            let text = match db.store().await {
                Ok(st) => match store::purge::load_retention(&*st).await {
                    Ok(s) => s.run_at,
                    Err(e) => {
                        tracing::error!("retention settings unreadable, using {configured}: {e}");
                        configured
                    }
                },
                Err(e) => {
                    tracing::error!(
                        "database unavailable, using {configured} as the purge time: {e}"
                    );
                    configured
                }
            };
            schedule::parse_run_at(&text)
                .or_else(|| schedule::parse_run_at(&RetentionSettings::default().run_at))
                .expect("the default run_at is valid")
        }
    };
    let job = || {
        let db = db.clone();
        let data_dir = data_dir.clone();
        async move {
            let st = match db.store().await {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("daily purge skipped, database unavailable: {e}");
                    return;
                }
            };
            let settings = match store::purge::load_retention(&*st).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("daily purge skipped, retention settings unreadable: {e}");
                    return;
                }
            };
            match Purger::new(st, data_dir, Arc::new(SystemClock))
                .run(settings.policy, false)
                .await
            {
                Ok(rep) => {
                    tracing::info!(
                        files_purged = rep.files_purged,
                        records_deleted = rep.records_deleted,
                        skipped = rep.skipped,
                        "daily purge done"
                    );
                    for e in &rep.errors {
                        tracing::error!("purge: {e}");
                    }
                }
                Err(e) => tracing::error!("daily purge failed: {e}"),
            }
        }
    };
    schedule::daily(
        || chrono::Local::now().naive_local(),
        tokio::time::sleep,
        run_at,
        job,
    )
    .await;
}

#[cfg(test)]
mod tests {
    //! F05: the route summary of `config check`; the masking of database errors.
    use super::*;

    #[test]
    fn mask_secrets_hides_the_url_and_the_password_alone() {
        let url = "postgres://fax:p%40ss%20w0rd@db.example.com:5432/fax";
        let msg =
            format!("cannot connect to {url}; password 'p@ss w0rd' refused (raw p%40ss%20w0rd)");
        let m = mask_secrets(&msg, url);
        assert!(
            m.contains("postgres://fax:***@db.example.com:5432/fax"),
            "{m}"
        );
        assert!(
            !m.contains("p@ss w0rd") && !m.contains("p%40ss%20w0rd"),
            "{m}"
        );
        // no password: nothing to hide, and a short one is left alone
        assert_eq!(mask_secrets("x", "sqlite://data/fax.db"), "x");
        assert_eq!(mask_secrets("ab ab", "mysql://u:ab@h/d"), "ab ab");
    }

    #[test]
    fn f05_the_route_summary_lists_everything_and_hides_every_secret() {
        let routes: delivery::Routes = toml::from_str(
            "[[routes]]\nname = \"sales\"\ndnis_prefix = \"2557\"\ntrunk = \"cht\"\n\
             [[routes.deliveries]]\nkind = \"folder\"\ndir = \"/srv/in\"\nformats = [\"pdf\"]\n\
             [[routes.deliveries]]\nkind = \"email\"\nhost = \"smtp.example.com\"\nusername = \"fax\"\npassword = \"mailpw-123\"\nfrom = \"fax@example.com\"\nto = [\"a@example.com\", \"b@example.com\"]\n\
             [[routes.deliveries]]\nkind = \"webhook\"\nurl = \"https://hookuser:urlpw-456@example.com/h?token=tok-789\"\nsecret = \"hmac-secret-000\"\nattach_pdf = true\n\
             [[routes]]\ndefault = true\ndeliver_partial = true\n\
             [[routes.deliveries]]\nkind = \"webhook\"\nurl = \"http://example.com/open\"\n",
        )
        .unwrap();
        routes.validate().unwrap();
        let text = routes_summary(&routes).join("\n");
        for must in [
            "route 1 (sales): dnis_prefix = 2557 and trunk = cht; partial faxes not delivered",
            "folder /srv/in (filename {date}_{time}_{ani}_{id}, pdf)",
            "email smtp.example.com:587 Starttls, user fax password ***, from fax@example.com, to a@example.com, b@example.com",
            "webhook https://hookuser:***@example.com/h?token=***, HMAC X-Sipfax-Signature secret ***, with PDF",
            "route 2: default (everything else); partial faxes delivered",
            "webhook http://example.com/open, unsigned",
        ] {
            assert!(text.contains(must), "missing {must:?} in:\n{text}");
        }
        for secret in ["mailpw-123", "urlpw-456", "tok-789", "hmac-secret-000"] {
            assert!(!text.contains(secret), "{secret} leaked:\n{text}");
        }
    }

    /// K03: the drivers' own messages (as seen on the real servers) get the right hint.
    #[test]
    fn k03_connection_hints_tell_the_causes_apart() {
        let cases = [
            // (message, a part of the hint)
            (
                "password authentication failed for user \"u\"",
                "user name or the password",
            ),
            ("Login failed for user 'u'.", "user name or the password"),
            (
                "Access denied for user 'u'@'172.17.0.1' (using password: YES)",
                "user name or the password",
            ),
            // SQL Server says "The login failed" with a missing database too: the database wins
            (
                "Cannot open database \"x\" requested by the login. The login failed.",
                "database does not exist",
            ),
            (
                "error returned from database: database \"x\" does not exist",
                "database does not exist",
            ),
            ("Unknown database 'x'", "database does not exist"),
            (
                "error with configuration: Failed to saslprep password: Error(ProhibitedCharacter('x'))",
                "SASLprep",
            ),
            (
                "invalid peer certificate: UnknownIssuer",
                "trust_server_certificate",
            ),
            (
                "An error occurred during the attempt of performing I/O: Connection refused (os error 111)",
                "nothing listens",
            ),
            (
                "failed to lookup address information: Name or service not known",
                "cannot be resolved",
            ),
            (
                "pool timed out while waiting for an open connection",
                "firewall",
            ),
            ("no answer within 15 s", "firewall"),
        ];
        for (msg, want) in cases {
            let h = connection_hint(msg).unwrap_or_else(|| panic!("no hint for {msg:?}"));
            assert!(h.contains(want), "{msg:?} -> {h}");
        }
        assert_eq!(connection_hint("something else entirely"), None);
    }
}
