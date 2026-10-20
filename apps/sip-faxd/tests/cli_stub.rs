//! B05: `sip-faxd serve` is a stub that must fail with exit code 2.

use std::process::Command;

#[test]
fn b05_serve_is_not_implemented() {
    let out = Command::new(env!("CARGO_BIN_EXE_sip-faxd"))
        .arg("serve")
        .output()
        .expect("run sip-faxd");
    assert_eq!(out.status.code(), Some(2), "exit code");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not implemented"), "stderr: {stderr}");
}

#[test]
fn b03_version_output_matches_cargo_version() {
    let out = Command::new(env!("CARGO_BIN_EXE_sip-faxd"))
        .arg("--version")
        .output()
        .expect("run sip-faxd");
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("sip-faxd {}", env!("CARGO_PKG_VERSION"))
    );
}
