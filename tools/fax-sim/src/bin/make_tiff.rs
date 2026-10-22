//! `make-tiff OUT.tif PAGES HEIGHT [standard|fine]`
//!
//! Writes a multi-page test TIFF (the pattern of `fax_sim::tiff_writer`) for `sip-uac --fax`.
//! Every page is HEIGHT lines tall: taller pages take SpanDSP longer to send, which gives a
//! watcher (the web E2E test U11) time to see each page arrive.

use fax_sim::tiff_writer::{PageSpec, write_file};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let (Some(out), Some(pages), Some(height)) = (
        a.first(),
        a.get(1).and_then(|v| v.parse::<usize>().ok()),
        a.get(2).and_then(|v| v.parse::<u32>().ok()),
    ) else {
        eprintln!("usage: make-tiff OUT.tif PAGES HEIGHT [standard|fine]");
        std::process::exit(2);
    };
    let fine = a.get(3).map(String::as_str) != Some("standard");
    let specs: Vec<PageSpec> = (0..pages)
        .map(|i| {
            let h = height + 40 * i as u32;
            if fine {
                PageSpec::fine(h)
            } else {
                PageSpec::standard(h)
            }
        })
        .collect();
    if let Err(e) = write_file(std::path::Path::new(out), &specs) {
        eprintln!("make-tiff: {e}");
        std::process::exit(1);
    }
}
