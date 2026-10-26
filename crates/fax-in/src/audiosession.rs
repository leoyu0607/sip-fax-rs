//! The G.711 fax session (M5e): an RTP socket, a jitter buffer and the sans-IO
//! [`AudioEngine`] driven every 20 ms, and the same output files as T.38.
//!
//! * The socket is bound on the call's RTP port. We send to the address in the peer's SDP; the
//!   first RTP packet *latches* the peer's real address, as UDPTL does
//!   ([`crate::t38session`]).
//! * Every 20 ms one block of 160 samples is taken from the jitter buffer and handed to the
//!   engine together with the block to send, which goes out at once as one RTP packet (PCMU or
//!   PCMA, the first codec of our answer; packets are received in either). A block the buffer
//!   reports as entirely missing is a lost packet ([`AudioEngine::process_lost`]). When the
//!   task falls behind, the ticks it missed are run back to back: no audio is dropped and the
//!   delay does not grow.
//! * Until [`AudioContext::go`] turns `true` the engine only listens (CNG, V.21) and we send
//!   silence; then T.30 starts (CED, DIS). A session cancelled before that reports nothing.
//! * `audio.log` (with capture on): the engine's lines (frames, modem changes, signal ends),
//!   the T.30 log, and the RTP events (peer latched, first packet, resyncs).

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use sip_core::rtp::{JitterBuffer, JitterConfig, Law, RtpPacket};
use tokio::net::UdpSocket;
use tokio::time::{Instant, MissedTickBehavior};

use crate::audioengine::{AudioConfig, AudioEngine};
use crate::event::{FaxMedia, RtpCounters, UdptlCounters};
use crate::session::{AudioContext, AudioFaxSession, SessionOutcome};
use crate::t38session::{Concluding, End, T38SessionEnv, conclude, parse_peer};

/// Samples per packet (20 ms at 8 kHz).
const BLOCK: usize = 160;
/// Largest datagram we read.
const MAX_DATAGRAM: usize = 2048;

/// A fax session receiving over G.711.
pub struct G711FaxSession {
    pub(crate) env: Arc<T38SessionEnv>,
}

impl AudioFaxSession for G711FaxSession {
    fn run(
        self: Box<Self>,
        ctx: AudioContext,
    ) -> Pin<Box<dyn Future<Output = SessionOutcome> + Send>> {
        Box::pin(async move { run_session(&self.env, ctx).await })
    }
}

/// The engine settings for a call.
fn engine_config(env: &T38SessionEnv, ctx: &AudioContext) -> AudioConfig {
    let mut cfg = AudioConfig::default();
    cfg.t30.local_id = ctx
        .station_id
        .clone()
        .unwrap_or_else(|| env.station_id.clone());
    cfg.t30.fine = ctx.fax.fine();
    cfg
}

/// The RTP sender: our SSRC, sequence number and timestamp.
struct RtpTx {
    law: Law,
    ssrc: u32,
    seq: u16,
    ts: u32,
    payload: Vec<u8>,
}

impl RtpTx {
    fn new(law: Law) -> Self {
        Self {
            law,
            ssrc: rand::random(),
            seq: rand::random(),
            ts: rand::random(),
            payload: Vec::with_capacity(BLOCK),
        }
    }

    /// The datagram of the next 20 ms.
    fn packet(&mut self, samples: &[i16]) -> Vec<u8> {
        self.payload.clear();
        self.law.encode(samples, &mut self.payload);
        let p = RtpPacket::new(
            self.law.payload_type(),
            self.seq,
            self.ts,
            self.ssrc,
            std::mem::take(&mut self.payload),
        );
        self.seq = self.seq.wrapping_add(1);
        self.ts = self.ts.wrapping_add(samples.len() as u32);
        let bytes = p.encode();
        self.payload = p.payload;
        bytes
    }
}

/// Where the session's diagnostic lines go: tracing, and `audio.log` when capture is on.
struct Log<'a> {
    env: &'a T38SessionEnv,
    call_id: &'a str,
    t0: Instant,
}

impl Log<'_> {
    fn line(&self, text: &str) {
        let ms = self.t0.elapsed().as_millis();
        tracing::debug!(call_id = %self.call_id, "audio: {text}");
        if let Some(c) = &self.env.capture {
            c.audio(
                self.call_id,
                SystemTime::now(),
                &format!("{ms:>7} ms {text}"),
            );
        }
    }
}

async fn run_session(env: &T38SessionEnv, mut ctx: AudioContext) -> SessionOutcome {
    let started_at = SystemTime::now();
    let bind = SocketAddr::new(env.bind_ip, ctx.audio.local_port);
    let sock = match UdpSocket::bind(bind).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(call_id = %ctx.call_id, %bind, "cannot bind the RTP port: {e}");
            return SessionOutcome::Failed(format!("cannot bind RTP port {bind}: {e}"));
        }
    };
    let mut remote = parse_peer(ctx.audio.peer_media.as_deref());
    let law = Law::from_payload_type(ctx.audio.payload_type).unwrap_or(Law::Ulaw);
    tracing::info!(call_id = %ctx.call_id, local = %bind, remote = ?remote,
        codec = law.encoding_name(), "G.711 session starts (listening)");

    let t0 = Instant::now();
    let log = Log {
        env,
        call_id: &ctx.call_id,
        t0,
    };
    let mut eng = AudioEngine::new(engine_config(env, &ctx));
    let mut jitter = JitterBuffer::new(JitterConfig::G711);
    let mut tx = RtpTx::new(law);
    let mut counters = RtpCounters::default();
    let mut latched = false;
    let mut started = false;
    let mut deadline: Option<Instant> = None;
    let mut cancel = ctx.cancel.clone();
    let mut tick = tokio::time::interval(Duration::from_millis(20));
    tick.set_missed_tick_behavior(MissedTickBehavior::Burst);
    let mut buf = vec![0u8; MAX_DATAGRAM];
    let mut decoded = Vec::with_capacity(BLOCK);
    let mut rx_block = [0i16; BLOCK];
    let mut tx_block = [0i16; BLOCK];
    let mut pages_reported = 0usize;
    let mut resyncs = 0u64;

    let end = loop {
        tokio::select! {
            biased;
            () = crate::session::cancelled(&mut cancel) => break End::Cancelled,
            r = ctx.go.changed(), if !started => {
                if r.is_err() {
                    // nobody can start the fax any more: listen until cancelled
                    started = true;
                    continue;
                }
                let reason = ctx.go.borrow_and_update().clone();
                if let Some(reason) = reason {
                    started = true;
                    deadline = Some(Instant::now() + ctx.max_duration.unwrap_or(env.max_duration));
                    log.line(&format!("answering the fax ({reason})"));
                    eng.start();
                }
            }
            r = sock.recv_from(&mut buf) => {
                let (n, src) = match r {
                    Ok(x) => x,
                    Err(e) => {
                        tracing::debug!(call_id = %ctx.call_id, "RTP receive error: {e}");
                        continue;
                    }
                };
                counters.rx_packets += 1;
                if latched && Some(src) != remote {
                    counters.rx_foreign += 1;
                    continue;
                }
                let Ok(p) = RtpPacket::decode(&buf[..n]) else {
                    counters.rx_ignored += 1;
                    continue;
                };
                // telephone-event, comfort noise, ...: not fax audio
                let Some(plaw) = Law::from_payload_type(p.payload_type) else {
                    counters.rx_ignored += 1;
                    continue;
                };
                if !latched {
                    latched = true;
                    if Some(src) != remote {
                        log.line(&format!("RTP peer latched at {src} (the SDP said {remote:?})"));
                    }
                    remote = Some(src);
                    log.line(&format!("first RTP packet: {}", plaw.encoding_name()));
                }
                decoded.clear();
                plaw.decode(&p.payload, &mut decoded);
                jitter.push(p.ssrc, p.timestamp, std::mem::take(&mut decoded));
                decoded = Vec::with_capacity(BLOCK);
            }
            _ = tick.tick() => {
                let pulled = jitter.pull(&mut rx_block);
                if pulled.playing && pulled.gap as usize == BLOCK {
                    eng.process_lost(BLOCK, &mut tx_block);
                } else {
                    eng.process(&rx_block, &mut tx_block);
                }
                if let Some(to) = remote {
                    let d = tx.packet(&tx_block);
                    match sock.send_to(&d, to).await {
                        Ok(_) => counters.tx_packets += 1,
                        Err(e) => tracing::debug!("RTP send to {to} failed: {e}"),
                    }
                }
                for line in eng.drain_log() {
                    log.line(&line);
                }
                let st = jitter.stats();
                if st.resyncs > resyncs {
                    resyncs = st.resyncs;
                    log.line("RTP stream restarted (new SSRC or timestamp jump)");
                }
                report_progress(&ctx, &eng, &mut pages_reported);
                if eng.is_done() {
                    break End::Done;
                }
                if deadline.is_some_and(|d| Instant::now() >= d) {
                    break End::MaxDuration;
                }
            }
        }
    };
    let st = jitter.stats();
    counters.late = st.late;
    counters.duplicate = st.duplicate;
    counters.resyncs = st.resyncs;
    counters.lost_ms = st.gap_samples / 8;
    drop(sock);
    if !started {
        // only listened: T.38 took over or the call ended before the fax
        for line in eng.drain_log() {
            log.line(&line);
        }
        return SessionOutcome::Aborted;
    }
    let mut state_at_cancel = None;
    if end != End::Done {
        state_at_cancel = Some(eng.state());
        eng.abort();
    }
    for line in eng.drain_log() {
        log.line(&line);
    }
    let result = eng.result().cloned();
    let pages = eng.take_pages();
    conclude(
        env,
        Concluding {
            call_id: &ctx.call_id,
            report: &ctx.report,
            started_at,
            end,
            state_at_cancel,
            result,
            pages,
            media: FaxMedia::G711,
            udptl: UdptlCounters::default(),
            rtp: Some(counters),
        },
    )
    .await
}

/// Tells the controller about pages T.30 confirmed since the last call.
fn report_progress(ctx: &AudioContext, eng: &AudioEngine, reported: &mut usize) {
    let n = eng.pages().len();
    while *reported < n {
        *reported += 1;
        if let Some(tx) = &ctx.progress {
            let _ = tx.send(u32::try_from(*reported).unwrap_or(u32::MAX));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtp_packets_advance_by_one_and_160() {
        let mut tx = RtpTx::new(Law::Alaw);
        let a = RtpPacket::decode(&tx.packet(&[0i16; BLOCK])).unwrap();
        let b = RtpPacket::decode(&tx.packet(&[0i16; BLOCK])).unwrap();
        assert_eq!(a.payload_type, 8);
        assert_eq!(b.sequence, a.sequence.wrapping_add(1));
        assert_eq!(b.timestamp, a.timestamp.wrapping_add(160));
        assert_eq!(a.ssrc, b.ssrc);
        assert_eq!(a.payload, vec![Law::Alaw.silence(); BLOCK]);
    }
}
