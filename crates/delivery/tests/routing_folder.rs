//! D01 (routing through the engine), D02 (folder), D06 (partial) against a real SQLite store.

mod common;

use chrono::Duration;
use common::*;
use delivery::EngineOptions;
use store::{DeliveryStatus, FaxStatus};

fn dir_toml(p: &std::path::Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

fn names(dir: &std::path::Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

#[tokio::test]
async fn d01_engine_uses_first_matching_route_and_all_its_deliveries() {
    let mut env = Env::new().await;
    let (a, b, c, d) = (
        env.dir.path().join("a"),
        env.dir.path().join("b"),
        env.dir.path().join("c"),
        env.dir.path().join("d"),
    );
    let r = routes(&format!(
        r#"
[[routes]]
dnis = "025570001"
[[routes.deliveries]]
kind = "folder"
dir = "{a}"
filename = "exact-{{id}}"
formats = ["pdf"]
[[routes.deliveries]]
kind = "folder"
dir = "{b}"
filename = "exact-{{id}}"
formats = ["tiff"]

[[routes]]
dnis_prefix = "2557"
[[routes.deliveries]]
kind = "folder"
dir = "{c}"
filename = "prefix-{{id}}"

[[routes]]
default = true
[[routes.deliveries]]
kind = "folder"
dir = "{d}"
filename = "default-{{id}}"
"#,
        a = dir_toml(&a),
        b = dir_toml(&b),
        c = dir_toml(&c),
        d = dir_toml(&d)
    ));
    let eng = env.engine(&r);
    let f1 = env.fax("tr", "025570001", FaxStatus::Received).await; // exact (also matches nothing else first)
    let f2 = env.fax("tr", "25570002", FaxStatus::Received).await; // prefix
    let f3 = env.fax("tr", "999", FaxStatus::Received).await; // default
    assert_eq!(eng.dispatch(f1).await.unwrap().len(), 2);
    assert_eq!(eng.dispatch(f2).await.unwrap().len(), 1);
    assert_eq!(eng.dispatch(f3).await.unwrap().len(), 1);
    // Dispatching twice does not queue twice.
    assert!(eng.dispatch(f1).await.unwrap().is_empty());
    let rep = eng.run_due().await.unwrap();
    assert_eq!((rep.attempted, rep.done), (4, 4));

    assert_eq!(names(&a), vec![format!("exact-{f1}.pdf")]);
    assert_eq!(names(&b), vec![format!("exact-{f1}.tif")]);
    assert_eq!(
        names(&c),
        vec![format!("prefix-{f2}.pdf"), format!("prefix-{f2}.tif")]
    );
    assert_eq!(
        names(&d),
        vec![format!("default-{f3}.pdf"), format!("default-{f3}.tif")]
    );
    for id in [f1, f2, f3] {
        for at in env.attempts(id).await {
            assert_eq!(at.status, DeliveryStatus::Done);
            assert_eq!(at.attempt, 1);
            assert_eq!(at.kind, "folder");
        }
    }
    // The ids in target are what a restart uses to find the configuration again.
    let t: Vec<String> = env
        .attempts(f1)
        .await
        .into_iter()
        .map(|a| a.target)
        .collect();
    assert_eq!(
        t,
        vec![
            format!("folder:{}", dir_toml(&a)),
            format!("folder:{}", dir_toml(&b))
        ]
    );
}

#[tokio::test]
async fn d01_no_matching_route_and_unfinished_faxes_queue_nothing() {
    let mut env = Env::new().await;
    let out = env.dir.path().join("out");
    let r = routes(&format!(
        "[[routes]]\ntrunk = \"cht\"\n[[routes.deliveries]]\nkind = \"folder\"\ndir = \"{}\"\n",
        dir_toml(&out)
    ));
    let eng = env.engine(&r);
    let f = env.fax("other-trunk", "1", FaxStatus::Received).await;
    assert!(eng.dispatch(f).await.unwrap().is_empty());
    assert!(env.attempts(f).await.is_empty());
    // Unknown id.
    assert!(eng.dispatch(f + 999).await.unwrap().is_empty());
    // A call still receiving is not delivered.
    let rec_id = env
        .store
        .insert_receiving(&store::NewFax {
            call_id: "still-receiving".into(),
            trunk: "cht".into(),
            ani: None,
            ani_source: None,
            dnis: Some("1".into()),
            dnis_source: None,
            started_at: env.clock.now_utc(),
            answered_at: None,
        })
        .await
        .unwrap();
    assert!(eng.dispatch(rec_id).await.unwrap().is_empty());
    assert_eq!(eng.run_due().await.unwrap().attempted, 0);
    assert!(!out.exists());
}

#[tokio::test]
async fn d01_invalid_configuration_is_rejected_by_the_engine() {
    let env = Env::new().await;
    let bad: delivery::Routes = toml::from_str("[[routes]]\ndnis = \"1\"\n").unwrap();
    let err = delivery::DeliveryEngine::new(
        env.store.clone(),
        &bad,
        &env.data,
        env.clock.clone(),
        EngineOptions::default(),
    )
    .err()
    .unwrap();
    assert!(
        matches!(err, delivery::Error::Config(ref v) if v[0].contains("no deliveries")),
        "{err}"
    );
    let ok = routes("");
    let opts = EngineOptions {
        max_attempts: 0,
        ..Default::default()
    };
    assert!(
        delivery::DeliveryEngine::new(env.store.clone(), &ok, &env.data, env.clock.clone(), opts)
            .is_err()
    );
    let opts = EngineOptions {
        utc_offset_secs: 24 * 3600,
        ..Default::default()
    };
    assert!(
        delivery::DeliveryEngine::new(env.store.clone(), &ok, &env.data, env.clock.clone(), opts)
            .is_err()
    );
}

#[tokio::test]
async fn d02_folder_missing_directory_retries_until_it_exists() {
    let mut env = Env::new().await;
    let out = env.dir.path().join("mnt/inbox");
    let r = routes(&format!(
        "[[routes]]\ndefault = true\n[[routes.deliveries]]\nkind = \"folder\"\ndir = \"{}\"\ncreate_dir = false\nfilename = \"f\"\n",
        dir_toml(&out)
    ));
    let eng = env.engine(&r);
    let f = env.fax("t", "1", FaxStatus::Received).await;
    eng.dispatch(f).await.unwrap();
    let rep = eng.run_due().await.unwrap();
    assert_eq!((rep.attempted, rep.retry_scheduled), (1, 1));
    let at = env.attempts(f).await;
    assert_eq!(at.len(), 2);
    assert_eq!(at[0].status, DeliveryStatus::Failed);
    assert!(
        at[0]
            .last_error
            .as_deref()
            .unwrap()
            .contains("does not exist")
    );
    assert_eq!((at[1].status, at[1].attempt), (DeliveryStatus::Pending, 2));
    assert_eq!(at[1].next_try_at, Some(t(T0 + 60_000)));
    // The share appears; the retry (not before the schedule) delivers.
    std::fs::create_dir_all(&out).unwrap();
    env.clock.advance(Duration::seconds(59));
    assert_eq!(eng.run_due().await.unwrap().attempted, 0);
    env.clock.advance(Duration::seconds(1));
    let rep = eng.run_due().await.unwrap();
    assert_eq!((rep.attempted, rep.done), (1, 1));
    assert_eq!(names(&out), vec!["f.pdf", "f.tif"]);
    assert_eq!(std::fs::read(out.join("f.pdf")).unwrap(), pdf_bytes(f));
    assert_eq!(std::fs::read(out.join("f.tif")).unwrap(), tiff_bytes(f));
    assert_eq!(env.attempts(f).await[1].status, DeliveryStatus::Done);
}

#[tokio::test]
async fn d02_folder_auto_creates_directory_by_default() {
    let mut env = Env::new().await;
    let out = env.dir.path().join("x/y/z");
    let r = routes(&format!(
        "[[routes]]\ndefault = true\n[[routes.deliveries]]\nkind = \"folder\"\ndir = \"{}\"\n",
        dir_toml(&out)
    ));
    let eng = env.engine(&r);
    let f = env.fax("t", "1", FaxStatus::Received).await;
    eng.dispatch(f).await.unwrap();
    assert_eq!(eng.run_due().await.unwrap().done, 1);
    assert_eq!(names(&out).len(), 2);
}

/// Permission denied (read-only directory) is retried and succeeds once the permission is fixed.
/// Needs a non-root user and POSIX permissions, so it only runs on unix (see the M3c report).
#[cfg(unix)]
#[tokio::test]
async fn d02_folder_permission_denied_is_retried() {
    use std::os::unix::fs::PermissionsExt;
    let mut env = Env::new().await;
    let out = env.dir.path().join("ro");
    std::fs::create_dir(&out).unwrap();
    std::fs::set_permissions(&out, std::fs::Permissions::from_mode(0o555)).unwrap();
    // Root ignores permission bits: nothing to test then.
    if std::fs::write(out.join("probe"), b"x").is_ok() {
        eprintln!("skip: running as root, permission bits are not enforced");
        return;
    }
    let r = routes(&format!(
        "[[routes]]\ndefault = true\n[[routes.deliveries]]\nkind = \"folder\"\ndir = \"{}\"\nfilename = \"f\"\n",
        dir_toml(&out)
    ));
    let eng = env.engine(&r);
    let f = env.fax("t", "1", FaxStatus::Received).await;
    eng.dispatch(f).await.unwrap();
    let rep = eng.run_due().await.unwrap();
    assert_eq!((rep.retry_scheduled, rep.failed), (1, 0));
    assert!(
        env.attempts(f).await[0]
            .last_error
            .as_deref()
            .unwrap()
            .contains("ermission")
    );
    assert!(names(&out).is_empty(), "no .part file left");
    std::fs::set_permissions(&out, std::fs::Permissions::from_mode(0o755)).unwrap();
    env.clock.advance(Duration::minutes(1));
    assert_eq!(eng.run_due().await.unwrap().done, 1);
    assert_eq!(names(&out), vec!["f.pdf", "f.tif"]);
}

#[tokio::test]
async fn d02_folder_template_with_utc_offset_and_traversal_attempt() {
    let mut env = Env::new().await;
    let out = env.dir.path().join("inbox");
    let r = routes(&format!(
        "[[routes]]\ndefault = true\n[[routes.deliveries]]\nkind = \"folder\"\ndir = \"{}\"\nfilename = \"{{date}}_{{time}}_{{dnis}}_{{trunk}}_{{pages}}\"\nformats = [\"pdf\"]\n",
        dir_toml(&out)
    ));
    let opts = EngineOptions {
        utc_offset_secs: 8 * 3600,
        ..Default::default()
    };
    let eng = env.engine_with(&r, opts);
    // started_at = 2026-10-21T00:00:00Z = 08:00:00 at UTC+8. The trunk name tries to escape.
    let f = env.fax("../../escape", "0255", FaxStatus::Received).await;
    eng.dispatch(f).await.unwrap();
    assert_eq!(eng.run_due().await.unwrap().done, 1);
    assert_eq!(names(&out), vec!["20261021_080000_0255__._.__escape_2.pdf"]);
    assert!(!env.dir.path().join("escape").exists());
    assert_eq!(
        names(env.dir.path())
            .iter()
            .filter(|n| n.contains("escape"))
            .count(),
        0
    );
}

#[tokio::test]
async fn d02_folder_missing_source_file_is_a_permanent_failure() {
    let mut env = Env::new().await;
    let out = env.dir.path().join("out");
    let r = routes(&format!(
        "[[routes]]\ndefault = true\n[[routes.deliveries]]\nkind = \"folder\"\ndir = \"{}\"\n",
        dir_toml(&out)
    ));
    let eng = env.engine(&r);
    let f = env.fax("t", "1", FaxStatus::Received).await;
    std::fs::remove_file(env.data.join(format!("faxes/{f}.pdf"))).unwrap();
    eng.dispatch(f).await.unwrap();
    let rep = eng.run_due().await.unwrap();
    assert_eq!((rep.failed, rep.retry_scheduled), (1, 0));
    let at = env.attempts(f).await;
    assert_eq!(at.len(), 1, "no retry for a permanent error");
    assert_eq!(at[0].status, DeliveryStatus::Failed);
    assert!(
        at[0]
            .last_error
            .as_deref()
            .unwrap()
            .contains("PDF file missing")
    );
    assert_eq!(at[0].next_try_at, None);
}

// ------------------------------------------------------------------------------------- D06

#[tokio::test]
async fn d06_partial_follows_the_route_setting() {
    let mut env = Env::new().await;
    let (strict, lenient) = (
        env.dir.path().join("strict"),
        env.dir.path().join("lenient"),
    );
    let r = routes(&format!(
        r#"
[[routes]]
trunk = "strict"
[[routes.deliveries]]
kind = "folder"
dir = "{s}"
filename = "f{{id}}"
formats = ["pdf"]

[[routes]]
trunk = "lenient"
deliver_partial = true
[[routes.deliveries]]
kind = "folder"
dir = "{l}"
filename = "f{{id}}"
formats = ["pdf"]
"#,
        s = dir_toml(&strict),
        l = dir_toml(&lenient)
    ));
    let eng = env.engine(&r);
    let s_part = env.fax("strict", "1", FaxStatus::Partial).await;
    let s_full = env.fax("strict", "1", FaxStatus::Received).await;
    let l_part = env.fax("lenient", "1", FaxStatus::Partial).await;
    let l_full = env.fax("lenient", "1", FaxStatus::Received).await;
    // Never delivered, whatever the route says.
    let failed = env.fax("lenient", "1", FaxStatus::Failed).await;
    let nofax = env.fax("lenient", "1", FaxStatus::NoFax).await;
    let mut queued = Vec::new();
    for id in [s_part, s_full, l_part, l_full, failed, nofax] {
        queued.push(eng.dispatch(id).await.unwrap().len());
    }
    assert_eq!(queued, vec![0, 1, 1, 1, 0, 0]);
    assert_eq!(eng.run_due().await.unwrap().done, 3);
    assert_eq!(names(&strict), vec![format!("f{s_full}.pdf")]);
    assert_eq!(
        names(&lenient),
        vec![format!("f{l_part}.pdf"), format!("f{l_full}.pdf")]
    );
    assert!(env.attempts(s_part).await.is_empty());
}

#[tokio::test]
async fn d06_catch_up_queues_missed_faxes_once() {
    let mut env = Env::new().await;
    let out = env.dir.path().join("out");
    let r = routes(&format!(
        "[[routes]]\ndefault = true\ndeliver_partial = true\n[[routes.deliveries]]\nkind = \"folder\"\ndir = \"{}\"\nfilename = \"f{{id}}\"\nformats = [\"pdf\"]\n",
        dir_toml(&out)
    ));
    let eng = env.engine(&r);
    let old = env.fax("t", "1", FaxStatus::Received).await; // before `since`
    env.clock.advance(Duration::hours(2));
    let since = env.clock.now_utc() - Duration::hours(1);
    let a = env.fax("t", "1", FaxStatus::Received).await;
    let b = env.fax("t", "1", FaxStatus::Partial).await;
    let _failed = env.fax("t", "1", FaxStatus::Failed).await;
    assert_eq!(eng.catch_up(since).await.unwrap(), 2);
    assert_eq!(
        eng.catch_up(since).await.unwrap(),
        0,
        "second call finds them queued"
    );
    assert!(env.attempts(old).await.is_empty());
    assert_eq!(eng.run_due().await.unwrap().done, 2);
    assert_eq!(names(&out), vec![format!("f{a}.pdf"), format!("f{b}.pdf")]);
}

// ----------------------------------------------------------------- background worker

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d02_background_worker_delivers_after_notification_and_stops() {
    let mut env = Env::new().await;
    let out = env.dir.path().join("out");
    let r = routes(&format!(
        "[[routes]]\ndefault = true\n[[routes.deliveries]]\nkind = \"folder\"\ndir = \"{}\"\nfilename = \"f{{id}}\"\nformats = [\"pdf\"]\n",
        dir_toml(&out)
    ));
    let opts = EngineOptions {
        poll_interval: std::time::Duration::from_millis(20),
        ..Default::default()
    };
    let eng = env.engine_with(&r, opts);
    let handle = eng.spawn();
    let f = env.fax("t", "1", FaxStatus::Received).await;
    handle.notify_finalized(f);
    let want = out.join(format!("f{f}.pdf"));
    let mut waited = 0;
    while !want.exists() && waited < 500 {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        waited += 1;
    }
    assert!(want.exists(), "worker did not deliver within 5 s");
    assert_eq!(std::fs::read(&want).unwrap(), pdf_bytes(f));
    handle.shutdown().await;
    // The row is closed as done by the time shutdown returns (attempts in flight finish first).
    let at = env.attempts(f).await;
    assert_eq!(at.len(), 1);
    assert_eq!(at[0].status, DeliveryStatus::Done);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d05_background_worker_picks_up_due_retries_by_polling() {
    let mut env = Env::new().await;
    let out = env.dir.path().join("late");
    let r = routes(&format!(
        "[[routes]]\ndefault = true\n[[routes.deliveries]]\nkind = \"folder\"\ndir = \"{}\"\ncreate_dir = false\nfilename = \"f{{id}}\"\nformats = [\"pdf\"]\n",
        dir_toml(&out)
    ));
    let opts = EngineOptions {
        poll_interval: std::time::Duration::from_millis(20),
        ..Default::default()
    };
    let eng = env.engine_with(&r, opts);
    let f = env.fax("t", "1", FaxStatus::Received).await;
    let handle = eng.clone().spawn();
    handle.notify_finalized(f);
    // First attempt fails (no directory); the retry is scheduled one minute ahead of the manual clock.
    let mut waited = 0;
    while env.attempts(f).await.len() < 2 && waited < 500 {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        waited += 1;
    }
    assert_eq!(env.attempts(f).await.len(), 2);
    std::fs::create_dir_all(&out).unwrap();
    env.clock.advance(Duration::minutes(1));
    let want = out.join(format!("f{f}.pdf"));
    waited = 0;
    while !want.exists() && waited < 500 {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        waited += 1;
    }
    assert!(want.exists(), "poll loop did not run the due retry");
    handle.shutdown().await;
}
