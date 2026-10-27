//! F01..F05 (configuration side of M3f).
use super::*;
use delivery::DeliveryConfig;

fn parse(text: &str) -> Result<AppConfig, String> {
    AppConfig::parse(text, Path::new("test.toml"))
}

const TRUNK: &str = "[[trunks]]\nname = \"t\"\nallowed_sources = [\"10.0.0.0/8\"]\n";

#[test]
fn f01_without_routes_or_a_delivery_section_the_defaults_apply() {
    let c = parse(TRUNK).unwrap();
    assert!(c.routes.routes.is_empty());
    assert_eq!(c.delivery, DeliverySection::default());
    assert_eq!(c.delivery.catch_up_hours, 24);
    assert_eq!(c.delivery.max_attempts, 10);
}

#[test]
fn f02_routes_and_the_delivery_section_are_read_with_every_kind() {
    let c = parse(&format!(
            "[delivery]\ncatch_up_hours = 6\nmax_attempts = 3\n{TRUNK}\
             [[routes]]\nname = \"sales\"\ndnis_prefix = \"2557\"\ndeliver_partial = true\n\
             [[routes.deliveries]]\nkind = \"folder\"\ndir = \"/srv/in\"\n\
             [[routes.deliveries]]\nkind = \"email\"\nhost = \"smtp.example.com\"\nfrom = \"fax@example.com\"\nto = [\"a@example.com\"]\n\
             [[routes.deliveries]]\nkind = \"webhook\"\nurl = \"https://example.com/hook\"\n\
             [[routes]]\ndefault = true\n[[routes.deliveries]]\nkind = \"folder\"\ndir = \"/srv/other\"\n"
        ))
        .unwrap();
    assert_eq!((c.delivery.catch_up_hours, c.delivery.max_attempts), (6, 3));
    assert_eq!(c.routes.routes.len(), 2);
    let r = &c.routes.routes[0];
    assert_eq!(r.name.as_deref(), Some("sales"));
    assert_eq!(r.dnis_prefix.as_deref(), Some("2557"));
    assert!(r.deliver_partial);
    let kinds: Vec<_> = r.deliveries.iter().map(DeliveryConfig::kind).collect();
    assert_eq!(kinds, ["folder", "email", "webhook"]);
    assert!(c.routes.routes[1].default);
    assert!(!c.routes.routes[1].deliver_partial);
}

#[test]
fn f03_invalid_routes_stop_the_start_and_name_every_problem() {
    let e = parse(&format!(
            "{TRUNK}[[routes]]\ndefault = true\n[[routes.deliveries]]\nkind = \"folder\"\ndir = \"/a\"\n\
             [[routes]]\ndnis = \"1\"\n"
        ))
        .err()
        .expect("a default route that is not last, a route without deliveries");
    assert!(e.contains("[[routes]]"), "{e}");
    assert!(e.contains("default route must be last"), "{e}");
    assert!(e.contains("no deliveries"), "{e}");
    for (text, needle) in [
        ("[[routes]]\ndnis = \"1\"\nbogus = 1\n", "bogus"),
        (
            "[[routes]]\ndnis = \"1\"\n[[routes.deliveries]]\nkind = \"ftp\"\n",
            "ftp",
        ),
        ("[delivery]\nmax_attempts = 0\n", "max_attempts"),
        ("[delivery]\nsurprise = 1\n", "surprise"),
        ("routes = 5\n", "[[routes]]"),
    ] {
        let e = parse(&format!("{text}{TRUNK}"))
            .err()
            .unwrap_or_else(|| panic!("{text} accepted"));
        assert!(e.contains(needle), "{text}: {e}");
    }
}

#[test]
fn f04_webhook_urls_are_masked_for_display() {
    assert_eq!(
        mask_http_url("https://user:hunter2@example.com/hook?token=abc&x=1#frag"),
        "https://user:***@example.com/hook?token=***&x=***"
    );
    assert_eq!(
        mask_http_url("http://example.com:8080/a/b"),
        "http://example.com:8080/a/b"
    );
    assert_eq!(mask_http_url("http://h/p?k"), "http://h/p?k=***");
}

#[test]
fn f06_the_routes_in_the_example_file_are_valid_once_uncommented() {
    let text = include_str!("../../../../conf/runtime.toml.example");
    let begin = text.find("# routes-example-begin").expect("begin marker");
    let end = text.find("# routes-example-end").expect("end marker");
    let routes: String = text[begin..end]
        .lines()
        .skip(1)
        .map(|l| {
            l.strip_prefix("# ")
                .or_else(|| l.strip_prefix('#'))
                .unwrap_or(l)
        })
        .collect::<Vec<_>>()
        .join("\n");
    // the example file itself delivers nothing ...
    assert!(parse(text).unwrap().routes.routes.is_empty());
    // ... and its documented routes work
    let c = parse(&format!("{TRUNK}{routes}")).unwrap();
    assert_eq!(c.routes.routes.len(), 2);
    assert_eq!(c.routes.routes[0].deliveries.len(), 3);
    assert!(c.routes.routes[1].default);
    let at = parse(text).unwrap().delivery;
    assert_eq!((at.catch_up_hours, at.max_attempts), (24, 10));
    assert!(text.contains("[delivery]") && text.contains("catch_up_hours"));
}
