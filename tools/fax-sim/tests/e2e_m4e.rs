//! M4e end-to-end case C12: a real SpanDSP fax into a `sip-faxd` on SQLite, marked read and
//! annotated; the service is stopped, `sip-faxd db copy --to <PostgreSQL>` moves everything, the
//! service is started again on PostgreSQL and receives a second fax.
//!
//! PostgreSQL comes from `SIP_FAXD_E2E_PG_URL` (an empty database; `scripts/e2e-docker.sh` starts
//! a container for it and the `e2e-spandsp` CI job a service container).

mod e2e_common;

use std::sync::Arc;
use std::time::Duration;

use e2e_common::*;
use fax_sim::sip_uac::{ReinviteBy, run_call};
use interop::{Faxd, FaxdOptions, faxd_cli};
use store::{ConnectOptions, FaxRecord, FaxStatus, FaxStore, PngKind, Preview, png_rel_path};

type Db = Arc<dyn FaxStore>;

async fn open(url: &str) -> Db {
    store::connect_with(
        url,
        ConnectOptions {
            auto_migrate: false,
            ..ConnectOptions::default()
        },
    )
    .await
    .unwrap_or_else(|e| panic!("cannot open {url}: {e}"))
}

async fn final_row(db: &Db, call_id: &str) -> FaxRecord {
    for _ in 0..1000 {
        if let Ok(Some(r)) = db.get_by_call_id(call_id).await
            && r.fax_status != FaxStatus::Receiving
        {
            return r;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the final row of {call_id} never appeared");
}

fn password_of(url: &str) -> String {
    let rest = url.split_once("://").expect("a URL").1;
    let userinfo = rest.split('@').next().expect("user info");
    userinfo.split_once(':').expect("a password").1.to_string()
}

/// The PNGs the server wrote next to a fax, read from disk (full page and thumbnail per page).
fn pngs(data_dir: &std::path::Path, tiff: &str, pages: i32) -> Vec<(String, Vec<u8>)> {
    let mut v = Vec::new();
    for p in 1..=pages as u32 {
        for kind in [PngKind::Full, PngKind::Thumb] {
            let rel = png_rel_path(tiff, p, kind);
            let bytes = std::fs::read(data_dir.join(&rel))
                .unwrap_or_else(|e| panic!("preview {rel} of the fax: {e}"));
            assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "{rel} is a PNG");
            v.push((rel, bytes));
        }
    }
    v
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn c12_a_real_fax_moves_from_sqlite_to_postgresql_with_read_note_previews_and_ids() {
    let pg_url = std::env::var("SIP_FAXD_E2E_PG_URL").expect(
        "SIP_FAXD_E2E_PG_URL: an empty PostgreSQL database (scripts/e2e-docker.sh starts one)",
    );
    let tag = "m4e-c12";
    let opts = FaxdOptions {
        call: "station_id = \"0287654321\"\n".into(),
        extra: format!("\n[media]\nport_range = \"{}\"\n", media_range(2)),
        real_fax: true,
        ..FaxdOptions::default()
    };

    // the target is empty to begin with
    let pg = store::connect(&pg_url)
        .await
        .unwrap_or_else(|e| panic!("PostgreSQL {pg_url}: {e}"));
    let before: u64 = store::test_row_counts(&pg_url)
        .await
        .unwrap()
        .iter()
        .map(|(_, n)| n)
        .sum();
    assert_eq!(before, 0, "SIP_FAXD_E2E_PG_URL must be an empty database");
    pg.close().await;

    // 1. SQLite: a real fax, then read + note
    let a = Server(Some(Faxd::start(tag, &opts)));
    let (a_port, a_cfg, a_data, a_db_url, a_calls) = (
        a.port,
        a.config_path.clone(),
        a.data_dir.clone(),
        a.db_url.clone(),
        a.calls_dir.clone(),
    );
    assert!(a_db_url.starts_with("sqlite:"), "{a_db_url}");
    let sqlite = open(&a_db_url).await;
    let dir = scratch(&format!("src-{tag}"));
    let pages1 = specs(2, true, 300);
    let src1 = source_tiff(&dir, "one.tif", &pages1);
    let rep = run_call(uac(&a, ReinviteBy::Gateway, &src1, "0000000001"))
        .await
        .unwrap_or_else(|e| panic!("{tag}: {e}\n{}", a.log().join("\n")));
    a.wait_results(1);
    let res = result_json(&a, &rep.call_id);
    assert_success(tag, &rep, &res, 2, "0000000001", "peer_reinvite");
    let row1 = final_row(&sqlite, &rep.call_id).await;
    assert_eq!((row1.fax_status, row1.pages), (FaxStatus::Received, 2));
    sqlite.set_read(row1.id, true, "amy").await.unwrap();
    sqlite
        .set_note(row1.id, "回電給客戶 📞 ref#42", "amy", None)
        .await
        .unwrap();
    let rec_sqlite = sqlite.get(row1.id).await.unwrap().unwrap();
    assert!(rec_sqlite.is_read && rec_sqlite.note.is_some());
    let tiff1 = rec_sqlite.tiff_path.clone().unwrap();
    let png_before = pngs(&a_data, &tiff1, 2);
    let users_before = { sqlite.user_count().await.unwrap() };
    let audit_before = { sqlite.config_audit_list(1000, None).await.unwrap() };
    assert!(users_before >= 1 && !audit_before.is_empty());
    sqlite.close().await;

    // 2. stop (SIGTERM), then `db copy`
    let a_log = a.stop().join("\n");
    assert!(!a_log.contains("ERROR"), "{a_log}");
    let (code, out, err) = faxd_cli(&["db", "copy", "--to", &pg_url], &a_cfg);
    assert_eq!(code, Some(0), "db copy failed:\n{out}\n{err}");
    assert!(
        out.contains(&format!(
            "-> {}",
            pg_url.replace(&password_of(&pg_url), "***")
        )),
        "the password is masked in the output:\n{out}"
    );
    assert!(
        !out.contains(&password_of(&pg_url)) && !err.contains(&password_of(&pg_url)),
        "the password leaked:\n{out}\n{err}"
    );
    assert!(
        out.contains("faxes") && out.contains("1 rows"),
        "one fax copied:\n{out}"
    );
    assert!(
        out.contains("from sqlite into postgres") && out.contains("verified"),
        "{out}"
    );
    assert!(out.contains("next: point [database] of the configuration file at"), "{out}");
    let counts = store::test_row_counts(&pg_url).await.unwrap();
    assert_eq!(counts[0], ("faxes", 1), "{counts:?}");
    assert!(counts.iter().find(|(t, _)| *t == "users").unwrap().1 >= 1);

    // 3. the same service on PostgreSQL (same data directory, same SIP port)
    let b_opts = FaxdOptions {
        database_url: Some(pg_url.clone()),
        data_dir: Some(a_data.clone()),
        port: Some(a_port),
        ..opts.clone()
    };
    let b = Server(Some(Faxd::start(&format!("{tag}-pg"), &b_opts)));
    let pg: Db = open(&pg_url).await;
    let rec_pg = pg
        .get(row1.id)
        .await
        .unwrap()
        .expect("the fax is in PostgreSQL");
    assert_eq!(
        rec_pg, rec_sqlite,
        "every column of the fax, read flag and note included"
    );
    assert!(rec_pg.is_read && rec_pg.read_changed_by.as_deref() == Some("amy"));
    assert_eq!(rec_pg.note.as_deref(), Some("回電給客戶 📞 ref#42"));
    assert_eq!(rec_pg.note_updated_by.as_deref(), Some("amy"));
    {
        assert_eq!(
            pg.user_count().await.unwrap(),
            users_before,
            "no second admin"
        );
        let audit = pg.config_audit_list(1000, None).await.unwrap();
        for old in &audit_before {
            assert!(audit.contains(old), "audit row {} is in PostgreSQL", old.id);
        }
    }
    // previews: the files are untouched and the record in PostgreSQL leads to them
    assert_eq!(
        pngs(&a_data, &tiff1, 2),
        png_before,
        "preview files unchanged"
    );
    let preview = Preview::new(pg.clone(), a_data.clone());
    for p in 1..=2u32 {
        for kind in [PngKind::Full, PngKind::Thumb] {
            let path = preview.page_png(row1.id, p, kind).await.unwrap();
            let want = &png_before
                .iter()
                .find(|(rel, _)| path.ends_with(rel))
                .unwrap_or_else(|| panic!("{path:?} is one of the files written by the server"))
                .1;
            assert_eq!(&std::fs::read(&path).unwrap(), want);
        }
    }
    assert_eq!(
        preview.conversions(),
        0,
        "nothing had to be converted again"
    );

    // 4. a second fax on PostgreSQL: the id continues
    let pages2 = specs(1, false, 260);
    let src2 = source_tiff(&dir, "two.tif", &pages2);
    let rep2 = run_call(uac(&b, ReinviteBy::Gateway, &src2, "0000000002"))
        .await
        .unwrap_or_else(|e| panic!("{tag}: {e}\n{}", b.log().join("\n")));
    b.wait_results(1);
    // [capture] is a runtime section: the copied database keeps the first service's capture directory
    let res2: serde_json::Value = {
        let path = a_calls
            .join(fax_in::capture::dir_name(&rep2.call_id))
            .join("result.json");
        wait_for("result.json", || {
            std::fs::read_to_string(&path)
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
        })
    };
    let fax2 = assert_success(tag, &rep2, &res2, 1, "0000000002", "peer_reinvite");
    verify_files(tag, &fax2, &src2, &pages2);
    let row2 = final_row(&pg, &rep2.call_id).await;
    assert_eq!(row2.fax_status, FaxStatus::Received);
    assert_eq!(
        row2.id,
        row1.id + 1,
        "the new id continues after the copied one"
    );
    assert!(!row2.is_read && row2.note.is_none());
    // the old fax is still as it was after the new one was stored
    assert_eq!(pg.get(row1.id).await.unwrap().unwrap(), rec_sqlite);
    let list = pg
        .list(&store::ListFilter::default(), store::PageRequest::default())
        .await
        .unwrap();
    assert_eq!(list.items.len(), 2);
    pg.close().await;
    let b_log = b.stop().join("\n");
    assert!(!b_log.contains("created the administrator"), "{b_log}");
    assert!(!b_log.contains("ERROR"), "{b_log}");
    assert!(
        !b_log.contains(&password_of(&pg_url)),
        "no database password in the log"
    );
}
