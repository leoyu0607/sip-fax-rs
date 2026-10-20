//! `gen-t38-vectors <out-dir>`: runs real SpanDSP T.38 sessions (non-ECM / ECM, T.38 version 0
//! and 3) through the loopback and dumps every IFP packet SpanDSP emits into length-prefixed
//! bundle files for the `t38` crate's golden-vector tests (I01).
//!
//! Bundle format (`*.ifpb`): a sequence of records `dir:u8 | len:u32 big-endian | ifp[len]`,
//! with `dir` 0 = emitted by the calling terminal, 1 = by the answering terminal.

use fax_sim::t38_loopback::{T38LoopbackConfig, run_detailed};
use fax_sim::tiff_writer::{self, PageSpec};
use std::path::{Path, PathBuf};

fn main() {
    let out: PathBuf = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            eprintln!("usage: gen-t38-vectors <out-dir>");
            std::process::exit(2);
        });
    std::fs::create_dir_all(&out).expect("create out dir");
    let tmp = std::env::temp_dir().join(format!("gen-t38-vectors-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).expect("tmp dir");
    let tx = tmp.join("in.tif");
    // Two pages with different resolutions so that MPS / EOP and a DCS change are exercised.
    tiff_writer::write_file(&tx, &[PageSpec::standard(300), PageSpec::fine(400)])
        .expect("write input");

    println!("SpanDSP {}", spandsp_sys::SPANDSP_VERSION);
    // (name, ecm, T.38 version, modem restriction)
    for (name, ecm, version, modems) in [
        ("non_ecm_v0", false, 0u8, None),
        ("non_ecm_v3", false, 3, None),
        ("ecm_v0", true, 0, None),
        ("ecm_v3", true, 3, None),
        (
            "non_ecm_v0_v29",
            false,
            0,
            Some(spandsp_sys::T30_SUPPORT_V29),
        ),
        ("ecm_v3_v29", true, 3, Some(spandsp_sys::T30_SUPPORT_V29)),
        (
            "non_ecm_v0_v27ter",
            false,
            0,
            Some(spandsp_sys::T30_SUPPORT_V27TER),
        ),
        (
            "ecm_v3_v27ter",
            true,
            3,
            Some(spandsp_sys::T30_SUPPORT_V27TER),
        ),
    ] {
        let mut cfg = T38LoopbackConfig::new(tx.clone(), tmp.join(format!("{name}.tif")));
        cfg.ecm = ecm;
        cfg.t38_version = Some(version);
        cfg.modems = modems;
        cfg.capture_ifp = true;
        let o = run_detailed(&cfg);
        assert!(!o.result.timed_out, "{name}: {:?}", o.result);
        assert_eq!(o.result.caller_code, Some(0), "{name}: {:?}", o.result);
        assert_eq!(o.result.answerer_code, Some(0), "{name}: {:?}", o.result);
        let mut bytes = Vec::new();
        for p in &o.captured {
            bytes.push(u8::from(!p.from_caller));
            bytes.extend_from_slice(&(p.ifp.len() as u32).to_be_bytes());
            bytes.extend_from_slice(&p.ifp);
        }
        let path: &Path = &out;
        std::fs::write(path.join(format!("{name}.ifpb")), &bytes).expect("write bundle");
        println!(
            "{name}: ecm={ecm} t38_version={version} packets={} bytes={} sim_ms={}",
            o.captured.len(),
            bytes.len(),
            o.result.sim_ms
        );
    }
    let _ = std::fs::remove_dir_all(&tmp);
}
