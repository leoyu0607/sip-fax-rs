//! K05 in detail: the per-field codes of every section, the file parser and the secret box.

use runtime_config::secret::{PREFIX, SecretBox, SecretError};
use runtime_config::{FieldError, RuntimeFile, Section, parse_section};
use serde_json::{Value, json};

fn errs(section: &str, body: Value) -> Vec<(String, String)> {
    match parse_section(&Section::parse(section).unwrap(), &body) {
        Ok(_) => vec![],
        Err(v) => v
            .into_iter()
            .map(|FieldError { field, code, .. }| (field, code))
            .collect(),
    }
}

fn has(list: &[(String, String)], field: &str, code: &str) {
    assert!(
        list.contains(&(field.to_string(), code.to_string())),
        "{field}/{code} not in {list:?}"
    );
}

fn trunk(patch: Value) -> Value {
    let mut base = json!({"name": "t", "allowed_sources": ["10.0.0.0/8"]});
    for (k, v) in patch.as_object().unwrap() {
        base[k] = v.clone();
    }
    base
}

#[test]
fn k05_a_valid_minimal_trunk_and_every_default_section_pass() {
    assert!(errs("trunk:t", trunk(json!({}))).is_empty());
    for s in [
        "sip",
        "media",
        "call",
        "capture",
        "delivery",
        "retention",
        "routes",
    ] {
        assert!(errs(s, json!({})).is_empty(), "{s}");
    }
}

#[test]
fn k05_trunk_fields_are_reported_with_their_paths() {
    let e = errs(
        "trunk:t",
        trunk(json!({"audio_codecs": ["PCMU", "G729"], "initiate_after_secs": -1.0})),
    );
    has(&e, "audio_codecs[1]", "invalid");
    has(&e, "initiate_after_secs", "out_of_range");
    let e = errs("trunk:t", trunk(json!({"audio_codecs": []})));
    has(&e, "audio_codecs", "empty");
    let e = errs(
        "trunk:t",
        trunk(
            json!({"t38": {"max_datagram": 0, "max_buffer": 0, "max_bit_rate": 0, "max_version": 9,
            "redundancy_depth": 9, "fec_span": 0, "fec_entries": 9}}),
        ),
    );
    for f in [
        "t38.max_datagram",
        "t38.max_buffer",
        "t38.max_bit_rate",
        "t38.fec_span",
        "t38.fec_entries",
    ] {
        has(&e, f, "out_of_range");
    }
    has(&e, "t38.max_version", "out_of_range");
    has(&e, "t38.redundancy_depth", "out_of_range");
    let e = errs("trunk:t", trunk(json!({"fax": {"resolutions": ["huge"]}})));
    has(&e, "fax.resolutions", "invalid");
    let e = errs("trunk:t", trunk(json!({"numbers": {"ani_sources": []}})));
    has(&e, "numbers.ani_sources", "empty");
    let e = errs(
        "trunk:t",
        trunk(json!({"numbers": {"dnis_sources": ["fax-header"]}})),
    );
    has(&e, "numbers.dnis_sources", "invalid");
    let e = errs(
        "trunk:t",
        trunk(json!({"allowed_sources": ["10.0.0.0/99", "nonsense"]})),
    );
    has(&e, "allowed_sources[0]", "invalid");
    has(&e, "allowed_sources[1]", "invalid");
    let e = errs("trunk:t", json!({"name": "t"}));
    has(&e, "allowed_sources", "required");
    let e = errs("trunk:t", trunk(json!({"peer": "not a host:name:5060"})));
    has(&e, "peer", "invalid");
    for bad in ["", "a b", "a/b", "ü"] {
        let e = errs(
            &format!("trunk:{}", if bad.is_empty() { "x" } else { bad }),
            trunk(json!({"name": bad})),
        );
        assert!(e.iter().any(|f| f.0 == "name"), "{bad:?}: {e:?}");
    }
    let e = errs("trunk:t", json!("not an object"));
    assert_eq!(e.len(), 1);
    let e = errs("trunk:t", trunk(json!({"preset": 5})));
    has(&e, "preset", "invalid");
}

#[test]
fn k05_registration_needs_what_it_needs() {
    let reg = |r: Value| trunk(json!({"peer": "203.0.113.1:5060", "register": r}));
    let e = errs("trunk:t", reg(json!({"enabled": true})));
    has(&e, "register.username", "required");
    has(&e, "register.password", "required");
    let e = errs(
        "trunk:t",
        reg(
            json!({"enabled": true, "username": "u", "password": "p", "expires": 0,
            "backoff_initial_secs": 10.0, "backoff_max_secs": 5.0, "registrar": "bad host:x:y"}),
        ),
    );
    has(&e, "register.expires", "out_of_range");
    has(&e, "register.backoff_initial_secs", "out_of_range");
    has(&e, "register.registrar", "invalid");
    let e = errs(
        "trunk:t",
        json!({"name": "t", "allowed_sources": ["10.0.0.1"], "register": {"enabled": true, "username": "u", "password": "p"}}),
    );
    has(&e, "register.registrar", "required");
    assert!(
        errs(
            "trunk:t",
            reg(json!({"enabled": true, "username": "u", "password": "p"}))
        )
        .is_empty()
    );
}

#[test]
fn k05_other_sections() {
    has(
        &errs("sip", json!({"user_agent": "x".repeat(129)})),
        "user_agent",
        "too_long",
    );
    has(
        &errs("sip", json!({"transports": []})),
        "transports",
        "empty",
    );
    has(
        &errs("sip", json!({"advertised": ":5060"})),
        "advertised",
        "invalid",
    );
    has(
        &errs("sip", json!({"bind": "not an address"})),
        "bind",
        "invalid",
    );
    has(
        &errs("sip", json!({"unknown_source": "ignore"})),
        "unknown_source",
        "invalid",
    );
    has(
        &errs("media", json!({"t38_port": 5000})),
        "t38_port",
        "removed",
    );
    has(
        &errs("media", json!({"port_range": "0-10"})),
        "port_range",
        "out_of_range",
    );
    has(
        &errs("media", json!({"port_range": "30001-30001"})),
        "port_range",
        "out_of_range",
    );
    has(
        &errs("call", json!({"reinvite_timeout_secs": 0})),
        "reinvite_timeout_secs",
        "out_of_range",
    );
    has(
        &errs("call", json!({"answer_delay_ms": 60001})),
        "answer_delay_ms",
        "out_of_range",
    );
    has(
        &errs("call", json!({"station_id": "caf\u{e9}"})),
        "station_id",
        "invalid",
    );
    has(
        &errs("capture", json!({"max_bytes_per_call": 0})),
        "max_bytes_per_call",
        "out_of_range",
    );
    has(
        &errs("delivery", json!({"catch_up_hours": 9000})),
        "catch_up_hours",
        "out_of_range",
    );
    has(
        &errs("delivery", json!({"max_attempts": 101})),
        "max_attempts",
        "out_of_range",
    );
    has(
        &errs("delivery", json!({"nonsense": 1})),
        "nonsense",
        "unknown_field",
    );
    has(
        &errs("retention", json!({"files_days": "x"})),
        "files_days",
        "wrong_type",
    );
    assert!(
        errs(
            "retention",
            json!({"files_days": 0, "records_days": 0, "run_at": "23:59"})
        )
        .is_empty()
    );
}

#[test]
fn k05_route_problems_name_the_route_and_the_delivery() {
    let folder = json!({"kind": "folder", "dir": "/x"});
    let e = errs(
        "routes",
        json!({"routes": [
            {"name": "a", "dnis": "1", "deliveries": [folder.clone(), folder.clone()]},
            {"dnis": "2", "deliveries": []},
            {"deliveries": [folder.clone()]},
            {"default": true, "dnis": "3", "deliveries": [folder.clone()]},
            {"default": true, "deliveries": [{"kind": "webhook", "url": "ftp://x"}]},
        ]}),
    );
    has(&e, "routes[0].deliveries[1]", "duplicate");
    has(&e, "routes[1]", "empty");
    has(&e, "routes[2]", "required");
    has(&e, "routes[3]", "invalid");
    has(&e, "routes[3]", "not_last");
    assert!(e.iter().any(|f| f.0 == "routes[4].deliveries[0]"), "{e:?}");
    let e = errs(
        "routes",
        json!({"routes": [{"dnis": "1", "deliveries": [{"kind": "ftp"}]}]}),
    );
    assert_eq!(e.len(), 1, "{e:?}");
    assert!(e[0].0.starts_with("routes[0].deliveries[0]"), "{e:?}");
}

#[test]
fn k01_file_parser_reports_the_table_and_remembers_which_tables_are_present() {
    let f = RuntimeFile::parse(
        "[call]\nmax_duration_secs = 5\n[[trunks]]\nname = \"a\"\npeer = \"10.0.0.1:5060\"\n",
    )
    .unwrap();
    assert_eq!(f.present, ["call", "trunks"]);
    assert_eq!(f.runtime.call.max_duration_secs, 5);
    assert!(RuntimeFile::parse("").unwrap().present.is_empty());
    for (text, needle) in [
        (
            "[retention]\nrecords_days = 1\nfiles_days = 5\n",
            "[retention]",
        ),
        ("[delivery]\nmax_attempts = 0\n", "max_attempts"),
        ("[retention]\nbogus = 1\n", "[retention]"),
        ("routes = 5\n", "[[routes]]"),
        ("[[routes]]\nname = \"x\"\n", "[[routes]]"),
        ("not toml [", "invalid TOML"),
        ("[sip]\nsurprise = 1\n", "surprise"),
    ] {
        let e = RuntimeFile::parse(text).unwrap_err();
        assert!(e.contains(needle), "{text:?}: {e}");
    }
}

#[test]
fn k10_the_secret_box_encrypts_per_field_and_reports_what_is_wrong() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sub").join("secret.key");
    let a = SecretBox::load_or_create(&path);
    assert!(a.available());
    assert_eq!(
        std::fs::read(&path).unwrap().len(),
        32,
        "the directory was created"
    );
    let b = SecretBox::load_or_create(&path);
    assert_eq!(
        a.raw_key(),
        b.raw_key(),
        "a second start loads the same key"
    );
    let enc = a.encrypt("trunk:x/register.password", "pässwörd").unwrap();
    assert!(enc.starts_with(PREFIX));
    assert_ne!(
        enc,
        a.encrypt("trunk:x/register.password", "pässwörd").unwrap(),
        "fresh nonce"
    );
    assert_eq!(
        b.decrypt("trunk:x/register.password", &enc).unwrap(),
        "pässwörd"
    );
    assert_eq!(
        a.decrypt("trunk:y/register.password", &enc),
        Err(SecretError::Undecryptable)
    );
    assert_eq!(a.decrypt("c", "plain"), Err(SecretError::Malformed));
    assert_eq!(a.decrypt("c", "enc:v1:!!!"), Err(SecretError::Malformed));
    assert_eq!(a.decrypt("c", "enc:v1:AAAA"), Err(SecretError::Malformed));
    let mut flipped = enc.clone().into_bytes();
    let n = flipped.len() - 6;
    flipped[n] = if flipped[n] == b'A' { b'B' } else { b'A' };
    assert_eq!(
        a.decrypt(
            "trunk:x/register.password",
            &String::from_utf8(flipped).unwrap()
        ),
        Err(SecretError::Undecryptable),
        "a changed ciphertext does not authenticate"
    );
    let other = SecretBox::from_key([7u8; 32]);
    assert_eq!(
        other.decrypt("trunk:x/register.password", &enc),
        Err(SecretError::Undecryptable)
    );
    let none = SecretBox::unavailable("why");
    assert!(!none.available());
    assert!(matches!(none.encrypt("c", "x"), Err(SecretError::NoKey(m)) if m == "why"));
    assert!(matches!(
        none.decrypt("c", &enc),
        Err(SecretError::NoKey(_))
    ));
    assert!(format!("{none:?}").contains("Unavailable"));
    // a path that cannot hold a file
    let blocked = SecretBox::load_or_create(dir.path());
    assert!(!blocked.available(), "{:?}", blocked.status());
}
