use super::*;

fn parse(s: &str) -> Routes {
    toml::from_str(s).unwrap()
}

#[test]
fn d01_parse_full_example() {
    let r = parse(
        r#"
[[routes]]
dnis_prefix = "2557"
deliver_partial = true
[[routes.deliveries]]
kind = "folder"
dir = "/srv/fax-inbox"
filename = "{date}_{ani}_{id}.pdf"
formats = ["pdf"]
[[routes.deliveries]]
kind = "email"
host = "smtp.example.com"
security = "smtps"
username = "u"
password = "p"
from = "Fax <fax@example.com>"
to = ["a@example.com", "b@example.com"]
attach_tiff = true
[[routes.deliveries]]
kind = "webhook"
url = "https://user:pw@example.com/hook?token=abc"
secret = "k"
attach_pdf = true
headers = { "X-Env" = "prod" }

[[routes]]
default = true
[[routes.deliveries]]
kind = "folder"
dir = "/srv/other"
"#,
    );
    assert_eq!(r.routes.len(), 2);
    assert!(r.routes[0].deliver_partial);
    assert!(!r.routes[1].deliver_partial);
    r.validate().unwrap();
    let t: Vec<String> = r.routes[0].deliveries.iter().map(|d| d.target()).collect();
    assert_eq!(
        t,
        vec![
            "folder:/srv/fax-inbox",
            "email:a@example.com,b@example.com",
            "webhook:https://example.com/hook"
        ]
    );
    match &r.routes[0].deliveries[1] {
        DeliveryConfig::Email(e) => {
            assert_eq!(e.effective_port(), 465);
            assert_eq!(e.timeout_secs, 30);
            assert!(e.subject.contains("{ani}"));
        }
        other => panic!("{other:?}"),
    }
    match &r.routes[1].deliveries[0] {
        DeliveryConfig::Folder(f) => {
            assert_eq!(f.formats, vec![Format::Pdf, Format::Tiff]);
            assert!(f.create_dir);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn d01_parse_rejects_unknown_fields_and_kinds() {
    assert!(toml::from_str::<Routes>("[[routes]]\ndnis = \"1\"\ntypo = 1\n").is_err());
    assert!(
        toml::from_str::<Routes>(
            "[[routes]]\ndefault = true\n[[routes.deliveries]]\nkind = \"fax\"\n"
        )
        .is_err()
    );
    assert!(
            toml::from_str::<Routes>(
                "[[routes]]\ndefault = true\n[[routes.deliveries]]\nkind = \"folder\"\ndir = \"x\"\nbogus = 1\n"
            )
            .is_err()
        );
}

fn folder(dir: &str) -> DeliveryConfig {
    DeliveryConfig::Folder(FolderConfig {
        dir: dir.into(),
        filename: default_filename(),
        formats: default_formats(),
        create_dir: true,
    })
}

fn route(f: impl FnOnce(&mut Route)) -> Route {
    let mut r = Route {
        name: None,
        dnis: None,
        dnis_prefix: None,
        trunk: None,
        default: false,
        deliver_partial: false,
        deliveries: vec![folder("/x")],
    };
    f(&mut r);
    r
}

fn errs(routes: Vec<Route>) -> String {
    Routes::new(routes).validate().unwrap_err().join("\n")
}

#[test]
fn d01_validate_route_problems() {
    assert!(errs(vec![route(|_| {})]).contains("needs dnis, dnis_prefix, trunk or default"));
    assert!(
        errs(vec![route(|r| {
            r.default = true;
            r.trunk = Some("a".into());
        })])
        .contains("cannot be combined")
    );
    let e = errs(vec![
        route(|r| r.default = true),
        route(|r| r.dnis = Some("1".into())),
    ]);
    assert!(e.contains("default route must be last"), "{e}");
    assert!(
        errs(vec![route(|r| {
            r.dnis = Some("".into());
        })])
        .contains("empty match value")
    );
    assert!(
        errs(vec![route(|r| {
            r.dnis = Some("1".into());
            r.deliveries.clear();
        })])
        .contains("no deliveries")
    );
    let e = errs(vec![route(|r| {
        r.dnis = Some("1".into());
        r.deliveries = vec![folder("/a"), folder("/a")];
    })]);
    assert!(e.contains("duplicate target folder:/a"), "{e}");
    Routes::default().validate().unwrap();
}

#[test]
fn d02_validate_folder_problems() {
    let bad = DeliveryConfig::Folder(FolderConfig {
        dir: " ".into(),
        filename: "{nope}".into(),
        formats: vec![],
        create_dir: true,
    });
    let e = errs(vec![route(|r| {
        r.dnis = Some("1".into());
        r.deliveries = vec![bad];
    })]);
    for want in ["dir is empty", "formats is empty", "unknown placeholder"] {
        assert!(e.contains(want), "{want} in {e}");
    }
    let dup = DeliveryConfig::Folder(FolderConfig {
        dir: "/x".into(),
        filename: "a".into(),
        formats: vec![Format::Pdf, Format::Pdf],
        create_dir: true,
    });
    let e = errs(vec![route(|r| {
        r.dnis = Some("1".into());
        r.deliveries = vec![dup];
    })]);
    assert!(e.contains("duplicates"), "{e}");
}

fn email() -> EmailConfig {
    EmailConfig {
        host: "h".into(),
        port: None,
        security: Security::Starttls,
        tls_insecure: false,
        username: None,
        password: None,
        from: "fax@example.com".into(),
        to: vec!["a@example.com".into()],
        subject: default_subject(),
        body: default_body(),
        attachment_name: None,
        attach_tiff: false,
        timeout_secs: 30,
    }
}

fn check_email(f: impl FnOnce(&mut EmailConfig)) -> String {
    let mut e = email();
    f(&mut e);
    match Routes::new(vec![route(|r| {
        r.dnis = Some("1".into());
        r.deliveries = vec![DeliveryConfig::Email(e)];
    })])
    .validate()
    {
        Ok(()) => String::new(),
        Err(v) => v.join("\n"),
    }
}

#[test]
fn d03_validate_email_problems() {
    assert_eq!(check_email(|_| {}), "");
    assert_eq!(check_email(|e| e.port = Some(2525)), "");
    assert!(check_email(|e| e.host = "".into()).contains("host is empty"));
    assert!(check_email(|e| e.to.clear()).contains("to is empty"));
    assert!(check_email(|e| e.from = "not an address".into()).contains("invalid from"));
    assert!(check_email(|e| e.to = vec!["@@".into()]).contains("invalid recipient"));
    assert!(check_email(|e| e.username = Some("u".into())).contains("go together"));
    assert!(
        check_email(|e| {
            e.security = Security::None;
            e.username = Some("u".into());
            e.password = Some("p".into());
        })
        .contains("unencrypted")
    );
    assert!(check_email(|e| e.subject = "{x}".into()).contains("subject: unknown placeholder"));
    assert!(check_email(|e| e.body = "{".into()).contains("body: unterminated"));
    assert!(check_email(|e| e.attachment_name = Some("{y}".into())).contains("attachment_name"));
    assert!(check_email(|e| e.timeout_secs = 0).contains("timeout_secs"));
    let mut e = email();
    e.security = Security::None;
    assert_eq!(e.effective_port(), 25);
    e.security = Security::Starttls;
    assert_eq!(e.effective_port(), 587);
}

fn check_hook(f: impl FnOnce(&mut WebhookConfig)) -> String {
    let mut w = WebhookConfig {
        url: "http://127.0.0.1:9/x".into(),
        secret: None,
        signature_header: default_sig_header(),
        attach_pdf: false,
        headers: Default::default(),
        timeout_secs: 30,
    };
    f(&mut w);
    match Routes::new(vec![route(|r| {
        r.dnis = Some("1".into());
        r.deliveries = vec![DeliveryConfig::Webhook(w)];
    })])
    .validate()
    {
        Ok(()) => String::new(),
        Err(v) => v.join("\n"),
    }
}

#[test]
fn d04_validate_webhook_problems() {
    assert_eq!(check_hook(|_| {}), "");
    assert!(check_hook(|w| w.url = "ftp://x/".into()).contains("http(s)"));
    assert!(check_hook(|w| w.url = "nonsense".into()).contains("http(s)"));
    assert!(check_hook(|w| w.secret = Some("".into())).contains("secret is empty"));
    assert!(check_hook(|w| w.signature_header = "bad header".into()).contains("signature_header"));
    assert!(
        check_hook(|w| {
            w.headers.insert("bad name".into(), "v".into());
        })
        .contains("invalid header")
    );
    assert!(check_hook(|w| w.timeout_secs = 0).contains("timeout_secs"));
    let long = format!("http://example.com/{}", "a".repeat(300));
    assert!(check_hook(|w| w.url = long).contains("longer than 255"));
}

#[test]
fn d04_redact_url() {
    assert_eq!(
        redact_url("https://u:p@h.example:8443/a/b?token=1#frag"),
        "https://h.example:8443/a/b"
    );
    assert_eq!(redact_url("garbage?x=1"), "garbage");
}
