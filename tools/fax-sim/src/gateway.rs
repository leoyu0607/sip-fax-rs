//! A simulated T.38 gateway on a real UDP socket and a real-time clock (M2d).
//!
//! SpanDSP's `t38_terminal` (calling party, transmitting a TIFF) is driven from a blocking
//! loop: datagrams are read from the socket as they arrive, the SpanDSP clock follows the wall
//! clock in 20 ms steps, and everything SpanDSP emits is framed as UDPTL by our `t38` crate
//! (configurable redundancy / FEC, optional seeded loss in both directions) and sent to the
//! server. The loop is meant to run inside `spawn_blocking` (the SpanDSP terminal is not
//! `Send` and is created inside the thread).

use std::collections::VecDeque;
use std::net::{SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use t38::{
    DataType, ErrorRecovery, Field, FieldType, Ifp, ReceiverConfig, ReceiverStats, RxEvent, Source,
    T38Version, UdptlReceiver, UdptlSender,
};

use crate::spandsp::{T30Config, T38Terminal, completion_str};
use crate::{STEP_SAMPLES, SplitMix64};

/// Behaviours of real gateways that SpanDSP does not show (field case F1).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GatewayQuirks {
    /// AudioCodes style: a frame that SpanDSP ends with `hdlc-fcs-OK-sig-end` goes out as
    /// `hdlc-fcs-OK`, and the `hdlc-sig-end` follows this long after it. Everything SpanDSP
    /// emits after that (the `no-signal`, up to and including it) is delayed by the same
    /// amount, so the order and the gaps of the rest of the burst stay as they were.
    pub split_fcs_sig_end: Option<Duration>,
    /// Half duplex like the gateway of the field case: while the gateway's own V.21 signal is
    /// on (from the split `hdlc-fcs-OK` to its `hdlc-sig-end`) everything received from the
    /// server is discarded, and so is any data that arrives without a signal indicator in front
    /// of it (a modem that never started modulates nothing). Without this a SpanDSP
    /// peer would forgive a server that talks over it. Only acts together with
    /// `split_fcs_sig_end`.
    pub half_duplex: bool,
}

/// What the simulated gateway sends and how.
#[derive(Debug, Clone)]
pub struct FaxTx {
    /// The TIFF SpanDSP transmits.
    pub tiff: PathBuf,
    /// Our station id (becomes the TSI).
    pub ident: String,
    /// T.38 version used to encode IFP packets (0..=3).
    pub version: u8,
    /// UDPTL error recovery of the datagrams we send.
    pub recovery: ErrorRecovery,
    /// Datagram loss in permille, applied to what we send and to what we receive.
    pub loss_permille: u32,
    /// Seed of the loss PRNG.
    pub seed: u64,
    /// `T30_SUPPORT_*` mask (`None` = V.27ter, V.29, V.17).
    pub modems: Option<i32>,
    /// `T4_COMPRESSION_*` mask (`None` = MH, MR, MMR).
    pub compressions: Option<i32>,
    /// `T30_IAF_MODE_*` (`None` = default, with TCF).
    pub iaf: Option<i32>,
    /// Gateway behaviours on top of SpanDSP's.
    pub quirks: GatewayQuirks,
    /// Wall-clock limit of the whole transfer.
    pub timeout: Duration,
    /// How long to keep running after SpanDSP reported its result (its last packets, the
    /// server's last packets).
    pub linger: Duration,
    /// After the server hung up: how long SpanDSP may still take to report its own result.
    pub hangup_grace: Duration,
}

impl FaxTx {
    /// Defaults: ident `0000000001`, T.38 version 0, redundancy 3, no loss, 2 minutes.
    pub fn new(tiff: PathBuf) -> Self {
        FaxTx {
            tiff,
            ident: "0000000001".into(),
            version: 0,
            recovery: ErrorRecovery::Redundancy { depth: 3 },
            loss_permille: 0,
            seed: 1,
            modems: None,
            compressions: None,
            iaf: None,
            quirks: GatewayQuirks::default(),
            timeout: Duration::from_secs(120),
            linger: Duration::from_millis(600),
            hangup_grace: Duration::from_secs(10),
        }
    }
}

/// What a run did.
#[derive(Debug, Clone, Default)]
pub struct FaxRun {
    /// SpanDSP's phase E completion code (`T30_ERR_*`; 0 = OK), if it reported one.
    pub caller_code: Option<i32>,
    /// SpanDSP's text for the code.
    pub caller_text: String,
    /// True if `timeout` (or the stop flag) ended the run before SpanDSP was done.
    pub timed_out: bool,
    /// Wall-clock duration in milliseconds.
    pub wall_ms: u64,
    /// UDPTL datagrams produced by us.
    pub tx_datagrams: u64,
    /// ... of which dropped by the loss injection (not sent).
    pub tx_dropped: u64,
    /// UDPTL datagrams read from the socket.
    pub rx_datagrams: u64,
    /// ... of which dropped by the loss injection (not delivered).
    pub rx_dropped: u64,
    /// Frames whose `hdlc-fcs-OK-sig-end` was split by [`GatewayQuirks::split_fcs_sig_end`].
    pub quirk_splits: u64,
    /// Packets from the server that arrived while the gateway's own V.21 signal was on (and were
    /// discarded by the half duplex quirk).
    pub half_duplex_dropped: u64,
    /// Unrecoverable losses reported by our UDPTL receiver.
    pub rx_gaps: u64,
    /// Our UDPTL receiver counters.
    pub rx_stats: ReceiverStats,
}

impl FaxRun {
    /// Datagram loss actually injected, in total.
    pub fn dropped(&self) -> u64 {
        self.tx_dropped + self.rx_dropped
    }
}

/// Runs the transfer to completion. `stop` ends it early (the server hung up).
pub fn run_fax(
    sock: &UdpSocket,
    remote: SocketAddr,
    tx: &FaxTx,
    stop: &Arc<AtomicBool>,
) -> Result<FaxRun, String> {
    sock.set_read_timeout(Some(Duration::from_millis(5)))
        .map_err(|e| format!("socket: {e}"))?;
    let cfg = T30Config {
        tx_file: Some(&tx.tiff),
        rx_file: None,
        ecm: false,
        ident: &tx.ident,
        modems: tx.modems,
        compressions: tx.compressions,
        iaf: tx.iaf,
    };
    let mut term = T38Terminal::new(true, &cfg).ok_or("t38_terminal_init failed")?;
    term.set_t38_version(i32::from(tx.version));
    let version = T38Version::from_number(u32::from(tx.version)).ok_or("T.38 version 0..=3")?;
    let mut sender = UdptlSender::new(tx.recovery, 0);
    let mut receiver = UdptlReceiver::new(ReceiverConfig::default());
    // one stream per direction: the pattern of each depends only on its own datagram count
    let mut rng_tx = SplitMix64::new(tx.seed);
    let mut rng_rx = SplitMix64::new(tx.seed ^ 0x5DEE_CE66_D1CE_4E5B);
    let mut run = FaxRun::default();
    let start = Instant::now();
    let mut samples_done: u64 = 0;
    let mut finished_at: Option<Instant> = None;
    let mut hangup_at: Option<Instant> = None;
    let mut buf = vec![0u8; 4096];
    // datagrams waiting for their release time (only ever delayed by the gateway quirks)
    let mut queue: VecDeque<(Instant, Vec<u8>)> = VecDeque::new();
    let mut shifting = false;
    // half duplex quirk: our V.21 is on until then; whether a server signal is "up"
    let mut v21_busy_until: Option<Instant> = None;
    let mut rx_signal_up = false;
    loop {
        // 1. everything that arrived
        match sock.recv_from(&mut buf) {
            Ok((n, src)) => {
                if src == remote {
                    run.rx_datagrams += 1;
                    if tx.loss_permille > 0 && rng_rx.next_permille() < tx.loss_permille {
                        run.rx_dropped += 1;
                    } else if let Ok(mut events) = receiver.on_datagram(&buf[..n]) {
                        // loopback does not reorder: a hole that is still open is a loss
                        events.extend(receiver.flush());
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
                                    if tx.quirks.half_duplex
                                        && tx.quirks.split_fcs_sig_end.is_some()
                                    {
                                        let busy =
                                            v21_busy_until.is_some_and(|t| Instant::now() < t);
                                        if !half_duplex_accepts(
                                            &data,
                                            version,
                                            busy,
                                            &mut rx_signal_up,
                                        ) {
                                            // only what arrived inside our own signal is a fault of the server
                                            if busy {
                                                run.half_duplex_dropped += 1;
                                            }
                                            continue;
                                        }
                                    }
                                    term.rx_packet(&data, seq);
                                }
                                RxEvent::Gap { .. } => run.rx_gaps += 1,
                            }
                        }
                    }
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::ConnectionRefused
                        | std::io::ErrorKind::Interrupted
                ) => {}
            Err(e) => return Err(format!("recv: {e}")),
        }
        // 2. the SpanDSP clock follows the wall clock
        let target = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX) * 8;
        while samples_done + STEP_SAMPLES as u64 <= target {
            term.advance(STEP_SAMPLES);
            samples_done += STEP_SAMPLES as u64;
            for pkt in term.take_packets() {
                let now = Instant::now();
                let splits_before = run.quirk_splits;
                let pieces = match tx.quirks.split_fcs_sig_end {
                    Some(gap) => quirk_split(&pkt.data, version, gap, &mut shifting, &mut run),
                    None => vec![(Duration::ZERO, pkt.data.clone())],
                };
                if run.quirk_splits > splits_before {
                    let gap = tx.quirks.split_fcs_sig_end.unwrap_or_default();
                    v21_busy_until = Some(now + gap);
                }
                for (delay, data) in &pieces {
                    for _ in 0..pkt.count.max(1) {
                        let release = (now + *delay).max(queue.back().map_or(now, |(t, _)| *t));
                        queue.push_back((release, data.clone()));
                    }
                }
            }
        }
        // 2b. send what is due
        let now = Instant::now();
        while queue.front().is_some_and(|(t, _)| *t <= now) {
            let Some((_, data)) = queue.pop_front() else {
                break;
            };
            let Ok(dg) = sender.build(&data) else {
                continue;
            };
            run.tx_datagrams += 1;
            if tx.loss_permille > 0 && rng_tx.next_permille() < tx.loss_permille {
                run.tx_dropped += 1;
            } else {
                let _ = sock.send_to(&dg, remote);
            }
        }
        // 3. end conditions. The server hangs up as soon as it has the DCN, which can be a
        // moment before SpanDSP reports its own result, so a hangup only starts the linger.
        if run.caller_code.is_none() {
            run.caller_code = term.completion_code();
            if run.caller_code.is_some() {
                finished_at = Some(Instant::now());
            }
        }
        if stop.load(Ordering::Relaxed) && hangup_at.is_none() {
            hangup_at = Some(Instant::now());
        }
        if let Some(f) = finished_at {
            if f.elapsed() >= tx.linger || hangup_at.is_some() {
                break;
            }
        } else if start.elapsed() >= tx.timeout
            || hangup_at.is_some_and(|h| h.elapsed() >= tx.hangup_grace)
        {
            run.timed_out = true;
            break;
        }
    }
    run.wall_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
    run.caller_text = run.caller_code.map(completion_str).unwrap_or_default();
    run.rx_stats = receiver.stats();
    Ok(run)
}

/// Applies [`GatewayQuirks::split_fcs_sig_end`] to one IFP packet: the pieces to send, each
/// with the delay after the instant SpanDSP produced the packet.
fn quirk_split(
    data: &[u8],
    version: T38Version,
    gap: Duration,
    shifting: &mut bool,
    run: &mut FaxRun,
) -> Vec<(Duration, Vec<u8>)> {
    let encode = |ifp: &Ifp| ifp.encode(version).ok();
    let Ok((ifp, used)) = Ifp::decode_prefix(data, version) else {
        return vec![(Duration::ZERO, data.to_vec())];
    };
    if let Ifp::Data {
        data_type: DataType::V21,
        fields,
    } = &ifp
    {
        if fields
            .last()
            .is_some_and(|f| f.field_type == FieldType::HdlcFcsOkSigEnd)
        {
            let mut head = fields.clone();
            if let Some(last) = head.last_mut() {
                last.field_type = FieldType::HdlcFcsOk;
            }
            let first = Ifp::Data {
                data_type: DataType::V21,
                fields: head,
            };
            let second = Ifp::Data {
                data_type: DataType::V21,
                fields: vec![Field::new(FieldType::HdlcSigEnd, Vec::new())],
            };
            if let (Some(a), Some(b)) = (encode(&first), encode(&second)) {
                *shifting = true;
                run.quirk_splits += 1;
                return vec![(Duration::ZERO, a), (gap, b)];
            }
        }
    }
    let delay = if *shifting { gap } else { Duration::ZERO };
    if matches!(ifp, Ifp::Indicator(t38::Indicator::NoSignal)) {
        *shifting = false;
    }
    vec![(delay, data[..used].to_vec())]
}

/// The half duplex quirk: does the gateway act on this packet from the server?
fn half_duplex_accepts(data: &[u8], version: T38Version, busy: bool, signal_up: &mut bool) -> bool {
    let Ok((ifp, _)) = Ifp::decode_prefix(data, version) else {
        return true;
    };
    if busy {
        return false;
    }
    match ifp {
        Ifp::Indicator(t38::Indicator::NoSignal) => {
            *signal_up = false;
            true
        }
        Ifp::Indicator(_) => {
            *signal_up = true;
            true
        }
        Ifp::Data { .. } => *signal_up,
    }
}
