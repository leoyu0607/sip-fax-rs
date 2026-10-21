//! In-process T.38 loopback between two SpanDSP T.38 terminals.
//!
//! Two transport models:
//!
//! * **raw** (`udptl: None`, the M0 model): SpanDSP's IFP packets are handed over directly; each
//!   of the `count` copies SpanDSP asks for is subject to loss independently and there is no
//!   error recovery.
//! * **UDPTL** (`udptl: Some(..)`, M2a/X06): every packet SpanDSP emits (and every requested
//!   copy of it, as FreeSWITCH does) becomes one UDPTL datagram built by the `t38` crate with
//!   the configured redundancy or FEC. Loss is applied per datagram; the receiving side runs
//!   the `t38` crate's [`UdptlReceiver`] and feeds recovered IFP packets, in order and with
//!   their original sequence numbers, to SpanDSP.

use crate::spandsp::{T30Config, T38Terminal};
use crate::{Fnv64, STEP_MS, STEP_SAMPLES, SessionResult, SplitMix64};
use std::path::PathBuf;
use t38::{
    ErrorRecovery, Ifp, ReceiverConfig, ReceiverStats, RxEvent, Source, T38Version, UdptlReceiver,
    UdptlSender,
};

/// UDPTL layer settings.
#[derive(Debug, Clone, Copy)]
pub struct UdptlLayer {
    /// Error recovery added by the sending side of both terminals.
    pub recovery: ErrorRecovery,
}

#[derive(Debug, Clone)]
pub struct T38LoopbackConfig {
    /// TIFF the caller transmits (may be nonexistent to exercise error paths).
    pub tx_file: PathBuf,
    /// TIFF the answerer writes received pages into.
    pub rx_file: PathBuf,
    pub ecm: bool,
    /// Loss probability in permille (50 = 5%): per packet copy (raw model) or per UDPTL
    /// datagram (UDPTL model). 0 disables loss.
    pub loss_permille: u32,
    pub seed: u64,
    /// Simulated-time cap; the run never takes longer than this.
    pub max_sim_ms: u64,
    /// T.38 version configured on both SpanDSP terminals (`None` keeps SpanDSP's default, 0).
    pub t38_version: Option<u8>,
    /// UDPTL layer; `None` selects the raw model.
    pub udptl: Option<UdptlLayer>,
    /// `T30_SUPPORT_*` modem mask for both terminals (`None` = V.27ter, V.29 and V.17).
    pub modems: Option<i32>,
    /// Record every IFP packet SpanDSP emits (see [`LoopbackOutcome::captured`]).
    pub capture_ifp: bool,
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
            t38_version: None,
            udptl: None,
            modems: None,
            capture_ifp: false,
        }
    }
}

/// One IFP packet as emitted by SpanDSP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedIfp {
    /// True if emitted by the calling (transmitting) terminal.
    pub from_caller: bool,
    /// The raw IFP octets handed to the transmit handler.
    pub ifp: Vec<u8>,
}

/// Statistics of the UDPTL layer, per direction (index 0 = receiver of the caller's packets,
/// i.e. the answerer side; index 1 = the caller side).
#[derive(Debug, Clone, Copy, Default)]
pub struct UdptlOutcome {
    /// Receiver counters for datagrams flowing caller -> answerer.
    pub to_answerer: ReceiverStats,
    /// Receiver counters for datagrams flowing answerer -> caller.
    pub to_caller: ReceiverStats,
    /// Number of `Gap` events (unrecoverable losses) over both directions.
    pub gap_events: u64,
}

/// Everything a loopback run produces.
#[derive(Debug, Clone)]
pub struct LoopbackOutcome {
    pub result: SessionResult,
    /// Empty unless `capture_ifp` was set.
    pub captured: Vec<CapturedIfp>,
    /// `Some` in the UDPTL model.
    pub udptl: Option<UdptlOutcome>,
}

struct Queued {
    data: Vec<u8>,
    seq: u16,
}

/// Run a caller->answerer T.38 session on a simulated clock (20 ms steps).
///
/// Packets emitted in step N are delivered at the start of step N+1 (20 ms
/// one-way latency).
pub fn run(cfg: &T38LoopbackConfig) -> SessionResult {
    run_detailed(cfg).result
}

/// Like [`run`] but also returns captured IFP packets and UDPTL statistics.
pub fn run_detailed(cfg: &T38LoopbackConfig) -> LoopbackOutcome {
    let caller_cfg = T30Config {
        tx_file: Some(&cfg.tx_file),
        rx_file: None,
        ecm: cfg.ecm,
        ident: "0000000001",
        modems: cfg.modems,
        ..T30Config::default()
    };
    let answerer_cfg = T30Config {
        tx_file: None,
        rx_file: Some(&cfg.rx_file),
        ecm: cfg.ecm,
        ident: "0000000002",
        modems: cfg.modems,
        ..T30Config::default()
    };
    let mut side = [
        T38Terminal::new(true, &caller_cfg).expect("caller init"),
        T38Terminal::new(false, &answerer_cfg).expect("answerer init"),
    ];
    if let Some(v) = cfg.t38_version {
        for s in &mut side {
            s.set_t38_version(i32::from(v));
        }
    }
    let version = T38Version::from_number(u32::from(cfg.t38_version.unwrap_or(0)))
        .expect("t38_version in 0..=3");
    let mut next_seq = [0u16; 2];
    // UDPTL model state, indexed by source side.
    let mut senders: [Option<UdptlSender>; 2] = [None, None];
    let mut receivers: [UdptlReceiver; 2] = [
        UdptlReceiver::new(ReceiverConfig::default()),
        UdptlReceiver::new(ReceiverConfig::default()),
    ]; // indexed by destination
    if let Some(l) = &cfg.udptl {
        senders = [
            Some(UdptlSender::new(l.recovery, 0)),
            Some(UdptlSender::new(l.recovery, 0)),
        ];
    }
    let mut gap_events = 0u64;
    let mut inflight: [Vec<Queued>; 2] = [Vec::new(), Vec::new()]; // raw model, by destination
    let mut inflight_dg: [Vec<Vec<u8>>; 2] = [Vec::new(), Vec::new()]; // UDPTL model
    let mut captured = Vec::new();
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
            let datagrams = std::mem::take(&mut inflight_dg[dst]);
            if datagrams.is_empty() {
                continue;
            }
            let mut events = Vec::new();
            for dg in &datagrams {
                // A malformed datagram cannot be produced by our own sender.
                events.extend(receivers[dst].on_datagram(dg).expect("own datagram parses"));
            }
            // Nothing is reordered in this model, so release anything still held.
            events.extend(receivers[dst].flush());
            for e in events {
                match e {
                    RxEvent::Packet { seq, ifp, source } => {
                        let data = if source == Source::Fec {
                            // FEC-rebuilt packets may be zero padded to the set's longest length.
                            match Ifp::decode_prefix(&ifp, version) {
                                Ok((_, used)) => ifp[..used].to_vec(),
                                Err(_) => ifp,
                            }
                        } else {
                            ifp
                        };
                        side[dst].rx_packet(&data, seq);
                    }
                    RxEvent::Gap { .. } => gap_events += 1,
                }
            }
        }
        for s in &mut side {
            s.advance(STEP_SAMPLES);
        }
        now_ms += STEP_MS;
        for src in 0..2 {
            for pkt in side[src].take_packets() {
                if cfg.capture_ifp {
                    captured.push(CapturedIfp {
                        from_caller: src == 0,
                        ifp: pkt.data.clone(),
                    });
                }
                if let Some(tx) = senders[src].as_mut() {
                    // One UDPTL datagram (own sequence number) per requested copy.
                    for copy in 0..pkt.count.max(1) {
                        let dg = tx.build(&pkt.data).expect("non-empty IFP");
                        sent += 1;
                        let lost = cfg.loss_permille > 0 && rng.next_permille() < cfg.loss_permille;
                        hash.write_u64(src as u64);
                        hash.write(&dg);
                        hash.write(&[copy as u8, u8::from(lost)]);
                        if lost {
                            dropped += 1;
                        } else {
                            inflight_dg[1 - src].push(dg);
                        }
                    }
                    continue;
                }
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

    let udptl = cfg.udptl.map(|_| UdptlOutcome {
        to_answerer: receivers[1].stats(),
        to_caller: receivers[0].stats(),
        gap_events,
    });
    LoopbackOutcome {
        result: SessionResult {
            caller_code: side[0].completion_code(),
            answerer_code: side[1].completion_code(),
            sim_ms: now_ms,
            timed_out,
            packets_sent: sent,
            packets_dropped: dropped,
            packet_hash: hash.finish(),
        },
        captured,
        udptl,
    }
}
