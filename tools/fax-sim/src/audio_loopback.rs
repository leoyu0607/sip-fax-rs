//! In-process audio-domain (G.711-style, 8 kHz int16) loopback between two
//! SpanDSP `fax_state_t` instances, exchanging 160-sample blocks.

use crate::spandsp::{FaxModem, T30Config};
use crate::{Fnv64, STEP_MS, STEP_SAMPLES, SessionResult};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct AudioLoopbackConfig {
    pub tx_file: PathBuf,
    pub rx_file: PathBuf,
    pub ecm: bool,
    pub max_sim_ms: u64,
}

impl AudioLoopbackConfig {
    pub fn new(tx_file: PathBuf, rx_file: PathBuf) -> Self {
        Self {
            tx_file,
            rx_file,
            ecm: false,
            max_sim_ms: 600_000,
        }
    }
}

pub fn run(cfg: &AudioLoopbackConfig) -> SessionResult {
    let caller_cfg = T30Config {
        tx_file: Some(&cfg.tx_file),
        rx_file: None,
        ecm: cfg.ecm,
        ident: "0000000001",
    };
    let answerer_cfg = T30Config {
        tx_file: None,
        rx_file: Some(&cfg.rx_file),
        ecm: cfg.ecm,
        ident: "0000000002",
    };
    let mut caller = FaxModem::new(true, &caller_cfg).expect("caller init");
    let mut answerer = FaxModem::new(false, &answerer_cfg).expect("answerer init");
    let mut hash = Fnv64::default();
    let mut now_ms = 0u64;
    let mut timed_out = false;
    let mut buf_c = [0i16; STEP_SAMPLES];
    let mut buf_a = [0i16; STEP_SAMPLES];

    loop {
        if caller.completion_code().is_some() && answerer.completion_code().is_some() {
            break;
        }
        if now_ms >= cfg.max_sim_ms {
            timed_out = true;
            break;
        }
        // each side's audio output block is the other side's input block
        buf_c.fill(0);
        buf_a.fill(0);
        let nc = caller.tx(&mut buf_c);
        let na = answerer.tx(&mut buf_a);
        for s in &buf_c[..nc] {
            hash.write(&s.to_le_bytes());
        }
        for s in &buf_a[..na] {
            hash.write(&s.to_le_bytes());
        }
        answerer.rx(&mut buf_c);
        caller.rx(&mut buf_a);
        now_ms += STEP_MS;
    }

    SessionResult {
        caller_code: caller.completion_code(),
        answerer_code: answerer.completion_code(),
        sim_ms: now_ms,
        timed_out,
        packets_sent: 0,
        packets_dropped: 0,
        packet_hash: hash.finish(),
    }
}
