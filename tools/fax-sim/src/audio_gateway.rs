//! A simulated G.711 gateway on a real UDP socket and a real-time clock (M5e).
//!
//! SpanDSP's audio fax modem (`fax_state`, calling party, transmitting a TIFF) produces 20 ms of
//! audio per tick; each block goes out as one RTP packet (PCMU or PCMA). What the server sends
//! is put in a jitter buffer (sip-core's, by RTP timestamp) and played to SpanDSP 20 ms per
//! tick. Optional seeded loss in both directions and send jitter (packets held back by a random
//! delay, so they may also arrive out of order) model a network. The loop is meant to run
//! inside `spawn_blocking`, like [`crate::gateway::run_fax`].

use std::collections::VecDeque;
use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use sip_core::rtp::{JitterBuffer, JitterConfig, Law, RtpPacket};

use crate::gateway::{FaxRun, FaxTx};
use crate::spandsp::{FaxModem, T30Config, completion_str};
use crate::{STEP_SAMPLES, SplitMix64};

/// The audio side of the simulated gateway.
#[derive(Debug, Clone, Copy)]
pub struct AudioNet {
    /// The codec we send (we receive both).
    pub law: Law,
    /// Each packet we send is held back by a random 0..=`jitter_ms` (so it may overtake or be
    /// overtaken by its neighbours).
    pub jitter_ms: u32,
}

impl Default for AudioNet {
    fn default() -> Self {
        AudioNet {
            law: Law::Ulaw,
            jitter_ms: 0,
        }
    }
}

/// Runs the transfer to completion over RTP. `stop` ends it early (the server hung up).
/// [`FaxTx::loss_permille`] drops packets in both directions; `version`, `recovery`, `iaf` and
/// `quirks` are T.38 settings and not used here.
pub fn run_audio_fax(
    sock: &UdpSocket,
    remote: SocketAddr,
    tx: &FaxTx,
    net: AudioNet,
    stop: &Arc<AtomicBool>,
) -> Result<FaxRun, String> {
    sock.set_read_timeout(Some(Duration::from_millis(2)))
        .map_err(|e| format!("socket: {e}"))?;
    let cfg = T30Config {
        tx_file: Some(&tx.tiff),
        rx_file: None,
        ecm: tx.ecm,
        ident: &tx.ident,
        modems: tx.modems,
        compressions: tx.compressions,
        iaf: None,
    };
    let mut modem = FaxModem::new(true, &cfg).ok_or("fax_init failed")?;
    let mut jitter = JitterBuffer::new(JitterConfig::G711);
    let mut rng_tx = SplitMix64::new(tx.seed);
    let mut rng_rx = SplitMix64::new(tx.seed ^ 0x5DEE_CE66_D1CE_4E5B);
    let mut rng_jitter = SplitMix64::new(tx.seed ^ 0x0A0D_10C0_FFEE_0001);
    let ssrc = 0x5EED_0000 ^ (tx.seed as u32);
    let mut seq: u16 = 1;
    let mut ts: u32 = 0;
    let mut run = FaxRun::default();
    let start = Instant::now();
    let mut ticks: u64 = 0;
    let mut finished_at: Option<Instant> = None;
    let mut hangup_at: Option<Instant> = None;
    let mut buf = vec![0u8; 2048];
    let mut decoded = Vec::with_capacity(STEP_SAMPLES);
    // packets waiting for their (jittered) release time, in release order
    let mut queue: VecDeque<(Instant, Vec<u8>)> = VecDeque::new();
    loop {
        // 1. everything that arrived
        match sock.recv_from(&mut buf) {
            Ok((n, src)) => {
                if src == remote {
                    run.rx_datagrams += 1;
                    if tx.loss_permille > 0 && rng_rx.next_permille() < tx.loss_permille {
                        run.rx_dropped += 1;
                    } else if let Ok(p) = RtpPacket::decode(&buf[..n])
                        && let Some(law) = Law::from_payload_type(p.payload_type)
                    {
                        decoded.clear();
                        law.decode(&p.payload, &mut decoded);
                        jitter.push(p.ssrc, p.timestamp, std::mem::take(&mut decoded));
                    }
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::ConnectionRefused
                        | std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::Interrupted
                ) => {}
            Err(e) => return Err(format!("recv: {e}")),
        }
        // 2. every 20 ms of wall clock: 20 ms of SpanDSP each way
        let due = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX) / 20;
        while ticks < due {
            ticks += 1;
            let mut out = [0i16; STEP_SAMPLES];
            let n = modem.tx(&mut out);
            out[n..].fill(0);
            let mut payload = Vec::with_capacity(STEP_SAMPLES);
            net.law.encode(&out, &mut payload);
            let p = RtpPacket::new(net.law.payload_type(), seq, ts, ssrc, payload);
            seq = seq.wrapping_add(1);
            ts = ts.wrapping_add(STEP_SAMPLES as u32);
            run.tx_datagrams += 1;
            if tx.loss_permille > 0 && rng_tx.next_permille() < tx.loss_permille {
                run.tx_dropped += 1;
            } else {
                let delay = if net.jitter_ms > 0 {
                    rng_jitter.next_u64() % (u64::from(net.jitter_ms) + 1)
                } else {
                    0
                };
                let release = Instant::now() + Duration::from_millis(delay);
                let at = queue.partition_point(|(t, _)| *t <= release);
                queue.insert(at, (release, p.encode()));
            }
            let mut inp = [0i16; STEP_SAMPLES];
            jitter.pull(&mut inp);
            modem.rx(&mut inp);
        }
        // 2b. send what is due
        let now = Instant::now();
        while queue.front().is_some_and(|(t, _)| *t <= now) {
            let Some((_, data)) = queue.pop_front() else {
                break;
            };
            let _ = sock.send_to(&data, remote);
        }
        // 3. end conditions, as in the T.38 gateway
        if run.caller_code.is_none() {
            run.caller_code = modem.completion_code();
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
    run.rx_gaps = jitter.stats().gap_samples / STEP_SAMPLES as u64;
    Ok(run)
}
