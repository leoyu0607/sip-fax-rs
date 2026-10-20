fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");
    match pkg_config::Config::new()
        .atleast_version("3.1")
        .probe("spandsp")
    {
        Ok(lib) => println!("cargo:rustc-env=SPANDSP_VERSION={}", lib.version),
        Err(e) => panic!(
            "\n\nspandsp-sys: cannot find SpanDSP >= 3.1 via pkg-config ({e}).\n\
             SpanDSP is only needed by the test tooling under tools/.\n\
             Run the tools workspace inside the prepared Rocky 9 container with:\n\n    \
             scripts/e2e-docker.sh\n\n"
        ),
    }
}
