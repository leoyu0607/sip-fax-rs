//! Is the service managed by systemd? Then `start` / `stop` / `restart` must not run it behind
//! systemd's back (`docs/design/m4-web.md` section 7.2).
//!
//! Managed means: `INVOCATION_ID` is in the environment (the command runs inside a unit), or
//! the process the PID file points to has PID 1 as its parent and a `NOTIFY_SOCKET` in its
//! environment (systemd started it).

/// What `/proc/<pid>` says about the running service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcInfo {
    /// Parent process id.
    pub ppid: u32,
    /// `NOTIFY_SOCKET` is in its environment.
    pub has_notify_socket: bool,
}

/// The decision, from its inputs.
pub fn managed_by_systemd(invocation_id: Option<&str>, service: Option<ProcInfo>) -> bool {
    if invocation_id.is_some_and(|v| !v.is_empty()) {
        return true;
    }
    matches!(service, Some(p) if p.ppid == 1 && p.has_notify_socket)
}

/// The parent pid out of `/proc/<pid>/stat` (`pid (comm) S ppid ...`; `comm` may hold spaces
/// and parentheses, so the last `)` ends it).
pub fn parse_ppid(stat: &str) -> Option<u32> {
    let rest = stat.rsplit_once(')')?.1;
    rest.split_whitespace().nth(1)?.parse().ok()
}

/// Whether a `/proc/<pid>/environ` blob holds `NOTIFY_SOCKET`.
pub fn environ_has_notify_socket(environ: &[u8]) -> bool {
    environ
        .split(|b| *b == 0)
        .any(|kv| kv.starts_with(b"NOTIFY_SOCKET="))
}

/// Reads `/proc/<pid>` (Linux). `None` when the process or the files are not readable (another
/// user's `environ` is not: then only `INVOCATION_ID` can tell).
pub fn proc_info(pid: u32) -> Option<ProcInfo> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let ppid = parse_ppid(&stat)?;
    let environ = std::fs::read(format!("/proc/{pid}/environ")).unwrap_or_default();
    Some(ProcInfo {
        ppid,
        has_notify_socket: environ_has_notify_socket(&environ),
    })
}

/// The message `start` / `stop` / `restart` print instead of acting.
pub fn advice(command: &str) -> String {
    format!(
        "sip-faxd is managed by systemd: use `systemctl {command} sip-faxd` \
         (or pass --ignore-systemd to {command} it anyway)"
    )
}

#[cfg(test)]
mod tests {
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
}
