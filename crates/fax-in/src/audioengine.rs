//! Sans-IO G.711 passthrough receive engine (ADR-061, M5): 8 kHz audio in, 8 kHz audio out, the
//! T.30 receiver in between.
//!
//! The clock is the audio itself: every call of [`AudioEngine::process`] hands over the samples
//! just received and takes the samples to send for the same stretch of time, so 160 samples are
//! 20 ms. T.30 timers run on that clock.
//!
//! ```text
//! engine.start();
//! every 20 ms: engine.process(&received_160, &mut to_send_160);
//! ```
//!
//! # Receive
//!
//! A presence detector tells when the peer's signal starts and ends (−43 / −48 dBm0); the CNG
//! detector reports calling tones; the V.21 receiver and the HDLC deframer turn the control
//! channel into frames. Octets are bit-reversed on the way to T.30: HDLC sends each octet least
//! significant bit first, `t30` writes them in T.30's notation (first bit sent = most
//! significant), like the T.38 path ([`crate::t38engine`]).
//!
//! # Transmit
//!
//! CED (200 ms of silence, 3 s of 2100 Hz) and V.21 bursts (1 s of flags, the frames, two
//! closing flags) are queued as audio; [`t30::Event::TxComplete`] is reported when the last
//! sample of a burst has been handed out. A response waits until the peer's V.21 signal has
//! ended (at most `peer_signal_wait_ms`) and starts at least `turnaround_ms` after that end
//! (T.30 5.3.2: 75 ms ± 20 ms).
//!
//! # Half duplex
//!
//! While we transmit, and for `echo_guard_ms` after, the received audio is not listened to: what
//! comes back then is our own signal's echo.
//!
//! # High speed
//!
//! The TCF and the image arrive on V.27ter or V.29. A good DCS selects the modem: V.27ter 4800
//! or 2400 starts the V.27ter receiver, whose output (training, octets, carrier lost) goes to
//! T.30 as image data; the receiver listens whether or not the V.21 channel is busy. V.29 is
//! not received yet (M5d): a DCS for it is logged and the training is not heard, so T.30 times
//! out.

use std::collections::VecDeque;

use fax_modem::hdlc::{self, HdlcEvent, HdlcRx};
use fax_modem::tone::{CngDetector, Presence, PresenceDetector, ToneGenerator};
use fax_modem::v21::{V21Rx, V21Tx};
use fax_modem::v27ter::{Rate, RxEvent, V27terRx};
use t30::{Action, Event, RxSignal, Timer};

use crate::t38engine::ReceivedPage;

/// Samples per millisecond.
const PER_MS: u64 = 8;

/// Engine settings.
#[derive(Debug, Clone)]
pub struct AudioConfig {
    /// T.30 capabilities, thresholds and timers.
    pub t30: t30::Config,
    /// Level of everything we send.
    pub tx_level_dbm0: f32,
    /// Silence before CED.
    pub ced_lead_silence_ms: u32,
    /// CED duration (T.30: 2.6 to 4 s).
    pub ced_ms: u32,
    /// Flags before the first frame of a burst (T.30 5.3.1: 1 s ± 15 %).
    pub preamble_ms: u32,
    /// Silence between the end of the peer's signal and our response.
    pub turnaround_ms: u32,
    /// How long a response waits for the peer's V.21 signal to end.
    pub peer_signal_wait_ms: u32,
    /// After we stop transmitting, input is ignored this long (our echo).
    pub echo_guard_ms: u32,
    /// Diagnostic lines kept for [`AudioEngine::drain_log`].
    pub max_log_lines: usize,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            t30: t30::Config::default(),
            tx_level_dbm0: -12.0,
            ced_lead_silence_ms: 200,
            ced_ms: 3_000,
            preamble_ms: 1_000,
            turnaround_ms: 75,
            peer_signal_wait_ms: 500,
            echo_guard_ms: 75,
            max_log_lines: 512,
        }
    }
}

/// A response waiting for the peer's signal to end.
#[derive(Debug)]
struct Held {
    frames: Vec<Vec<u8>>,
    /// Sample after which it goes regardless.
    deadline: u64,
}

/// The engine.
#[derive(Debug)]
pub struct AudioEngine {
    cfg: AudioConfig,
    t30: t30::Receiver,
    /// Samples received so far: the clock.
    now: u64,
    timers: [Option<u64>; 4],
    // receive
    presence: PresenceDetector,
    cng: CngDetector,
    v21: V21Rx,
    hdlc: HdlcRx,
    /// The high speed receiver of the modem the last DCS selected (V.27ter), if any.
    hs: Option<V27terRx>,
    /// Octet being packed from the high speed bits, and how many of its bits are in.
    hs_octet: u8,
    hs_bits: u8,
    /// The peer's signal is on (presence detector).
    peer_on: bool,
    /// Sample at which the peer's last signal ended.
    peer_off_at: u64,
    /// Input is ignored before this sample (we are transmitting, or its echo).
    deaf_until: u64,
    // transmit
    tx: V21Tx,
    queue: VecDeque<i16>,
    /// Sample positions (on the clock) at which bursts end, for TxComplete.
    burst_ends: VecDeque<u64>,
    held: Option<Held>,
    // results
    pages: Vec<ReceivedPage>,
    result: Option<t30::T30Result>,
    log: VecDeque<String>,
}

fn timer_index(t: Timer) -> usize {
    Timer::ALL.iter().position(|x| *x == t).unwrap_or(0)
}

/// T.30 notation ⇄ HDLC wire order.
fn reverse_octets(frame: &[u8]) -> Vec<u8> {
    frame.iter().map(|b| b.reverse_bits()).collect()
}

impl AudioEngine {
    /// An engine; nothing happens until [`AudioEngine::start`].
    pub fn new(cfg: AudioConfig) -> Self {
        Self {
            t30: t30::Receiver::new(cfg.t30.clone()),
            now: 0,
            timers: [None; 4],
            presence: PresenceDetector::new(),
            cng: CngDetector::new(),
            v21: V21Rx::new(),
            hdlc: HdlcRx::new(),
            hs: None,
            hs_octet: 0,
            hs_bits: 0,
            peer_on: false,
            peer_off_at: 0,
            deaf_until: 0,
            tx: V21Tx::new(cfg.tx_level_dbm0),
            queue: VecDeque::new(),
            burst_ends: VecDeque::new(),
            held: None,
            pages: Vec::new(),
            result: None,
            log: VecDeque::new(),
            cfg,
        }
    }

    /// The T.30 state, for diagnostics.
    pub fn state(&self) -> t30::State {
        self.t30.state()
    }

    /// Milliseconds of audio processed.
    pub fn now_ms(&self) -> u64 {
        self.now / PER_MS
    }

    /// The call is answered: CED, CSI and DIS are queued.
    pub fn start(&mut self) {
        self.feed(Event::Start);
    }

    /// Handles `rx` (just received) and fills `tx` (to send over the same time). The two are
    /// normally the same length (one RTP packet); the clock advances by `rx.len()`.
    pub fn process(&mut self, rx: &[i16], tx: &mut [i16]) {
        // what we send now: queued audio, then silence
        let start = self.now;
        for o in tx.iter_mut() {
            *o = self.queue.pop_front().unwrap_or(0);
        }
        let transmitting =
            !self.queue.is_empty() || self.burst_ends.front().is_some_and(|e| *e > start);
        if transmitting || tx.iter().any(|&s| s != 0) {
            let guard = u64::from(self.cfg.echo_guard_ms) * PER_MS;
            self.deaf_until = self
                .deaf_until
                .max(start + tx.len() as u64 + self.queue.len() as u64 + guard);
        }
        self.now += rx.len() as u64;
        // bursts that have now left completely
        while let Some(&end) = self.burst_ends.front() {
            if end > self.now {
                break;
            }
            self.burst_ends.pop_front();
            self.feed(Event::TxComplete);
        }
        if self.now > self.deaf_until && start < self.deaf_until {
            // our echo is over: the receivers start fresh on what follows
            let skip = (self.deaf_until - start) as usize;
            self.listen(&rx[skip.min(rx.len())..]);
        } else if start >= self.deaf_until {
            self.listen(rx);
        }
        self.run_timers();
        self.release_held_if_due();
    }

    /// Ends the call (hangup, error); queued audio is dropped.
    pub fn abort(&mut self) {
        self.feed(Event::Abort);
        self.queue.clear();
        self.burst_ends.clear();
        self.held = None;
    }

    /// True once the call is over.
    pub fn is_done(&self) -> bool {
        self.result.is_some()
    }

    /// The T.30 result, once done.
    pub fn result(&self) -> Option<&t30::T30Result> {
        self.result.as_ref()
    }

    /// Takes the pages accepted so far.
    pub fn take_pages(&mut self) -> Vec<ReceivedPage> {
        std::mem::take(&mut self.pages)
    }

    /// Pages accepted so far.
    pub fn pages(&self) -> &[ReceivedPage] {
        &self.pages
    }

    /// Takes the diagnostic lines collected so far.
    pub fn drain_log(&mut self) -> Vec<String> {
        self.log.drain(..).collect()
    }

    // ------------------------------------------------------------------------------ receive

    fn listen(&mut self, rx: &[i16]) {
        if rx.is_empty() || self.t30.is_done() {
            return;
        }
        if self.cng.feed(rx) {
            self.note("CNG heard".into());
            self.feed(Event::Signal(RxSignal::Cng));
        }
        // Before the presence loop: a signal end in this chunk (NoSignal) ends the TCF or the
        // image in T.30, so the octets of the chunk must reach T.30 first. A receiver made by a
        // DCS below starts on the next chunk, which is in time: the TCF follows the DCS after
        // at least 75 ms.
        self.listen_high_speed(rx);
        for p in self.presence.feed(rx) {
            match p {
                Presence::On(_) => {
                    self.peer_on = true;
                    self.v21 = V21Rx::new();
                    self.hdlc.reset();
                }
                Presence::Off(pos) => {
                    self.peer_on = false;
                    self.peer_off_at = pos;
                    self.hdlc.reset();
                    self.feed(Event::Signal(RxSignal::NoSignal));
                    self.release_held();
                }
            }
        }
        if self.peer_on {
            for bit in self.v21.feed(rx) {
                match self.hdlc.bit(bit) {
                    Some(HdlcEvent::Preamble) => self.feed(Event::Signal(RxSignal::V21Preamble)),
                    Some(HdlcEvent::Frame { data, fcs_ok }) => {
                        let frame = reverse_octets(&data);
                        // (the X bit, the first bit sent, is the most significant in T.30 notation)
                        let is_dcs = fcs_ok && frame.len() >= 3 && frame[2] & 0x7F == t30::fcf::DCS;
                        // the peer trains with what its DCS says, whatever T.30 decided
                        let fif = is_dcs.then(|| frame[3..].to_vec());
                        self.feed(Event::Frame {
                            data: frame,
                            fcs_ok,
                        });
                        if let Some(fif) = fif {
                            self.select_high_speed(&fif);
                        }
                    }
                    Some(HdlcEvent::Abort) | None => {}
                }
            }
        }
    }

    /// The modem of a good DCS (its information field) gets its receiver: V.27ter 4800 or 2400.
    /// Other modems leave none; a reserved rate is logged.
    fn select_high_speed(&mut self, fif: &[u8]) {
        self.hs = None;
        self.hs_octet = 0;
        self.hs_bits = 0;
        let (modem, rate) = match t30::caps::parse_rate(fif) {
            Ok(v) => v,
            Err(e) => {
                self.note(format!("DCS rate not usable ({e}): no high speed receiver"));
                return;
            }
        };
        match (modem, rate) {
            (t30::Modem::V27ter, 4800) => {
                self.hs = Some(V27terRx::new(Rate::B4800));
                self.note("V.27ter 4800 receiver ready".into());
            }
            (t30::Modem::V27ter, 2400) => {
                self.hs = Some(V27terRx::new(Rate::B2400));
                self.note("V.27ter 2400 receiver ready".into());
            }
            (modem, rate) => self.note(format!(
                "DCS selects {modem:?} {rate} bit/s: not received yet (V.29 arrives in M5d)"
            )),
        }
    }

    /// Feeds `rx` to the high speed receiver, if any. Not gated by the V.21 presence: the
    /// receiver has its own.
    fn listen_high_speed(&mut self, rx: &[i16]) {
        let events = match self.hs.as_mut() {
            Some(hs) => hs.feed(rx),
            None => return,
        };
        for ev in events {
            match ev {
                RxEvent::Training => self.feed(Event::Signal(RxSignal::Training)),
                RxEvent::Trained => self.note("V.27ter: trained, image data follows".into()),
                RxEvent::Bits(bits) => {
                    let octets = self.pack_octets(&bits);
                    if !octets.is_empty() {
                        self.feed(Event::ImageData(octets));
                    }
                }
                RxEvent::CarrierLost => {
                    // a part octet at the end of the carrier is not data
                    self.hs_octet = 0;
                    self.hs_bits = 0;
                    self.feed(Event::DataEnd);
                }
            }
        }
    }

    /// Packs bits (first received = most significant) into octets, carrying the part octet on.
    fn pack_octets(&mut self, bits: &[bool]) -> Vec<u8> {
        let mut out = Vec::with_capacity(bits.len() / 8);
        for &b in bits {
            self.hs_octet = (self.hs_octet << 1) | u8::from(b);
            self.hs_bits += 1;
            if self.hs_bits == 8 {
                out.push(self.hs_octet);
                self.hs_octet = 0;
                self.hs_bits = 0;
            }
        }
        out
    }

    // ----------------------------------------------------------------------------- transmit

    fn queue_silence(&mut self, ms: u32) {
        self.queue
            .extend(std::iter::repeat_n(0i16, (u64::from(ms) * PER_MS) as usize));
    }

    fn queue_ced(&mut self) {
        self.queue_silence(self.cfg.ced_lead_silence_ms);
        let mut g = ToneGenerator::new(
            fax_modem::tone::CED_HZ,
            self.cfg.tx_level_dbm0,
            self.cfg.ced_ms,
            0,
        );
        let mut buf = vec![0i16; g.remaining() as usize];
        let n = g.fill(&mut buf);
        self.queue.extend(&buf[..n]);
    }

    /// A V.21 burst: after the turnaround silence (counted from the end of the peer's signal),
    /// the preamble flags, the frames, two closing flags.
    fn queue_frames(&mut self, frames: &[Vec<u8>]) {
        let turnaround = u64::from(self.cfg.turnaround_ms) * PER_MS;
        // the sample at which the queue's next sample goes out
        let at = self.now + self.queue.len() as u64;
        let earliest = self.peer_off_at + turnaround;
        if at < earliest {
            self.queue
                .extend(std::iter::repeat_n(0i16, (earliest - at) as usize));
        }
        let wire: Vec<Vec<u8>> = frames.iter().map(|f| reverse_octets(f)).collect();
        let flags =
            (self.cfg.preamble_ms as usize * fax_modem::v21::BAUD as usize / 1000).div_ceil(8);
        let bits = hdlc::encode_burst(&wire, flags, 2);
        let audio = self.tx.modulate(&bits);
        self.queue.extend(audio);
        self.burst_ends
            .push_back(self.now + self.queue.len() as u64);
    }

    fn release_held(&mut self) {
        if let Some(h) = self.held.take() {
            self.queue_frames(&h.frames);
        }
    }

    fn release_held_if_due(&mut self) {
        if self.held.as_ref().is_some_and(|h| self.now >= h.deadline) {
            self.note(format!(
                "peer signal end not seen within {} ms: answering anyway",
                self.cfg.peer_signal_wait_ms
            ));
            self.peer_off_at = self.now;
            self.release_held();
        }
    }

    // ---------------------------------------------------------------------------------- T.30

    fn run_timers(&mut self) {
        loop {
            let due = self
                .timers
                .iter()
                .enumerate()
                .filter_map(|(i, d)| d.map(|d| (d, i)))
                .filter(|(d, _)| *d <= self.now)
                .min();
            let Some((_, i)) = due else { break };
            self.timers[i] = None;
            if let Some(t) = Timer::ALL.get(i) {
                self.feed(Event::Timer(*t));
            }
        }
    }

    fn note(&mut self, line: String) {
        tracing::debug!(target: "fax_in::audioengine", "{line}");
        if self.log.len() >= self.cfg.max_log_lines.max(1) {
            self.log.pop_front();
        }
        self.log.push_back(line);
    }

    fn feed(&mut self, event: Event) {
        let actions = self.t30.on_event(self.now / PER_MS, event);
        for a in actions {
            match a {
                Action::SendSignal(t30::TxSignal::Ced) => self.queue_ced(),
                Action::SendFrames(frames) => {
                    if self.peer_on {
                        self.note(format!(
                            "peer signal still on: response held (at most {} ms)",
                            self.cfg.peer_signal_wait_ms
                        ));
                        let deadline = self.now + u64::from(self.cfg.peer_signal_wait_ms) * PER_MS;
                        if self.held.replace(Held { frames, deadline }).is_some() {
                            // superseded: it will never leave
                            self.feed(Event::TxComplete);
                        }
                    } else {
                        self.queue_frames(&frames);
                    }
                }
                Action::StartTimer { timer, ms } => {
                    self.timers[timer_index(timer)] = Some(self.now + u64::from(ms) * PER_MS);
                }
                Action::CancelTimer(timer) => self.timers[timer_index(timer)] = None,
                Action::Page(p) => {
                    let (page, err) = ReceivedPage::from_t30(&p);
                    if let Some(e) = err {
                        self.note(e);
                    }
                    self.pages.push(page);
                }
                Action::Log(line) => self.note(line),
                Action::Done(result) => {
                    self.note(format!(
                        "T.30 done: {} after {} page(s), {} ms",
                        result.code.name(),
                        result.pages,
                        result.duration_ms
                    ));
                    self.result = Some(*result);
                    self.queue.clear();
                    self.burst_ends.clear();
                    self.held = None;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fax_modem::hdlc::HdlcEvent;
    use fax_modem::v27ter::testtx::Tx;

    /// Runs the engine for `ms` with `input` (silence when it runs out); returns what it sent.
    fn run(e: &mut AudioEngine, input: &[i16], ms: u64) -> Vec<i16> {
        let mut out = Vec::new();
        let mut pos = 0usize;
        for _ in 0..ms / 20 {
            let mut rx = [0i16; 160];
            let n = input.len().saturating_sub(pos).min(160);
            rx[..n].copy_from_slice(&input[pos..pos + n]);
            pos += n;
            let mut tx = [0i16; 160];
            e.process(&rx, &mut tx);
            out.extend_from_slice(&tx);
        }
        out
    }

    /// The frames (T.30 notation) in a stretch of audio.
    fn frames_in(audio: &[i16]) -> Vec<Vec<u8>> {
        let mut rx = V21Rx::new();
        let mut h = HdlcRx::new();
        rx.feed(audio)
            .into_iter()
            .filter_map(|b| match h.bit(b) {
                Some(HdlcEvent::Frame { data, fcs_ok: true }) => Some(reverse_octets(&data)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn answers_with_ced_then_csi_and_dis_on_v21() {
        let mut e = AudioEngine::new(AudioConfig::default());
        e.start();
        let out = run(&mut e, &[], 6_000);
        // 200 ms silence, then CED
        assert!(out[..1600].iter().all(|&s| s == 0));
        let ced = &out[1600..1600 + 24_000];
        let level = fax_modem::level::dbm0(fax_modem::level::block_mean_square(ced));
        assert!((level + 12.0).abs() < 0.2, "{level}");
        // then frames: CSI and DIS, the DIS with the final control field
        let frames = frames_in(&out[1600 + 24_000..]);
        let fcfs: Vec<u8> = frames.iter().map(|f| f[2]).collect();
        assert!(fcfs.contains(&t30::fcf::DIS), "{frames:02x?}");
        let dis = frames.iter().find(|f| f[2] == t30::fcf::DIS).unwrap();
        assert_eq!(dis[1], t30::frame::CONTROL_FINAL);
        assert_eq!(e.state(), t30::State::WaitCommand);
    }

    #[test]
    fn a_dcs_from_the_peer_is_heard_after_our_burst() {
        let mut e = AudioEngine::new(AudioConfig::default());
        e.start();
        let _ = run(&mut e, &[], 6_000);
        // the peer: TSI and DCS (V.29 9600, standard), in T.30 notation, on V.21
        let tsi = vec![0xFF, 0xC0, t30::fcf::TSI, b' ', b' ', b'1'];
        let dcs = vec![0xFF, 0xC8, t30::fcf::DCS, 0x00, 0x46, 0x00];
        let wire: Vec<Vec<u8>> = [tsi, dcs].iter().map(|f| reverse_octets(f)).collect();
        let mut tx = V21Tx::new(-15.0);
        let mut peer = vec![0i16; 800];
        peer.extend(tx.modulate(&hdlc::encode_burst(&wire, 38, 2)));
        let _ = run(&mut e, &peer, 3_000);
        assert!(
            e.drain_log().iter().any(|l| l.contains("DCS selects")),
            "the DCS reached T.30"
        );
    }

    /// The DCS information field for V.27ter 4800 bit/s: bit 12 set (T.30 Table 2).
    fn dcs_fif_v27ter_4800() -> Vec<u8> {
        let fif = vec![0x00, 0x10, 0x00];
        assert_eq!(
            t30::caps::parse_rate(&fif).unwrap(),
            (t30::caps::Modem::V27ter, 4800)
        );
        fif
    }

    /// The peer's frames as one V.21 burst, then 3 s of silence.
    fn send_v21(e: &mut AudioEngine, frames: &[Vec<u8>]) {
        let wire: Vec<Vec<u8>> = frames.iter().map(|f| reverse_octets(f)).collect();
        let mut v21 = V21Tx::new(-15.0);
        let mut peer = vec![0i16; 800];
        peer.extend(v21.modulate(&hdlc::encode_burst(&wire, 38, 2)));
        let _ = run(e, &peer, 3_000);
    }

    #[test]
    fn the_last_dcs_selects_the_receiver_from_its_own_fif() {
        let mut e = AudioEngine::new(AudioConfig::default());
        e.start();
        let _ = run(&mut e, &[], 6_000);
        let dcs = |fif: &[u8]| {
            let mut f = vec![0xFF, 0xC8, t30::fcf::DCS];
            f.extend_from_slice(fif);
            f
        };
        let fif_2400 = vec![0x00, 0x00, 0x00];
        assert_eq!(
            t30::caps::parse_rate(&fif_2400).unwrap(),
            (t30::caps::Modem::V27ter, 2400)
        );
        let tsi = vec![0xFF, 0xC0, t30::fcf::TSI, b' ', b' ', b'1'];
        send_v21(&mut e, &[tsi, dcs(&dcs_fif_v27ter_4800())]);
        send_v21(&mut e, &[dcs(&fif_2400)]);
        let log = e.drain_log();
        let at = |s: &str| log.iter().position(|l| l == s);
        assert!(
            matches!((at("V.27ter 4800 receiver ready"), at("V.27ter 2400 receiver ready")),
                (Some(a), Some(b)) if a < b),
            "{log:#?}"
        );
    }

    #[test]
    fn a_dcs_of_a_calling_terminal_carries_the_x_bit_and_still_selects_the_receiver() {
        // A calling terminal sets X (the first bit sent) in the FCF after it has seen a DIS:
        // `FF C8 C1 ...`. SpanDSP does; M5c test M01 found the receiver missing it.
        let mut e = AudioEngine::new(AudioConfig::default());
        e.start();
        let _ = run(&mut e, &[], 6_000);
        let tsi = vec![0xFF, 0xC0, t30::fcf::TSI | 0x80, b' ', b' ', b'1'];
        let mut dcs = vec![0xFF, 0xC8, t30::fcf::DCS | 0x80];
        dcs.extend(dcs_fif_v27ter_4800());
        send_v21(&mut e, &[tsi, dcs]);
        let log = e.drain_log();
        assert!(
            log.iter().any(|l| l == "V.27ter 4800 receiver ready"),
            "{log:#?}"
        );
    }

    /// A call up to and through the TCF: the answer, the peer's TSI and a V.27ter 4800 DCS, then
    /// `tcf` on V.27ter 4800 and silence. Returns the frames the engine sent after the TCF and
    /// the log.
    fn tcf_call(tcf: &[bool]) -> (Vec<Vec<u8>>, Vec<String>) {
        let mut e = AudioEngine::new(AudioConfig::default());
        e.start();
        let _ = run(&mut e, &[], 6_000);
        let tsi = vec![0xFF, 0xC0, t30::fcf::TSI, b' ', b' ', b'1'];
        let mut dcs = vec![0xFF, 0xC8, t30::fcf::DCS];
        dcs.extend(dcs_fif_v27ter_4800());
        let wire: Vec<Vec<u8>> = [tsi, dcs].iter().map(|f| reverse_octets(f)).collect();
        let mut v21 = V21Tx::new(-15.0);
        let mut peer = vec![0i16; 800];
        peer.extend(v21.modulate(&hdlc::encode_burst(&wire, 38, 2)));
        let _ = run(&mut e, &peer, 3_000);
        let mut log = e.drain_log();
        let tx = Tx {
            rate: Rate::B4800,
            level_dbm0: -15.0,
            long: true,
            carrier_offset_hz: 0.0,
            clock: 1.0,
        };
        let input = tx.modulate(tcf);
        // whole 20 ms blocks, and 4 s of silence after the signal for our answer
        let blocks = input.len() / 160 + 1 + 200;
        let out = run(&mut e, &input, blocks as u64 * 20);
        log.extend(e.drain_log());
        (frames_in(&out), log)
    }

    #[test]
    fn a_v27ter_tcf_of_zeros_is_answered_with_cfr() {
        let tcf = vec![false; 7_200]; // 1.5 s at 4800 bit/s
        let (frames, log) = tcf_call(&tcf);
        let tcf_line = log.iter().find(|l| l.starts_with("TCF:")).cloned();
        assert!(
            log.iter().any(|l| l == "V.27ter 4800 receiver ready"),
            "{log:#?}"
        );
        let fcfs: Vec<u8> = frames.iter().map(|f| f[2]).collect();
        assert!(
            fcfs.contains(&t30::fcf::CFR),
            "no CFR ({tcf_line:?}); frames {fcfs:02x?}"
        );
    }

    #[test]
    fn a_v27ter_tcf_of_random_bits_is_answered_with_ftt() {
        // a fixed pseudo-random sequence (xorshift64): about half of the bits are ones
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        let tcf: Vec<bool> = (0..7_200)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x & 1 != 0
            })
            .collect();
        let (frames, log) = tcf_call(&tcf);
        let tcf_line = log.iter().find(|l| l.starts_with("TCF:")).cloned();
        let fcfs: Vec<u8> = frames.iter().map(|f| f[2]).collect();
        assert!(
            fcfs.contains(&t30::fcf::FTT) && !fcfs.contains(&t30::fcf::CFR),
            "expected FTT ({tcf_line:?}); frames {fcfs:02x?}"
        );
    }
}
