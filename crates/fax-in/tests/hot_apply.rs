//! K07 and K09 (docs/test-plans/M4.md section 1) at the controller level: a configuration
//! change reaches the calls that arrive afterwards, never the calls in progress; a change to a
//! trunk's registration de-registers the old one and registers with the new settings.

mod common;

use std::net::SocketAddr;
use std::time::Duration;

use common::*;
use fax_in::{CallEvent, FaxIn, FaxInConfig, FaxSessionKind, StartOptions};
use sip_core::transport::Decision;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

fn cfg(max_secs: u64, ani_sources: &str) -> FaxInConfig {
    let mut c = FaxInConfig::parse(&format!(
        "{}\n[call]\nmax_duration_secs = {max_secs}\n",
        base_config(&format!("[trunks.numbers]\nani_sources = {ani_sources}\n"))
    ))
    .unwrap();
    c.call.fax_session = FaxSessionKind::Hold;
    c
}

fn started_numbers(w: &mut W) -> Vec<(String, Option<String>, Option<String>)> {
    w.events()
        .into_iter()
        .filter_map(|e| match e {
            CallEvent::Started {
                call_id, numbers, ..
            } => Some((call_id, numbers.ani, numbers.ani_source)),
            _ => None,
        })
        .collect()
}

#[tokio::test(start_paused = true)]
async fn k07_a_change_applies_to_the_next_call_and_not_to_the_call_in_progress() {
    let mut w = W::from_config(cfg(30, "[\"pai\", \"from\"]")).await;
    // call A starts under the old configuration (ANI from P-Asserted-Identity, 30 s limit)
    w.feed(
        Req::invite("a@gw", T38_SDP)
            .hdr("P-Asserted-Identity", "<sip:0299990000@carrier.example>")
            .build(),
    )
    .await;
    let tag_a = w.our_tag();
    w.feed(Req::in_dialog("ACK", "a@gw", "ack1", 1, &tag_a).build())
        .await;
    w.at(10_000).await;
    // the administrator changes the trunk (ANI only from `From`) and the call limit (5 s)
    let out = w
        .fx
        .as_ref()
        .unwrap()
        .apply_config(cfg(5, "[\"from\"]"))
        .await
        .unwrap();
    assert_eq!(out.trunks_changed, ["gw"]);
    assert!(out.restart_required.is_empty(), "{:?}", out.restart_required);
    assert!(out.registrations_restarted.is_empty());
    // call B arrives after the change
    w.feed(
        Req::invite("b@gw", T38_SDP)
            .hdr("P-Asserted-Identity", "<sip:0288880000@carrier.example>")
            .build(),
    )
    .await;
    let tag_b = w
        .responses(200, "INVITE")
        .into_iter()
        .find(|(_, r)| r.headers.call_id().unwrap_or("") == "b@gw")
        .map(|(_, r)| r.headers.to().unwrap().tag().unwrap().to_string())
        .expect("B was answered");
    w.feed(Req::in_dialog("ACK", "b@gw", "ack1", 1, &tag_b).build())
        .await;
    let numbers = started_numbers(&mut w);
    let a = numbers.iter().find(|n| n.0 == "a@gw").unwrap();
    let b = numbers.iter().find(|n| n.0 == "b@gw").unwrap();
    assert_eq!(
        (a.1.as_deref(), a.2.as_deref()),
        (Some("0299990000"), Some("pai")),
        "A took its ANI from the old order"
    );
    assert_eq!(
        (b.1.as_deref(), b.2.as_deref()),
        (Some("gw"), Some("from")),
        "B uses the new order: P-Asserted-Identity is no longer consulted"
    );
    // B hangs up after its new 5 s limit, A is still held by the old 30 s (retransmissions of a
    // BYE that nobody answers are the same BYE: look at the first one of each call)
    let first_bye = |w: &W, call: &str| {
        w.requests("BYE")
            .into_iter()
            .find(|(_, r)| r.headers.call_id().unwrap_or("") == call)
            .map(|(t, _)| t)
    };
    w.at(14_999).await;
    assert_eq!(first_bye(&w, "b@gw"), None);
    w.at(15_000).await;
    assert_eq!(first_bye(&w, "b@gw"), Some(15_000), "B ends at 10 s + the new 5 s limit");
    w.at(29_999).await;
    assert_eq!(first_bye(&w, "a@gw"), None, "A is untouched by the change");
    w.at(30_000).await;
    assert_eq!(first_bye(&w, "a@gw"), Some(30_000), "A ended exactly at its own, old limit");
    w.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn k07_an_invalid_configuration_is_refused_and_the_old_one_stays() {
    let mut w = W::from_config(cfg(30, "[\"pai\"]")).await;
    let mut bad = cfg(30, "[\"pai\"]");
    bad.trunks.push(bad.trunks[0].clone()); // the same trunk name twice
    let err = w.fx.as_ref().unwrap().apply_config(bad).await.unwrap_err();
    assert!(err.to_string().contains("duplicate trunk name"), "{err}");
    assert_eq!(w.fx.as_ref().unwrap().snapshot().cfg.call.max_duration_secs, 30);
    assert_eq!(w.fx.as_ref().unwrap().snapshot().table.trunks().len(), 1);
    w.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn k08_settings_fixed_at_start_are_reported_as_pending_and_not_applied() {
    let mut w = W::from_config(cfg(30, "[\"pai\"]")).await;
    assert!(w.fx.as_ref().unwrap().pending_restart().is_empty());
    let mut c = cfg(30, "[\"pai\"]");
    c.sip.bind = "192.0.2.10:5070".parse().unwrap();
    c.media.port_range = "31000-31099".into();
    c.capture.enabled = false;
    c.call.max_491_retries = 9;
    let out = w.fx.as_ref().unwrap().apply_config(c).await.unwrap();
    assert_eq!(
        out.restart_required,
        ["sip.bind", "media.port_range", "call.max_491_retries", "capture"]
    );
    assert_eq!(w.fx.as_ref().unwrap().pending_restart(), out.restart_required);
    // calls keep being accepted on what is running
    w.feed(Req::invite("x@gw", T38_SDP).build()).await;
    assert_eq!(w.responses(200, "INVITE").len(), 1);
    w.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn k07_the_source_filter_follows_the_trunks() {
    let mut w = W::from_config(cfg(30, "[\"pai\"]")).await;
    let ip: std::net::IpAddr = "172.16.0.9".parse().unwrap();
    let t = w.fx.as_ref().unwrap().endpoint().transactions().transport().clone();
    // the detached transport starts with an allow-all policy; applying installs the trunks'
    w.fx.as_ref().unwrap().apply_config(cfg(30, "[\"pai\"]")).await.unwrap();
    assert_eq!(t.source_policy().decide("10.0.0.9".parse().unwrap()), Decision::Allow);
    assert_ne!(t.source_policy().decide(ip), Decision::Allow, "outside every trunk");
    // a second trunk opens another network
    let mut c = FaxInConfig::parse(&format!(
        "{}\n[[trunks]]\nname = \"new\"\nallowed_sources = [\"172.16.0.0/16\"]\n",
        base_config("")
    ))
    .unwrap();
    c.call.fax_session = FaxSessionKind::Hold;
    w.fx.as_ref().unwrap().apply_config(c).await.unwrap();
    assert_eq!(t.source_policy().decide(ip), Decision::Allow);
    w.shutdown().await;
}

// ---------------------------------------------------------------- K09 (real sockets)

async fn registrar() -> (SocketAddr, mpsc::UnboundedReceiver<String>, tokio::task::JoinHandle<()>) {
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = sock.local_addr().unwrap();
    let (tx, rx) = mpsc::unbounded_channel();
    let j = tokio::spawn(async move {
        let mut buf = vec![0u8; 4096];
        loop {
            let Ok((n, from)) = sock.recv_from(&mut buf).await else {
                return;
            };
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let h = |name: &str| {
                req.lines()
                    .find(|l| l.starts_with(name))
                    .unwrap_or("")
                    .to_string()
            };
            let ok = format!(
                "SIP/2.0 200 OK\r\n{}\r\n{}\r\n{};tag=reg1\r\n{}\r\n{}\r\nExpires: 600\r\nContent-Length: 0\r\n\r\n",
                h("Via:"),
                h("From:"),
                h("To:"),
                h("Call-ID:"),
                h("CSeq:")
            );
            let _ = tx.send(req);
            let _ = sock.send_to(ok.as_bytes(), from).await;
        }
    });
    (addr, rx, j)
}

fn free_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn reg_cfg(registrar: SocketAddr, port: u16, user: &str, extra: &str) -> FaxInConfig {
    let mut c = FaxInConfig::parse(&format!(
        "[sip]\nbind = \"127.0.0.1:{port}\"\n[capture]\nenabled = false\n\
         [[trunks]]\nname = \"reg\"\nallowed_sources = [\"127.0.0.1\"]\n\
         [trunks.register]\nenabled = true\nregistrar = \"{registrar}\"\ndomain = \"example.test\"\n\
         username = \"{user}\"\npassword = \"pw\"\nexpires = 600\n{extra}"
    ))
    .unwrap();
    c.call.fax_session = FaxSessionKind::Hold;
    c
}

async fn next(rx: &mut mpsc::UnboundedReceiver<String>) -> String {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("a REGISTER within 5 s")
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn k09_changing_the_registration_deregisters_the_old_one_and_registers_the_new() {
    let (raddr, mut rx, task) = registrar().await;
    let port = free_port();
    let (fx, _ev) = FaxIn::start(reg_cfg(raddr, port, "fax1", ""), StartOptions::default())
        .await
        .unwrap();
    let first = next(&mut rx).await;
    assert!(first.contains("\r\nTo: <sip:fax1@example.test>"), "{first}");
    assert!(first.contains("\r\nExpires: 600"));
    // the same configuration again: nothing happens on the wire, nothing restarts
    let out = fx
        .apply_config(reg_cfg(raddr, port, "fax1", ""))
        .await
        .unwrap();
    assert!(out.registrations_restarted.is_empty(), "{out:?}");
    assert!(
        tokio::time::timeout(Duration::from_millis(400), rx.recv()).await.is_err(),
        "no REGISTER for an unchanged registration"
    );
    // a new account: first the old binding is removed, then the new one is made
    let out = fx
        .apply_config(reg_cfg(raddr, port, "fax2", ""))
        .await
        .unwrap();
    assert_eq!(out.registrations_restarted, ["reg"]);
    let dereg = next(&mut rx).await;
    assert!(dereg.contains("\r\nTo: <sip:fax1@example.test>"), "{dereg}");
    assert!(dereg.contains("\r\nExpires: 0"), "un-REGISTER of the old one first: {dereg}");
    let again = next(&mut rx).await;
    assert!(again.contains("\r\nTo: <sip:fax2@example.test>"), "{again}");
    assert!(again.contains("\r\nExpires: 600"), "{again}");
    let mut registered = false;
    for _ in 0..50 {
        let st = fx.registrations();
        assert_eq!(st.len(), 1);
        if matches!(st[0].1, sip_core::registration::RegistrationState::Registered { .. }) {
            registered = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(registered, "{:?}", fx.registrations());
    // switching registration off de-registers and leaves nothing behind
    let mut off = reg_cfg(raddr, port, "fax2", "");
    off.trunks[0].register.enabled = false;
    let out = fx.apply_config(off).await.unwrap();
    assert_eq!(out.registrations_restarted, ["reg"]);
    let dereg = next(&mut rx).await;
    assert!(dereg.contains("\r\nTo: <sip:fax2@example.test>") && dereg.contains("\r\nExpires: 0"), "{dereg}");
    assert!(fx.registrations().is_empty());
    fx.shutdown().await;
    task.abort();
}
