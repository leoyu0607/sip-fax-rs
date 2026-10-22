//! The fax routes over HTTP (W01–W08), against a real SQLite store and real files.

mod common;

use accounts::Role;
use common::{add_user, change, env, fax, login};
use reqwest::Method;
use serde_json::{Value, json};
use store::{Clock, DeliveryStatus, NewDelivery, PngKind};

async fn body(r: reqwest::Response) -> (u16, Value) {
    let s = r.status().as_u16();
    (s, r.json().await.unwrap_or(Value::Null))
}

#[tokio::test]
async fn w01_the_list_filters_pages_and_refuses_a_bad_cursor() {
    let e = env().await;
    add_user(&e, "alice", Role::Agent, false).await;
    let (c, csrf) = login(&e, "alice").await;
    let old = fax(&e, "old@gw", "0911111111", 30).await;
    let mid = fax(&e, "mid@gw", "0922222222", 20).await;
    let new = fax(&e, "new@gw", "0911119999", 10).await;
    let ids = |v: &Value| -> Vec<i64> {
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["id"].as_i64().unwrap())
            .collect()
    };
    let (s, v) = body(c.get(e.url("/api/v1/faxes")).send().await.unwrap()).await;
    assert_eq!(s, 200);
    assert_eq!(ids(&v), [new, mid, old], "newest first");
    // the same answer as the store gives
    let direct = e
        .store
        .list(&store::ListFilter::default(), store::PageRequest::default())
        .await
        .unwrap();
    assert_eq!(
        ids(&v),
        direct.items.iter().map(|r| r.id).collect::<Vec<_>>()
    );
    // ANI prefix
    let (_, v) = body(
        c.get(e.url("/api/v1/faxes?ani=091111"))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(ids(&v), [new, old]);
    // read filter after marking one read
    change(
        &c,
        Method::PUT,
        e.url(&format!("/api/v1/faxes/{mid}/read")),
        &csrf,
    )
    .json(&json!({"read": true}))
    .send()
    .await
    .unwrap();
    let (_, v) = body(
        c.get(e.url("/api/v1/faxes?is_read=false"))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(ids(&v), [new, old]);
    // pages of one
    let (_, p1) = body(c.get(e.url("/api/v1/faxes?limit=1")).send().await.unwrap()).await;
    assert_eq!(ids(&p1), [new]);
    let cur = p1["next_cursor"].as_str().unwrap().to_string();
    let (_, p2) = body(
        c.get(e.url(&format!("/api/v1/faxes?limit=1&cursor={cur}")))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(ids(&p2), [mid]);
    // a tampered cursor, a bad parameter
    for q in [
        "cursor=12x",
        "cursor=..%2F",
        "limit=1000",
        "status=lost",
        "colour=red",
    ] {
        let (s, v) = body(
            c.get(e.url(&format!("/api/v1/faxes?{q}")))
                .send()
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(
            (s, v["error"]["code"].as_str()),
            (400, Some("bad_request")),
            "{q}"
        );
    }
    e.stop().await;
}

#[tokio::test]
async fn w02_one_fax_with_its_deliveries_and_404_for_an_unknown_one() {
    let e = env().await;
    add_user(&e, "alice", Role::Agent, false).await;
    let (c, _) = login(&e, "alice").await;
    let id = fax(&e, "one@gw", "0212345678", 5).await;
    e.store
        .add_delivery(&NewDelivery {
            fax_id: id,
            target: "webhook:https://hook.example/x?token=s3cret".into(),
            kind: "webhook".into(),
            attempt: 1,
            status: DeliveryStatus::Failed,
            next_try_at: None,
        })
        .await
        .unwrap();
    let (s, v) = body(
        c.get(e.url(&format!("/api/v1/faxes/{id}")))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(s, 200);
    assert_eq!(v["call_id"], "one@gw");
    assert_eq!(v["ani"], "0212345678");
    assert_eq!(v["fax_status"], "received");
    assert_eq!(v["pages"], 1);
    assert_eq!(v["has_pdf"], true);
    assert!(v.get("pdf_path").is_none() && v.get("tiff_path").is_none());
    let d = v["deliveries"].as_array().unwrap();
    assert_eq!(d.len(), 1);
    assert_eq!(d[0]["status"], "failed");
    assert!(!d[0]["target"].as_str().unwrap().contains("s3cret"));
    for bad in ["999999", "0", "-3", "abc"] {
        let (s, v) = body(
            c.get(e.url(&format!("/api/v1/faxes/{bad}")))
                .send()
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(
            (s, v["error"]["code"].as_str()),
            (404, Some("not_found")),
            "{bad}"
        );
    }
    e.stop().await;
}

#[tokio::test]
async fn w03_the_read_flag_records_who_changed_it() {
    let e = env().await;
    add_user(&e, "alice", Role::Agent, false).await;
    let (c, csrf) = login(&e, "alice").await;
    let id = fax(&e, "r@gw", "02", 5).await;
    let (s, v) = body(
        change(
            &c,
            Method::PUT,
            e.url(&format!("/api/v1/faxes/{id}/read")),
            &csrf,
        )
        .json(&json!({"read": true}))
        .send()
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(s, 200);
    assert_eq!(v["is_read"], true);
    let row = e.store.get(id).await.unwrap().unwrap();
    assert!(row.is_read);
    assert_eq!(row.read_changed_by.as_deref(), Some("alice"));
    // a body that is not {"read": bool}
    let r = change(
        &c,
        Method::PUT,
        e.url(&format!("/api/v1/faxes/{id}/read")),
        &csrf,
    )
    .json(&json!({"read": "yes"}))
    .send()
    .await
    .unwrap();
    assert_eq!(r.status(), 400);
    e.stop().await;
}

#[tokio::test]
async fn w04_a_note_conflict_is_409_with_the_current_note_and_changes_nothing() {
    let e = env().await;
    add_user(&e, "alice", Role::Agent, false).await;
    add_user(&e, "bob", Role::Agent, false).await;
    let (a, ca) = login(&e, "alice").await;
    let (b, cb) = login(&e, "bob").await;
    let id = fax(&e, "n@gw", "02", 5).await;
    let url = e.url(&format!("/api/v1/faxes/{id}/note"));
    let (s, v) = body(
        change(&a, Method::PUT, url.clone(), &ca)
            .json(&json!({"text": "call back", "expected_updated_at": null}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(s, 200);
    let seen = v["note_updated_at"].clone();
    // bob still thinks the note was never edited
    let (s, v) = body(
        change(&b, Method::PUT, url.clone(), &cb)
            .json(&json!({"text": "done", "expected_updated_at": null}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(s, 409);
    assert_eq!(v["error"]["code"], "conflict");
    assert_eq!(v["error"]["fields"]["note"], "call back");
    assert_eq!(v["error"]["fields"]["note_updated_by"], "alice");
    assert_eq!(v["error"]["fields"]["note_updated_at"], seen);
    let row = e.store.get(id).await.unwrap().unwrap();
    assert_eq!(
        row.note.as_deref(),
        Some("call back"),
        "the refused edit changed nothing"
    );
    // with the time he now knows it works
    let (s, v) = body(
        change(&b, Method::PUT, url, &cb)
            .json(&json!({"text": "done", "expected_updated_at": seen}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(s, 200);
    assert_eq!(v["note_updated_by"], "bob");
    e.stop().await;
}

#[tokio::test]
async fn w05_page_png_is_the_preview_file_with_etag_304_404_and_410() {
    let e = env().await;
    add_user(&e, "alice", Role::Agent, false).await;
    let (c, _) = login(&e, "alice").await;
    let id = fax(&e, "p@gw", "02", 5).await;
    let url = e.url(&format!("/api/v1/faxes/{id}/pages/1.png"));
    let r = c.get(&url).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["content-type"], "image/png");
    assert_eq!(r.headers()["cache-control"], "private, max-age=3600");
    let tag = r.headers()["etag"].to_str().unwrap().to_string();
    let bytes = r.bytes().await.unwrap();
    // the file next to the TIFF (ADR-057)
    let rec = e.store.get(id).await.unwrap().unwrap();
    let file = e.data_dir.join(store::png_rel_path(
        rec.tiff_path.as_deref().unwrap(),
        1,
        PngKind::Full,
    ));
    assert_eq!(bytes.as_ref(), std::fs::read(file).unwrap().as_slice());
    let r = c
        .get(&url)
        .header("If-None-Match", &tag)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 304);
    let thumb = c.get(format!("{url}?kind=thumb")).send().await.unwrap();
    assert_eq!(thumb.status(), 200);
    assert_ne!(thumb.bytes().await.unwrap(), bytes);
    for bad in ["pages/2.png", "pages/0.png", "pages/1.jpg", "pages/x.png"] {
        let r = c
            .get(e.url(&format!("/api/v1/faxes/{id}/{bad}")))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 404, "{bad}");
    }
    e.store
        .mark_files_purged(&[id], e.clock.now())
        .await
        .unwrap();
    let (s, v) = body(c.get(&url).send().await.unwrap()).await;
    assert_eq!((s, v["error"]["code"].as_str()), (410, Some("purged")));
    e.stop().await;
}

#[tokio::test]
async fn w06_downloads_are_the_files_with_a_safe_name_and_410_once_purged() {
    let e = env().await;
    add_user(&e, "alice", Role::Agent, false).await;
    let (c, _) = login(&e, "alice").await;
    let id = fax(&e, "d@gw", "+886 2", 5).await;
    let r = c
        .get(e.url(&format!("/api/v1/faxes/{id}/file.pdf")))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["content-type"], "application/pdf");
    let cd = r.headers()["content-disposition"]
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(
        cd,
        format!(
            "attachment; filename=\"fax-{id}_+886_2.pdf\"; filename*=UTF-8''fax-{id}_+886_2.pdf"
        )
    );
    let disk = std::fs::read(e.data_dir.join("faxes/2026/10/22/d@gw.pdf")).unwrap();
    assert_eq!(r.bytes().await.unwrap().as_ref(), disk.as_slice());
    let r = c
        .get(e.url(&format!("/api/v1/faxes/{id}/file.tif")))
        .send()
        .await
        .unwrap();
    assert_eq!(r.headers()["content-type"], "image/tiff");
    assert_eq!(
        r.bytes().await.unwrap().as_ref(),
        common::tiff_bytes().as_slice()
    );
    let r = c
        .get(e.url(&format!("/api/v1/faxes/{id}/file.exe")))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    e.store
        .mark_files_purged(&[id], e.clock.now())
        .await
        .unwrap();
    let (s, v) = body(
        c.get(e.url(&format!("/api/v1/faxes/{id}/file.pdf")))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!((s, v["error"]["code"].as_str()), (410, Some("purged")));
    e.stop().await;
}

#[tokio::test]
async fn w07_tricks_in_the_path_never_read_outside_the_data_directory() {
    let e = env().await;
    add_user(&e, "alice", Role::Agent, false).await;
    let (c, _) = login(&e, "alice").await;
    // a record that names a file outside data_dir (a damaged or forged row)
    let id = fax(&e, "evil@gw", "02", 5).await;
    std::fs::write(
        e.data_dir.parent().unwrap().join("secret.txt"),
        b"TOP SECRET",
    )
    .unwrap();
    let rec = e.store.get(id).await.unwrap().unwrap();
    std::fs::remove_file(e.data_dir.join(rec.pdf_path.as_deref().unwrap())).unwrap();
    // point the stored path out of the data directory through the backfill path
    let mut fin_new = store::NewFax {
        call_id: "evil@gw".into(),
        trunk: rec.trunk.clone(),
        ani: rec.ani.clone(),
        ani_source: None,
        dnis: rec.dnis.clone(),
        dnis_source: None,
        started_at: rec.started_at,
        answered_at: rec.answered_at,
    };
    fin_new.ani_source = None;
    let fin = store::FaxFinal {
        ended_at: rec.ended_at.unwrap(),
        answered_at: rec.answered_at,
        duration_sec: rec.duration_sec,
        hangup_direction: store::HangupDirection::Remote,
        hangup_code: "completed".into(),
        hangup_text: None,
        sip_status: Some(200),
        t30_code: Some(0),
        fax_status: store::FaxStatus::Received,
        pages: 1,
        remote_ident: None,
        resolution: None,
        bit_rate: None,
        compression: None,
        ecm: false,
        tiff_path: Some("../secret.txt".into()),
        tiff_size: Some(10),
        pdf_path: Some("faxes/../../secret.txt".into()),
        pdf_size: Some(10),
    };
    e.store.upsert_finished(&fin_new, &fin).await.unwrap();
    for f in ["file.pdf", "file.tif"] {
        let r = c
            .get(e.url(&format!("/api/v1/faxes/{id}/{f}")))
            .send()
            .await
            .unwrap();
        let s = r.status().as_u16();
        let t = r.text().await.unwrap();
        assert_eq!(s, 404, "{f}: {t}");
        assert!(
            !t.contains("TOP SECRET") && !t.contains("secret.txt"),
            "{t}"
        );
    }
    let r = c
        .get(e.url(&format!("/api/v1/faxes/{id}/pages/1.png")))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404, "a path outside data_dir is not a picture");
    assert!(!r.text().await.unwrap().contains("TOP SECRET"));
    // encoded slashes, dot segments and huge numbers in the URL
    for p in [
        "/api/v1/faxes/..%2F..%2Fsecret.txt/file.pdf",
        "/api/v1/faxes/1%2F2/file.pdf",
        "/api/v1/faxes/99999999999999999999999/file.pdf",
        "/api/v1/faxes/1/pages/99999999999999999999.png",
        "/api/v1/faxes/%2e%2e/file.pdf",
    ] {
        let r = c.get(e.url(p)).send().await.unwrap();
        let s = r.status().as_u16();
        assert!(s == 400 || s == 404, "{p}: {s}");
        assert!(!r.text().await.unwrap().contains("TOP SECRET"), "{p}");
    }
    e.stop().await;
}

#[tokio::test]
async fn w08_redelivery_is_for_admins_and_only_requeues_failed_targets() {
    let e = env().await;
    add_user(&e, "alice", Role::Agent, false).await;
    add_user(&e, "root", Role::Admin, false).await;
    let id = fax(&e, "rd@gw", "02", 5).await;
    for (target, status) in [
        ("folder:/out", DeliveryStatus::Failed),
        ("email:a@example.com", DeliveryStatus::Done),
    ] {
        e.store
            .add_delivery(&NewDelivery {
                fax_id: id,
                target: target.into(),
                kind: target.split(':').next().unwrap().into(),
                attempt: 1,
                status,
                next_try_at: None,
            })
            .await
            .unwrap();
    }
    let url = e.url(&format!("/api/v1/faxes/{id}/redeliver"));
    let (a, ca) = login(&e, "alice").await;
    let (s, v) = body(
        change(&a, Method::POST, url.clone(), &ca)
            .json(&json!({}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!((s, v["error"]["code"].as_str()), (403, Some("forbidden")));
    assert_eq!(
        e.store.list_deliveries(id).await.unwrap().len(),
        2,
        "the agent changed nothing"
    );
    let (r, cr) = login(&e, "root").await;
    let (s, v) = body(
        change(&r, Method::POST, url, &cr)
            .json(&json!({}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(s, 200);
    assert_eq!(v["queued"].as_array().unwrap().len(), 1);
    let all = e.store.list_deliveries(id).await.unwrap();
    assert_eq!(all.len(), 3);
    let new = all.last().unwrap();
    assert_eq!(
        (new.target.as_str(), new.status, new.attempt),
        ("folder:/out", DeliveryStatus::Pending, 2)
    );
    e.stop().await;
}

#[tokio::test]
async fn a01_a11_without_a_session_401_and_before_the_password_change_403() {
    let e = env().await;
    add_user(&e, "admin", Role::Admin, true).await;
    let id = fax(&e, "x@gw", "02", 5).await;
    let anon = reqwest::Client::new();
    for p in [
        "/api/v1/faxes".to_string(),
        format!("/api/v1/faxes/{id}"),
        format!("/api/v1/faxes/{id}/file.pdf"),
        format!("/api/v1/faxes/{id}/pages/1.png"),
    ] {
        let (s, v) = body(anon.get(e.url(&p)).send().await.unwrap()).await;
        assert_eq!(
            (s, v["error"]["code"].as_str()),
            (401, Some("unauthenticated")),
            "{p}"
        );
    }
    let (c, _) = login(&e, "admin").await;
    let (s, v) = body(c.get(e.url("/api/v1/faxes")).send().await.unwrap()).await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (403, Some("password_change_required"))
    );
    e.stop().await;
}
