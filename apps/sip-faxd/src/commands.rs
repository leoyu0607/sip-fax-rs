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
    let backend = store::backend_for_url(&cfg.database.url).map_or("?", |k| k.name());
    println!(
        "  database: {} ({backend}), auto_migrate = {}, max_connections = {}",
        mask_url(&cfg.database.url),
        cfg.database.auto_migrate,
        cfg.database.max_connections
    );
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
                "  empty: the runtime sections of the configuration file are imported at the first start of `serve` (or with `config import`)"
            );
            ExitCode::SUCCESS
        }
        Ok(ins) => {
            let rt = &ins.config;
            println!(
                "  {} stored section(s); the values above under sip / media / call / capture / trunks / routes / delivery / retention are ignored while these exist",
                ins.sections
            );
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
                "next: set [database] url = \"{}\" in the configuration file (keep conf/secret.key), then start the service",
                mask_url(to)
            );
            ExitCode::SUCCESS
        }
        Err(e) => fail(format!("db copy: {e}")),
    }
}

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
    //! F05: the route summary of `config check`.
    use super::*;

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
}
