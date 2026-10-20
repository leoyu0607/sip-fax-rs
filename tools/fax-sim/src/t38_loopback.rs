//! In-process T.38 loopback between two SpanDSP T.38 terminals.

use crate::spandsp::{T30Config, T38Terminal};
use crate::{Fnv64, STEP_MS, STEP_SAMPLES, SessionResult, SplitMix64};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct T38LoopbackConfig {
    /// TIFF the caller transmits (may be nonexistent to exercise error paths).
    pub tx_file: PathBuf,
    /// TIFF the answerer writes received pages into.
    pub rx_file: PathBuf,
    pub ecm: bool,
    /// Per-copy loss probability in permille (50 = 5%). 0 disables loss.
    pub loss_permille: u32,
    pub seed: u64,
    /// Simulated-time cap; the run never takes longer than this.
    pub max_sim_ms: u64,
}

impl T38LoopbackConfig {
    pub fn new(tx_file: PathBuf, rx_file: PathBuf) -> Self {
        Self {
            tx_file,
            rx_file,
            ecm: false,
            loss_permille: 0,
            seed: 1,
            max_sim_ms: 600_000,
        }
    }
}

struct Queued {
    data: Vec<u8>,
    seq: u16,
}

/// Run a caller->answerer T.38 session on a simulated clock (20 ms steps).
///
/// Packets emitted in step N are delivered at the start of step N+1 (20 ms
/// one-way latency). Each of the `count` copies SpanDSP asks for is subject to
/// loss independently.
pub fn run(cfg: &T38LoopbackConfig) -> SessionResult {
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
    let mut side = [
        T38Terminal::new(true, &caller_cfg).expect("caller init"),
        T38Terminal::new(false, &answerer_cfg).expect("answerer init"),
    ];
    let mut next_seq = [0u16; 2];
    let mut inflight: [Vec<Queued>; 2] = [Vec::new(), Vec::new()]; // indexed by destination
    let mut rng = SplitMix64::new(cfg.seed);
    let mut hash = Fnv64::default();
    let (mut sent, mut dropped) = (0u64, 0u64);
    let mut now_ms = 0u64;
    let mut timed_out = false;

    loop {
        if side[0].completion_code().is_some() && side[1].completion_code().is_some() {
            break;
        }
        if now_ms >= cfg.max_sim_ms {
            timed_out = true;
            break;
        }
        // deliver last step's packets
        for dst in 0..2 {
            for q in std::mem::take(&mut inflight[dst]) {
                side[dst].rx_packet(&q.data, q.seq);
            }
        }
        for s in &mut side {
            s.advance(STEP_SAMPLES);
        }
        now_ms += STEP_MS;
        for src in 0..2 {
            for pkt in side[src].take_packets() {
                let seq = next_seq[src];
                next_seq[src] = seq.wrapping_add(1);
                hash.write_u64(src as u64);
                hash.write_u64(u64::from(seq));
                hash.write_u64(pkt.count as u64);
                hash.write(&pkt.data);
                for copy in 0..pkt.count.max(1) {
                    sent += 1;
                    let lost = cfg.loss_permille > 0 && rng.next_permille() < cfg.loss_permille;
                    hash.write(&[copy as u8, u8::from(lost)]);
                    if lost {
                        dropped += 1;
                    } else {
                        inflight[1 - src].push(Queued {
                            data: pkt.data.clone(),
                            seq,
                        });
                    }
                }
            }
        }
    }

    SessionResult {
        caller_code: side[0].completion_code(),
        answerer_code: side[1].completion_code(),
        sim_ms: now_ms,
        timed_out,
        packets_sent: sent,
        packets_dropped: dropped,
        packet_hash: hash.finish(),
    }
}
