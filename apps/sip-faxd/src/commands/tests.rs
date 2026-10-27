//! F05: the route summary of `config check`; the masking of database errors.
use super::*;

#[test]
fn mask_secrets_hides_the_url_and_the_password_alone() {
    let url = "postgres://fax:p%40ss%20w0rd@db.example.com:5432/fax";
    let msg = format!("cannot connect to {url}; password 'p@ss w0rd' refused (raw p%40ss%20w0rd)");
    let m = mask_secrets(&msg, url);
    assert!(
        m.contains("postgres://fax:***@db.example.com:5432/fax"),
        "{m}"
    );
    assert!(
        !m.contains("p@ss w0rd") && !m.contains("p%40ss%20w0rd"),
        "{m}"
    );
    // no password: nothing to hide, and a short one is left alone
    assert_eq!(mask_secrets("x", "sqlite://data/fax.db"), "x");
    assert_eq!(mask_secrets("ab ab", "mysql://u:ab@h/d"), "ab ab");
}

#[test]
fn f05_the_route_summary_lists_everything_and_hides_every_secret() {
    let routes: delivery::Routes = toml::from_str(
            "[[routes]]\nname = \"sales\"\ndnis_prefix = \"2557\"\ntrunk = \"cht\"\n\
             [[routes.deliveries]]\nkind = \"folder\"\ndir = \"/srv/in\"\nformats = [\"pdf\"]\n\
             [[routes.deliveries]]\nkind = \"email\"\nhost = \"smtp.example.com\"\nusername = \"fax\"\npassword = \"mailpw-123\"\nfrom = \"fax@example.com\"\nto = [\"a@example.com\", \"b@example.com\"]\n\
             [[routes.deliveries]]\nkind = \"webhook\"\nurl = \"https://hookuser:urlpw-456@example.com/h?token=tok-789\"\nsecret = \"hmac-secret-000\"\nattach_pdf = true\n\
             [[routes]]\ndefault = true\ndeliver_partial = true\n\
             [[routes.deliveries]]\nkind = \"webhook\"\nurl = \"http://example.com/open\"\n",
        )
        .unwrap();
    routes.validate().unwrap();
    let text = routes_summary(&routes).join("\n");
    for must in [
        "route 1 (sales): dnis_prefix = 2557 and trunk = cht; partial faxes not delivered",
        "folder /srv/in (filename {date}_{time}_{ani}_{id}, pdf)",
        "email smtp.example.com:587 Starttls, user fax password ***, from fax@example.com, to a@example.com, b@example.com",
        "webhook https://hookuser:***@example.com/h?token=***, HMAC X-Sipfax-Signature secret ***, with PDF",
        "route 2: default (everything else); partial faxes delivered",
        "webhook http://example.com/open, unsigned",
    ] {
        assert!(text.contains(must), "missing {must:?} in:\n{text}");
    }
    for secret in ["mailpw-123", "urlpw-456", "tok-789", "hmac-secret-000"] {
        assert!(!text.contains(secret), "{secret} leaked:\n{text}");
    }
}

/// K03: the drivers' own messages (as seen on the real servers) get the right hint.
#[test]
fn k03_connection_hints_tell_the_causes_apart() {
    let cases = [
        // (message, a part of the hint)
        (
            "password authentication failed for user \"u\"",
            "user name or the password",
        ),
        ("Login failed for user 'u'.", "user name or the password"),
        (
            "Access denied for user 'u'@'172.17.0.1' (using password: YES)",
            "user name or the password",
        ),
        // SQL Server says "The login failed" with a missing database too: the database wins
        (
            "Cannot open database \"x\" requested by the login. The login failed.",
            "database does not exist",
        ),
        (
            "error returned from database: database \"x\" does not exist",
            "database does not exist",
        ),
        ("Unknown database 'x'", "database does not exist"),
        (
            "error with configuration: Failed to saslprep password: Error(ProhibitedCharacter('x'))",
            "SASLprep",
        ),
        (
            "invalid peer certificate: UnknownIssuer",
            "trust_server_certificate",
        ),
        (
            "An error occurred during the attempt of performing I/O: Connection refused (os error 111)",
            "nothing listens",
        ),
        (
            "failed to lookup address information: Name or service not known",
            "cannot be resolved",
        ),
        (
            "pool timed out while waiting for an open connection",
            "firewall",
        ),
        ("no answer within 15 s", "firewall"),
    ];
    for (msg, want) in cases {
        let h = connection_hint(msg).unwrap_or_else(|| panic!("no hint for {msg:?}"));
        assert!(h.contains(want), "{msg:?} -> {h}");
    }
    assert_eq!(connection_hint("something else entirely"), None);
}
