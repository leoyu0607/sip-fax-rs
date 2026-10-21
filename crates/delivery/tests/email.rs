//! D03: e-mail delivery.
//!
//! Real SMTP servers in containers receive the mail and are asked for it through their own HTTP
//! API; the message is parsed with `mail-parser` (independent of lettre, which sends it) and the
//! attachment bytes are compared with the original file. Select the servers with environment
//! variables, otherwise those cases print a message and return (CI starts the containers):
//!
//! | variable | server |
//! |---|---|
//! | `SIP_FAXD_TEST_SMTP=host:port`, `SIP_FAXD_TEST_SMTP_API=http://host:port` | MailHog, plain SMTP |
//! | `SIP_FAXD_TEST_SMTP_STARTTLS=host:port`, `..._API=http://host:port` | smtp4dev, STARTTLS, self-signed, AUTH `faxuser`/`faxpass` required |
//! | `SIP_FAXD_TEST_SMTP_SMTPS=host:port`, `..._API=http://host:port` | smtp4dev, implicit TLS, self-signed |
//!
//! The cases that need no container (SMTP error classes, message construction) use a scripted SMTP
//! server on localhost and always run.

mod common;

use common::*;
use delivery::config::{EmailConfig, Security};
use delivery::{DeliveryEngine, FailureKind};
use mail_parser::{MessageParser, MimeHeaders};
use std::time::Duration;
use store::{DeliveryStatus, FaxStatus};

struct Server {
    host: String,
    port: u16,
    api: String,
}

fn server(var: &str) -> Option<Server> {
    let Ok(addr) = std::env::var(var) else {
        eprintln!("skip: {var} is not set (no SMTP container)");
        return None;
    };
    let api =
        std::env::var(format!("{var}_API")).unwrap_or_else(|_| panic!("{var}_API is required"));
    let (host, port) = addr.rsplit_once(':').expect("host:port");
    Some(Server {
        host: host.into(),
        port: port.parse().unwrap(),
        api: api.trim_end_matches('/').into(),
    })
}

fn token() -> String {
    format!("tok{:016x}", rand::random::<u64>())
}

/// Fetch the raw RFC 5322 text of the message whose subject contains `needle`, polling up to 20 s.
async fn fetch_raw(srv: &Server, mailhog: bool, needle: &str) -> Vec<u8> {
    let c = reqwest::Client::new();
    for _ in 0..200 {
        if mailhog {
            let v: serde_json::Value = c
                .get(format!("{}/api/v2/messages?limit=200", srv.api))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            for m in v["items"].as_array().unwrap() {
                let raw = m["Raw"]["Data"].as_str().unwrap();
                if raw.contains(needle) {
                    return raw.as_bytes().to_vec();
                }
            }
        } else {
            let v: serde_json::Value = c
                .get(format!("{}/api/Messages?pageSize=200", srv.api))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            for m in v["results"].as_array().unwrap() {
                if m["subject"].as_str().unwrap_or("").contains(needle) {
                    let id = m["id"].as_str().unwrap();
                    return c
                        .get(format!("{}/api/Messages/{id}/raw", srv.api))
                        .send()
                        .await
                        .unwrap()
                        .bytes()
                        .await
                        .unwrap()
                        .to_vec();
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("message with {needle:?} did not arrive");
}

fn email_routes(host: &str, port: u16, security: &str, extra: &str) -> delivery::Routes {
    routes(&format!(
        r#"
[[routes]]
default = true
[[routes.deliveries]]
kind = "email"
host = "{host}"
port = {port}
security = "{security}"
from = "Fax Server <fax@example.com>"
to = ["alice@example.com", "Bob <bob@example.org>"]
{extra}
"#
    ))
}

/// Parse and assert everything D03 requires about the delivered message.
fn check_message(raw: &[u8], tok: &str, id: i64, attach_tiff: bool) {
    let msg = MessageParser::default()
        .parse(raw)
        .expect("parsable message");
    let from = msg.from().unwrap().first().unwrap();
    assert_eq!(from.address(), Some("fax@example.com"));
    assert_eq!(from.name(), Some("Fax Server"));
    let to: Vec<_> = msg
        .to()
        .unwrap()
        .iter()
        .filter_map(|a| a.address())
        .collect();
    assert_eq!(to, vec!["alice@example.com", "bob@example.org"]);
    assert_eq!(
        msg.subject(),
        Some(format!("{tok} fax from 0227001234 (2 pages)").as_str())
    );
    let body = msg.body_text(0).expect("text body").to_string();
    assert!(body.contains("From: 0227001234"), "{body}");
    assert!(body.contains("To: 025570001"), "{body}");
    assert!(body.contains("Pages: 2"), "{body}");
    assert!(body.contains("Trunk: audiocodes"), "{body}");
    assert!(body.contains(&format!("ID: {id}")), "{body}");
    assert!(body.contains("Time: 2026-10-21T00:00:00+00:00"), "{body}");

    let atts: Vec<_> = msg.attachments().collect();
    assert_eq!(atts.len(), 1 + attach_tiff as usize);
    let pdf = atts
        .iter()
        .find(|a| a.attachment_name() == Some(&format!("fax-{id}.pdf")))
        .expect("pdf attachment");
    assert_eq!(pdf.content_type().map(|c| c.ctype()), Some("application"));
    assert_eq!(pdf.content_type().and_then(|c| c.subtype()), Some("pdf"));
    assert_eq!(
        pdf.contents(),
        pdf_bytes(id).as_slice(),
        "PDF bytes identical to the original"
    );
    if attach_tiff {
        let tif = atts
            .iter()
            .find(|a| a.attachment_name() == Some(&format!("fax-{id}.tif")))
            .expect("tiff attachment");
        assert_eq!(tif.contents(), tiff_bytes(id).as_slice());
    }
}

async fn run_one(env: &mut Env, r: &delivery::Routes) -> (DeliveryEngine, i64) {
    let eng = env.engine(r);
    let f = env
        .fax("audiocodes", "025570001", FaxStatus::Received)
        .await;
    eng.dispatch(f).await.unwrap();
    (eng, f)
}

#[tokio::test]
async fn d03_mailhog_plain_message_fields_and_attachment_bytes() {
    let Some(srv) = server("SIP_FAXD_TEST_SMTP") else {
        return;
    };
    let tok = token();
    let mut env = Env::new().await;
    let r = email_routes(
        &srv.host,
        srv.port,
        "none",
        &format!("subject = \"{tok} fax from {{ani}} ({{pages}} pages)\""),
    );
    let (eng, f) = run_one(&mut env, &r).await;
    let rep = eng.run_due().await.unwrap();
    assert_eq!(
        (rep.attempted, rep.done),
        (1, 1),
        "{:?}",
        env.attempts(f).await
    );
    let raw = fetch_raw(&srv, true, &tok).await;
    check_message(&raw, &tok, f, false);
    // MailHog also records the SMTP envelope: the recipients that were really used.
    let c = reqwest::Client::new();
    let v: serde_json::Value = c
        .get(format!("{}/api/v2/messages?limit=200", srv.api))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let m = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["Raw"]["Data"].as_str().unwrap().contains(&tok))
        .unwrap();
    assert_eq!(m["Raw"]["From"], "fax@example.com");
    let mut to: Vec<String> = m["Raw"]["To"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap().to_string())
        .collect();
    to.sort();
    assert_eq!(to, vec!["alice@example.com", "bob@example.org"]);
}

#[tokio::test]
async fn d03_mailhog_tiff_attachment_and_custom_names() {
    let Some(srv) = server("SIP_FAXD_TEST_SMTP") else {
        return;
    };
    let tok = token();
    let mut env = Env::new().await;
    let r = email_routes(
        &srv.host,
        srv.port,
        "none",
        &format!(
            "subject = \"{tok} fax from {{ani}} ({{pages}} pages)\"\nattach_tiff = true\nattachment_name = \"fax-{{id}}.pdf\""
        ),
    );
    let (eng, f) = run_one(&mut env, &r).await;
    assert_eq!(eng.run_due().await.unwrap().done, 1);
    check_message(&fetch_raw(&srv, true, &tok).await, &tok, f, true);
}

#[tokio::test]
async fn d03_starttls_with_auth_and_attachment_bytes() {
    let Some(srv) = server("SIP_FAXD_TEST_SMTP_STARTTLS") else {
        return;
    };
    let tok = token();
    let mut env = Env::new().await;
    let r = email_routes(
        &srv.host,
        srv.port,
        "starttls",
        &format!(
            "tls_insecure = true\nusername = \"faxuser\"\npassword = \"faxpass\"\nsubject = \"{tok} fax from {{ani}} ({{pages}} pages)\""
        ),
    );
    let (eng, f) = run_one(&mut env, &r).await;
    let rep = eng.run_due().await.unwrap();
    assert_eq!(
        (rep.attempted, rep.done),
        (1, 1),
        "{:?}",
        env.attempts(f).await
    );
    check_message(&fetch_raw(&srv, false, &tok).await, &tok, f, false);
}

#[tokio::test]
async fn d03_starttls_wrong_password_is_a_permanent_failure() {
    let Some(srv) = server("SIP_FAXD_TEST_SMTP_STARTTLS") else {
        return;
    };
    let mut env = Env::new().await;
    let r = email_routes(
        &srv.host,
        srv.port,
        "starttls",
        "tls_insecure = true\nusername = \"faxuser\"\npassword = \"WRONG\"",
    );
    let (eng, f) = run_one(&mut env, &r).await;
    let rep = eng.run_due().await.unwrap();
    assert_eq!(
        (rep.failed, rep.retry_scheduled),
        (1, 0),
        "{:?}",
        env.attempts(f).await
    );
    let row = &env.attempts(f).await[0];
    assert_eq!(row.status, DeliveryStatus::Failed);
    assert!(
        row.last_error.as_deref().unwrap().starts_with("smtp:"),
        "{row:?}"
    );
}

#[tokio::test]
async fn d03_starttls_untrusted_certificate_is_refused_unless_insecure() {
    let Some(srv) = server("SIP_FAXD_TEST_SMTP_STARTTLS") else {
        return;
    };
    let mut env = Env::new().await;
    let r = email_routes(
        &srv.host,
        srv.port,
        "starttls",
        "username = \"faxuser\"\npassword = \"faxpass\"",
    );
    let (eng, f) = run_one(&mut env, &r).await;
    let rep = eng.run_due().await.unwrap();
    // The self-signed certificate must not be accepted by default; the failure is retried (an
    // operator may install the CA) and no mail is sent.
    assert_eq!(
        (rep.done, rep.retry_scheduled),
        (0, 1),
        "{:?}",
        env.attempts(f).await
    );
    let err = env.attempts(f).await[0].last_error.clone().unwrap();
    assert!(err.contains("certificate"), "{err}");
}

#[tokio::test]
async fn d03_smtps_implicit_tls() {
    let Some(srv) = server("SIP_FAXD_TEST_SMTP_SMTPS") else {
        return;
    };
    let tok = token();
    let mut env = Env::new().await;
    let r = email_routes(
        &srv.host,
        srv.port,
        "smtps",
        &format!("tls_insecure = true\nsubject = \"{tok} fax from {{ani}} ({{pages}} pages)\""),
    );
    let (eng, f) = run_one(&mut env, &r).await;
    let rep = eng.run_due().await.unwrap();
    assert_eq!(
        (rep.attempted, rep.done),
        (1, 1),
        "{:?}",
        env.attempts(f).await
    );
    check_message(&fetch_raw(&srv, false, &tok).await, &tok, f, false);
}

// ------------------------------------------------------------- no container needed

fn cfg(port: u16) -> EmailConfig {
    let r = email_routes("127.0.0.1", port, "none", "");
    match r.routes[0].deliveries[0].clone() {
        delivery::DeliveryConfig::Email(e) => e,
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn d03_scripted_server_receives_exactly_the_built_message() {
    let smtp = FakeSmtp::start(SmtpScript::default()).await;
    let mut env = Env::new().await;
    let r = email_routes("127.0.0.1", smtp.port, "none", "attach_tiff = true");
    let (eng, f) = run_one(&mut env, &r).await;
    assert_eq!(eng.run_due().await.unwrap().done, 1);
    let cmds = smtp.commands.lock().unwrap().clone();
    assert!(
        cmds.iter().any(|c| c == "MAIL FROM:<fax@example.com>"),
        "{cmds:?}"
    );
    assert!(
        cmds.iter().any(|c| c == "RCPT TO:<alice@example.com>"),
        "{cmds:?}"
    );
    assert!(
        cmds.iter().any(|c| c == "RCPT TO:<bob@example.org>"),
        "{cmds:?}"
    );
    let msgs = smtp.messages.lock().unwrap().clone();
    assert_eq!(msgs.len(), 1);
    // Dot-stuffing and CRLF handling must not corrupt the binary attachment (the PDF contains a
    // line consisting of a single dot).
    let msg = MessageParser::default().parse(&msgs[0]).unwrap();
    let atts: Vec<_> = msg.attachments().collect();
    assert_eq!(atts.len(), 2);
    assert_eq!(atts[0].contents(), pdf_bytes(f).as_slice());
    assert_eq!(atts[1].contents(), tiff_bytes(f).as_slice());
    assert_eq!(msg.subject(), Some("Fax from 0227001234 (2 pages)"));
}

#[tokio::test]
async fn d03_smtp_error_classes() {
    // (RCPT reply, DATA-end reply, transient?)
    let cases: &[(&str, &str, bool)] = &[
        ("450 mailbox busy", "250 ok", true),
        ("451 local error", "250 ok", true),
        ("550 no such user", "250 ok", false),
        ("553 bad address", "250 ok", false),
        ("250 ok", "452 insufficient storage", true),
        ("250 ok", "554 rejected as spam", false),
    ];
    for (rcpt, data_end, transient) in cases {
        let smtp = FakeSmtp::start(SmtpScript {
            rcpt: Box::leak(rcpt.to_string().into_boxed_str()),
            data_end: Box::leak(data_end.to_string().into_boxed_str()),
            ..Default::default()
        })
        .await;
        let mut env = Env::new().await;
        let (eng, f) = run_one(&mut env, &email_routes("127.0.0.1", smtp.port, "none", "")).await;
        let rep = eng.run_due().await.unwrap();
        let rows = env.attempts(f).await;
        if *transient {
            assert_eq!(
                (rep.retry_scheduled, rep.failed),
                (1, 0),
                "{rcpt} / {data_end}: {rows:?}"
            );
            assert_eq!(rows[1].status, DeliveryStatus::Pending);
        } else {
            assert_eq!(
                (rep.retry_scheduled, rep.failed),
                (0, 1),
                "{rcpt} / {data_end}: {rows:?}"
            );
            assert_eq!(rows.len(), 1);
        }
        assert!(
            rows[0].last_error.as_deref().unwrap().starts_with("smtp:"),
            "{rows:?}"
        );
    }
}

#[tokio::test]
async fn d03_connection_refused_is_transient() {
    let port = closed_port().await;
    let mut env = Env::new().await;
    let (eng, f) = run_one(&mut env, &email_routes("127.0.0.1", port, "none", "")).await;
    let rep = eng.run_due().await.unwrap();
    assert_eq!((rep.retry_scheduled, rep.failed), (1, 0));
    assert_eq!(env.attempts(f).await[1].attempt, 2);
}

#[tokio::test]
async fn d03_missing_pdf_is_permanent_and_nothing_is_sent() {
    let smtp = FakeSmtp::start(SmtpScript::default()).await;
    let mut env = Env::new().await;
    let (eng, f) = run_one(&mut env, &email_routes("127.0.0.1", smtp.port, "none", "")).await;
    std::fs::remove_file(env.data.join(format!("faxes/{f}.pdf"))).unwrap();
    let rep = eng.run_due().await.unwrap();
    assert_eq!((rep.failed, rep.retry_scheduled), (1, 0));
    assert!(
        smtp.commands.lock().unwrap().is_empty(),
        "no SMTP session was opened"
    );
}

#[tokio::test]
async fn d03_build_message_rejects_bad_input_and_neutralises_header_injection() {
    let mut env = Env::new().await;
    let f = env
        .fax(
            "trunk\r\nBcc: evil@example.com",
            "025570001",
            FaxStatus::Received,
        )
        .await;
    let rec = env.record(f).await;
    let off = chrono::FixedOffset::east_opt(0).unwrap();
    let mut c = cfg(25);
    c.subject = "Fax via {trunk}".into();
    let m = delivery::email::build_message(&c, &rec, &env.data, off)
        .await
        .unwrap();
    let wire = String::from_utf8_lossy(&m.formatted()).into_owned();
    assert!(!wire.contains("\r\nBcc:"), "{wire}");
    assert!(wire.contains("Subject: Fax via trunk"), "{wire}");
    // Bad addresses and templates are permanent errors, as is a path escaping data_dir.
    let mut bad = cfg(25);
    bad.from = "no at sign".into();
    let e = delivery::email::build_message(&bad, &rec, &env.data, off)
        .await
        .unwrap_err();
    assert_eq!(e.kind, FailureKind::Permanent);
    let mut bad = cfg(25);
    bad.to = vec!["@".into()];
    assert_eq!(
        delivery::email::build_message(&bad, &rec, &env.data, off)
            .await
            .unwrap_err()
            .kind,
        FailureKind::Permanent
    );
    let mut bad = cfg(25);
    bad.subject = "{nope}".into();
    assert_eq!(
        delivery::email::build_message(&bad, &rec, &env.data, off)
            .await
            .unwrap_err()
            .kind,
        FailureKind::Permanent
    );
    let mut bad = cfg(25);
    bad.attachment_name = Some("{nope}".into());
    assert_eq!(
        delivery::email::build_message(&bad, &rec, &env.data, off)
            .await
            .unwrap_err()
            .kind,
        FailureKind::Permanent
    );
    // The default is STARTTLS on 587; `Security` is exported for the daemon config.
    let mut e = cfg(25);
    e.port = None;
    e.security = Security::Starttls;
    assert_eq!(e.effective_port(), 587);
}
