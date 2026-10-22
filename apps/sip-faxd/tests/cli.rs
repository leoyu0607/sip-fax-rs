//! B03 / B05: command line behaviour of the sip-faxd executable.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_sip-faxd"))
}

fn example_config() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../conf/sip-faxd.toml.example")
}

fn temp_file(name: &str, content: &str) -> PathBuf {
    let dir =
        PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, content).unwrap();
    p
}

#[test]
fn b03_version_output_matches_cargo_version() {
    let out = bin().arg("--version").output().expect("run sip-faxd");
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("sip-faxd {}", env!("CARGO_PKG_VERSION"))
    );
}

/// B05 (M1d): `serve` is real now. With an invalid configuration it exits with code 2 and
/// says which file and which key is wrong.
#[test]
fn b05_serve_with_an_invalid_config_exits_non_zero_with_a_clear_message() {
    let cfg = temp_file(
        "bad-key.toml",
        "[sip]\nbind = \"127.0.0.1:5060\"\nbogus_option = 1\n",
    );
    let out = bin()
        .arg("serve")
        .arg("--config")
        .arg(&cfg)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "exit code");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("sip-faxd: error:"), "stderr: {stderr}");
    assert!(stderr.contains("bad-key.toml"), "names the file: {stderr}");
    assert!(stderr.contains("bogus_option"), "names the key: {stderr}");
    assert!(out.stdout.is_empty(), "nothing on stdout");

    // semantic errors are reported with the trunk name
    let cfg = temp_file(
        "bad-trunk.toml",
        "[[trunks]]\nname = \"cht\"\npeer = \"203.0.113.10:5060\"\nallowed_sources = [\"not-a-cidr\"]\n",
    );
    let out = bin()
        .arg("serve")
        .arg("--config")
        .arg(&cfg)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("trunk \"cht\""), "{stderr}");
    assert!(stderr.contains("not-a-cidr"), "{stderr}");

    // a missing file
    let out = bin()
        .args(["serve", "--config", "/nonexistent/sip-faxd.toml"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("cannot read configuration"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// B05 (M1d): `config check` accepts the shipped example.
#[test]
fn b05_config_check_accepts_the_example_config() {
    let out = bin()
        .args(["config", "check", "--config"])
        .arg(example_config())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("configuration OK"), "{stdout}");
    // the startup settings only (ADR-060): the runtime ones live in the database
    assert!(
        stdout.contains("none in this file, they live in the database"),
        "{stdout}"
    );
    assert!(
        !stdout.contains("warning:"),
        "the example has no warnings: {stdout}"
    );
}

/// `config check` rejects what `serve` would reject.
#[test]
fn b05_config_check_rejects_a_broken_config() {
    let cfg = temp_file("no-source.toml", "[[trunks]]\nname = \"x\"\n");
    let out = bin()
        .args(["config", "check", "--config"])
        .arg(&cfg)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("trunk \"x\""), "{stderr}");
    assert!(stderr.contains("allowed_sources"), "{stderr}");
}

/// Without `--config` the file is looked up next to the executable.
#[test]
fn b05_default_config_path_is_conf_next_to_the_executable() {
    let out = bin().arg("serve").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("conf"), "{stderr}");
    assert!(stderr.contains("sip-faxd.toml"), "{stderr}");
}
