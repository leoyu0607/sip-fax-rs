//! SIPp interop (S01-S11 and the SIPp variants of C03/C04/C06/C07): the shipped `sip-faxd
//! serve` against SIPp 3.8.0, an independent SIP implementation. Scenarios live in
//! `tools/sipp/`; each one checks the headers/SDP we send with `<ereg check_it="true">`, and
//! the Rust side asserts the exit code, the `Successful call` counters, timing taken from
//! SIPp's message log, and what sip-faxd reported for the call.

use std::time::Duration;

use interop::{
    Faxd, FaxdOptions, LoggedMessage, SippOptions, SippRun, digest_response, free_port, run_sipp,
    spawn_sipp,
};

fn faxd_opts(trunk: &str) -> FaxdOptions {
    FaxdOptions {
        trunk: trunk.into(),
        ..FaxdOptions::default()
    }
}

fn sipp_for(name: &str, scenario: &str, faxd: &Faxd) -> SippOptions {
    let mut o = SippOptions::new(name, scenario, &faxd.addr());
    o.args = vec!["-s".into(), "fax".into()];
    o
}

fn cseq(m: &LoggedMessage) -> u32 {
    m.text
        .lines()
        .find_map(|l| l.strip_prefix("CSeq: "))
        .and_then(|v| v.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no CSeq in {}", m.text))
}

fn header<'a>(m: &'a LoggedMessage, name: &str) -> Option<&'a str> {
    m.text
        .lines()
        .find_map(|l| l.strip_prefix(&format!("{name}: ")))
        .map(str::trim)
}

fn count_log(faxd: &Faxd, needle: &str) -> usize {
    faxd.log().iter().filter(|l| l.contains(needle)).count()
}

fn assert_close(what: &str, got: f64, want: f64, tol: f64) {
    assert!(
        (got - want).abs() <= tol,
        "{what}: {got:.3} s, expected {want:.3} s +- {tol:.3} s"
    );
}

// ======================================================================== S01 / S09

#[test]
fn s01_basic_call_server_sends_the_t38_reinvite() {
    let faxd = Faxd::start("s01", &faxd_opts("initiate_after_secs = 0.3\n"));
    let run = run_sipp(&sipp_for(
        "s01-sipp",
        "s01_basic_server_reinvite.xml",
        &faxd,
    ));
    run.assert_successful(1);
    let res = faxd.wait_results(1);
    assert_eq!(
        res[0]["outcome"]["status"],
        "completed",
        "{}",
        faxd.log().join("\n")
    );
    assert_eq!(res[0]["t38"]["path"], "our_reinvite");
    // the re-INVITE came ~0.3 s after the ACK (initiate_after_secs)
    let ack = run.messages_starting("ACK", false)[0].at;
    let inv = run.messages_starting("INVITE", true)[0].at;
    assert_close("re-INVITE delay after ACK", inv - ack, 0.3, 0.15);
    faxd.stop();
}

#[test]
fn s09_the_same_call_over_tcp() {
    let faxd = Faxd::start("s09", &faxd_opts("initiate_after_secs = 0.3\n"));
    let mut o = sipp_for("s09-sipp", "s01_basic_server_reinvite.xml", &faxd);
    o.transport = "t1".into();
    let run = run_sipp(&o);
    run.assert_successful(1);
    let first = &run.messages_starting("INVITE", false)[0].text;
    assert!(first.contains("Via: SIP/2.0/TCP "), "{first}");
    let res = faxd.wait_results(1);
    assert_eq!(res[0]["outcome"]["status"], "completed");
    assert_eq!(res[0]["t38"]["path"], "our_reinvite");
    faxd.stop();
}

// ======================================================================== S02

#[test]
fn s02_gateway_sends_the_t38_reinvite_and_we_answer_with_t38() {
    let faxd = Faxd::start("s02", &faxd_opts(""));
    let run = run_sipp(&sipp_for(
        "s02-sipp",
        "s02_gateway_initiated_t38.xml",
        &faxd,
    ));
    run.assert_successful(1);
    assert!(
        run.messages_starting("INVITE", true).is_empty(),
        "sip-faxd must not send a re-INVITE of its own when the gateway already did"
    );
    let res = faxd.wait_results(1);
    assert_eq!(res[0]["outcome"]["status"], "completed");
    assert_eq!(res[0]["t38"]["path"], "peer_reinvite");
    assert_eq!(res[0]["t38"]["version"], 0);
    assert_eq!(res[0]["t38"]["max_bit_rate"], 14400);
    faxd.stop();
}

// ======================================================================== S03

#[test]
fn s03_unacked_200_is_retransmitted_on_the_g02_schedule_then_bye() {
    let faxd = Faxd::start("s03", &faxd_opts(""));
    let mut o = sipp_for("s03-sipp", "s03_no_ack_200_retransmissions.xml", &faxd);
    o.timeout = Duration::from_secs(80);
    let run = run_sipp(&o);
    run.assert_successful(1);
    let ok: Vec<f64> = run
        .messages
        .iter()
        .filter(|m| m.received && m.first_line() == "SIP/2.0 200 OK" && cseq(m) == 1)
        .map(|m| m.at)
        .collect();
    let rel: Vec<f64> = ok.iter().map(|t| t - ok[0]).collect();
    println!("S03 200 OK instants seen by SIPp (s after the first): {rel:.3?}");
    let want = [0.0, 0.5, 1.5, 3.5, 7.5, 11.5, 15.5, 19.5, 23.5, 27.5, 31.5];
    assert_eq!(
        rel.len(),
        want.len(),
        "number of 200s seen by SIPp: {rel:?}"
    );
    for (i, (g, w)) in rel.iter().zip(want).enumerate() {
        assert_close(&format!("200 #{i}"), *g, w, 0.12);
    }
    let bye = run.messages_starting("BYE", true);
    assert_eq!(bye.len(), 1);
    assert_close("BYE after the first 200", bye[0].at - ok[0], 32.0, 0.2);
    let res = faxd.wait_results(1);
    assert_eq!(res[0]["outcome"]["status"], "failed");
    assert_eq!(res[0]["outcome"]["reason"]["kind"], "ack_timeout");
    faxd.stop();
}

// ======================================================================== S04

#[test]
fn s04_retransmitted_invite_gets_the_same_answer_and_no_second_dialog() {
    let mut fo = faxd_opts("t38_policy = \"wait-peer\"\n");
    fo.call = "answer_delay_ms = 1500\n".into();
    let faxd = Faxd::start("s04", &fo);
    let mut o = sipp_for("s04-sipp", "s04_invite_retransmission.xml", &faxd);
    // -nr: SIPp's own automatic retransmission handling would answer a retransmitted 180 with
    // yet another INVITE (a ping-pong); the scenario sends the retransmission explicitly.
    o.args.push("-nr".into());
    let run = run_sipp(&o);
    run.assert_successful(1);
    let sent = run.messages_starting("INVITE", false);
    assert_eq!(sent.len(), 2);
    assert_eq!(
        sent[0].text, sent[1].text,
        "retransmission is byte-identical"
    );
    let r180 = run.messages_starting("SIP/2.0 180", true);
    assert_eq!(
        r180.len(),
        2,
        "180 once, then retransmitted for the retransmitted INVITE"
    );
    assert_eq!(r180[0].text, r180[1].text);
    let r200: Vec<_> = run
        .messages
        .iter()
        .filter(|m| m.received && m.first_line() == "SIP/2.0 200 OK" && cseq(m) == 1)
        .collect();
    assert_eq!(r200.len(), 1, "exactly one 200 for the INVITE");
    let tag = |m: &LoggedMessage| {
        header(m, "To")
            .and_then(|t| t.split("tag=").nth(1))
            .map(str::to_string)
    };
    assert_eq!(
        tag(r200[0]),
        tag(r180[0]),
        "one dialog: same To tag everywhere"
    );
    assert!(tag(r200[0]).is_some());
    faxd.wait_results(1);
    assert_eq!(count_log(&faxd, "inbound call"), 1, "one call, not two");
    assert_eq!(count_log(&faxd, "T.38 established"), 0);
    faxd.stop();
}

// ======================================================================== S05

#[test]
fn s05_our_requests_follow_the_record_route_set() {
    let mut fo = faxd_opts("initiate_after_secs = 0.3\n");
    fo.call = "max_duration_secs = 2\n".into();
    let faxd = Faxd::start("s05", &fo);
    let run = run_sipp(&sipp_for("s05-sipp", "s05_record_route.xml", &faxd));
    run.assert_successful(1);
    let reinvite = &run.messages_starting("INVITE", true)[0].text;
    let routes: Vec<&str> = reinvite
        .lines()
        .filter(|l| l.starts_with("Route:"))
        .collect();
    println!(
        "S05 re-INVITE request line: {}",
        reinvite.lines().next().unwrap()
    );
    println!("S05 re-INVITE routes: {routes:?}");
    let res = faxd.wait_results(1);
    assert_eq!(res[0]["t38"]["path"], "our_reinvite");
    assert_eq!(res[0]["outcome"]["reason"]["kind"], "max_duration");
    faxd.stop();
}

// ======================================================================== S06

#[test]
fn s06_reinvite_491_is_retried_within_two_seconds_with_a_higher_cseq() {
    let faxd = Faxd::start("s06", &faxd_opts("initiate_after_secs = 0.3\n"));
    let run = run_sipp(&sipp_for("s06-sipp", "s06_reinvite_491_retry.xml", &faxd));
    run.assert_successful(1);
    let invites = run.messages_starting("INVITE", true);
    assert_eq!(invites.len(), 2, "original and one retry");
    let r491 = run
        .messages
        .iter()
        .find(|m| !m.received && m.first_line().starts_with("SIP/2.0 491"))
        .expect("491 sent");
    let delay = invites[1].at - r491.at;
    println!("S06 retry delay after the 491: {delay:.3} s");
    assert!(
        (0.0..=2.1).contains(&delay),
        "retry after {delay:.3} s, expected within 0-2 s"
    );
    assert_eq!(cseq(invites[1]), cseq(invites[0]) + 1);
    let acks = run.messages_starting("ACK", true);
    assert_eq!(
        acks.len(),
        2,
        "ACK of the 491 (transaction layer) and ACK of the 200"
    );
    assert_eq!(
        cseq(acks[0]),
        cseq(invites[0]),
        "the 491 is ACKed with the INVITE's CSeq"
    );
    let res = faxd.wait_results(1);
    assert_eq!(res[0]["t38"]["path"], "our_reinvite");
    faxd.stop();
}

// ======================================================================== S07 / S07b

#[test]
fn s07_peer_488_to_our_t38_reinvite_is_acked_then_we_hang_up() {
    let faxd = Faxd::start("s07", &faxd_opts("initiate_after_secs = 0.3\n"));
    let run = run_sipp(&sipp_for("s07-sipp", "s07_reinvite_488.xml", &faxd));
    run.assert_successful(1);
    let acks = run.messages_starting("ACK", true);
    assert_eq!(acks.len(), 1, "ACK for the 488");
    let invite = &run.messages_starting("INVITE", true)[0];
    assert_eq!(cseq(acks[0]), cseq(invite));
    let bye = &run.messages_starting("BYE", true)[0];
    assert_eq!(cseq(bye), cseq(invite) + 1);
    let res = faxd.wait_results(1);
    assert_eq!(res[0]["outcome"]["status"], "failed");
    assert_eq!(
        res[0]["outcome"]["reason"]["kind"],
        "g711_passthrough_unsupported"
    );
    assert!(
        res[0]["outcome"]["reason"]["cause"]
            .as_str()
            .unwrap()
            .contains("488")
    );
    assert!(
        faxd.log()
            .iter()
            .any(|l| l.contains("G.711 passthrough not yet supported")),
        "logged"
    );
    faxd.stop();
}

#[test]
fn s07b_audio_only_refuses_t38_with_488_and_the_dialog_survives() {
    let faxd = Faxd::start("s07b", &faxd_opts("t38_policy = \"audio-only\"\n"));
    let run = run_sipp(&sipp_for(
        "s07b-sipp",
        "s07b_dialog_survives_488.xml",
        &faxd,
    ));
    run.assert_successful(1);
    let r488 = run
        .messages
        .iter()
        .filter(|m| m.received && m.first_line().starts_with("SIP/2.0 488"))
        .count();
    assert_eq!(r488, 1);
    let bye_ok = run
        .messages
        .iter()
        .filter(|m| m.received && m.first_line() == "SIP/2.0 200 OK" && cseq(m) == 3)
        .count();
    assert_eq!(
        bye_ok, 1,
        "SIPp's BYE got 200 although the re-INVITE was refused"
    );
    let res = faxd.wait_results(1);
    assert_eq!(
        res[0]["outcome"]["reason"]["kind"],
        "g711_passthrough_unsupported"
    );
    faxd.stop();
}

// ======================================================================== S08

#[test]
fn s08_cancel_after_180_gets_200_and_487() {
    let mut fo = faxd_opts("");
    fo.call = "answer_delay_ms = 3000\n".into();
    let faxd = Faxd::start("s08", &fo);
    let run = run_sipp(&sipp_for("s08-sipp", "s08_cancel.xml", &faxd));
    run.assert_successful(1);
    let order: Vec<String> = run
        .messages
        .iter()
        .map(|m| {
            format!(
                "{} {}",
                if m.received { "<-" } else { "->" },
                m.first_line()
            )
        })
        .collect();
    println!("S08 sequence: {order:?}");
    assert!(
        order
            .iter()
            .any(|l| l == "<- SIP/2.0 487 Request Terminated"),
        "{order:?}"
    );
    assert!(
        order.iter().any(|l| l.starts_with("-> CANCEL")),
        "{order:?}"
    );
    let cancel_ok = run
        .messages
        .iter()
        .filter(|m| {
            m.received && m.first_line() == "SIP/2.0 200 OK" && m.text.contains("CSeq: 1 CANCEL")
        })
        .count();
    assert_eq!(cancel_ok, 1);
    assert!(order.iter().any(|l| l.starts_with("-> ACK")), "{order:?}");
    let res = faxd.wait_results(1);
    assert_eq!(res[0]["outcome"]["reason"]["kind"], "cancelled");
    assert!(res[0]["answered_at"].is_null(), "never answered");
    faxd.stop();
}

// ======================================================================== S10

#[test]
fn s10_load_200_calls_at_20_cps() {
    let faxd = Faxd::start("s10", &faxd_opts(""));
    let mut o = sipp_for("s10-sipp", "s10_load_simple_call.xml", &faxd);
    o.calls = 200;
    o.rate = 20;
    o.timeout = Duration::from_secs(120);
    let started = std::time::Instant::now();
    let run = run_sipp(&o);
    println!("S10 wall time {:.1} s", started.elapsed().as_secs_f64());
    run.assert_successful(200);
    let res = faxd.wait_results(200);
    assert!(
        res.iter().all(|r| r["outcome"]["status"] == "completed"),
        "all completed"
    );
    assert!(res.iter().all(|r| r["t38"]["path"] == "initial_invite"));
    let mut ids: Vec<&str> = res.iter().map(|r| r["call_id"].as_str().unwrap()).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 200, "200 distinct calls");
    let calls_dir = faxd.calls_dir.clone();
    faxd.stop();
    let dirs = std::fs::read_dir(&calls_dir).unwrap().count();
    assert_eq!(dirs, 200, "one capture directory per call");
}

// ======================================================================== S11

#[test]
fn s11_every_retransmitted_200_to_our_reinvite_gets_the_identical_ack() {
    let faxd = Faxd::start("s11", &faxd_opts("initiate_after_secs = 0.3\n"));
    let mut o = sipp_for("s11-sipp", "s11_reack_on_200_retransmission.xml", &faxd);
    // -nr: a duplicate ACK would make SIPp re-send its last message (the 200) automatically
    o.args.push("-nr".into());
    let run = run_sipp(&o);
    run.assert_successful(1);
    let acks = run.messages_starting("ACK", true);
    assert_eq!(
        acks.len(),
        3,
        "one ACK per 200 (original + two retransmissions)"
    );
    assert_eq!(acks[0].text, acks[1].text, "ACK #2 identical to #1");
    assert_eq!(acks[0].text, acks[2].text, "ACK #3 identical to #1");
    let invite = &run.messages_starting("INVITE", true)[0];
    assert_eq!(cseq(acks[0]), cseq(invite), "ACK CSeq = re-INVITE CSeq");
    let branch = |m: &LoggedMessage| {
        header(m, "Via")
            .and_then(|v| v.split("branch=").nth(1))
            .map(str::to_string)
    };
    assert_ne!(
        branch(acks[0]),
        branch(invite),
        "a 2xx ACK is a new transaction"
    );
    // each ACK follows its 200 immediately
    let sent200: Vec<f64> = run
        .messages
        .iter()
        .filter(|m| !m.received && m.first_line() == "SIP/2.0 200 OK" && cseq(m) == cseq(invite))
        .map(|m| m.at)
        .collect();
    assert_eq!(sent200.len(), 3);
    for (i, (r, a)) in sent200.iter().zip(&acks).enumerate() {
        let d = a.at - r;
        println!("S11 ACK #{} {:.3} s after SIPp's 200 #{}", i + 1, d, i + 1);
        assert!(
            (0.0..0.25).contains(&d),
            "ACK #{} came {d:.3} s after the 200",
            i + 1
        );
    }
    let res = faxd.wait_results(1);
    assert_eq!(res[0]["t38"]["path"], "our_reinvite");
    faxd.stop();
}

// ======================================================================== C06 / C07

#[test]
fn c06_sipp_t38_in_the_initial_invite_no_reinvite_follows() {
    let faxd = Faxd::start("c06", &faxd_opts("initiate_after_secs = 0.3\n"));
    let run = run_sipp(&sipp_for(
        "c06-sipp",
        "c06_t38_in_initial_invite.xml",
        &faxd,
    ));
    run.assert_successful(1);
    assert!(
        run.messages_starting("INVITE", true).is_empty(),
        "no re-INVITE"
    );
    let res = faxd.wait_results(1);
    assert_eq!(res[0]["outcome"]["status"], "completed");
    assert_eq!(res[0]["t38"]["path"], "initial_invite");
    faxd.stop();
}

#[test]
fn c07_sipp_audio_plus_image_offer_picks_one_line_by_policy() {
    for (name, policy, audio, image) in [
        (
            "auto",
            "",
            "m=audio 0 RTP/AVP 0 8",
            "m=image 30000 udptl t38",
        ),
        (
            "audio-only",
            "t38_policy = \"audio-only\"\n",
            "m=audio 20000 RTP/AVP 0 8",
            "m=image 0 udptl t38",
        ),
    ] {
        let faxd = Faxd::start(&format!("c07-{name}"), &faxd_opts(policy));
        let mut o = sipp_for(
            &format!("c07-{name}-sipp"),
            "c07_audio_and_image_offer.xml",
            &faxd,
        );
        o.substitutions = vec![
            ("@AUDIO_LINE@".into(), audio.into()),
            ("@IMAGE_LINE@".into(), image.into()),
        ];
        let run = run_sipp(&o);
        run.assert_successful(1);
        let ok = run
            .messages
            .iter()
            .find(|m| m.received && m.first_line() == "SIP/2.0 200 OK")
            .unwrap();
        let lines: Vec<&str> = ok
            .text
            .lines()
            .filter(|l| l.starts_with("m="))
            .map(str::trim)
            .collect();
        println!("C07 {name}: {lines:?}");
        assert_eq!(lines, vec![audio, image]);
        faxd.stop();
    }
}

// ======================================================================== C03 / C04

const REG_USER: &str = "fax1";
const REG_PASS: &str = "secret";
const NONCE: &str = "dcd98b7102dd2f0e8b11d0f600bfb0c093";

fn registrar_setup(name: &str, scenario: &str, register: &str) -> (interop::SippHandle, Faxd) {
    let registrar_port = free_port();
    let faxd_port = free_port();
    let mut so = SippOptions::new(&format!("{name}-registrar"), scenario, "");
    so.local_port = Some(registrar_port);
    so.timeout = Duration::from_secs(60);
    so.substitutions = vec![
        ("@FAXD_PORT@".into(), faxd_port.to_string()),
        (
            "@EXPECTED_RESPONSE@".into(),
            digest_response(
                REG_USER,
                "sipp.test",
                REG_PASS,
                NONCE,
                "REGISTER",
                "sip:127.0.0.1",
            ),
        ),
    ];
    let sipp = spawn_sipp(&so);
    std::thread::sleep(Duration::from_millis(500));
    let trunk = format!(
        "t38_policy = \"wait-peer\"\n[trunks.register]\nenabled = true\nregistrar = \"127.0.0.1:{registrar_port}\"\n\
         username = \"{REG_USER}\"\npassword = \"{REG_PASS}\"\n{register}"
    );
    let faxd = Faxd::start(
        name,
        &FaxdOptions {
            trunk,
            port: Some(faxd_port),
            ..FaxdOptions::default()
        },
    );
    (sipp, faxd)
}

fn registers(run: &SippRun) -> Vec<&LoggedMessage> {
    run.messages_starting("REGISTER", true)
}

#[test]
fn c03_register_with_digest_against_a_sipp_registrar_and_refresh_at_half_lifetime() {
    let (sipp, faxd) = registrar_setup("c03", "c03_registrar_digest.xml", "expires = 10\n");
    let run = sipp.wait();
    run.assert_successful(1);
    let regs = registers(&run);
    assert_eq!(regs.len(), 3, "REGISTER, REGISTER+Authorization, refresh");
    assert!(header(regs[0], "Authorization").is_none());
    let auth = header(regs[1], "Authorization").unwrap();
    println!("C03 Authorization: {auth}");
    assert!(auth.contains("response=\""), "{auth}");
    // the refresh follows the 200 by lifetime/2 (10 s granted -> 5 s; below the 30 s margin)
    let ok_sent = run
        .messages
        .iter()
        .find(|m| !m.received && m.first_line() == "SIP/2.0 200 OK")
        .unwrap();
    let refresh_after = regs[2].at - ok_sent.at;
    println!("C03 refresh {refresh_after:.3} s after the 200");
    assert_close("refresh delay", refresh_after, 5.0, 0.4);
    assert!(
        faxd.log()
            .iter()
            .any(|l| l.contains("registration state") && l.contains("Registered")),
        "{}",
        faxd.log().join("\n")
    );
    faxd.stop();
}

#[test]
fn c04_register_failures_are_retried_with_exponential_backoff_and_recover() {
    let (sipp, faxd) = registrar_setup(
        "c04",
        "c04_registrar_failures_backoff.xml",
        "backoff_initial_secs = 1\nbackoff_max_secs = 8\n",
    );
    let run = sipp.wait();
    run.assert_successful(1);
    let regs = registers(&run);
    assert_eq!(regs.len(), 4);
    let r403: Vec<f64> = run
        .messages
        .iter()
        .filter(|m| !m.received && m.first_line().starts_with("SIP/2.0 403"))
        .map(|m| m.at)
        .collect();
    assert_eq!(r403.len(), 2);
    let d1 = regs[1].at - r403[0];
    let d2 = regs[2].at - r403[1];
    println!("C04 backoff delays: {d1:.3} s, {d2:.3} s");
    assert_close("first retry", d1, 1.0, 0.3);
    assert_close("second retry (doubled)", d2, 2.0, 0.3);
    assert!(header(regs[3], "Authorization").is_some());
    let log = faxd.log().join("\n");
    assert!(log.contains("Failed"), "Failed state logged:\n{log}");
    assert!(log.contains("Registered"), "recovered:\n{log}");
    faxd.stop();
}
