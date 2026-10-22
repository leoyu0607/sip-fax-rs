//! M4f end-to-end case F02: a new installation (no trunk anywhere) refuses a call, a trunk is
//! added through the web API, and the same process then receives a real SpanDSP fax without a
//! restart.
//!
//! The configuration file of the harness still has `[sip]`, `[media]`, `[call]` and `[capture]`
//! (the SIP port and the capture directory have to be known to the test); what makes it a new
//! installation is that no `[[trunks]]` exist in the file or in the database.

mod e2e_common;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use e2e_common::*;
use fax_sim::sip_uac::{ReinviteBy, run_call};
use interop::{Faxd, FaxdOptions, faxd_command};
use store::{ConnectOptions, FaxRecord, FaxStatus, FaxStore, ListFilter, PageRequest};

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

/// One HTTP/1.1 request on a new connection: status, headers (lower-cased names) and body.
fn http(
    port: u16,
    method: &str,
    path: &str,
    headers: &[(&str, String)],
    body: Option<&serde_json::Value>,
) -> (u16, Vec<(String, String)>, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).expect("connect to the web interface");
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    if !body.is_empty() {
        req.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            body.len()
        ));
    }
    req.push_str("\r\n");
    req.push_str(&body);
    s.write_all(req.as_bytes()).unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").expect("an HTTP response");
    let mut lines = head.lines();
    let status: u16 = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .expect("a status line");
    let hs = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    // (Connection: close and no Content-Length / chunking handling needed for small JSON bodies:
    // axum answers with Content-Length, so the body is what follows the head)
    (status, hs, body.to_string())
}

fn json(body: &str) -> serde_json::Value {
    serde_json::from_str(body).unwrap_or_else(|e| panic!("JSON expected: {e}: {body}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn f02_a_new_installation_refuses_calls_then_a_trunk_added_in_the_web_receives_a_real_fax_without_a_restart()
 {
    let tag = "m4f-f02";
    let web_port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let opts = FaxdOptions {
        // a call from a source that belongs to no trunk is answered 403 (the default drops it
        // silently, which a gateway can only see as a timeout)
        sip: "unknown_source = \"reject\"\n".into(),
        call: "station_id = \"0287654321\"\n".into(),
        extra: format!("\n[media]\nport_range = \"{}\"\n", media_range(2)),
        real_fax: true,
        no_trunk: true,
        web_port: Some(web_port),
        ..FaxdOptions::default()
    };
    let a = Server(Some(Faxd::start(tag, &opts)));
    let log = a.log().join("\n");
    assert!(
        log.contains("no trunk is configured: every call is refused"),
        "the start says so:\n{log}"
    );
    assert!(log.contains("Settings -> Trunks"), "{log}");
    let db = open(&a.db_url).await;
    let dir = scratch(tag);
    let pages = specs(2, true, 300);
    let src = source_tiff(&dir, "one.tif", &pages);

    // 1. the gateway calls: refused with 403, nothing is stored
    let err = run_call(uac(&a, ReinviteBy::Gateway, &src, "0000000001"))
        .await
        .expect_err("a call without any trunk must be refused");
    assert!(err.contains("403"), "refused with 403 Forbidden: {err}");
    let list = db
        .list(&ListFilter::default(), PageRequest::default())
        .await
        .unwrap();
    assert!(list.items.is_empty(), "no fax row for a refused call");

    // 2. an administrator (made with the CLI) adds the trunk through the web API
    let mut child = faxd_command(
        &["user", "add", "root", "--role", "admin", "--password-stdin"],
        &a.config_path,
    )
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"root password 12\n")
        .unwrap();
    let o = child.wait_with_output().unwrap();
    assert!(
        o.status.success(),
        "user add: {}",
        String::from_utf8_lossy(&o.stderr)
    );

    let (st, hs, _) = http(
        web_port,
        "POST",
        "/api/v1/auth/login",
        &[],
        Some(&serde_json::json!({"username": "root", "password": "root password 12"})),
    );
    assert_eq!(st, 200);
    let cookie = hs
        .iter()
        .filter(|(k, v)| k == "set-cookie" && v.starts_with("sipfax_session="))
        .map(|(_, v)| v.split(';').next().unwrap().to_string())
        .next()
        .expect("a session cookie");
    let auth = [("Cookie", cookie.clone())];
    let (st, _, body) = http(web_port, "GET", "/api/v1/auth/me", &auth, None);
    assert_eq!(st, 200, "{body}");
    let csrf = json(&body)["csrf"].as_str().unwrap().to_string();
    let (st, _, body) = http(web_port, "GET", "/api/v1/status", &auth, None);
    assert_eq!(st, 200, "{body}");
    assert_eq!(
        json(&body)["service"]["trunks"].as_array().unwrap().len(),
        0,
        "{body}"
    );

    let put = [auth[0].clone(), ("X-Sipfax-Csrf", csrf)];
    let (st, _, body) = http(
        web_port,
        "PUT",
        "/api/v1/config/trunk%3Agw",
        &put,
        Some(&serde_json::json!({
            "version": 0,
            "body": {"name": "gw", "preset": "generic", "peer": "127.0.0.1:5090",
                     "allowed_sources": ["127.0.0.0/8"]}
        })),
    );
    assert_eq!(st, 200, "adding the trunk: {body}");
    let (_, _, body) = http(web_port, "GET", "/api/v1/status", &auth, None);
    assert_eq!(
        json(&body)["service"]["trunks"].as_array().unwrap().len(),
        1,
        "{body}"
    );

    // 3. the same process receives a real fax from the same gateway
    let rep = run_call(uac(&a, ReinviteBy::Gateway, &src, "0000000002"))
        .await
        .unwrap_or_else(|e| panic!("{tag}: {e}\n{}", a.log().join("\n")));
    a.wait_results(1);
    let res = result_json(&a, &rep.call_id);
    let fax = assert_success(tag, &rep, &res, 2, "0000000002", "peer_reinvite");
    verify_files(tag, &fax, &src, &pages);
    let row = final_row(&db, &rep.call_id).await;
    assert_eq!((row.fax_status, row.pages), (FaxStatus::Received, 2));
    assert_eq!(
        row.trunk, "gw",
        "received through the trunk added in the web interface"
    );
    let list = db
        .list(&ListFilter::default(), PageRequest::default())
        .await
        .unwrap();
    assert_eq!(list.items.len(), 1, "only the second call left a record");
    let (st, _, body) = http(web_port, "GET", "/api/v1/faxes", &auth, None);
    assert_eq!(st, 200, "{body}");
    assert_eq!(json(&body)["items"].as_array().unwrap().len(), 1, "{body}");

    // no restart: the log has exactly one start
    db.close().await;
    let log = a.stop().join("\n");
    assert_eq!(
        log.matches("sip-faxd ready").count(),
        1,
        "one start only:\n{log}"
    );
    assert!(!log.contains("ERROR"), "{log}");
}
