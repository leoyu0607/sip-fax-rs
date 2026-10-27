use super::*;
use crate::service::protocol::TrunkStatus;

#[test]
fn m07_the_status_text_has_every_field() {
    let s = StatusInfo {
        pid: 1234,
        version: "0.3.0".into(),
        state: "running".into(),
        started_at: "2026-10-21T08:00:00.000Z".into(),
        uptime_secs: 3 * 3600 + 25 * 60 + 7,
        listen: vec!["udp 0.0.0.0:5060".into(), "tcp 0.0.0.0:5060".into()],
        active_calls: 2,
        trunks: vec![
            TrunkStatus {
                name: "cht".into(),
                registration: "registered".into(),
            },
            TrunkStatus {
                name: "lab".into(),
                registration: "none".into(),
            },
        ],
    };
    let t = format_status(&s);
    for want in [
        "sip-faxd is running",
        "PID:      1234",
        "Version:  0.3.0",
        "2026-10-21T08:00:00.000Z (up 3h 25m 7s)",
        "Listen:   udp 0.0.0.0:5060, tcp 0.0.0.0:5060",
        "Calls:    2 in progress",
        "cht: registered",
        "lab: none",
    ] {
        assert!(t.contains(want), "{want:?} missing from\n{t}");
    }
    let none = StatusInfo {
        trunks: vec![],
        state: "stopping".into(),
        ..s
    };
    let t = format_status(&none);
    assert!(
        t.contains("sip-faxd is stopping") && t.contains("Trunks:   none"),
        "{t}"
    );
}

#[test]
fn m07_uptimes_drop_leading_zero_units() {
    assert_eq!(format_uptime(5), "5s");
    assert_eq!(format_uptime(65), "1m 5s");
    assert_eq!(format_uptime(3600), "1h 0m 0s");
    assert_eq!(format_uptime(90_061), "1d 1h 1m 1s");
}

#[test]
fn m04_the_stop_lines_say_what_is_waited_for() {
    assert_eq!(stopping_line(7, 0, 300, false), "Stopping sip-faxd (pid 7)");
    assert!(stopping_line(7, 2, 300, false).contains("2 call(s) in progress, waiting up to 300 s"));
    assert!(stopping_line(7, 2, 0, true).contains("now: 2 call(s)"));
    assert_eq!(
        waiting_line(1, 295),
        "Waiting for 1 call(s) to finish (295 s left)"
    );
}

#[test]
fn m08_a_managed_service_is_refused_unless_told_otherwise() {
    let d = tempfile::tempdir().unwrap();
    let cfg = d.path().join("c.toml");
    std::fs::write(&cfg, "").unwrap();
    let ctx = Ctx::load(&cfg, d.path()).unwrap();
    // nothing in the environment of the test run says "systemd"
    assert!(
        std::env::var_os("INVOCATION_ID").is_none()
            || systemd_refusal(&ctx, "stop", true).is_none()
    );
    // the override always wins
    assert_eq!(systemd_refusal(&ctx, "stop", true), None);
}

#[test]
fn m00_a_missing_or_broken_config_is_exit_code_2() {
    let d = tempfile::tempdir().unwrap();
    assert!(Ctx::load(&d.path().join("missing.toml"), d.path()).is_err());
    let bad = d.path().join("bad.toml");
    std::fs::write(&bad, "[service]\nbogus = 1\n").unwrap();
    assert!(Ctx::load(&bad, d.path()).unwrap_err().contains("[service]"));
}
