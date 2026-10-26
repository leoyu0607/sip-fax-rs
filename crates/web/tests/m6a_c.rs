//! M6a (ADR-070) the change feed `GET /api/v1/faxes/changes` over real HTTP (C01-C04): every
//! change once, in the order of `(updated_at, id)`, with the settling time of 2 seconds and an
//! injected clock. The same walk on the four server databases is in
//! `crates/store/src/tests/m6a_tests.rs` (C05).

mod common;

use std::collections::{HashMap, HashSet};

use accounts::Role;
use chrono::{DateTime, Duration as Dur, Utc};
use common::*;
use reqwest::Method;
use serde_json::{Value, json};
use store::{Clock, FaxFinal, FaxStatus, HangupDirection, NewFax};

struct Feed {
    key: String,
    base: String,
    client: reqwest::Client,
}

impl Feed {
    async fn call(&self, query: &str) -> (u16, Value) {
        let r = self
            .client
            .get(format!("{}/api/v1/faxes/changes{query}", self.base))
            .header("Authorization", format!("Bearer {}", self.key))
            .send()
            .await
            .unwrap();
        (r.status().as_u16(), r.json().await.unwrap())
    }

    async fn ok(&self, query: &str) -> Value {
        let (s, v) = self.call(query).await;
        assert_eq!(s, 200, "{query}: {v}");
        v
    }

    /// Walks from `after` (a cursor, or `None` for the start) in pages of `limit` until a page is
    /// empty; returns every item and the last `next`.
    async fn walk(&self, after: Option<&str>, limit: u32) -> (Vec<Value>, String) {
        let mut items = Vec::new();
        let mut cursor = after.map(str::to_string);
        loop {
            let q = match &cursor {
                Some(c) => format!("?after={c}&limit={limit}"),
                None => format!("?limit={limit}"),
            };
            let v = self.ok(&q).await;
            let page = v["items"].as_array().unwrap().clone();
            let next = v["next"].as_str().unwrap().to_string();
            if page.is_empty() {
                return (
                    items,
                    cursor.map_or(next.clone(), |c| {
                        assert_eq!(next, c, "an empty page keeps the position");
                        c
                    }),
                );
            }
            assert!(page.len() <= limit as usize);
            cursor = Some(next);
            items.extend(page);
        }
    }
}

/// An admin session, a read-and-write key and a client for the feed.
async fn setup() -> (Env, Feed, Admin) {
    let e = env().await;
    add_user(&e, "boss", Role::Admin, false).await;
    let (c, csrf) = login(&e, "boss").await;
    let r = change(&c, Method::POST, e.url("/api/v1/api-keys"), &csrf)
        .json(&json!({"name": "feed", "write": true}))
        .send()
        .await
        .unwrap();
    let key = r.json::<Value>().await.unwrap()["key"]
        .as_str()
        .unwrap()
        .to_string();
    let feed = Feed {
        key,
        base: e.base.clone(),
        client: reqwest::Client::new(),
    };
    (e, feed, Admin { c, csrf })
}

struct Admin {
    c: reqwest::Client,
    csrf: String,
}

fn ms(v: &Value) -> i64 {
    DateTime::parse_from_rfc3339(v["updated_at"].as_str().unwrap())
        .unwrap()
        .timestamp_millis()
}

fn finish(status: FaxStatus, at: DateTime<Utc>) -> FaxFinal {
    FaxFinal {
        ended_at: at,
        answered_at: None,
        duration_sec: Some(10),
        hangup_direction: HangupDirection::Remote,
        hangup_code: "completed".into(),
        hangup_text: None,
        sip_status: None,
        t30_code: Some(0),
        fax_status: status,
        pages: i32::from(matches!(status, FaxStatus::Received | FaxStatus::Partial)),
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
    }
}

/// A fax of `status` without files, written at the clock's time.
async fn quick(e: &Env, call: &str, status: Option<FaxStatus>) -> i64 {
    let now = e.clock.now();
    let id = e
        .store
        .insert_receiving(&NewFax {
            call_id: call.into(),
            trunk: "gw".into(),
            ani: Some("0911".into()),
            ani_source: Some("from".into()),
            dnis: Some("2557".into()),
            dnis_source: Some("ruri".into()),
            started_at: now,
            answered_at: None,
        })
        .await
        .unwrap();
    if let Some(s) = status {
        e.store.finalize(id, &finish(s, now)).await.unwrap();
    }
    id
}

/// 300 faxes in every state, ten at a time with the same time stamp, 7 ms apart.
async fn many(e: &Env) -> Vec<i64> {
    let states = [
        None,
        Some(FaxStatus::Received),
        Some(FaxStatus::Partial),
        Some(FaxStatus::Failed),
        Some(FaxStatus::NoFax),
    ];
    let mut ids = Vec::new();
    for i in 0..300usize {
        e.clock
            .set(DateTime::from_timestamp_millis(T0 + 123 + (i as i64 / 10) * 7).unwrap());
        ids.push(quick(e, &format!("feed-{i}"), states[i % 5]).await);
    }
    ids
}

// ---------------------------------------------------------------- C01
#[tokio::test(flavor = "multi_thread")]
async fn c01_every_fax_once_in_the_order_of_updated_at_and_id() {
    let (e, feed, _a) = setup().await;
    let ids = many(&e).await;
    e.clock.advance(Dur::seconds(3));
    for limit in [1u32, 7, 200] {
        let (all, last) = feed.walk(None, limit).await;
        assert_eq!(all.len(), 300, "limit {limit}");
        let got: Vec<i64> = all.iter().map(|v| v["id"].as_i64().unwrap()).collect();
        assert_eq!(
            got.iter().copied().collect::<HashSet<_>>(),
            ids.iter().copied().collect(),
            "each once"
        );
        let keys: Vec<(i64, i64)> = all
            .iter()
            .map(|v| (ms(v), v["id"].as_i64().unwrap()))
            .collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted, "order");
        // `next` is the position of the last item: <ms>.<id>
        let (lm, li) = *keys.last().unwrap();
        assert_eq!(last, format!("{lm}.{li}"));
        // after the last page: no more, the same position
        let v = feed.ok(&format!("?after={last}&limit={limit}")).await;
        assert_eq!(v["items"], json!([]));
        assert_eq!(v["next"], last);
        // the states are all there
        let statuses: HashSet<&str> = all
            .iter()
            .map(|v| v["fax_status"].as_str().unwrap())
            .collect();
        assert_eq!(statuses.len(), 5, "{statuses:?}");
    }
    // the first call without a cursor on an empty feed: next is a valid start position
    let v = feed.ok("?limit=1").await;
    assert_eq!(v["items"].as_array().unwrap().len(), 1);
    e.stop().await;
    let (e, feed, _a) = setup().await;
    let v = feed.ok("").await;
    assert_eq!(
        (v["items"].clone(), v["next"].clone()),
        (json!([]), json!("0.0"))
    );
    let v = feed.ok("?after=0.0").await;
    assert_eq!(v["items"], json!([]));
    e.stop().await;
}

// ---------------------------------------------------------------- C02
#[tokio::test(flavor = "multi_thread")]
async fn c02_changes_during_the_walk_come_again_and_the_last_two_seconds_wait() {
    let (e, feed, a) = setup().await;
    let ids = many(&e).await;
    e.clock.advance(Dur::seconds(3));

    // walk 100, then change things in the middle
    let mut seen: HashMap<i64, i64> = HashMap::new();
    let mut cursor: Option<String> = None;
    for _ in 0..4 {
        let q = cursor
            .as_ref()
            .map_or("?limit=25".to_string(), |c| format!("?after={c}&limit=25"));
        let v = feed.ok(&q).await;
        for f in v["items"].as_array().unwrap() {
            seen.insert(f["id"].as_i64().unwrap(), ms(f));
        }
        cursor = Some(v["next"].as_str().unwrap().to_string());
    }
    assert_eq!(seen.len(), 100);
    e.clock.advance(Dur::seconds(30));
    let (seen_one, unseen_one, receiving) = (ids[3], ids[250], ids[200]);
    // a read flag (a session), a note (the key), a reception that ends (the daemon), a new fax
    change(
        &a.c,
        Method::PUT,
        e.url(&format!("/api/v1/faxes/{seen_one}/read")),
        &a.csrf,
    )
    .json(&json!({"read": true}))
    .send()
    .await
    .unwrap();
    let r = reqwest::Client::new()
        .put(e.url(&format!("/api/v1/faxes/{unseen_one}/note")))
        .header("Authorization", format!("Bearer {}", feed.key))
        .json(&json!({"text": "n", "expected_updated_at": null}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    e.store
        .finalize(receiving, &finish(FaxStatus::Received, e.clock.now()))
        .await
        .unwrap();
    let fresh = quick(&e, "feed-new", None).await;

    // inside the settling time: not yet
    let v = feed
        .ok(&format!("?after={}&limit=200", cursor.as_ref().unwrap()))
        .await;
    let early: Vec<i64> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["id"].as_i64().unwrap())
        .collect();
    assert!(
        early
            .iter()
            .all(|i| ![seen_one, receiving, fresh].contains(i)),
        "a change of this very moment is held back: {early:?}"
    );
    e.clock.advance(Dur::milliseconds(1_999));
    let v = feed
        .ok(&format!("?after={}&limit=200", cursor.as_ref().unwrap()))
        .await;
    assert!(
        !v["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["id"] == fresh),
        "1999 ms after the change: not yet"
    );
    e.clock.advance(Dur::milliseconds(1));
    // finish the walk
    loop {
        let q = format!("?after={}&limit=25", cursor.as_ref().unwrap());
        let v = feed.ok(&q).await;
        let page = v["items"].as_array().unwrap();
        if page.is_empty() {
            break;
        }
        for f in page {
            seen.insert(f["id"].as_i64().unwrap(), ms(f));
        }
        cursor = Some(v["next"].as_str().unwrap().to_string());
    }
    // for every fax the feed has delivered its latest state, nothing is missing
    let all = json_all(&a.c, &e).await;
    assert_eq!(all.len(), 301);
    for f in &all {
        let id = f["id"].as_i64().unwrap();
        assert_eq!(
            seen.get(&id),
            Some(&ms(f)),
            "fax {id} is up to date in the feed"
        );
    }
    // the changed ones came once more, after the end of the original walk
    let again = feed
        .ok(&format!(
            "?since={}",
            (e.clock.now() - Dur::seconds(20))
                .to_rfc3339()
                .replace('+', "%2B")
        ))
        .await;
    let got: HashSet<i64> = again["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["id"].as_i64().unwrap())
        .collect();
    assert_eq!(
        got,
        [seen_one, unseen_one, receiving, fresh]
            .into_iter()
            .collect()
    );
    let is_read = again["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["id"] == seen_one)
        .unwrap()["is_read"]
        .clone();
    assert_eq!(is_read, true);

    // the race that the 2 seconds are for: a feed call, and then a slower writer commits a row
    // that is older than the call (its time stamp was taken earlier)
    let (_, last) = feed.walk(Some(cursor.as_ref().unwrap()), 200).await;
    e.clock.advance(Dur::seconds(10));
    let t = e.clock.now();
    e.clock.set(t - Dur::milliseconds(1_500));
    let early_row = quick(&e, "early", Some(FaxStatus::Received)).await; // time stamp t - 1.5 s
    e.clock.set(t);
    // a call now (at t) hands out nothing newer than t - 2 s: the row of t - 1.5 s waits
    let v = feed.ok(&format!("?after={last}")).await;
    assert_eq!(v["items"], json!([]));
    assert_eq!(
        v["next"], last,
        "the position did not move past the row of t - 1.5 s"
    );
    // ... and the late writer's row (time stamp t - 1.8 s), committed after that call, is not lost
    e.clock.set(t - Dur::milliseconds(1_800));
    let late_row = quick(&e, "late", Some(FaxStatus::Received)).await;
    e.clock.set(t + Dur::seconds(5));
    let v = feed.ok(&format!("?after={last}")).await;
    let got: Vec<i64> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["id"].as_i64().unwrap())
        .collect();
    assert_eq!(
        got,
        [late_row, early_row],
        "both, the older time stamp first"
    );
    e.stop().await;
}

/// Every fax of the list (an administrator's session), newest first.
async fn json_all(c: &reqwest::Client, e: &Env) -> Vec<Value> {
    let mut out = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let url = cursor
            .as_ref()
            .map_or(e.url("/api/v1/faxes?limit=200"), |c| {
                e.url(&format!("/api/v1/faxes?limit=200&cursor={c}"))
            });
        let v: Value = c.get(url).send().await.unwrap().json().await.unwrap();
        out.extend(v["items"].as_array().unwrap().iter().cloned());
        match v["next_cursor"].as_str() {
            Some(n) => cursor = Some(n.to_string()),
            None => return out,
        }
    }
}

// ---------------------------------------------------------------- C03
#[tokio::test(flavor = "multi_thread")]
async fn c03_equal_times_are_told_apart_by_id_across_pages() {
    let (e, feed, _a) = setup().await;
    e.clock
        .set(DateTime::from_timestamp_millis(T0 + 777).unwrap());
    let mut ids = Vec::new();
    for i in 0..25 {
        ids.push(quick(&e, &format!("tie-{i}"), None).await);
    }
    e.clock
        .set(DateTime::from_timestamp_millis(T0 + 778).unwrap());
    for i in 25..30 {
        ids.push(quick(&e, &format!("tie-{i}"), Some(FaxStatus::Received)).await);
    }
    e.clock.advance(Dur::seconds(5));
    let want: Vec<i64> = {
        let mut w = ids.clone();
        w.sort();
        w
    };
    for limit in [1u32, 4, 6, 25, 26, 30, 200] {
        let (all, _) = feed.walk(None, limit).await;
        let got: Vec<i64> = all.iter().map(|v| v["id"].as_i64().unwrap()).collect();
        assert_eq!(
            got, want,
            "limit {limit}: none lost, none twice at a page border"
        );
    }
    // a cursor inside a run of equal times: exactly the rest of it
    let mid = format!("{}.{}", T0 + 777, ids[9]);
    let v = feed.ok(&format!("?after={mid}")).await;
    let got: Vec<i64> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["id"].as_i64().unwrap())
        .collect();
    assert_eq!(got, want[10..]);
    // `since` is inclusive: the whole run of that time, nothing before
    let v = feed
        .ok(&format!("?since={}", "2026-10-21T00:00:00.778Z"))
        .await;
    assert_eq!(v["items"].as_array().unwrap().len(), 5);
    assert_eq!(v["next"], format!("{}.{}", T0 + 778, ids[29]));
    // an empty `since` answer: the position is that time (ids start at 1)
    let v = feed.ok("?since=2030-01-01T00:00:00Z").await;
    assert_eq!(v["items"], json!([]));
    let next = v["next"].as_str().unwrap().to_string();
    assert!(next.ends_with(".0"), "{next}");
    assert_eq!(feed.ok(&format!("?after={next}")).await["items"], json!([]));
    e.stop().await;
}

// ---------------------------------------------------------------- C04
#[tokio::test(flavor = "multi_thread")]
async fn c04_bad_requests_are_refused_and_good_edges_are_accepted() {
    let (e, feed, a) = setup().await;
    quick(&e, "one", Some(FaxStatus::Received)).await;
    e.clock.advance(Dur::seconds(5));
    let bad = [
        "?after=1.2&since=2026-01-01T00:00:00Z",
        "?since=2026-01-01T00:00:00Z&after=1.2",
        "?after=abc",
        "?after=1",
        "?after=1.2.3",
        "?after=1.x",
        "?after=x.1",
        "?after=-",
        "?after=99999999999999999999.1",
        "?after=1.99999999999999999999",
        "?since=yesterday",
        "?since=2026-13-01T00:00:00Z",
        "?limit=0",
        "?limit=201",
        "?limit=-1",
        "?limit=abc",
        "?limit=1.5",
        "?limit=99999999999999999999",
        "?foo=1",
        "?after=1.2&foo=1",
        "?Limit=5",
        "?limit=5&limit=6x",
    ];
    for q in bad {
        let (s, v) = feed.call(q).await;
        assert_eq!(s, 400, "{q}: {v}");
        assert_eq!(v["error"]["code"], "bad_request", "{q}");
        // a session is judged alike
        let r =
            a.c.get(e.url(&format!("/api/v1/faxes/changes{q}")))
                .send()
                .await
                .unwrap();
        assert_eq!(r.status(), 400, "{q} (session)");
    }
    // the edges that are fine
    for q in [
        // an empty value is "not given" (as in the list: a form leaves blanks)
        "?after=",
        "?after=1.2%20", // blanks around a value are cut
        "?since=",
        "?limit=",
        "?limit=1",
        "?limit=200",
        "?after=0.0",
        "?after=-5.0",
        "?after=1.0&limit=10",
        "?since=1970-01-01T00:00:00Z",
        "?since=2026-10-21T00:00:00.5%2B08:00",
        "",
    ] {
        let (s, v) = feed.call(q).await;
        assert_eq!(s, 200, "{q}: {v}");
    }
    // a session and an agent may read the feed as well
    let r =
        a.c.get(e.url("/api/v1/faxes/changes"))
            .send()
            .await
            .unwrap();
    assert_eq!(r.status(), 200);
    add_user(&e, "amy", Role::Agent, false).await;
    let (amy, _) = login(&e, "amy").await;
    let r = amy
        .get(e.url("/api/v1/faxes/changes"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    // without credentials: 401 (the bad query is not looked at first)
    let r = reqwest::get(e.url("/api/v1/faxes/changes?limit=0"))
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    e.stop().await;
}
