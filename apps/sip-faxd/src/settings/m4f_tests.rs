//! D01, D03, D04 (M4f: `[database]` field by field, ADR-060).
use super::*;
use store::BackendKind;

fn parse(text: &str) -> Result<AppConfig, String> {
    AppConfig::parse(text, Path::new("test.toml"))
}

fn url_of(db: &str) -> String {
    parse(&format!("[database]\n{db}\n")).unwrap().database.url
}

fn err_of(db: &str) -> String {
    match parse(&format!("[database]\n{db}\n")) {
        Ok(c) => panic!("accepted {db:?} as {}", c.database.url),
        Err(e) => e,
    }
}

// ------------------------------------------------------------ D01
#[test]
fn d01_the_fields_build_the_same_url_as_the_one_written_by_hand() {
    // (fields, the URL an operator would write by hand, family)
    let cases = [
        (
            "type = \"postgres\"\nhost = \"db.example\"\nname = \"faxdb\"\nuser = \"faxu\"\npassword = \"pw1\"",
            "postgres://faxu:pw1@db.example:5432/faxdb",
            BackendKind::Postgres,
        ),
        (
            "type = \"postgres\"\nhost = \"10.1.2.3\"\nport = 6543\nname = \"faxdb\"\nuser = \"faxu\"\npassword = \"pw1\"",
            "postgres://faxu:pw1@10.1.2.3:6543/faxdb",
            BackendKind::Postgres,
        ),
        (
            "type = \"mysql\"\nhost = \"db.example\"\nname = \"faxdb\"\nuser = \"faxu\"\npassword = \"pw1\"",
            "mysql://faxu:pw1@db.example:3306/faxdb",
            BackendKind::Mysql,
        ),
        (
            "type = \"mariadb\"\nhost = \"db.example\"\nname = \"faxdb\"\nuser = \"faxu\"\npassword = \"pw1\"",
            "mariadb://faxu:pw1@db.example:3306/faxdb",
            BackendKind::Mysql,
        ),
        (
            "type = \"mssql\"\nhost = \"db.example\"\nname = \"faxdb\"\nuser = \"sa\"\npassword = \"pw1\"",
            "mssql://sa:pw1@db.example:1433/faxdb",
            BackendKind::Mssql,
        ),
        (
            "type = \"mssql\"\nhost = \"db.example\"\nport = 14330\nname = \"faxdb\"\nuser = \"sa\"\npassword = \"pw1\"\ntrust_server_certificate = true",
            "mssql://sa:pw1@db.example:14330/faxdb?trust_server_certificate=true",
            BackendKind::Mssql,
        ),
        (
            "type = \"mssql\"\nhost = \"db.example\"\nname = \"faxdb\"\nuser = \"sa\"\npassword = \"pw1\"\ntrust_server_certificate = false",
            "mssql://sa:pw1@db.example:1433/faxdb",
            BackendKind::Mssql,
        ),
        (
            "type = \"postgres\"\nhost = \"db.example\"\nname = \"faxdb\"",
            "postgres://db.example:5432/faxdb",
            BackendKind::Postgres,
        ),
        (
            "type = \"postgres\"\nhost = \"db.example\"\nname = \"faxdb\"\nuser = \"faxu\"",
            "postgres://faxu@db.example:5432/faxdb",
            BackendKind::Postgres,
        ),
        (
            "type = \"postgres\"\nhost = \"::1\"\nname = \"faxdb\"\nuser = \"u\"\npassword = \"p\"",
            "postgres://u:p@[::1]:5432/faxdb",
            BackendKind::Postgres,
        ),
        (
            "type = \"sqlite\"\npath = \"data/x.db\"",
            "sqlite://data/x.db",
            BackendKind::Sqlite,
        ),
        (
            "type = \"sqlite\"",
            "sqlite://data/sip-faxd.db",
            BackendKind::Sqlite,
        ),
    ];
    for (fields, hand, kind) in cases {
        let got = url_of(fields);
        assert_eq!(got, hand, "{fields}");
        assert_eq!(store::backend_for_url(&got).unwrap(), kind, "{got}");
    }
    // `url` is taken as written, and no [database] at all is the SQLite default
    assert_eq!(
        url_of("url = \"postgres://u:p%40@h/db\""),
        "postgres://u:p%40@h/db"
    );
    assert_eq!(parse("").unwrap().database.url, DEFAULT_DATABASE_URL);
    // the rest of [database] keeps its defaults next to the fields
    let c = parse("[database]\ntype = \"sqlite\"\nauto_migrate = false\nmax_connections = 9\n")
        .unwrap();
    assert!(!c.database.auto_migrate);
    assert_eq!(c.database.max_connections, 9);
}

#[test]
fn d01_awkward_user_names_passwords_and_database_names_decode_to_what_was_written() {
    let pw = "p@ss:w/rd?#1%41&+ 中😀";
    let user = "fax user@corp";
    let name = "fax db/1";
    let text = format!(
        "type = \"postgres\"\nhost = \"db\"\nname = {name:?}\nuser = {user:?}\npassword = {pw:?}"
    );
    let built = url_of(&text);
    // the URL is valid and, parsed by an independent implementation, gives back the input
    let u = url::Url::parse(&built).unwrap_or_else(|e| panic!("{built}: {e}"));
    assert_eq!(u.host_str(), Some("db"));
    assert_eq!(u.port(), Some(5432));
    let dec = |s: &str| {
        percent_encoding::percent_decode_str(s)
            .decode_utf8()
            .unwrap()
            .into_owned()
    };
    assert_eq!(dec(u.username()), user);
    assert_eq!(dec(u.password().unwrap()), pw);
    assert_eq!(dec(u.path().trim_start_matches('/')), name);
    assert!(!built.contains(pw), "the plain password is not in the URL");
    // masked, only *** remains
    let masked = mask_url(&built);
    assert!(masked.contains(":***@"), "{masked}");
    assert!(
        !masked.contains("ss%3Aw") && !masked.contains("p%40"),
        "{masked}"
    );
}

// ------------------------------------------------------------ D03
#[cfg(unix)]
fn pw_file(dir: &Path, name: &str, bytes: &[u8], mode: u32) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
    p
}

#[cfg(unix)]
fn from_file(p: &Path) -> Result<DatabaseSection, String> {
    let c = parse(&format!(
        "[database]\ntype = \"mysql\"\nhost = \"h\"\nname = \"d\"\nuser = \"u\"\npassword_file = {:?}\n",
        p.display().to_string()
    ))?;
    Ok(c.database)
}

#[cfg(unix)]
#[test]
fn d03_password_file_plain_and_with_line_ends_gives_the_first_line() {
    let d = tempfile::tempdir().unwrap();
    for (name, bytes) in [
        ("plain", &b"s3cr@t:pw"[..]),
        ("lf", b"s3cr@t:pw\n"),
        ("crlf", b"s3cr@t:pw\r\n"),
        ("more", b"s3cr@t:pw\nsecond line\n"),
    ] {
        let p = pw_file(d.path(), name, bytes, 0o600);
        let db = from_file(&p).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(db.url, "mysql://u:s3cr%40t%3Apw@h:3306/d", "{name}");
        assert!(db.warnings.is_empty(), "{name}: {:?}", db.warnings);
    }
    // spaces inside are kept (only the line end goes)
    let p = pw_file(d.path(), "sp", "a b 中\n".as_bytes(), 0o600);
    assert_eq!(
        from_file(&p).unwrap().url,
        "mysql://u:a%20b%20%E4%B8%AD@h:3306/d"
    );
}

#[cfg(unix)]
#[test]
fn d03_password_file_empty_or_missing_is_an_error_naming_the_file() {
    let d = tempfile::tempdir().unwrap();
    for (name, bytes) in [
        ("empty", &b""[..]),
        ("only-newline", b"\n"),
        ("crlf-only", b"\r\n"),
    ] {
        let p = pw_file(d.path(), name, bytes, 0o600);
        let e = from_file(&p).unwrap_err();
        assert!(e.contains("[database] password_file"), "{e}");
        assert!(e.contains(name) && e.contains("empty"), "{e}");
    }
    let missing = d.path().join("no-such-file");
    let e = from_file(&missing).unwrap_err();
    assert!(
        e.contains("[database] password_file") && e.contains("no-such-file"),
        "{e}"
    );
    assert!(e.contains("cannot read"), "{e}");
    // a directory is not a file either
    let e = from_file(d.path()).unwrap_err();
    assert!(e.contains("[database] password_file"), "{e}");
}

#[cfg(unix)]
#[test]
fn d03_password_file_readable_by_others_warns_but_works() {
    let d = tempfile::tempdir().unwrap();
    let ok = pw_file(d.path(), "ok", b"pw\n", 0o600);
    assert!(from_file(&ok).unwrap().warnings.is_empty());
    let ro = pw_file(d.path(), "ro", b"pw\n", 0o400);
    assert!(from_file(&ro).unwrap().warnings.is_empty());
    for mode in [0o644, 0o640, 0o604] {
        let p = pw_file(d.path(), &format!("m{mode:o}"), b"pw\n", mode);
        let db = from_file(&p).unwrap();
        assert_eq!(db.url, "mysql://u:pw@h:3306/d", "mode {mode:o}");
        assert_eq!(db.warnings.len(), 1, "mode {mode:o}: {:?}", db.warnings);
        let w = &db.warnings[0];
        assert!(
            w.contains("[database] password_file") && w.contains("chmod 600"),
            "{w}"
        );
        assert!(w.contains(&format!("{mode:o}")), "{w}");
        assert!(!w.contains("pw\n") && !w.ends_with("pw"), "{w}");
    }
}

#[cfg(unix)]
#[test]
fn d03_a_relative_password_file_is_relative_to_the_executable_directory() {
    let exe = crate::exe_dir();
    let name = format!("m4f-d03-rel-{}.pw", std::process::id());
    let p = exe.join(&name);
    std::fs::write(&p, "relpw\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
    let r = parse(&format!(
        "[database]\ntype = \"postgres\"\nhost = \"h\"\nname = \"d\"\nuser = \"u\"\npassword_file = {name:?}\n"
    ));
    let _ = std::fs::remove_file(&p);
    assert_eq!(r.unwrap().database.url, "postgres://u:relpw@h:5432/d");
}

// ------------------------------------------------------------ D04
#[test]
fn d04_every_wrong_combination_is_refused_with_the_field_named() {
    let srv = "type = \"postgres\"\nhost = \"h\"\nname = \"d\"\nuser = \"u\"\n";
    let cases: Vec<(String, &str)> = vec![
        (
            "url = \"sqlite://x.db\"\ntype = \"sqlite\"".into(),
            "url and type",
        ),
        (
            "url = \"postgres://h/d\"\ntype = \"postgres\"\nhost = \"h\"".into(),
            "url and type",
        ),
        ("url = \"postgres://h/d\"\nhost = \"h\"".into(), "host"),
        (
            "url = \"postgres://h/d\"\npassword = \"x\"".into(),
            "password",
        ),
        ("host = \"h\"".into(), "host"),
        (
            format!("{srv}password = \"a\"\npassword_file = \"/tmp/x\""),
            "password and password_file",
        ),
        (
            "type = \"oracle\"\nhost = \"h\"\nname = \"d\"".into(),
            "type",
        ),
        ("type = \"\"".into(), "type"),
        ("type = \"postgres\"\nname = \"d\"".into(), "host"),
        (
            "type = \"postgres\"\nhost = \"  \"\nname = \"d\"".into(),
            "host",
        ),
        ("type = \"mysql\"\nhost = \"h\"".into(), "name"),
        ("type = \"mssql\"\nhost = \"h\"\nname = \"\"".into(), "name"),
        (
            format!("{srv}trust_server_certificate = true"),
            "trust_server_certificate",
        ),
        (
            "type = \"mysql\"\nhost = \"h\"\nname = \"d\"\ntrust_server_certificate = false".into(),
            "trust_server_certificate",
        ),
        (
            "type = \"mariadb\"\nhost = \"h\"\nname = \"d\"\ntrust_server_certificate = true"
                .into(),
            "trust_server_certificate",
        ),
        ("type = \"sqlite\"\nhost = \"h\"".into(), "host"),
        ("type = \"sqlite\"\nuser = \"u\"".into(), "user"),
        ("type = \"sqlite\"\npassword = \"p\"".into(), "password"),
        ("type = \"sqlite\"\nname = \"d\"".into(), "name"),
        ("type = \"sqlite\"\nport = 5".into(), "port"),
        (
            "type = \"sqlite\"\ntrust_server_certificate = true".into(),
            "trust_server_certificate",
        ),
        ("type = \"sqlite\"\npath = \" \"".into(), "path"),
        (format!("{srv}path = \"x.db\""), "path"),
        (
            "type = \"postgres\"\nhost = \"h\"\nname = \"d\"\npassword = \"p\"".into(),
            "user",
        ),
        (format!("{srv}colour = \"red\""), "colour"),
        (format!("{srv}port = 70000"), "70000"),
        (format!("{srv}max_connections = 0"), "max_connections"),
    ];
    for (db, field) in cases {
        let e = err_of(&db);
        assert!(
            e.contains(field),
            "case {db:?}: the message does not name {field}: {e}"
        );
        assert!(e.contains("test.toml"), "the file is named: {e}");
    }
}

#[test]
fn d04_an_error_never_contains_the_password() {
    let e = err_of(
        "type = \"oracle\"\nhost = \"h\"\nname = \"d\"\nuser = \"u\"\npassword = \"Sup3r:Secret@\"",
    );
    assert!(!e.contains("Sup3r"), "{e}");
    let e = err_of("type = \"postgres\"\nname = \"d\"\nuser = \"u\"\npassword = \"Sup3r:Secret@\"");
    assert!(!e.contains("Sup3r"), "{e}");
    let e = err_of("type = \"sqlite\"\npassword = \"Sup3r:Secret@\"");
    assert!(!e.contains("Sup3r"), "{e}");
}
