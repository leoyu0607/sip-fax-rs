//! Embeds the built web interface (`web-ui/dist`, spec 6) into the crate: every file becomes a
//! `(path, content type, bytes)` entry of `$OUT_DIR/ui_assets.rs`. Without a `dist` the table is
//! empty and the server shows a page saying the interface was not built, so `cargo build` and
//! the API tests work without Node. Release builds set `SIPFAX_REQUIRE_WEB_UI=1`: then a missing
//! `dist` is an error.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "txt" => "text/plain; charset=utf-8",
        "map" => "application/json",
        _ => "application/octet-stream",
    }
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else if p.is_file() {
            out.push(p);
        }
    }
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=SIPFAX_WEB_UI_DIST");
    println!("cargo:rerun-if-env-changed=SIPFAX_REQUIRE_WEB_UI");
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let ui = manifest.join("../../web-ui");
    let dist = std::env::var_os("SIPFAX_WEB_UI_DIST")
        .map(PathBuf::from)
        .unwrap_or_else(|| ui.join("dist"));
    let require = std::env::var("SIPFAX_REQUIRE_WEB_UI").is_ok_and(|v| v == "1");

    let mut files = Vec::new();
    if dist.join("index.html").is_file() {
        // the whole directory: a new build changes it
        println!("cargo:rerun-if-changed={}", dist.display());
        walk(&dist, &mut files);
        files.sort();
    } else {
        if require {
            panic!(
                "the web interface is not built ({} has no index.html): run `npm ci && npm run build` in web-ui/",
                dist.display()
            );
        }
        // not built: look again when web-ui/dist appears (web-ui itself changes then)
        if ui.is_dir() {
            println!(
                "cargo:rerun-if-changed={}",
                ui.join("package.json").display()
            );
            println!("cargo:rerun-if-changed={}", ui.display());
        }
        println!(
            "cargo:warning=web-ui/dist not found: the web interface is not embedded (run `npm ci && npm run build` in web-ui/)"
        );
    }

    let mut src = String::from(
        "/// The files of the built interface: path (no leading `/`), content type, bytes.\n",
    );
    src.push_str("pub static ASSETS: &[(&str, &str, &[u8])] = &[\n");
    for f in &files {
        let rel = f
            .strip_prefix(&dist)
            .expect("under dist")
            .to_string_lossy()
            .replace('\\', "/");
        let _ = writeln!(
            src,
            "    ({rel:?}, {:?}, include_bytes!({:?})),",
            content_type(f),
            f.display().to_string()
        );
    }
    src.push_str("];\n");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("ui_assets.rs");
    std::fs::write(out, src).expect("write ui_assets.rs");
}
