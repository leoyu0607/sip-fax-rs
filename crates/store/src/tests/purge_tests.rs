//! R01-R08: retention at the store level (injected clock, real files in a temp dir).

use super::*;
use crate::purge::{FileOps, resolve_in_data_dir};
use chrono::Duration;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

const DAY: i64 = 86_400_000;
const NOW: i64 = T0 + 100 * DAY;
const POLICY: RetentionPolicy = RetentionPolicy {
    files_days: 7,
    records_days: 30,
};

fn data_dir(env: &Env) -> PathBuf {
    let d = env.dir.path().join("data");
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn purger(env: &Env) -> Purger {
    env.clock.set(t(NOW));
    Purger::new(env.dyn_store(), data_dir(env), env.clock.clone())
}

/// Finished fax ended at `ended_ms`, with TIFF, PDF and a PNG cache on disk. Returns (id, tiff, pdf, png dir).
async fn mk(
    env: &Env,
    call: &str,
    ended_ms: i64,
    status: FaxStatus,
) -> (i64, PathBuf, PathBuf, PathBuf) {
    let dd = data_dir(env);
    let mut f = ff(ended_ms, status, 2);
    f.tiff_path = Some(format!("faxes/{call}.tif"));
    f.pdf_path = Some(format!("faxes/{call}.pdf"));
    let (id, _) = env
        .store
        .upsert_finished(&nf(call, "r", ended_ms - 10_000), &f)
        .await
        .unwrap();
    std::fs::create_dir_all(dd.join("faxes")).unwrap();
    let (tiff, pdf) = (
        dd.join(format!("faxes/{call}.tif")),
        dd.join(format!("faxes/{call}.pdf")),
    );
    std::fs::write(&tiff, b"II*\0tiff").unwrap();
    std::fs::write(&pdf, b"%PDF-1.4").unwrap();
    let png = dd.join("cache/png").join(id.to_string());
    std::fs::create_dir_all(&png).unwrap();
    std::fs::write(png.join("1-full.png"), b"png").unwrap();
    (id, tiff, pdf, png)
}

// ---------------------------------------------------------------- R01
db_case!(r01_boundary, r01_boundary_body);
async fn r01_boundary_body(env: &Env) {
    let p = purger(env);
    let cut_f = NOW - 7 * DAY;
    let (old, old_tiff, ..) = mk(env, "r01-old", cut_f - 1_000, FaxStatus::Received).await;
    let (exact, exact_tiff, ..) = mk(env, "r01-exact", cut_f, FaxStatus::Received).await;
    let (young, young_tiff, ..) = mk(env, "r01-young", cut_f + 1_000, FaxStatus::Received).await;
    let rep = p.run(POLICY, false).await.unwrap();
    assert_eq!(
        (rep.files_purged, rep.records_deleted, rep.skipped),
        (1, 0, 0),
        "{}: {rep:?}",
        env.label
    );
    assert!(!old_tiff.exists() && exact_tiff.exists() && young_tiff.exists());
    assert!(
        env.store
            .get(old)
            .await
            .unwrap()
            .unwrap()
            .files_purged_at
            .is_some()
    );
    assert!(
        env.store
            .get(exact)
            .await
            .unwrap()
            .unwrap()
            .files_purged_at
            .is_none(),
        "ended_at == cutoff is not older"
    );
    assert!(
        env.store
            .get(young)
            .await
            .unwrap()
            .unwrap()
            .files_purged_at
            .is_none()
    );

    // Records: 1 s either side of the 30-day cut.
    let cut_r = NOW - 30 * DAY;
    let (rold, ..) = mk(env, "r01-rold", cut_r - 1_000, FaxStatus::Received).await;
    let (rnew, ..) = mk(env, "r01-rnew", cut_r + 1_000, FaxStatus::Received).await;
    let rep = p.run(POLICY, false).await.unwrap();
    assert_eq!(rep.records_deleted, 1);
    assert!(env.store.get(rold).await.unwrap().is_none());
    assert!(env.store.get(rnew).await.unwrap().is_some());
    assert!(
        env.store.get(exact).await.unwrap().is_some()
            && env.store.get(young).await.unwrap().is_some()
    );
    // The same instant moves with the clock: one second later `exact` becomes due.
    env.clock.advance(Duration::seconds(1));
    let rep = p.run(POLICY, false).await.unwrap();
    assert_eq!(rep.files_purged, 1);
    assert!(!exact_tiff.exists());
}

// ---------------------------------------------------------------- R02
db_case!(r02_file_purge, r02_file_purge_body);
async fn r02_file_purge_body(env: &Env) {
    let p = purger(env);
    let (id, tiff, pdf, png) = mk(env, "r02", NOW - 10 * DAY, FaxStatus::Received).await;
    let (nofile, ..) = {
        // A voice call has no files: it must never be marked purged.
        let mut f = ff(NOW - 10 * DAY, FaxStatus::NoFax, 0);
        f.tiff_path = None;
        f.pdf_path = None;
        f.tiff_size = None;
        f.pdf_size = None;
        let (i, _) = env
            .store
            .upsert_finished(&nf("r02-voice", "r", NOW - 11 * DAY), &f)
            .await
            .unwrap();
        (i, ())
    };
    let before = env.store.get(id).await.unwrap().unwrap();
    let rep = p.run(POLICY, false).await.unwrap();
    assert_eq!(
        (rep.files_purged, rep.files_removed, rep.file_batches),
        (1, 2, 1),
        "{}: {rep:?}",
        env.label
    );
    assert!(
        !tiff.exists() && !pdf.exists() && !png.exists(),
        "TIFF, PDF and PNG cache are gone"
    );
    let after = env.store.get(id).await.unwrap().unwrap();
    assert_eq!(after.files_purged_at, Some(t(NOW)));
    assert_eq!(
        (after.tiff_path, after.tiff_size, after.pages, after.ani),
        (before.tiff_path, before.tiff_size, before.pages, before.ani),
        "record kept"
    );
    assert!(
        env.store
            .get(nofile)
            .await
            .unwrap()
            .unwrap()
            .files_purged_at
            .is_none()
    );
    // Idempotent.
    let rep = p.run(POLICY, false).await.unwrap();
    assert_eq!((rep.files_purged, rep.files_removed), (0, 0));
    // A file that vanished earlier counts as success.
    let (id2, tiff2, ..) = mk(env, "r02-gone", NOW - 10 * DAY, FaxStatus::Received).await;
    std::fs::remove_file(&tiff2).unwrap();
    let rep = p.run(POLICY, false).await.unwrap();
    assert_eq!((rep.files_purged, rep.skipped), (1, 0));
    assert!(
        env.store
            .get(id2)
            .await
            .unwrap()
            .unwrap()
            .files_purged_at
            .is_some()
    );
}

// ---------------------------------------------------------------- R03
db_case!(r03_record_purge, r03_record_purge_body);
async fn r03_record_purge_body(env: &Env) {
    let p = purger(env);
    // files_days = 0 (never): the leftover files must be removed by the record purge itself.
    let policy = RetentionPolicy {
        files_days: 0,
        records_days: 30,
    };
    let (id, tiff, pdf, png) = mk(env, "r03-old", NOW - 40 * DAY, FaxStatus::Received).await;
    let (keep, keep_tiff, ..) = mk(env, "r03-keep", NOW - 5 * DAY, FaxStatus::Received).await;
    for k in 0..2 {
        env.store
            .add_delivery(&NewDelivery {
                fax_id: id,
                target: format!("t{k}"),
                kind: "folder".into(),
                attempt: 1,
                status: DeliveryStatus::Done,
                next_try_at: None,
            })
            .await
            .unwrap();
    }
    env.store
        .add_delivery(&NewDelivery {
            fax_id: keep,
            target: "t".into(),
            kind: "email".into(),
            attempt: 1,
            status: DeliveryStatus::Pending,
            next_try_at: None,
        })
        .await
        .unwrap();
    let rep = p.run(policy, false).await.unwrap();
    assert_eq!(
        (rep.records_deleted, rep.files_purged, rep.files_removed),
        (1, 0, 2),
        "{}: {rep:?}",
        env.label
    );
    assert!(!tiff.exists() && !pdf.exists() && !png.exists());
    assert!(env.store.get(id).await.unwrap().is_none());
    assert!(
        env.store.list_deliveries(id).await.unwrap().is_empty(),
        "delivery attempts removed with the record"
    );
    assert!(keep_tiff.exists());
    assert_eq!(env.store.list_deliveries(keep).await.unwrap().len(), 1);
}

// ---------------------------------------------------------------- R04
struct Flaky {
    fail: AtomicBool,
    calls: AtomicU32,
}
impl FileOps for Flaky {
    fn remove_file(&self, path: &Path) -> std::io::Result<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail.load(Ordering::SeqCst) {
            return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        }
        std::fs::remove_file(path)
    }
    fn remove_dir_all(&self, path: &Path) -> std::io::Result<()> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        }
        std::fs::remove_dir_all(path)
    }
}

db_case!(r04_delete_failure, r04_delete_failure_body);
async fn r04_delete_failure_body(env: &Env) {
    let ops = Arc::new(Flaky {
        fail: AtomicBool::new(true),
        calls: AtomicU32::new(0),
    });
    let p = purger(env).with_file_ops(ops.clone());
    let (id, tiff, ..) = mk(env, "r04", NOW - 10 * DAY, FaxStatus::Received).await;
    let (ok_id, ok_tiff, ..) = {
        ops.fail.store(false, Ordering::SeqCst);
        let x = mk(env, "r04-fine", NOW - 10 * DAY + 1, FaxStatus::Received).await;
        ops.fail.store(true, Ordering::SeqCst);
        x
    };
    let _ = (ok_id, ok_tiff);
    let rep = p.run(POLICY, false).await.unwrap();
    assert_eq!(
        (rep.files_purged, rep.skipped),
        (0, 2),
        "{}: {rep:?}",
        env.label
    );
    assert_eq!(rep.errors.len(), 2);
    assert!(rep.errors[0].contains("delete"), "{:?}", rep.errors);
    assert!(tiff.exists(), "file untouched");
    assert!(
        env.store
            .get(id)
            .await
            .unwrap()
            .unwrap()
            .files_purged_at
            .is_none(),
        "not marked purged"
    );
    // Record purge must not delete the row while its files cannot be removed.
    let old = RetentionPolicy {
        files_days: 0,
        records_days: 8,
    };
    let rep = p.run(old, false).await.unwrap();
    assert_eq!((rep.records_deleted, rep.skipped), (0, 2));
    assert!(env.store.get(id).await.unwrap().is_some());
    // Fixed (permissions restored): next run succeeds.
    ops.fail.store(false, Ordering::SeqCst);
    let rep = p.run(POLICY, false).await.unwrap();
    assert_eq!(
        (rep.files_purged, rep.skipped),
        (2, 0),
        "{}: {rep:?}",
        env.label
    );
    assert!(!tiff.exists());
    assert!(
        env.store
            .get(id)
            .await
            .unwrap()
            .unwrap()
            .files_purged_at
            .is_some()
    );
}

// ---------------------------------------------------------------- R05
db_case!(r05_never_cases, r05_never_cases_body);
async fn r05_never_cases_body(env: &Env) {
    let p = purger(env);
    // A call still receiving, however old, is never purged.
    let rid = env
        .store
        .insert_receiving(&nf("r05-recv", "r", T0))
        .await
        .unwrap();
    let (old, tiff, ..) = mk(env, "r05-old", NOW - 1000 * DAY, FaxStatus::Received).await;
    let rep = p.run(POLICY, false).await.unwrap();
    assert_eq!(rep.records_deleted, 1, "only the finished one");
    assert!(env.store.get(rid).await.unwrap().is_some());
    assert!(env.store.get(old).await.unwrap().is_none());
    assert!(!tiff.exists());
    let rep = p.run(POLICY, true).await.unwrap();
    assert_eq!(
        (rep.files_due, rep.records_due),
        (0, 0),
        "receiving rows are not counted either"
    );

    // files_days = 0 and records_days = 0 mean "keep forever".
    let (keep, keep_tiff, ..) = mk(env, "r05-keep", NOW - 1000 * DAY, FaxStatus::Received).await;
    let rep = p
        .run(
            RetentionPolicy {
                files_days: 0,
                records_days: 0,
            },
            false,
        )
        .await
        .unwrap();
    assert_eq!(rep, PurgeReport::default());
    assert!(keep_tiff.exists() && env.store.get(keep).await.unwrap().is_some());
    // files_days = 0 alone: records still age out, files are not purged separately.
    let rep = p
        .run(
            RetentionPolicy {
                files_days: 0,
                records_days: 30,
            },
            false,
        )
        .await
        .unwrap();
    assert_eq!((rep.files_purged, rep.records_deleted), (0, 1));
    // records_days < files_days is refused.
    assert!(matches!(
        p.run(
            RetentionPolicy {
                files_days: 90,
                records_days: 30
            },
            false
        )
        .await,
        Err(Error::Invalid(_))
    ));
}

// ---------------------------------------------------------------- R06
db_case!(r06_path_safety, r06_path_safety_body);
async fn r06_path_safety_body(env: &Env) {
    let p = purger(env);
    let outside = env.dir.path().join("outside-secret.txt");
    std::fs::write(&outside, b"do not delete").unwrap();
    let abs_other = env.dir.path().join("abs-secret.txt");
    std::fs::write(&abs_other, b"do not delete").unwrap();
    let evil = ["../outside-secret.txt", "faxes/../../outside-secret.txt"];
    let mut ids = Vec::new();
    for (i, path) in evil
        .iter()
        .map(|s| s.to_string())
        .chain([
            abs_other.display().to_string(),
            "/etc/hostname".to_string(),
            String::new(),
        ])
        .enumerate()
    {
        let mut f = ff(NOW - 10 * DAY, FaxStatus::Received, 1);
        f.tiff_path = Some(path);
        f.pdf_path = None;
        let (id, _) = env
            .store
            .upsert_finished(&nf(&format!("r06-{i}"), "r", NOW - 11 * DAY), &f)
            .await
            .unwrap();
        ids.push(id);
    }
    #[cfg(unix)]
    {
        // A symlink inside data_dir that points outside.
        let dd = data_dir(env);
        std::fs::create_dir_all(dd.join("faxes")).unwrap();
        std::os::unix::fs::symlink(&outside, dd.join("faxes/link.tif")).unwrap();
        let mut f = ff(NOW - 10 * DAY, FaxStatus::Received, 1);
        f.tiff_path = Some("faxes/link.tif".into());
        f.pdf_path = None;
        let (id, _) = env
            .store
            .upsert_finished(&nf("r06-link", "r", NOW - 11 * DAY), &f)
            .await
            .unwrap();
        ids.push(id);
    }
    let rep = p.run(POLICY, false).await.unwrap();
    assert_eq!(rep.files_purged, 0, "{}: {rep:?}", env.label);
    assert_eq!(rep.skipped as usize, ids.len());
    assert!(
        rep.errors.iter().all(|e| e.contains("refusing")),
        "{:?}",
        rep.errors
    );
    assert!(
        outside.exists() && abs_other.exists(),
        "files outside data_dir survive"
    );
    for id in &ids {
        assert!(
            env.store
                .get(*id)
                .await
                .unwrap()
                .unwrap()
                .files_purged_at
                .is_none()
        );
    }
    // Records with bad paths are kept too (their files could not be handled).
    let rep = p
        .run(
            RetentionPolicy {
                files_days: 0,
                records_days: 8,
            },
            false,
        )
        .await
        .unwrap();
    assert_eq!(rep.records_deleted, 0);
    assert!(outside.exists());

    // Pure path logic.
    let dd = Path::new("/srv/fax/data");
    assert_eq!(
        resolve_in_data_dir(dd, "faxes/a.tif").unwrap(),
        Path::new("/srv/fax/data/faxes/a.tif")
    );
    assert_eq!(
        resolve_in_data_dir(dd, "faxes/./x/../a.tif").unwrap(),
        Path::new("/srv/fax/data/faxes/a.tif")
    );
    assert_eq!(
        resolve_in_data_dir(dd, "/srv/fax/data/faxes/a.tif").unwrap(),
        Path::new("/srv/fax/data/faxes/a.tif")
    );
    for bad in [
        "../x",
        "faxes/../../x",
        "/etc/passwd",
        "/srv/fax/data",
        "/srv/fax/data/../x",
        "",
        ".",
        "a\0b",
        "/srv/fax/database/x",
    ] {
        assert!(
            resolve_in_data_dir(dd, bad).is_err(),
            "{bad:?} must be rejected"
        );
    }
    assert!(resolve_in_data_dir(Path::new("/"), "../x").is_err());
}

// ---------------------------------------------------------------- R07
db_case!(r07_dry_run, r07_dry_run_body);
async fn r07_dry_run_body(env: &Env) {
    let p = purger(env);
    let (a, a_tiff, ..) = mk(env, "r07-a", NOW - 10 * DAY, FaxStatus::Received).await; // files due
    let (b, b_tiff, ..) = mk(env, "r07-b", NOW - 40 * DAY, FaxStatus::Partial).await; // files + record due
    let (c, ..) = mk(env, "r07-c", NOW - DAY, FaxStatus::Received).await; // neither
    let rep = p.run(POLICY, true).await.unwrap();
    assert!(rep.dry_run);
    assert_eq!(
        (rep.files_due, rep.records_due),
        (2, 1),
        "{}: {rep:?}",
        env.label
    );
    assert_eq!(
        (rep.files_purged, rep.files_removed, rep.records_deleted),
        (0, 0, 0)
    );
    assert!(a_tiff.exists() && b_tiff.exists());
    for id in [a, b, c] {
        assert!(
            env.store
                .get(id)
                .await
                .unwrap()
                .unwrap()
                .files_purged_at
                .is_none()
        );
    }
    // The real run then does exactly what the dry run announced.
    let rep = p.run(POLICY, false).await.unwrap();
    assert_eq!((rep.files_purged, rep.records_deleted), (2, 1));
}

// ---------------------------------------------------------------- R08
db_case!(r08_batches, r08_batches_body);
async fn r08_batches_body(env: &Env) {
    let st = env.dyn_store();
    let p = purger(env);
    let mut tasks = Vec::new();
    for w in 0..8i64 {
        let st = st.clone();
        tasks.push(tokio::spawn(async move {
            for i in 0..150i64 {
                let n = w * 150 + i;
                let mut f = ff(NOW - 40 * DAY + n, FaxStatus::Received, 1);
                f.tiff_path = Some(format!("faxes/r08-{n}.tif")); // files are not on disk: NotFound = success
                f.pdf_path = None;
                st.upsert_finished(&nf(&format!("r08-{n}"), "r", NOW - 41 * DAY), &f)
                    .await
                    .unwrap();
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    let rep = p.run(POLICY, true).await.unwrap();
    assert_eq!(
        (rep.files_due, rep.records_due),
        (1200, 1200),
        "{}",
        env.label
    );
    let rep = p.run(POLICY, false).await.unwrap();
    assert_eq!(rep.file_batches, 3, "1200 rows -> 500 + 500 + 200: {rep:?}");
    assert_eq!(rep.record_batches, 3);
    assert_eq!(
        (rep.files_purged, rep.records_deleted, rep.skipped),
        (1200, 1200, 0)
    );
    assert_eq!(st.count_records_due(t(NOW)).await.unwrap(), 0);
    let left = st
        .list(
            &ListFilter {
                trunk: Some("r".into()),
                ..Default::default()
            },
            PageRequest::default(),
        )
        .await
        .unwrap();
    assert!(left.items.is_empty());
}
