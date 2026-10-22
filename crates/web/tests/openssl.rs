//! H01, H05 and H08 checked by `openssl`, an implementation that is not ours (test plan section
//! 0: "憑證用 `openssl x509`（Docker 裡）解析，不用我們自己的程式碼驗證內容").
//!
//! These tests need the `openssl` command line tool, so they are `#[ignore]`d in a plain
//! `cargo test`. `scripts/web-openssl-docker.sh` runs them inside the Rocky 9 test image:
//!
//! ```text
//! cargo test -p web --test openssl -- --ignored --nocapture
//! ```
//!
//! Every command and what `openssl` printed is written to the output (`--nocapture`).

use std::net::IpAddr;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use chrono::Utc;
use web::config::WebConfig;
use web::server::WebServer;
use web::tls;

/// Runs `openssl args`, prints the command and the answer, returns (success, stdout, stderr).
fn openssl(args: &[&str], envs: &[(&str, &str)]) -> (bool, String, String) {
    let out = Command::new("openssl")
        .args(args)
        .envs(envs.iter().copied())
        .stdin(Stdio::null())
        .output()
        .expect("the openssl command line tool is installed");
    let (o, e) = (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    );
    println!("$ openssl {}", args.join(" "));
    for l in o.lines().chain(e.lines()) {
        println!("  {l}");
    }
    println!("  (exit {})", out.status.code().unwrap_or(-1));
    (out.status.success(), o, e)
}

fn cfg() -> WebConfig {
    WebConfig {
        enabled: true,
        ..WebConfig::default()
    }
}

/// A self-signed certificate made the way `serve` makes it (bind `0.0.0.0`, two extra names).
fn make(dir: &Path) -> tls::TlsMaterial {
    let ips: Vec<IpAddr> = vec!["127.0.0.1".parse().unwrap(), "192.0.2.10".parse().unwrap()];
    let names = tls::subject_names(
        Some("faxsrv"),
        "0.0.0.0".parse().unwrap(),
        &ips,
        &["fax.example.test".to_string(), "198.51.100.4".to_string()],
    );
    tls::load(&cfg(), dir, dir, &names, Utc::now().date_naive()).unwrap()
}

#[test]
#[ignore = "needs the openssl command line tool: scripts/web-openssl-docker.sh"]
fn h01_openssl_reads_an_ecdsa_p256_certificate_of_825_days_with_the_names_and_serverauth() {
    let dir = tempfile::tempdir().unwrap();
    let m = make(dir.path());
    let crt = m.cert_path.display().to_string();
    let key = tls::self_signed_paths(dir.path()).1.display().to_string();

    let (ok, text, _) = openssl(&["x509", "-in", &crt, "-noout", "-text"], &[]);
    assert!(ok);
    // ECDSA P-256
    assert!(
        text.contains("Public Key Algorithm: id-ecPublicKey"),
        "{text}"
    );
    assert!(
        text.contains("ASN1 OID: prime256v1") || text.contains("NIST CURVE: P-256"),
        "{text}"
    );
    assert!(
        text.contains("Signature Algorithm: ecdsa-with-SHA256"),
        "{text}"
    );
    // EKU serverAuth, and nothing a browser would object to as the only usage
    assert!(
        text.contains("X509v3 Extended Key Usage")
            && text.contains("TLS Web Server Authentication"),
        "{text}"
    );
    assert!(text.contains("Digital Signature"), "{text}");
    // SAN: host name, localhost, 127.0.0.1, the interface address (bind 0.0.0.0) and tls_names
    let san = text
        .lines()
        .skip_while(|l| !l.contains("Subject Alternative Name"))
        .nth(1)
        .unwrap_or_default()
        .to_string();
    for want in [
        "DNS:faxsrv",
        "DNS:localhost",
        "IP Address:127.0.0.1",
        "IP Address:192.0.2.10",
        "DNS:fax.example.test",
        "IP Address:198.51.100.4",
    ] {
        assert!(san.contains(want), "{want} not in the SAN: {san}");
    }
    // validity: 825 days (+-1), measured by openssl's own -checkend (exit 0: does not end
    // within that many seconds)
    let day = 86_400u64;
    let ends_after = |days: u64| {
        openssl(
            &[
                "x509",
                "-in",
                &crt,
                "-noout",
                "-checkend",
                &(days * day).to_string(),
            ],
            &[],
        )
        .0
    };
    assert!(ends_after(823), "still valid in 823 days");
    assert!(!ends_after(826), "over in 826 days");
    // it started today (UTC), not in the future: valid now
    assert!(openssl(&["x509", "-in", &crt, "-noout", "-checkend", "0"], &[]).0);
    let (_, dates, _) = openssl(
        &["x509", "-in", &crt, "-noout", "-startdate", "-enddate"],
        &[],
    );
    let today = Utc::now();
    let start = dates
        .lines()
        .find_map(|l| l.strip_prefix("notBefore="))
        .unwrap();
    let want_start = today.format("%b %e").to_string().replace("  ", " ");
    assert!(
        start.replace("  ", " ").starts_with(&want_start) || {
            // the test ran across midnight UTC
            let y = (today - chrono::Duration::days(1))
                .format("%b %e")
                .to_string();
            start.replace("  ", " ").starts_with(&y.replace("  ", " "))
        },
        "notBefore {start} is today ({want_start})"
    );
    // the key is a P-256 key that belongs to the certificate (same public key)
    let (ok, k, _) = openssl(&["pkey", "-in", &key, "-noout", "-text"], &[]);
    assert!(
        ok && (k.contains("prime256v1") || k.contains("P-256")),
        "{k}"
    );
    let (_, from_cert, _) = openssl(&["x509", "-in", &crt, "-noout", "-pubkey"], &[]);
    let (_, from_key, _) = openssl(&["pkey", "-in", &key, "-pubout"], &[]);
    assert!(
        !from_cert.is_empty() && from_cert == from_key,
        "the key does not match"
    );
    // the private key is the owner's only
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&key).unwrap().permissions().mode() & 0o777;
        println!("$ stat -c %a {key}\n  {mode:o}");
        assert_eq!(mode, 0o600);
    }
    // H02: the second start keeps the very same certificate
    let again = tls::load(&cfg(), dir.path(), dir.path(), &[], Utc::now().date_naive()).unwrap();
    assert_eq!(again.fingerprint, m.fingerprint);
}

#[test]
#[ignore = "needs the openssl command line tool: scripts/web-openssl-docker.sh"]
fn h08_the_fingerprint_we_print_is_the_one_openssl_computes() {
    let dir = tempfile::tempdir().unwrap();
    let m = make(dir.path());
    let crt = m.cert_path.display().to_string();
    let (ok, out, _) = openssl(
        &["x509", "-in", &crt, "-noout", "-fingerprint", "-sha256"],
        &[],
    );
    assert!(ok);
    // "sha256 Fingerprint=AB:CD:..." (OpenSSL 3) or "SHA256 Fingerprint=..." (1.1)
    let theirs = out
        .trim()
        .split_once("Fingerprint=")
        .map(|(_, f)| f.trim().to_string())
        .unwrap();
    println!("ours:   {}", m.fingerprint);
    println!("openssl {theirs}");
    assert_eq!(m.fingerprint, theirs);
    assert_eq!(theirs.len(), 32 * 3 - 1);
    // the same for a certificate that was made with other names (not a coincidence of one file)
    let other = tempfile::tempdir().unwrap();
    let m2 = tls::load(
        &cfg(),
        other.path(),
        other.path(),
        &["only.example.test".to_string()],
        Utc::now().date_naive(),
    )
    .unwrap();
    let (_, out2, _) = openssl(
        &[
            "x509",
            "-in",
            &m2.cert_path.display().to_string(),
            "-noout",
            "-fingerprint",
            "-sha256",
        ],
        &[],
    );
    assert!(out2.contains(&m2.fingerprint), "{out2}");
    assert_ne!(m.fingerprint, m2.fingerprint);
}

/// The environment that lets the openssl client speak TLS 1.1 at all (distributions switch it
/// off in `openssl.cnf`): without that a refusal could come from the client.
const LEGACY: &[(&str, &str)] = &[("OPENSSL_CONF", "/dev/null")];

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the openssl command line tool: scripts/web-openssl-docker.sh"]
async fn h05_tls_1_2_and_1_3_connect_and_1_1_and_1_0_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let m = make(dir.path());
    let app = axum::Router::new().route("/", axum::routing::get(|| async { "ok" }));
    let srv = WebServer::start("127.0.0.1:0".parse().unwrap(), app, Some(m.config.clone()))
        .await
        .unwrap();
    let target = format!("127.0.0.1:{}", srv.local_addr().port());

    // what the client can do: a control server that speaks only TLS 1.1 (or TLS 1.0), with a
    // throw-away RSA certificate and a cipher without a signed key exchange (some builds
    // refuse the MD5+SHA1 signature of TLS 1.0/1.1 key exchanges)
    let ckey = dir.path().join("control.key").display().to_string();
    let ccrt = dir.path().join("control.crt").display().to_string();
    let (ok, _, _) = openssl(
        &[
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            &ckey,
            "-out",
            &ccrt,
            "-subj",
            "/CN=control",
            "-days",
            "2",
        ],
        &[],
    );
    assert!(ok);
    const OLD_CIPHER: &str = "AES128-SHA:@SECLEVEL=0";
    for (flag, name) in [("-tls1_1", "TLSv1.1"), ("-tls1", "TLSv1")] {
        let port = free_port();
        let mut control = Command::new("openssl")
            .args([
                "s_server",
                "-accept",
                &port.to_string(),
                "-cert",
                &ccrt,
                "-key",
                &ckey,
                flag,
                "-cipher",
                OLD_CIPHER,
                "-quiet",
            ])
            .envs(LEGACY.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("openssl s_server");
        tokio::time::sleep(Duration::from_millis(700)).await;
        let (ok, _, e) = tokio::task::spawn_blocking(move || {
            openssl(
                &[
                    "s_client",
                    "-connect",
                    &format!("127.0.0.1:{port}"),
                    flag,
                    "-cipher",
                    OLD_CIPHER,
                    "-brief",
                ],
                LEGACY,
            )
        })
        .await
        .unwrap();
        let _ = control.kill();
        let _ = control.wait();
        assert!(
            ok && e.contains(&format!("Protocol version: {name}")),
            "control: this openssl cannot speak {name}, a refusal below would prove nothing: {e}"
        );
    }

    // ours: 1.3 and 1.2 connect, and the answer is the protocol that was asked for
    for (flag, name) in [("-tls1_3", "TLSv1.3"), ("-tls1_2", "TLSv1.2")] {
        let t = target.clone();
        let (ok, _, e) = tokio::task::spawn_blocking(move || {
            openssl(&["s_client", "-connect", &t, flag, "-brief"], &[])
        })
        .await
        .unwrap();
        assert!(
            ok && e.contains(&format!("Protocol version: {name}")),
            "{name}: {e}"
        );
        assert!(
            e.contains("CN = sip-faxd") || e.contains("CN=sip-faxd"),
            "{e}"
        );
    }
    // no flag: the best one
    let t = target.clone();
    let (ok, _, e) =
        tokio::task::spawn_blocking(move || openssl(&["s_client", "-connect", &t, "-brief"], &[]))
            .await
            .unwrap();
    assert!(ok && e.contains("Protocol version: TLSv1.3"), "{e}");

    // ours: 1.1 and 1.0 are refused, by the same client that just spoke them to the control
    for (flag, name) in [("-tls1_1", "TLSv1.1"), ("-tls1", "TLSv1")] {
        let t = target.clone();
        let (ok, o, e) = tokio::task::spawn_blocking(move || {
            openssl(
                &[
                    "s_client", "-connect", &t, flag, "-cipher", OLD_CIPHER, "-brief",
                ],
                LEGACY,
            )
        })
        .await
        .unwrap();
        assert!(!ok, "{name} must be refused");
        let all = format!("{o}{e}");
        assert!(
            !all.contains(&format!("Protocol version: {name}")),
            "{name} was negotiated: {all}"
        );
        assert!(
            all.contains("alert protocol version")
                || all.contains("alert handshake failure")
                || all.contains("wrong version number")
                || all.contains("unsupported protocol"),
            "{name}: refused, but not by a protocol alert: {all}"
        );
    }
    srv.shutdown(Duration::from_secs(1)).await;
}
