use super::testutil::sample;
use super::*;

fn utc() -> FixedOffset {
    FixedOffset::east_opt(0).unwrap()
}

#[test]
fn d02_template_all_placeholders() {
    let t = Template::parse(
        "{date}_{time}_{ani}_{dnis}_{id}_{trunk}_{pages}_{status}_{duration}_{resolution}",
    )
    .unwrap();
    assert_eq!(
        t.render(&sample(), utc(), Mode::FileName),
        "20261021_083015_0227001234_025570001_42_audiocodes_3_received_90_fine"
    );
    let t = Template::parse("{call_id}|{tsi}|{started_at}|{ended_at}|{{x}}").unwrap();
    assert_eq!(
        t.render(&sample(), utc(), Mode::Text),
        "abc@host|+886 2 2700|2026-10-21T08:30:15+00:00|2026-10-21T08:31:45+00:00|{x}"
    );
}

#[test]
fn d02_template_uses_utc_offset_for_date_and_time() {
    let t = Template::parse("{date}-{time}").unwrap();
    let tw = FixedOffset::east_opt(8 * 3600).unwrap();
    assert_eq!(t.render(&sample(), tw, Mode::FileName), "20261021-163015");
    let tw = FixedOffset::east_opt(-9 * 3600).unwrap();
    assert_eq!(t.render(&sample(), tw, Mode::FileName), "20261020-233015");
}

#[test]
fn d02_template_missing_values() {
    let mut r = sample();
    r.ani = None;
    r.dnis = Some(String::new());
    r.ended_at = None;
    r.duration_sec = None;
    let t = Template::parse("{ani}/{dnis}/{ended_at}/{duration}").unwrap();
    assert_eq!(t.render(&r, utc(), Mode::Text), "unknown/unknown//");
}

#[test]
fn d02_template_rejects_bad_syntax() {
    assert!(
        Template::parse("{nope}")
            .unwrap_err()
            .contains("unknown placeholder {nope}")
    );
    assert!(
        Template::parse("abc{id")
            .unwrap_err()
            .contains("unterminated")
    );
    assert!(Template::parse("abc}").unwrap_err().contains("unmatched"));
}

#[test]
fn d02_sanitize_blocks_path_traversal() {
    let mut r = sample();
    r.ani = Some("../../etc/passwd".into());
    r.trunk = "a\\b:c*?\"<>|\0".into();
    let t = Template::parse("{ani}_{trunk}").unwrap();
    let n = t.render(&r, utc(), Mode::FileName);
    assert!(
        !n.contains('/') && !n.contains('\\') && !n.contains(".."),
        "{n}"
    );
    assert_eq!(n, "_._.__etc_passwd_a_b_c_______");
    // A template literal cannot smuggle a separator either.
    let t = Template::parse("../sub/{id}").unwrap();
    let n = t.render(&r, utc(), Mode::FileName);
    assert_eq!(n, "_._sub_42");
    assert_eq!(sanitize_name(""), "fax");
    assert_eq!(sanitize_name("."), "_");
    assert_eq!(sanitize_name("..."), "_._");
    assert_eq!(sanitize_name("中文 檔.pdf"), "中文_檔.pdf");
    let long = "あ".repeat(100);
    let s = sanitize_name(&long);
    assert!(s.len() <= MAX_NAME_BYTES && s.chars().all(|c| c == 'あ'));
}

#[test]
fn d03_text_mode_strips_header_injection() {
    let mut r = sample();
    r.trunk = "x\r\nBcc: evil@example.com".into();
    let t = Template::parse("Fax {trunk}").unwrap();
    let s = t.render(&r, utc(), Mode::Text);
    assert!(!s.contains('\r') && !s.contains('\n'), "{s:?}");
}
