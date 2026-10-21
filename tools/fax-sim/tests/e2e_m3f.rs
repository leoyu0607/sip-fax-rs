//! M3f end-to-end cases (F30..F33): delivery inside the real `sip-faxd serve`. The simulated
//! gateway (SpanDSP `t38_terminal` on a real UDP socket and clock) sends a fax; the route sends
//! it on; the test looks at the destination and at the `delivery_attempts` table.
//!
//! * F30: two pages to a folder: the PDF and the TIFF appear with exactly the bytes of the files
//!   in `data_dir`, the attempt row is `done`, the process stops gracefully.
//! * F31: the gateway hangs up in the middle (`partial`) and the route has
//!   `deliver_partial = false` (the default): nothing is delivered, no attempt row exists.
//! * F32: the same with `deliver_partial = true`: the partial fax (fewer pages than sent) is
//!   delivered, bytes equal to the stored files.
//! * F33: webhook to an HTTP server inside the test: JSON fields, HMAC-SHA256 signature
//!   recomputed here, and the PDF as multipart (bytes equal to the stored PDF).

mod e2e_common;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use e2e_common::*;
use fax_sim::sip_uac::{ReinviteBy, run_call};
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use store::{
    ConnectOptions, DeliveryAttempt, DeliveryStatus, FaxRecord, FaxStatus, FaxStore, ListFilter,
    PageRequest,
};

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

/// Polls the page count of the newest row (the gateway hangs up when it reaches `pages`).
fn hang_up_after(db: Db, pages: i32, flag: Arc<AtomicBool>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let deadline = Instant::now() + Duration::from_secs(110);
        while Instant::now() < deadline {
            if let Ok(p) = db
                .list(&ListFilter::default(), PageRequest::default())
                .await
                && let Some(r) = p.items.first()
            {
                if r.pages >= pages {
                    flag.store(true, Ordering::Relaxed);
                }
                if r.fax_status != FaxStatus::Receiving {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
}

async fn attempts_where(
    db: &Db,
    id: i64,
    pred: impl Fn(&[DeliveryAttempt]) -> bool,
) -> Vec<DeliveryAttempt> {
    for _ in 0..1000 {
        let a = db.list_deliveries(id).await.unwrap();
        if pred(&a) {
            return a;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "delivery attempts never reached the expected state: {:?}",
        db.list_deliveries(id).await.unwrap()
    );
}

fn any_done(a: &[DeliveryAttempt]) -> bool {
    a.iter().any(|x| x.status == DeliveryStatus::Done)
}

fn wait_file(p: &Path) -> Vec<u8> {
    wait_for(&format!("{}", p.display()), || std::fs::read(p).ok())
}

fn folder_route(out: &Path, extra: &str) -> String {
    format!(
        "[[routes]]\ndnis = \"fax\"\n{extra}\n[[routes.deliveries]]\nkind = \"folder\"\ndir = {:?}\nfilename = \"fax-{{id}}\"\n",
        out.display().to_string()
    )
}

// ------------------------------------------------------------------------------ F30

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn f30_a_two_page_fax_is_delivered_to_the_folder_byte_for_byte() {
    let tag = "m3f-f30";
    let out = scratch(&format!("out-{tag}"));
    let f = faxd_routes(tag, 2, &folder_route(&out, ""));
    let db = open(&f.db_url).await;
    let dir = scratch(&format!("src-{tag}"));
    let pages = specs(2, true, 600);
    let src = source_tiff(&dir, "src.tif", &pages);
    let rep = run_call(uac(&f, ReinviteBy::Gateway, &src, "0000000001"))
        .await
        .unwrap_or_else(|e| panic!("{tag}: {e}\n{}", f.log().join("\n")));
    f.wait_results(1);
    let res = result_json(&f, &rep.call_id);
    assert_success(tag, &rep, &res, 2, "0000000001", "peer_reinvite");
    let row = final_row(&db, &rep.call_id).await;
    assert_eq!(row.fax_status, FaxStatus::Received);
    assert_eq!(row.pages, 2);

    let pdf = wait_file(&out.join(format!("fax-{}.pdf", row.id)));
    let tif = wait_file(&out.join(format!("fax-{}.tif", row.id)));
    let stored_pdf = std::fs::read(f.data_dir.join(row.pdf_path.as_deref().unwrap())).unwrap();
    let stored_tif = std::fs::read(f.data_dir.join(row.tiff_path.as_deref().unwrap())).unwrap();
    assert!(pdf.starts_with(b"%PDF-"), "a PDF");
    assert_eq!(
        pdf.len() as i64,
        row.pdf_size.unwrap(),
        "size of the record"
    );
    assert_eq!(pdf, stored_pdf, "the delivered PDF is the stored PDF");
    assert_eq!(tif, stored_tif, "the delivered TIFF is the stored TIFF");

    let a = attempts_where(&db, row.id, any_done).await;
    assert_eq!(a.len(), 1, "{a:?}");
    assert_eq!(a[0].kind, "folder");
    assert_eq!(a[0].target, format!("folder:{}", out.display()));
    assert_eq!((a[0].attempt, a[0].last_error.clone()), (1, None));
    let ls: Vec<_> = std::fs::read_dir(&out)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(ls.len(), 2, "no `.part` files are left: {ls:?}");

    // SIGTERM: the server leaves cleanly, no delivery was cut off
    let log = f.stop().join("\n");
    assert!(log.contains("delivery engine running"), "{log}");
    assert!(log.contains("shutting down"), "{log}");
    assert!(!log.contains("still running"), "{log}");
    assert!(!log.contains("ERROR"), "{log}");
}

// ------------------------------------------------------------------------------ F31 / F32

/// The gateway sends six pages and hangs up as soon as two are in; returns the final row.
async fn partial_call(tag: &str, f: &Server, db: &Db) -> FaxRecord {
    let dir = scratch(&format!("src-{tag}"));
    let pages = specs(6, true, 1500);
    let src = source_tiff(&dir, "src.tif", &pages);
    let hangup = Arc::new(AtomicBool::new(false));
    let mut cfg = uac(f, ReinviteBy::Gateway, &src, "0000000001");
    cfg.peer_bye_wait = Duration::ZERO;
    cfg.fax.as_mut().unwrap().hangup_grace = Duration::from_millis(500);
    cfg.hangup = Some(hangup.clone());
    let watcher = hang_up_after(db.clone(), 2, hangup);
    let rep = run_call(cfg)
        .await
        .unwrap_or_else(|e| panic!("{tag}: {e}\n{}", f.log().join("\n")));
    f.wait_results(1);
    let _ = watcher.await;
    let row = final_row(db, &rep.call_id).await;
    assert_eq!(row.fax_status, FaxStatus::Partial, "{row:?}");
    assert!((2..6).contains(&row.pages), "{}", row.pages);
    row
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn f31_a_partial_fax_is_not_delivered_when_the_route_does_not_ask_for_it() {
    let tag = "m3f-f31";
    let out = scratch(&format!("out-{tag}"));
    let f = faxd_routes(tag, 2, &folder_route(&out, "deliver_partial = false"));
    let db = open(&f.db_url).await;
    let row = partial_call(tag, &f, &db).await;
    // the server looked at the fax and decided: that is a log line, not a guess
    wait_for("the decision not to deliver", || {
        f.log()
            .iter()
            .any(|l| l.contains("status not delivered by route") && l.contains("partial"))
            .then_some(())
    });
    assert!(
        db.list_deliveries(row.id).await.unwrap().is_empty(),
        "no delivery_attempts row"
    );
    assert_eq!(
        std::fs::read_dir(&out).unwrap().count(),
        0,
        "nothing in the destination"
    );
    assert_eq!(
        db.get_by_call_id(&row.call_id)
            .await
            .unwrap()
            .unwrap()
            .fax_status,
        FaxStatus::Partial
    );
    f.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn f32_a_partial_fax_is_delivered_when_the_route_asks_for_it() {
    let tag = "m3f-f32";
    let out = scratch(&format!("out-{tag}"));
    let f = faxd_routes(tag, 2, &folder_route(&out, "deliver_partial = true"));
    let db = open(&f.db_url).await;
    let row = partial_call(tag, &f, &db).await;
    let pdf = wait_file(&out.join(format!("fax-{}.pdf", row.id)));
    let tif = wait_file(&out.join(format!("fax-{}.tif", row.id)));
    assert_eq!(
        pdf,
        std::fs::read(f.data_dir.join(row.pdf_path.as_deref().unwrap())).unwrap()
    );
    assert_eq!(
        tif,
        std::fs::read(f.data_dir.join(row.tiff_path.as_deref().unwrap())).unwrap()
    );
    attempts_where(&db, row.id, any_done).await;
    f.stop();
}

// ------------------------------------------------------------------------------ F33

/// What the test's HTTP server saw.
#[derive(Clone, Debug)]
struct Hit {
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

/// A minimal HTTP/1.1 server on 127.0.0.1 that answers every POST with 200.
fn http_server() -> (u16, Arc<Mutex<Vec<Hit>>>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let hits = Arc::new(Mutex::new(Vec::new()));
    let h2 = hits.clone();
    std::thread::spawn(move || {
        for conn in l.incoming() {
            let Ok(mut c) = conn else { break };
            let hits = h2.clone();
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 8192];
                let head_end = loop {
                    let n = c.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break i + 4;
                    }
                };
                let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
                let mut headers = HashMap::new();
                for line in head.lines().skip(1) {
                    if let Some((k, v)) = line.split_once(':') {
                        headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
                    }
                }
                let len: usize = headers
                    .get("content-length")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                while buf.len() < head_end + len {
                    let n = c.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let body = buf[head_end..(head_end + len).min(buf.len())].to_vec();
                hits.lock().unwrap().push(Hit { headers, body });
                let _ = c.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                );
            });
        }
    });
    (port, hits)
}

fn hmac_hex(key: &str, body: &[u8]) -> String {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(key.as_bytes()).unwrap();
    mac.update(body);
    let tag = mac.finalize().into_bytes();
    let hex: String = tag.iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256={hex}")
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    hay[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|i| i + from)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn f33_a_webhook_gets_the_record_a_valid_signature_and_the_pdf() {
    let tag = "m3f-f33";
    let (port, hits) = http_server();
    let routes = format!(
        "[[routes]]\ndnis = \"fax\"\n[[routes.deliveries]]\nkind = \"webhook\"\nurl = \"http://127.0.0.1:{port}/fax-hook?token=abc\"\nsecret = \"e2e-secret\"\nattach_pdf = true\n"
    );
    let f = faxd_routes(tag, 2, &routes);
    let db = open(&f.db_url).await;
    let dir = scratch(&format!("src-{tag}"));
    let pages = specs(2, true, 600);
    let src = source_tiff(&dir, "src.tif", &pages);
    let rep = run_call(uac(&f, ReinviteBy::Gateway, &src, "0000000001"))
        .await
        .unwrap_or_else(|e| panic!("{tag}: {e}\n{}", f.log().join("\n")));
    f.wait_results(1);
    let row = final_row(&db, &rep.call_id).await;
    assert_eq!(row.fax_status, FaxStatus::Received);
    let a = attempts_where(&db, row.id, any_done).await;
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].kind, "webhook");
    assert_eq!(
        a[0].target,
        format!("webhook:http://127.0.0.1:{port}/fax-hook"),
        "the token in the query is not stored"
    );

    let got = hits.lock().unwrap().clone();
    assert_eq!(got.len(), 1, "exactly one POST: {got:?}");
    let hit = &got[0];
    // the signature is over the exact body
    assert_eq!(
        hit.headers.get("x-sipfax-signature").map(String::as_str),
        Some(hmac_hex("e2e-secret", &hit.body).as_str())
    );
    assert_ne!(
        hit.headers["x-sipfax-signature"],
        hmac_hex("another-secret", &hit.body),
        "the check is not vacuous"
    );
    assert_eq!(hit.headers["x-sipfax-event"], "fax.received");
    let ct = &hit.headers["content-type"];
    assert!(ct.starts_with("multipart/form-data; boundary="), "{ct}");
    let boundary = ct.split("boundary=").nth(1).unwrap();
    let body = &hit.body;
    // part "record": JSON
    let rec_at = find(body, b"name=\"record\"", 0).expect("record part");
    let json_start = find(body, b"\r\n\r\n", rec_at).unwrap() + 4;
    let json_end = find(body, format!("\r\n--{boundary}").as_bytes(), json_start).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body[json_start..json_end]).unwrap();
    assert_eq!(json["id"], row.id);
    assert_eq!(json["call_id"], rep.call_id.as_str());
    assert_eq!(json["event"], "fax.received");
    assert_eq!(json["fax_status"], "received");
    assert_eq!(json["pages"], 2);
    assert_eq!(json["ani"], "gw");
    assert_eq!(json["dnis"], "fax");
    assert_eq!(json["remote_ident"], "0000000001");
    assert_eq!(json["pdf_size"], row.pdf_size.unwrap());
    // part "pdf": the stored file
    let pdf_at = find(body, b"name=\"pdf\"", json_end).expect("pdf part");
    let pdf_start = find(body, b"\r\n\r\n", pdf_at).unwrap() + 4;
    let pdf_end = find(body, format!("\r\n--{boundary}--").as_bytes(), pdf_start).unwrap();
    let stored = std::fs::read(f.data_dir.join(row.pdf_path.as_deref().unwrap())).unwrap();
    assert_eq!(
        &body[pdf_start..pdf_end],
        &stored[..],
        "the uploaded PDF is the stored PDF"
    );
    f.stop();
}
