use super::*;

fn parse(t: &str) -> Result<ServiceConfig, String> {
    ServiceConfig::parse(t, Path::new("c.toml"))
}

#[test]
fn m00_defaults_follow_the_spec() {
    let c = parse("").unwrap();
    assert_eq!(c.log.file, "logs/sip-faxd.log");
    assert_eq!((c.log.max_size_mb, c.log.keep), (50.0, 14));
    assert!(c.log.daily && c.log.compress);
    assert_eq!(c.service.pid_file, "run/sip-faxd.pid");
    assert_eq!(c.service.socket, "run/sip-faxd.sock");
    assert_eq!(c.service.stop_timeout_secs, 300);
    assert_eq!(c.service.start_timeout_secs, 30);
}

#[test]
fn m00_values_are_read_and_paths_are_relative_to_the_executable() {
    let c = parse(
            "[log]\nlevel=\"debug\"\nfile=\"l/x.log\"\nmax_size_mb=0.5\ndaily=false\nkeep=3\ncompress=false\n\
             [service]\npid_file=\"/var/run/f.pid\"\nsocket=\"s.sock\"\nstop_timeout_secs=5\n\
             [sip]\nbogus = 1\n",
        )
        .unwrap();
    assert_eq!(c.log.level, "debug");
    assert_eq!(
        c.log.file_path(Path::new("/opt/fax")),
        Some(PathBuf::from("/opt/fax/l/x.log"))
    );
    assert!(!c.log.daily && !c.log.compress);
    assert_eq!(c.log.max_size_mb, 0.5);
    assert_eq!(
        c.service.pid_path(Path::new("/opt/fax")),
        PathBuf::from("/var/run/f.pid")
    );
    assert_eq!(
        c.service.socket_path(Path::new("/opt/fax")),
        PathBuf::from("/opt/fax/s.sock")
    );
    assert_eq!(c.service.stop_timeout_secs, 5);
    // an empty file name switches file logging off
    let off = parse("[log]\nfile = \"\"\n").unwrap();
    assert_eq!(off.log.file_path(Path::new("/opt")), None);
}

#[test]
fn m00_bad_values_and_unknown_keys_are_refused_with_the_section_name() {
    for (text, needle) in [
        ("[log]\nbogus = 1\n", "[log]"),
        ("[service]\nbogus = 1\n", "[service]"),
        ("[log]\nmax_size_mb = 0\n", "max_size_mb"),
        ("[log]\nkeep = 0\n", "keep"),
        ("[service]\nsocket = \"\"\n", "socket"),
        ("[service]\nstart_timeout_secs = 0\n", "start_timeout_secs"),
        ("[log\n", "invalid TOML"),
    ] {
        let e = parse(text).unwrap_err();
        assert!(e.contains(needle) && e.contains("c.toml"), "{text}: {e}");
    }
}
