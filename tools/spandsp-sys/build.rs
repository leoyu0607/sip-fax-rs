use std::path::PathBuf;
use std::process::Command;

/// Directory holding SpanDSP's reference UDPTL (`udptl.c`, `udptl.h`), copied there by
/// `docker/test-rocky9.Dockerfile`. libspandsp itself does not export UDPTL.
const UDPTL_DIR: &str = "/usr/local/share/spandsp-udptl";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");
    println!("cargo:rerun-if-env-changed=SPANDSP_UDPTL_DIR");
    let lib = match pkg_config::Config::new()
        .atleast_version("3.1")
        .probe("spandsp")
    {
        Ok(lib) => {
            println!("cargo:rustc-env=SPANDSP_VERSION={}", lib.version);
            lib
        }
        Err(e) => panic!(
            "\n\nspandsp-sys: cannot find SpanDSP >= 3.1 via pkg-config ({e}).\n\
             SpanDSP is only needed by the test tooling under tools/.\n\
             Run the tools workspace inside the prepared Rocky 9 container with:\n\n    \
             scripts/e2e-docker.sh\n\n"
        ),
    };

    // Reference UDPTL (U01): compile SpanDSP's own tests/udptl.c into a static library.
    let dir =
        PathBuf::from(std::env::var("SPANDSP_UDPTL_DIR").unwrap_or_else(|_| UDPTL_DIR.into()));
    let src = dir.join("udptl.c");
    println!("cargo:rerun-if-changed={}", src.display());
    if !src.exists() {
        panic!(
            "\n\nspandsp-sys: {} not found. The reference UDPTL source is installed by \
             docker/test-rocky9.Dockerfile; rebuild the image with scripts/e2e-docker.sh.\n\n",
            src.display()
        );
    }
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let obj = out.join("udptl.o");
    let mut cc = Command::new(std::env::var("CC").unwrap_or_else(|_| "cc".into()));
    cc.args(["-c", "-O2", "-fPIC", "-w", "-DHAVE_STDBOOL_H"])
        .arg(format!("-I{}", dir.display()))
        .arg(&src)
        .arg("-o")
        .arg(&obj);
    for inc in &lib.include_paths {
        cc.arg(format!("-I{}", inc.display()));
    }
    let status = cc.status().expect("run C compiler");
    assert!(status.success(), "compiling reference udptl.c failed");
    let archive = out.join("libspandsp_udptl_ref.a");
    let _ = std::fs::remove_file(&archive);
    let status = Command::new("ar")
        .arg("crs")
        .arg(&archive)
        .arg(&obj)
        .status()
        .expect("run ar");
    assert!(status.success(), "ar failed");
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=spandsp_udptl_ref");
}
