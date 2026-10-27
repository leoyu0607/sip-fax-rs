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
//! or 2400, or V.29 9600 or 7200, starts that receiver, whose output (training, octets, carrier
//! lost) goes to T.30 as image data; the receiver listens whether or not the V.21 channel is
//! busy. The DIS offers V.27ter and V.29 only (ADR-061); a DCS for another modem (V.17, V.33:
//! some senders try it anyway) has no receiver, and the end of the TCF that follows it is
//! reported to T.30 as a TCF without data, so that T.30 answers FTT at once and the sender
//! steps down (M5e ⑥). V.29 starts its bits with the 48 symbols of scrambled ones of Segment 4;
//! they are not passed on, so a TCF is judged on what the sender sent as TCF (M5e ⑦).
//!
//! # ECM
//!
//! The audio path has no error correction mode: [`AudioEngine::new`] turns `ecm` and `mmr` off
//! in the T.30 settings, whatever the trunk says (M5e ⑤).
//!
//! # Lost packets
//!
//! [`AudioEngine::process_lost`] stands for a block that never arrived. Up to
//! `max_lost_skip_ms` of consecutive lost audio the receivers are simply not fed, so they carry
//! on afterwards with their timing and carrier intact (20 ms is a whole number of symbols and
//! carrier cycles of every modem here): the image gets a few bad lines instead of ending at the
//! gap (M5e ④). Longer outages are fed as silence, so the end of a signal is still heard.

use std::collections::VecDeque;

use fax_modem::hdlc::{self, HdlcEvent, HdlcRx};
use fax_modem::tone::{CngDetector, Presence, PresenceDetector, ToneGenerator};
use fax_modem::v21::{V21Rx, V21Tx};
use fax_modem::v27ter::{self, RxEvent, V27terRx};
use fax_modem::v29::{self, V29Rx};
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
    /// Consecutive lost audio skipped over; beyond it lost audio is taken as silence.
    pub max_lost_skip_ms: u32,
}

impl Default for AudioConfig {
    fn default() -> Self {
        let mut t30 = t30::Config::default();
        // ADR-061: no V.17 receiver, so the DIS offers V.27ter and V.29 only. t30's own default
        // keeps V.17 (T.38 uses it).
        t30.modems = t30::Modems {
            v27ter: true,
            v29: true,
            v17: false,
        };
        Self {
            t30,
            tx_level_dbm0: -12.0,
            ced_lead_silence_ms: 200,
            ced_ms: 3_000,
            preamble_ms: 1_000,
            turnaround_ms: 75,
            peer_signal_wait_ms: 500,
            echo_guard_ms: 75,
            max_log_lines: 512,
            max_lost_skip_ms: 60,
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

/// Where the TCF of a DCS without a receiver is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Foreign {
    /// The V.21 burst of the DCS is still on.
    DcsBurst,
    /// The DCS burst has ended: the next signal is the TCF.
    BeforeTcf,
    /// The TCF is on.
    Tcf,
}

/// The high speed receiver of the modem the last DCS selected.
#[derive(Debug)]
enum HighSpeed {
    V27ter(V27terRx),
    V29(V29Rx),
}

impl HighSpeed {
    fn name(&self) -> &'static str {
        match self {
            HighSpeed::V27ter(_) => "V.27ter",
            HighSpeed::V29(_) => "V.29",
        }
    }

    fn feed(&mut self, rx: &[i16]) -> Vec<RxEvent> {
        match self {
            HighSpeed::V27ter(r) => r.feed(rx),
            HighSpeed::V29(r) => r.feed(rx),
        }
    }
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
    /// The high speed receiver of the modem the last DCS selected, if any.
    hs: Option<HighSpeed>,
    /// Octet being packed from the high speed bits, and how many of its bits are in.
    hs_octet: u8,
    hs_bits: u8,
    /// Bits still to drop at the start of a transmission (V.29 Segment 4).
    hs_skip: usize,
    /// The last DCS selected a modem without a receiver: where its TCF is.
    foreign: Option<Foreign>,
    /// Samples of consecutive lost audio skipped so far.
    lost_run: u64,
    /// The peer's signal is on (presence detector).
    peer_on: bool,
    /// Sample at which the peer's last signal ended.
    peer_off_at: u64,
    /// Whether anything but silence was sent yet (before it the line's noise is learnt).
    sent_signal: bool,
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
    /// Signal events given to T.30 and the sample (on the clock) at which they were, for tests
    /// and diagnostics; the last `max_log_lines`.
    signals: VecDeque<(u64, RxSignal)>,
}

fn timer_index(t: Timer) -> usize {
    Timer::ALL.iter().position(|x| *x == t).unwrap_or(0)
}

/// `audio.log` line of one T.30 frame (address, control, FCF, FIF in T.30 notation): its name
/// and every octet in hex.
fn frame_line(what: &str, frame: &[u8]) -> String {
    let name = frame
        .get(2)
        .map_or("empty", |f| t30::frame::fcf_name(f & 0x7F));
    let hex: Vec<String> = frame.iter().map(|b| format!("{b:02X}")).collect();
    format!("{what} {name} {}", hex.join(" "))
}

/// T.30 notation ⇄ HDLC wire order.
fn reverse_octets(frame: &[u8]) -> Vec<u8> {
    frame.iter().map(|b| b.reverse_bits()).collect()
}

impl AudioEngine {
    /// An engine; nothing happens until [`AudioEngine::start`].
    pub fn new(mut cfg: AudioConfig) -> Self {
        // no ECM on the audio path (M5e ⑤); MMR is only valid with ECM
        cfg.t30.ecm = false;
        cfg.t30.mmr = false;
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
            hs_skip: 0,
            foreign: None,
            lost_run: 0,
            peer_on: false,
            peer_off_at: 0,
            sent_signal: false,
            deaf_until: 0,
            tx: V21Tx::new(cfg.tx_level_dbm0),
            queue: VecDeque::new(),
            burst_ends: VecDeque::new(),
            held: None,
            pages: Vec::new(),
            result: None,
            log: VecDeque::new(),
            signals: VecDeque::new(),
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
        self.lost_run = 0;
        self.step(Some(rx), rx.len(), tx);
    }

    /// Like [`AudioEngine::process`] for `n` samples that never arrived (a lost RTP packet):
    /// the receivers skip them, up to `max_lost_skip_ms` in a row; beyond that they hear
    /// silence.
    pub fn process_lost(&mut self, n: usize, tx: &mut [i16]) {
        let limit = u64::from(self.cfg.max_lost_skip_ms) * PER_MS;
        self.lost_run += n as u64;
        if self.lost_run <= limit {
            self.step(None, n, tx);
        } else {
            self.step(Some(&vec![0i16; n]), n, tx);
        }
    }

    fn step(&mut self, rx: Option<&[i16]>, n: usize, tx: &mut [i16]) {
        // what we send now: queued audio, then silence
        let start = self.now;
        for o in tx.iter_mut() {
            *o = self.queue.pop_front().unwrap_or(0);
        }
        let transmitting =
            !self.queue.is_empty() || self.burst_ends.front().is_some_and(|e| *e > start);
        let sending = tx.iter().any(|&s| s != 0);
        self.sent_signal |= sending;
        if transmitting || sending {
            let guard = u64::from(self.cfg.echo_guard_ms) * PER_MS;
            self.deaf_until = self
                .deaf_until
                .max(start + tx.len() as u64 + self.queue.len() as u64 + guard);
        }
        self.now += n as u64;
        // bursts that have now left completely
        while let Some(&end) = self.burst_ends.front() {
            if end > self.now {
                break;
            }
            self.burst_ends.pop_front();
            self.feed(Event::TxComplete);
        }
        // The presence detector's positions are on our clock (the turnaround counts from the
        // end of the peer's signal): what is not heard is skipped. Before our first signal (the
        // silence before CED) it hears the line, so that it knows the noise when the peer's
        // first DCS may already be on as we start to listen.
        let deaf = (self.deaf_until.saturating_sub(start) as usize).min(n);
        match rx {
            Some(rx) => {
                let (unheard, heard) = rx.split_at(deaf.min(rx.len()));
                if self.sent_signal {
                    self.presence.skip(unheard.len());
                } else {
                    let _ = self.presence.feed(unheard);
                }
                // our echo is over: the receivers start fresh on what follows
                self.listen(heard);
            }
            None => self.presence.skip(n),
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

    /// Takes the signal events (CNG, V.21 preamble, end of signal) given to T.30 so far, each
    /// with the sample count of the engine's clock at which it was given (the end of the chunk
    /// that showed it). Only the last `max_log_lines` are kept.
    pub fn drain_signals(&mut self) -> Vec<(u64, RxSignal)> {
        self.signals.drain(..).collect()
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
                    if self.foreign == Some(Foreign::BeforeTcf) {
                        self.foreign = Some(Foreign::Tcf);
                    }
                }
                Presence::Off(pos) => {
                    self.peer_on = false;
                    self.peer_off_at = pos;
                    self.hdlc.reset();
                    match self.foreign {
                        Some(Foreign::DcsBurst) => self.foreign = Some(Foreign::BeforeTcf),
                        Some(Foreign::Tcf) => {
                            // a TCF without a receiver: no data, so FTT
                            self.foreign = None;
                            self.note("the TCF of a modem without a receiver ended: FTT".into());
                            self.feed(Event::DataEnd);
                        }
                        _ => {}
                    }
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
                        if fcs_ok {
                            self.note(frame_line("V.21 rx", &frame));
                        }
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

    /// The modem of a good DCS (its information field) gets its receiver: V.27ter 4800 or 2400,
    /// V.29 9600 or 7200. Other modems leave none; a reserved rate is logged.
    fn select_high_speed(&mut self, fif: &[u8]) {
        self.hs = None;
        self.hs_octet = 0;
        self.hs_bits = 0;
        self.foreign = None;
        let (modem, rate) = match t30::caps::parse_rate(fif) {
            Ok(v) => v,
            Err(e) => {
                self.note(format!("DCS rate not usable ({e}): no high speed receiver"));
                return;
            }
        };
        match (modem, rate) {
            (t30::Modem::V27ter, 4800) => {
                self.hs = Some(HighSpeed::V27ter(V27terRx::new(v27ter::Rate::B4800)));
                self.note("V.27ter 4800 receiver ready".into());
            }
            (t30::Modem::V27ter, 2400) => {
                self.hs = Some(HighSpeed::V27ter(V27terRx::new(v27ter::Rate::B2400)));
                self.note("V.27ter 2400 receiver ready".into());
            }
            (t30::Modem::V29, 9600) => {
                self.hs = Some(HighSpeed::V29(V29Rx::new(v29::Rate::B9600)));
                self.note("V.29 9600 receiver ready".into());
            }
            (t30::Modem::V29, 7200) => {
                self.hs = Some(HighSpeed::V29(V29Rx::new(v29::Rate::B7200)));
                self.note("V.29 7200 receiver ready".into());
            }
            (modem, rate) => {
                self.note(format!(
                    "DCS selects {modem:?} {rate} bit/s: not received (this receiver has V.27ter and V.29 only)"
                ));
                // (the DCS was just decoded, so its V.21 burst is normally still on)
                self.foreign = Some(if self.peer_on {
                    Foreign::DcsBurst
                } else {
                    Foreign::BeforeTcf
                });
            }
        }
    }

    /// Feeds `rx` to the high speed receiver, if any. Not gated by the V.21 presence: the
    /// receiver has its own.
    fn listen_high_speed(&mut self, rx: &[i16]) {
        let Some(hs) = self.hs.as_mut() else {
            return;
        };
        let name = hs.name();
        let events = hs.feed(rx);
        for ev in events {
            match ev {
                RxEvent::Training => self.feed(Event::Signal(RxSignal::Training)),
                RxEvent::Trained => {
                    self.note(format!("{name}: trained, image data follows"));
                    self.hs_octet = 0;
                    self.hs_bits = 0;
                    // V.29 hands out the scrambled ones of Segment 4 as its first bits
                    self.hs_skip = match &self.hs {
                        Some(HighSpeed::V29(r)) => v29::SEGMENT_4 * r.rate().bits_per_symbol(),
                        _ => 0,
                    };
                }
                RxEvent::Bits(bits) => {
                    let skip = self.hs_skip.min(bits.len());
                    self.hs_skip -= skip;
                    let octets = self.pack_octets(&bits[skip..]);
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
            self.cfg.turnaround_ms,
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
        for f in frames {
            self.note(frame_line("V.21 tx", f));
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
        if let Event::Signal(sig) = &event {
            if self.signals.len() >= self.cfg.max_log_lines {
                self.signals.pop_front();
            }
            self.signals.push_back((self.now, *sig));
        }
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
mod tests;
