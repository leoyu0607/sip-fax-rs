use super::*;

#[test]
fn m08_the_invocation_id_alone_decides() {
    assert!(managed_by_systemd(Some("0123abcd"), None));
    assert!(!managed_by_systemd(Some(""), None), "empty is not set");
    assert!(!managed_by_systemd(None, None));
}

#[test]
fn m08_a_service_that_systemd_started_has_pid_1_as_parent_and_a_notify_socket() {
    let p = |ppid, has| {
        Some(ProcInfo {
            ppid,
            has_notify_socket: has,
        })
    };
    assert!(managed_by_systemd(None, p(1, true)));
    assert!(
        !managed_by_systemd(None, p(1, false)),
        "an init-adopted daemon of ours"
    );
    assert!(
        !managed_by_systemd(None, p(4321, true)),
        "started by a shell"
    );
    assert!(!managed_by_systemd(None, p(4321, false)));
}

#[test]
fn m08_proc_files_are_parsed() {
    assert_eq!(
        parse_ppid("123 (sip-faxd) S 1 123 123 0 -1 4194560"),
        Some(1)
    );
    assert_eq!(
        parse_ppid("123 (tricky) name) S 77 1 1"),
        Some(77),
        "a ')' inside the command name"
    );
    assert_eq!(parse_ppid("garbage"), None);
    assert!(environ_has_notify_socket(
        b"A=1\0NOTIFY_SOCKET=/run/systemd/notify\0B=2\0"
    ));
    assert!(!environ_has_notify_socket(b"A=1\0MY_NOTIFY_SOCKET=x\0"));
    assert!(!environ_has_notify_socket(b""));
}

#[cfg(target_os = "linux")]
#[test]
fn m08_this_process_can_be_read_from_proc() {
    let me = proc_info(std::process::id()).expect("/proc is readable");
    assert!(me.ppid > 0);
    assert!(!me.has_notify_socket, "tests do not run under Type=notify");
    assert_eq!(proc_info(u32::MAX - 1), None);
}

#[test]
fn m08_the_advice_names_systemctl_and_the_override() {
    let a = advice("stop");
    assert!(a.contains("systemctl stop sip-faxd") && a.contains("--ignore-systemd"));
}
