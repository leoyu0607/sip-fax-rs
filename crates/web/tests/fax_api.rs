//! More of W01-W08 over HTTP: every filter against the store, paging through everything, the PNG
//! and the downloads byte for byte with an independent look at them, the safe name of a download
//! whose number has characters outside ASCII, and the rules of a redelivery.

mod common;

use accounts::Role;
use chrono::{Duration, SecondsFormat};
use common::*;
use reqwest::Method;
use serde_json::{Value, json};
use store::{Clock, DeliveryStatus, FaxFinal, FaxStatus, HangupDirection, NewDelivery, NewFax};

async fn body(r: reqwest::Response) -> (u16, Value) {
    let s = r.status().as_u16();
    (s, r.json().await.unwrap_or(Value::Null))
}

/// A finished fax without files: only the row.
async fn row(
    e: &Env,
    call: &str,
    trunk: &str,
    ani: &str,
    dnis: &str,
    status: FaxStatus,
    minutes_ago: i64,
) -> i64 {
    let start = e.clock.now() - Duration::minutes(minutes_ago);
    let new = NewFax {
        call_id: call.into(),
        trunk: trunk.into(),
        ani: Some(ani.into()),
        ani_source: None,
        dnis: Some(dnis.into()),
        dnis_source: None,
        started_at: start,
        answered_at: Some(start),
    };
    let fin = FaxFinal {
        ended_at: start + Duration::seconds(30),
        answered_at: Some(start),
        duration_sec: Some(30),
        hangup_direction: HangupDirection::Remote,
        hangup_code: "completed".into(),
        hangup_text: None,
        sip_status: Some(200),
        t30_code: Some(0),
        fax_status: status,
        pages: 1,
        remote_ident: None,
        resolution: None,
        bit_rate: None,
        compression: None,
        ecm: false,
        tiff_path: None,
        tiff_size: None,
        pdf_path: None,
        pdf_size: None,
        modem: None,
        media: None,
    };
    e.store.upsert_finished(&new, &fin).await.unwrap().0
}

fn ids(v: &Value) -> Vec<i64> {
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["id"].as_i64().unwrap())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn w01_every_filter_gives_what_the_store_gives_and_paging_walks_each_fax_once() {
    let e = env().await;
    add_user(&e, "alice", Role::Agent, false).await;
    let (c, csrf) = login(&e, "alice").await;
    // eight faxes over two trunks, three statuses, numbers with a common prefix
    let specs = [
        ("a1", "gw1", "0912000001", "2557", FaxStatus::Received, 80),
        ("a2", "gw1", "0912000002", "2557", FaxStatus::Received, 70),
        ("a3", "gw2", "0933000003", "2558", FaxStatus::Failed, 60),
        ("a4", "gw2", "0912000004", "2558", FaxStatus::Partial, 50),
        ("a5", "gw1", "0944000005", "2557", FaxStatus::Received, 40),
        ("a6", "gw2", "0912000006", "2559", FaxStatus::NoFax, 30),
        ("a7", "gw1", "0912000007", "2557", FaxStatus::Failed, 20),
        ("a8", "gw2", "0955000008", "2558", FaxStatus::Received, 10),
    ];
    let mut all = Vec::new();
    for (call, trunk, ani, dnis, st, m) in specs {
        all.push(row(&e, call, trunk, ani, dnis, st, m).await);
    }
    // one of them is read
    change(
        &c,
        Method::PUT,
        e.url(&format!("/api/v1/faxes/{}/read", all[4])),
        &csrf,
    )
    .json(&json!({"read": true}))
    .send()
    .await
    .unwrap();
    let t =
        |m: i64| (e.clock.now() - Duration::minutes(m)).to_rfc3339_opts(SecondsFormat::Secs, true);
    let queries: Vec<(String, store::ListFilter)> = vec![
        (
            "status=failed".into(),
            store::ListFilter {
                fax_status: Some(FaxStatus::Failed),
                ..Default::default()
            },
        ),
        (
            "status=partial".into(),
            store::ListFilter {
                fax_status: Some(FaxStatus::Partial),
                ..Default::default()
            },
        ),
        (
            "status=no_fax".into(),
            store::ListFilter {
                fax_status: Some(FaxStatus::NoFax),
                ..Default::default()
            },
        ),
        (
            "trunk=gw2".into(),
            store::ListFilter {
                trunk: Some("gw2".into()),
                ..Default::default()
            },
        ),
        (
            "dnis=2558".into(),
            store::ListFilter {
                dnis_prefix: Some("2558".into()),
                ..Default::default()
            },
        ),
        (
            "dnis=255".into(),
            store::ListFilter {
                dnis_prefix: Some("255".into()),
                ..Default::default()
            },
        ),
        (
            "ani=0912".into(),
            store::ListFilter {
                ani_prefix: Some("0912".into()),
                ..Default::default()
            },
        ),
        (
            "is_read=true".into(),
            store::ListFilter {
                is_read: Some(true),
                ..Default::default()
            },
        ),
        (
            "is_read=false".into(),
            store::ListFilter {
                is_read: Some(false),
                ..Default::default()
            },
        ),
        (
            format!("from={}", t(55)),
            store::ListFilter {
                from: Some(e.clock.now() - Duration::minutes(55)),
                ..Default::default()
            },
        ),
        (
            format!("to={}", t(35)),
            store::ListFilter {
                to: Some(e.clock.now() - Duration::minutes(35)),
                ..Default::default()
            },
        ),
        (
            format!("from={}&to={}&trunk=gw2&ani=0912", t(65), t(15)),
            store::ListFilter {
                from: Some(e.clock.now() - Duration::minutes(65)),
                to: Some(e.clock.now() - Duration::minutes(15)),
                trunk: Some("gw2".into()),
                ani_prefix: Some("0912".into()),
                ..Default::default()
            },
        ),
        (
            "trunk=nobody".into(),
            store::ListFilter {
                trunk: Some("nobody".into()),
                ..Default::default()
            },
        ),
        // wildcard characters of SQL are plain characters
        (
            "ani=09%25".into(),
            store::ListFilter {
                ani_prefix: Some("09%".into()),
                ..Default::default()
            },
        ),
    ];
    let mut nonempty = 0;
    for (q, filter) in queries {
        let (s, v) = body(
            c.get(e.url(&format!("/api/v1/faxes?{q}")))
                .send()
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(s, 200, "{q}: {v}");
        let direct = e
            .store
            .list(&filter, store::PageRequest::default())
            .await
            .unwrap();
        let want: Vec<i64> = direct.items.iter().map(|r| r.id).collect();
        assert_eq!(ids(&v), want, "{q}");
        if !want.is_empty() {
            nonempty += 1;
        }
    }
    assert!(nonempty >= 11, "the filters select something: {nonempty}");
    // some of the answers are known by hand, as a check on the store's side too
    let (_, v) = body(
        c.get(e.url("/api/v1/faxes?trunk=gw2&status=received"))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(ids(&v), [all[7]]);
    let (_, v) = body(
        c.get(e.url("/api/v1/faxes?ani=0912&dnis=2557"))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(ids(&v), [all[6], all[1], all[0]]);
    // paging: pages of three walk the eight faxes, newest first, each once, and the last page
    // has no cursor
    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0;
    loop {
        let q = match &cursor {
            Some(cu) => format!("limit=3&cursor={cu}"),
            None => "limit=3".to_string(),
        };
        let (s, v) = body(
            c.get(e.url(&format!("/api/v1/faxes?{q}")))
                .send()
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(s, 200);
        pages += 1;
        seen.extend(ids(&v));
        match v["next_cursor"].as_str() {
            Some(n) => cursor = Some(n.to_string()),
            None => break,
        }
        assert!(pages < 10, "paging does not end");
    }
    let mut newest_first = all.clone();
    newest_first.reverse();
    assert_eq!(seen, newest_first);
    assert_eq!(pages, 3);
    // a cursor of one query used with another filter is not an error (it is only a position),
    // but a cursor that was made up is
    for bad in [
        "",
        "abc",
        "-1",
        "1:2",
        "99999999999999999999999",
        "%00",
        "%27%20OR%201=1",
    ] {
        let r = c
            .get(e.url(&format!("/api/v1/faxes?cursor={bad}")))
            .send()
            .await
            .unwrap();
        let s = r.status().as_u16();
        // an empty cursor is "no cursor"; every other one is a 400
        let want = if bad.is_empty() { 200 } else { 400 };
        assert_eq!(s, want, "cursor={bad}");
    }
    e.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn w03_the_read_flag_toggles_back_records_the_account_and_the_time_each_time() {
    let e = env().await;
    add_user(&e, "alice", Role::Agent, false).await;
    add_user(&e, "bob", Role::Admin, false).await;
    let (a, ca) = login(&e, "alice").await;
    let (b, cb) = login(&e, "bob").await;
    let id = fax(&e, "tg@gw", "02", 5).await;
    let url = e.url(&format!("/api/v1/faxes/{id}/read"));
    let set = |c: &reqwest::Client, csrf: &str, v: bool| {
        change(c, Method::PUT, url.clone(), csrf)
            .json(&json!({"read": v}))
            .send()
    };
    let (s, v) = body(set(&a, &ca, true).await.unwrap()).await;
    assert_eq!(
        (s, v["is_read"].as_bool(), v["read_changed_by"].as_str()),
        (200, Some(true), Some("alice"))
    );
    let t1 = v["read_changed_at"].as_str().unwrap().to_string();
    e.clock.advance(Duration::minutes(3));
    let (s, v) = body(set(&b, &cb, false).await.unwrap()).await;
    assert_eq!(
        (s, v["is_read"].as_bool(), v["read_changed_by"].as_str()),
        (200, Some(false), Some("bob"))
    );
    assert_ne!(
        v["read_changed_at"].as_str().unwrap(),
        t1,
        "the time moved on"
    );
    let row = e.store.get(id).await.unwrap().unwrap();
    assert!(!row.is_read);
    assert_eq!(row.read_changed_by.as_deref(), Some("bob"));
    // a fax that is not there
    let (s, v) = body(
        change(&a, Method::PUT, e.url("/api/v1/faxes/424242/read"), &ca)
            .json(&json!({"read": true}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!((s, v["error"]["code"].as_str()), (404, Some("not_found")));
    e.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn w05_the_png_is_a_real_png_of_the_page_and_the_thumbnail_is_smaller() {
    let e = env().await;
    add_user(&e, "alice", Role::Agent, false).await;
    let (c, _) = login(&e, "alice").await;
    let id = fax(&e, "png@gw", "02", 5).await;
    let url = e.url(&format!("/api/v1/faxes/{id}/pages/1.png"));
    let full = c.get(&url).send().await.unwrap().bytes().await.unwrap();
    let thumb = c
        .get(format!("{url}?kind=thumb"))
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    // PNG signature, then the IHDR chunk: width and height are big-endian u32 at 16 and 20
    let dims = |b: &[u8]| {
        assert_eq!(&b[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&b[12..16], b"IHDR");
        (
            u32::from_be_bytes(b[16..20].try_into().unwrap()),
            u32::from_be_bytes(b[20..24].try_into().unwrap()),
        )
    };
    let (fw, fh) = dims(&full);
    let (tw, th) = dims(&thumb);
    assert!(fw >= 800 && fh > 0, "{fw}x{fh}");
    assert!(tw < fw && th < fh && tw > 0, "{tw}x{th} against {fw}x{fh}");
    // the tag is for these bytes: another page of another fax has another one
    let id2 = fax(&e, "png2@gw", "02", 4).await;
    let t1 = c.get(&url).send().await.unwrap().headers()["etag"].clone();
    let t2 = c
        .get(e.url(&format!("/api/v1/faxes/{id2}/pages/1.png")))
        .send()
        .await
        .unwrap()
        .headers()["etag"]
        .clone();
    assert_eq!(t1, t2, "the same picture has the same tag");
    let t3 = c
        .get(format!("{url}?kind=thumb"))
        .send()
        .await
        .unwrap()
        .headers()["etag"]
        .clone();
    assert_ne!(t1, t3, "another picture has another tag");
    // a stale tag gets the picture, the right one gets 304 with no body
    let r = c
        .get(&url)
        .header("If-None-Match", "\"stale\"")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let r = c
        .get(&url)
        .header("If-None-Match", t1)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 304);
    assert!(r.bytes().await.unwrap().is_empty());
    e.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn w06_a_number_with_characters_outside_ascii_gives_a_safe_download_name() {
    let e = env().await;
    add_user(&e, "alice", Role::Agent, false).await;
    let (c, _) = login(&e, "alice").await;
    let id = fax(&e, "cn@gw", "分機\"2\\3/..", 5).await;
    let r = c
        .get(e.url(&format!("/api/v1/faxes/{id}/file.pdf")))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let cd = r.headers()["content-disposition"]
        .to_str()
        .unwrap()
        .to_string();
    assert!(cd.is_ascii(), "the header is plain ASCII: {cd}");
    assert!(cd.starts_with("attachment; filename=\""), "{cd}");
    let plain = cd.split('"').nth(1).unwrap();
    assert!(
        !plain.contains(['/', '\\', '"', ' ']) && !plain.contains(".."),
        "{plain}"
    );
    assert!(
        plain.starts_with(&format!("fax-{id}_")) && plain.ends_with(".pdf"),
        "{plain}"
    );
    let star = cd.split("filename*=UTF-8''").nth(1).unwrap();
    assert_eq!(star, plain, "ASCII name: both forms carry the same");
    assert_eq!(
        r.headers()["cache-control"],
        "private, no-store",
        "a fax is not kept by shared caches"
    );
    e.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn w07_a_stored_path_outside_data_dir_is_404_for_the_png_too_and_nothing_leaks() {
    let e = env().await;
    add_user(&e, "alice", Role::Agent, false).await;
    let (c, _) = login(&e, "alice").await;
    std::fs::write(
        e.data_dir.parent().unwrap().join("secret.tif"),
        common::tiff_bytes(),
    )
    .unwrap();
    let id = fax(&e, "esc@gw", "02", 5).await;
    // the same record again, now naming a TIFF outside the data directory
    let rec = e.store.get(id).await.unwrap().unwrap();
    let new = NewFax {
        call_id: "esc@gw".into(),
        trunk: rec.trunk.clone(),
        ani: rec.ani.clone(),
        ani_source: None,
        dnis: rec.dnis.clone(),
        dnis_source: None,
        started_at: rec.started_at,
        answered_at: rec.answered_at,
    };
    let fin = FaxFinal {
        ended_at: rec.ended_at.unwrap(),
        answered_at: rec.answered_at,
        duration_sec: rec.duration_sec,
        hangup_direction: HangupDirection::Remote,
        hangup_code: "completed".into(),
        hangup_text: None,
        sip_status: Some(200),
        t30_code: Some(0),
        fax_status: FaxStatus::Received,
        pages: 1,
        remote_ident: None,
        resolution: None,
        bit_rate: None,
        compression: None,
        ecm: false,
        tiff_path: Some("../secret.tif".into()),
        tiff_size: Some(10),
        pdf_path: None,
        pdf_size: None,
        modem: None,
        media: None,
    };
    e.store.upsert_finished(&new, &fin).await.unwrap();
    for kind in ["full", "thumb"] {
        let r = c
            .get(e.url(&format!("/api/v1/faxes/{id}/pages/1.png?kind={kind}")))
            .send()
            .await
            .unwrap();
        let s = r.status().as_u16();
        let t = r.bytes().await.unwrap();
        assert!(s == 404, "{kind}: {s} {}", String::from_utf8_lossy(&t));
        assert!(
            !t.starts_with(b"\x89PNG"),
            "{kind}: a picture of a file outside data_dir"
        );
        assert!(
            !String::from_utf8_lossy(&t).contains("secret.tif"),
            "{kind}"
        );
    }
    let r = c
        .get(e.url(&format!("/api/v1/faxes/{id}/file.tif")))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    e.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn w08_giving_up_is_queued_again_a_target_that_later_succeeded_is_not_and_unknown_is_404() {
    let e = env().await;
    add_user(&e, "root", Role::Admin, false).await;
    let (c, csrf) = login(&e, "root").await;
    let id = fax(&e, "rd2@gw", "02", 5).await;
    let add = |target: &str, attempt: i32, status: DeliveryStatus| NewDelivery {
        fax_id: id,
        target: target.into(),
        kind: target.split(':').next().unwrap().into(),
        attempt,
        status,
        next_try_at: None,
    };
    for d in [
        add("email:a@example.com", 1, DeliveryStatus::GivingUp),
        // failed first, done on the retry: nothing to do for it
        add("folder:/out", 1, DeliveryStatus::Failed),
        add("folder:/out", 2, DeliveryStatus::Done),
        add(
            "webhook:https://hook.example/x?token=s3cret",
            1,
            DeliveryStatus::Pending,
        ),
    ] {
        e.store.add_delivery(&d).await.unwrap();
    }
    let url = e.url(&format!("/api/v1/faxes/{id}/redeliver"));
    let mut events = e.events.attach(None).0;
    let (s, v) = body(
        change(&c, Method::POST, url.clone(), &csrf)
            .json(&json!({}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["queued"].as_array().unwrap().len(), 1, "{v}");
    let all = e.store.list_deliveries(id).await.unwrap();
    assert_eq!(all.len(), 5);
    let new = all.last().unwrap();
    assert_eq!(
        (new.target.as_str(), new.status, new.attempt),
        ("email:a@example.com", DeliveryStatus::Pending, 2)
    );
    // the answer shows the deliveries with the webhook secret masked
    let shown = v["deliveries"].to_string();
    assert!(!shown.contains("s3cret"), "{shown}");
    // the change is announced
    let ev = events.try_recv().expect("delivery.updated was published");
    assert!(ev.json.contains("delivery.updated"), "{}", ev.json);
    // a second redeliver finds nothing to queue: the new attempt is pending, not failed
    let (s, v) = body(
        change(&c, Method::POST, url, &csrf)
            .json(&json!({}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(s, 200);
    assert_eq!(v["queued"].as_array().unwrap().len(), 0);
    assert_eq!(e.store.list_deliveries(id).await.unwrap().len(), 5);
    assert!(
        events.try_recv().is_err(),
        "nothing queued, nothing announced"
    );
    // an unknown fax and a bad id
    for bad in ["4242", "0", "x"] {
        let (s, v) = body(
            change(
                &c,
                Method::POST,
                e.url(&format!("/api/v1/faxes/{bad}/redeliver")),
                &csrf,
            )
            .json(&json!({}))
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
