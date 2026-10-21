//! SpanDSP `t38_terminal` (calling party, transmits a TIFF) against our receive engine
//! (`fax_in::t38engine`, answering party) over UDPTL, on a simulated clock (M2c, X01..X05).
//!
//! Every packet either side emits becomes one UDPTL datagram (one per requested copy, as
//! FreeSWITCH does) built by the `t38` crate with the configured redundancy; loss is applied per
//! datagram. SpanDSP's side is fed through a [`UdptlReceiver`]; the engine side is a
//! [`UdptlEngine`]. The one-way latency is one 20 ms step.

use crate::spandsp::{T30Config, T38Terminal};
use crate::{STEP_MS, STEP_SAMPLES, SplitMix64};
use fax_in::t38engine::{EngineConfig, ReceivedPage, UdptlEngine};
use std::path::PathBuf;
use t38::{
    DataType, ErrorRecovery, FieldType, Ifp, ReceiverConfig, ReceiverStats, RxEvent, Source,
    T38Version, UdptlReceiver, UdptlSender,
};

/// Run parameters.
#[derive(Debug, Clone)]
pub struct EngineRunConfig {
    /// TIFF the SpanDSP caller transmits (may not exist).
    pub tx_file: PathBuf,
    /// T.38 version of both sides (0..=3).
    pub version: u8,
    /// `T30_SUPPORT_*` mask for the SpanDSP caller (`None` = V.27ter, V.29, V.17).
    pub modems: Option<i32>,
    /// `T4_COMPRESSION_*` mask for the SpanDSP caller (`None` = MH, MR, MMR).
    pub compressions: Option<i32>,
    /// `T30_IAF_MODE_*` for the caller (`None` = default, with TCF).
    pub iaf: Option<i32>,
    /// ECM enabled on the SpanDSP caller (`t30_set_ecm_capability`).
    pub ecm: bool,
    /// Drop, on every transmission, the ECM frame with this number (caller to engine). The
    /// datagrams are still numbered, so the receiver sees real sequence gaps.
    pub drop_frame: Option<u8>,
    /// Our T.30 / engine configuration.
    pub engine: EngineConfig,
    /// Error recovery used by both senders.
    pub recovery: ErrorRecovery,
    /// Datagram loss in permille.
    pub loss_permille: u32,
    /// Apply `loss_permille` only to the datagrams that carry image-rate data (`hdlc-data` /
    /// `t4-non-ecm-data` on a V.27ter / V.29 / V.17 data type), caller to engine; everything else
    /// (indicators, V.21 control frames, the engine's answers) is delivered.
    pub loss_image_data_only: bool,
    /// PRNG seed for the loss.
    pub seed: u64,
    /// Simulated time limit.
    pub max_sim_ms: u64,
    /// Stop the SpanDSP caller (`t30_terminate`) at this simulated time.
    pub caller_hangup_at_ms: Option<u64>,
}

impl EngineRunConfig {
    /// Defaults: version 0, redundancy 3, no loss, 10 minute cap.
    pub fn new(tx_file: PathBuf) -> Self {
        let mut engine = EngineConfig::default();
        engine.t30.local_id = "0287654321".into();
        Self {
            tx_file,
            version: 0,
            modems: None,
            compressions: None,
            iaf: None,
            ecm: false,
            drop_frame: None,
            engine,
            recovery: ErrorRecovery::Redundancy { depth: 3 },
            loss_permille: 0,
            loss_image_data_only: false,
            seed: 1,
            max_sim_ms: 600_000,
            caller_hangup_at_ms: None,
        }
    }
}

/// What a run produced.
#[derive(Debug)]
pub struct EngineRunOutcome {
    /// SpanDSP's completion code (`T30_ERR_*`), if it reported one.
    pub caller_code: Option<i32>,
    /// Our T.30 result, if the engine finished.
    pub result: Option<t30::T30Result>,
    /// Pages we received, re-encoded as MMR.
    pub pages: Vec<ReceivedPage>,
    /// Simulated duration.
    pub sim_ms: u64,
    /// True if `max_sim_ms` was hit.
    pub timed_out: bool,
    /// Datagrams emitted by both sides / dropped by the loss injection.
    pub datagrams_sent: u64,
    /// Datagrams dropped.
    pub datagrams_dropped: u64,
    /// UDPTL receiver counters of the engine side (what SpanDSP sent us).
    pub engine_rx: ReceiverStats,
    /// UDPTL receiver counters of the SpanDSP side (what we sent).
    pub caller_rx: ReceiverStats,
    /// Unrecoverable losses seen by the engine side / the caller side.
    pub gaps: (u64, u64),
    /// Engine diagnostic lines.
    pub log: Vec<String>,
    /// ECM frame transmissions removed by `drop_frame`.
    pub frames_dropped: u32,
}

/// Recognises the packets of one ECM frame (`hdlc-data` on an image data type starting
/// `FF C0 60 <number LSB first>`, up to the packet with the frame's FCS field).
struct FrameDropper {
    target: u8,
    active: bool,
    frames: u32,
}

impl FrameDropper {
    fn check(&mut self, ifp: &Ifp) -> bool {
        let Ifp::Data { data_type, fields } = ifp else {
            return false;
        };
        if *data_type == DataType::V21 {
            return false;
        }
        let mut drop = false;
        for f in fields {
            match f.field_type {
                FieldType::HdlcData => {
                    if !self.active
                        && f.data.len() >= 4
                        && f.data[..3] == [0xFF, 0xC0, 0x60]
                        && f.data[3].reverse_bits() == self.target
                    {
                        self.active = true;
                        self.frames += 1;
                    }
                    drop |= self.active;
                }
                FieldType::HdlcFcsOk
                | FieldType::HdlcFcsBad
                | FieldType::HdlcFcsOkSigEnd
                | FieldType::HdlcFcsBadSigEnd => {
                    drop |= self.active;
                    self.active = false;
                }
                _ => {}
            }
        }
        drop
    }
}

/// Runs one call.
pub fn run(cfg: &EngineRunConfig) -> EngineRunOutcome {
    let caller_cfg = T30Config {
        tx_file: Some(&cfg.tx_file),
        rx_file: None,
        ecm: cfg.ecm,
        ident: "0000000001",
        modems: cfg.modems,
        compressions: cfg.compressions,
        iaf: cfg.iaf,
    };
    let mut caller = T38Terminal::new(true, &caller_cfg).expect("caller init");
    caller.set_t38_version(i32::from(cfg.version));
    let version = T38Version::from_number(u32::from(cfg.version)).expect("version 0..=3");
    let mut ecfg = cfg.engine.clone();
    ecfg.version = version;
    let mut eng = UdptlEngine::new(ecfg, cfg.recovery, ReceiverConfig::default());
    let mut caller_tx = UdptlSender::new(cfg.recovery, 0);
    let mut caller_rx = UdptlReceiver::new(ReceiverConfig::default());
    let mut rng = SplitMix64::new(cfg.seed);
    let mut to_engine: Vec<Vec<u8>> = Vec::new();
    let mut to_caller: Vec<Vec<u8>> = Vec::new();
    let (mut sent, mut dropped) = (0u64, 0u64);
    let mut caller_gaps = 0u64;
    let mut now_ms = 0u64;
    let mut timed_out = false;
    let mut hung_up = false;
    // FAXSIM_TRACE=1 prints every packet the SpanDSP caller emits (debugging aid).
    let trace = std::env::var_os("FAXSIM_TRACE").is_some();
    let mut dropper = cfg.drop_frame.map(|target| FrameDropper {
        target,
        active: false,
        frames: 0,
    });

    eng.start(0);
    loop {
        if caller.completion_code().is_some() && eng.is_done() {
            break;
        }
        if now_ms >= cfg.max_sim_ms {
            timed_out = true;
            break;
        }
        // deliver last step's datagrams
        for dg in std::mem::take(&mut to_engine) {
            let _ = eng.on_datagram(now_ms, &dg);
        }
        let batch = std::mem::take(&mut to_caller);
        if !batch.is_empty() {
            let mut events = Vec::new();
            for dg in &batch {
                if let Ok(ev) = caller_rx.on_datagram(dg) {
                    events.extend(ev);
                }
            }
            events.extend(caller_rx.flush());
            for e in events {
                match e {
                    RxEvent::Packet { seq, ifp, source } => {
                        let data = if source == Source::Fec {
                            match Ifp::decode_prefix(&ifp, version) {
                                Ok((_, used)) => ifp[..used].to_vec(),
                                Err(_) => ifp,
                            }
                        } else {
                            ifp
                        };
                        caller.rx_packet(&data, seq);
                    }
                    RxEvent::Gap { .. } => caller_gaps += 1,
                }
            }
        }
        if let Some(at) = cfg.caller_hangup_at_ms {
            if !hung_up && now_ms >= at {
                hung_up = true;
                caller.terminate();
            }
        }
        caller.advance(STEP_SAMPLES);
        now_ms += STEP_MS;
        eng.advance(now_ms);
        // caller -> engine
        for pkt in caller.take_packets() {
            if trace {
                if let Ok((ifp, _)) = Ifp::decode_prefix(&pkt.data, version) {
                    eprintln!("[{now_ms:>6} ms] caller -> engine x{} {ifp:?}", pkt.count);
                }
            }
            let decoded = Ifp::decode_prefix(&pkt.data, version).ok().map(|(i, _)| i);
            let targeted = match (dropper.as_mut(), &decoded) {
                (Some(d), Some(ifp)) => d.check(ifp),
                _ => false,
            };
            let lossy = !cfg.loss_image_data_only
                || matches!(&decoded, Some(Ifp::Data { data_type, .. }) if *data_type != DataType::V21);
            for _ in 0..pkt.count.max(1) {
                if let Ok(dg) = caller_tx.build(&pkt.data) {
                    sent += 1;
                    if targeted
                        || (lossy
                            && cfg.loss_permille > 0
                            && rng.next_permille() < cfg.loss_permille)
                    {
                        dropped += 1;
                    } else {
                        to_engine.push(dg);
                    }
                }
            }
        }
        // engine -> caller
        for d in eng.poll_datagrams() {
            let lossy = !cfg.loss_image_data_only;
            if trace {
                eprintln!("[{now_ms:>6} ms] engine -> caller {} octets", d.bytes.len());
            }
            sent += 1;
            if lossy && cfg.loss_permille > 0 && rng.next_permille() < cfg.loss_permille {
                dropped += 1;
            } else {
                to_caller.push(d.bytes);
            }
        }
    }
    let engine_rx = eng.receiver_stats();
    let gaps = (eng.gaps, caller_gaps);
    let log = eng.engine_mut().drain_log();
    let result = eng.engine().result().cloned();
    let pages = eng.engine_mut().take_pages();
    EngineRunOutcome {
        caller_code: caller.completion_code(),
        result,
        pages,
        sim_ms: now_ms,
        timed_out,
        datagrams_sent: sent,
        datagrams_dropped: dropped,
        engine_rx,
        caller_rx: caller_rx.stats(),
        gaps,
        log,
        frames_dropped: dropper.map_or(0, |d| d.frames),
    }
}
