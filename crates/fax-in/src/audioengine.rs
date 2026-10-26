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
    /// and diagnostics.
    signals: Vec<(u64, RxSignal)>,
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
            signals: Vec::new(),
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
    /// that showed it).
    pub fn drain_signals(&mut self) -> Vec<(u64, RxSignal)> {
        std::mem::take(&mut self.signals)
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
            self.signals.push((self.now, *sig));
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
mod tests {
    use super::*;
    use fax_modem::hdlc::HdlcEvent;
    use fax_modem::v27ter::testtx::Tx as V27Tx;
    use fax_modem::v29::testtx::Tx as V29Tx;

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
        // audio.log: our frame by name, with its octets (no station id: no CSI)
        let log = e.drain_log();
        assert!(
            log.iter().any(|l| l.starts_with("V.21 tx DIS FF C8 ")),
            "{log:#?}"
        );
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
        // FIF 00 46 00 is V.17 14400: not received by this receiver
        let log = e.drain_log();
        assert!(
            log.iter()
                .any(|l| l.contains("DCS selects") && l.contains("not received")),
            "the DCS reached T.30"
        );
        // every V.21 frame is in the log (audio.log): ours sent, the peer's received
        for want in [
            "V.21 tx DIS FF C8 ",
            "V.21 rx TSI FF C0 ",
            "V.21 rx DCS FF C8 ",
        ] {
            assert!(
                log.iter().any(|l| l.starts_with(want)),
                "no {want:?} in {log:#?}"
            );
        }
        let dcs = log.iter().find(|l| l.starts_with("V.21 rx DCS")).unwrap();
        assert_eq!(dcs, "V.21 rx DCS FF C8 41 00 46 00", "all octets, in hex");
    }

    /// The DIS the engine sends advertises V.27ter and V.29, and not V.17 (ADR-061).
    #[test]
    fn the_dis_offers_v27ter_and_v29_but_not_v17() {
        let mut e = AudioEngine::new(AudioConfig::default());
        e.start();
        let out = run(&mut e, &[], 6_000);
        let frames = frames_in(&out[1600 + 24_000..]);
        let dis = frames
            .iter()
            .find(|f| f[2] == t30::fcf::DIS)
            .unwrap_or_else(|| panic!("no DIS in {frames:02x?}"));
        let dis = t30::Dis::parse(&dis[3..]);
        assert!(
            dis.v27ter() && dis.v29() && !dis.v17() && !dis.v33(),
            "DIS modems: {dis:?}"
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

    /// The DCS information field for V.29 at `rate` bit/s: bit 11 (9600), bits 11 and 12 (7200).
    fn dcs_fif_v29(rate: u32) -> Vec<u8> {
        let fif = if rate == 9600 {
            vec![0x00, 0x20, 0x00]
        } else {
            vec![0x00, 0x30, 0x00]
        };
        assert_eq!(
            t30::caps::parse_rate(&fif).unwrap(),
            (t30::caps::Modem::V29, rate)
        );
        fif
    }

    /// A V.27ter 4800 sender of the TCF and the image.
    fn v27ter_sender() -> impl Fn(&[bool]) -> Vec<i16> {
        |bits| {
            V27Tx {
                rate: v27ter::Rate::B4800,
                level_dbm0: -15.0,
                long: true,
                carrier_offset_hz: 0.0,
                clock: 1.0,
            }
            .modulate(bits)
        }
    }

    /// A V.29 sender of the TCF and the image at `rate`.
    fn v29_sender(rate: v29::Rate) -> impl Fn(&[bool]) -> Vec<i16> {
        move |bits| {
            V29Tx {
                rate,
                level_dbm0: -15.0,
                carrier_offset_hz: 0.0,
                clock: 1.0,
            }
            .modulate(bits)
        }
    }

    /// `n` pseudo-random bits (xorshift64, a fixed seed): about half of them are ones.
    fn random_bits(n: usize) -> Vec<bool> {
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x & 1 != 0
            })
            .collect()
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

    /// A call up to and through the TCF: the answer, the peer's TSI and a DCS with `fif`, then
    /// `tcf` sent by `transmit` and silence. Returns the frames the engine sent after the TCF and
    /// the log.
    fn tcf_call(
        fif: &[u8],
        transmit: &dyn Fn(&[bool]) -> Vec<i16>,
        tcf: &[bool],
    ) -> (Vec<Vec<u8>>, Vec<String>) {
        let mut e = AudioEngine::new(AudioConfig::default());
        e.start();
        let _ = run(&mut e, &[], 5_000);
        let tsi = vec![0xFF, 0xC0, t30::fcf::TSI, b' ', b' ', b'1'];
        let mut dcs = vec![0xFF, 0xC8, t30::fcf::DCS];
        dcs.extend_from_slice(fif);
        let wire: Vec<Vec<u8>> = [tsi, dcs].iter().map(|f| reverse_octets(f)).collect();
        let mut v21 = V21Tx::new(-15.0);
        let mut peer = vec![0i16; 800];
        peer.extend(v21.modulate(&hdlc::encode_burst(&wire, 38, 2)));
        let _ = run(&mut e, &peer, 3_000);
        let mut log = e.drain_log();
        let input = transmit(tcf);
        // whole 20 ms blocks, and 4 s of silence after the signal for our answer
        let blocks = input.len() / 160 + 1 + 200;
        let out = run(&mut e, &input, blocks as u64 * 20);
        log.extend(e.drain_log());
        (frames_in(&out), log)
    }

    /// The TCF `tcf` on V.27ter 4800 bit/s: its FCFs and the log.
    fn v27ter_tcf(tcf: &[bool]) -> (Vec<u8>, Vec<String>) {
        let (frames, log) = tcf_call(&dcs_fif_v27ter_4800(), &v27ter_sender(), tcf);
        (frames.iter().map(|f| f[2]).collect(), log)
    }

    /// The TCF `tcf` on V.29 at `rate` bit/s: its FCFs and the log.
    fn v29_tcf(rate: u32, tcf: &[bool]) -> (Vec<u8>, Vec<String>) {
        let modem = if rate == 9600 {
            v29::Rate::B9600
        } else {
            v29::Rate::B7200
        };
        let (frames, log) = tcf_call(&dcs_fif_v29(rate), &v29_sender(modem), tcf);
        (frames.iter().map(|f| f[2]).collect(), log)
    }

    #[test]
    fn a_v27ter_tcf_of_zeros_is_answered_with_cfr() {
        let tcf = vec![false; 7_200]; // 1.5 s at 4800 bit/s
        let (fcfs, log) = v27ter_tcf(&tcf);
        let tcf_line = log.iter().find(|l| l.starts_with("TCF:")).cloned();
        assert!(
            log.iter().any(|l| l == "V.27ter 4800 receiver ready"),
            "{log:#?}"
        );
        assert!(
            fcfs.contains(&t30::fcf::CFR),
            "no CFR ({tcf_line:?}); frames {fcfs:02x?}"
        );
    }

    #[test]
    fn a_v27ter_tcf_of_random_bits_is_answered_with_ftt() {
        let (fcfs, log) = v27ter_tcf(&random_bits(7_200));
        let tcf_line = log.iter().find(|l| l.starts_with("TCF:")).cloned();
        assert!(
            fcfs.contains(&t30::fcf::FTT) && !fcfs.contains(&t30::fcf::CFR),
            "expected FTT ({tcf_line:?}); frames {fcfs:02x?}"
        );
    }

    #[test]
    fn a_v29_9600_tcf_of_zeros_is_answered_with_cfr() {
        let tcf = vec![false; 14_400]; // 1.5 s at 9600 bit/s
        let (fcfs, log) = v29_tcf(9_600, &tcf);
        let tcf_line = log.iter().find(|l| l.starts_with("TCF:")).cloned();
        assert!(
            log.iter().any(|l| l == "V.29 9600 receiver ready"),
            "{log:#?}"
        );
        assert!(
            fcfs.contains(&t30::fcf::CFR),
            "no CFR ({tcf_line:?}); frames {fcfs:02x?}"
        );
    }

    #[test]
    fn a_v29_9600_tcf_of_random_bits_is_answered_with_ftt() {
        let (fcfs, log) = v29_tcf(9_600, &random_bits(14_400));
        let tcf_line = log.iter().find(|l| l.starts_with("TCF:")).cloned();
        assert!(
            fcfs.contains(&t30::fcf::FTT) && !fcfs.contains(&t30::fcf::CFR),
            "expected FTT ({tcf_line:?}); frames {fcfs:02x?}"
        );
    }

    #[test]
    fn a_v29_7200_tcf_of_zeros_is_answered_with_cfr() {
        let tcf = vec![false; 10_800]; // 1.5 s at 7200 bit/s
        let (fcfs, log) = v29_tcf(7_200, &tcf);
        let tcf_line = log.iter().find(|l| l.starts_with("TCF:")).cloned();
        assert!(
            log.iter().any(|l| l == "V.29 7200 receiver ready"),
            "{log:#?}"
        );
        assert!(
            fcfs.contains(&t30::fcf::CFR),
            "no CFR ({tcf_line:?}); frames {fcfs:02x?}"
        );
    }

    /// M5e ⑤: the trunk may offer ECM, the audio path does not.
    #[test]
    fn the_dis_never_offers_ecm_or_mmr() {
        let mut cfg = AudioConfig::default();
        cfg.t30.ecm = true;
        cfg.t30.mmr = true;
        let mut e = AudioEngine::new(cfg);
        e.start();
        let out = run(&mut e, &[], 6_000);
        let frames = frames_in(&out[1600 + 24_000..]);
        let dis = frames
            .iter()
            .find(|f| f[2] == t30::fcf::DIS)
            .unwrap_or_else(|| panic!("no DIS in {frames:02x?}"));
        let dis = t30::Dis::parse(&dis[3..]);
        assert!(!dis.ecm && !dis.mmr, "DIS: {dis:?}");
    }

    /// M5e ⑥: a DCS for V.17 (not offered) and its TCF: FTT right after the TCF, not after T2.
    #[test]
    fn a_tcf_on_a_modem_without_a_receiver_is_answered_with_ftt_at_once() {
        let mut e = AudioEngine::new(AudioConfig::default());
        e.start();
        let _ = run(&mut e, &[], 6_000);
        let tsi = vec![0xFF, 0xC0, t30::fcf::TSI, b' ', b' ', b'1'];
        // V.17 14400
        let fif = vec![0x00, 0x46, 0x00];
        assert_eq!(
            t30::caps::parse_rate(&fif).unwrap(),
            (t30::caps::Modem::V17, 14400)
        );
        let mut dcs = vec![0xFF, 0xC8, t30::fcf::DCS];
        dcs.extend(fif);
        let wire: Vec<Vec<u8>> = [tsi, dcs].iter().map(|f| reverse_octets(f)).collect();
        let mut v21 = V21Tx::new(-15.0);
        let mut peer = vec![0i16; 800];
        peer.extend(v21.modulate(&hdlc::encode_burst(&wire, 38, 2)));
        // 75 ms, then 1.5 s of "V.17" (any signal: there is no receiver for it)
        peer.extend(vec![0i16; 600]);
        let tcf_end = peer.len();
        peer.extend(
            V29Tx {
                rate: v29::Rate::B9600,
                level_dbm0: -15.0,
                carrier_offset_hz: 0.0,
                clock: 1.0,
            }
            .modulate(&vec![false; 14_400]),
        );
        let tcf_end = tcf_end + (peer.len() - tcf_end);
        let out = run(&mut e, &peer, (tcf_end as u64 / 8) + 3_000);
        let log = e.drain_log();
        // the FTT starts (preamble) within 1 s of the end of the TCF (T2 is 6 s)
        let window = &out[tcf_end..tcf_end + 8 * 2_500];
        let fcfs: Vec<u8> = frames_in(window).iter().map(|f| f[2]).collect();
        assert!(fcfs.contains(&t30::fcf::FTT), "{fcfs:02x?} {log:#?}");
        let first_sound = window.iter().position(|&s| s != 0).unwrap_or(usize::MAX);
        assert!(
            first_sound < 8 * 1_000,
            "FTT {} ms after the TCF",
            first_sound / 8
        );
    }

    /// M5e ⑦: the scrambled ones of V.29 Segment 4 are not counted in the TCF.
    #[test]
    fn a_v29_tcf_starts_after_segment_4() {
        let (_, log) = v29_tcf(9_600, &vec![false; 14_400]);
        let line = log
            .iter()
            .find(|l| l.starts_with("TCF:"))
            .unwrap_or_else(|| panic!("{log:#?}"));
        // "TCF: n octets (need m), z of t bits zero": the ones left are the end of the
        // carrier (the test transmitter's tail of 8 symbols and the 10 ms it takes to hear the
        // end, about 72 bits); Segment 4 alone would add 192
        let nums: Vec<u64> = line
            .split(|c: char| !c.is_ascii_digit())
            .filter_map(|w| w.parse().ok())
            .collect();
        let (zero, total) = (nums[2], nums[3]);
        assert!(total - zero <= 100, "{line}");
    }

    /// M5e ④: a 20 ms packet lost in the middle of a TCF of zeros: the receiver keeps going and
    /// the TCF is still good (before, the gap ended the carrier and the TCF was too short).
    #[test]
    fn a_lost_packet_in_the_middle_of_a_tcf_does_not_end_it() {
        for rate in [9_600u32, 7_200] {
            let modem = if rate == 9600 {
                v29::Rate::B9600
            } else {
                v29::Rate::B7200
            };
            let mut e = AudioEngine::new(AudioConfig::default());
            e.start();
            let _ = run(&mut e, &[], 6_000);
            let tsi = vec![0xFF, 0xC0, t30::fcf::TSI, b' ', b' ', b'1'];
            let mut dcs = vec![0xFF, 0xC8, t30::fcf::DCS];
            dcs.extend(dcs_fif_v29(rate));
            send_v21(&mut e, &[tsi, dcs]);
            let input = v29_sender(modem)(&vec![false; rate as usize * 3 / 2]);
            let mut out = Vec::new();
            let blocks = input.len() / 160 + 1 + 200;
            // a block about 0.9 s into the TCF is lost
            let lost = (8 * 900 + 400 + 8 * 300) / 160;
            for b in 0..blocks {
                let mut rx = [0i16; 160];
                let from = (b * 160).min(input.len());
                let n = input.len().saturating_sub(from).min(160);
                rx[..n].copy_from_slice(&input[from..from + n]);
                let mut tx = [0i16; 160];
                if b == lost {
                    e.process_lost(160, &mut tx);
                } else {
                    e.process(&rx, &mut tx);
                }
                out.extend_from_slice(&tx);
            }
            let fcfs: Vec<u8> = frames_in(&out).iter().map(|f| f[2]).collect();
            let log = e.drain_log();
            assert!(
                fcfs.contains(&t30::fcf::CFR),
                "{rate}: {fcfs:02x?} {:?}",
                log.iter().find(|l| l.starts_with("TCF:"))
            );
        }
    }

    /// M5e ④: an outage longer than `max_lost_skip_ms` is silence, so the signal ends.
    #[test]
    fn a_long_outage_counts_as_silence() {
        let mut e = AudioEngine::new(AudioConfig::default());
        e.start();
        let _ = run(&mut e, &[], 6_000);
        let mut v21 = V21Tx::new(-15.0);
        let flags = v21.modulate(&hdlc::encode_burst(&[], 38, 0));
        let _ = run(
            &mut e,
            &flags[..flags.len() / 160 * 160],
            flags.len() as u64 / 8,
        );
        assert!(e.peer_on, "the flags are heard");
        let mut tx = [0i16; 160];
        for _ in 0..3 {
            e.process_lost(160, &mut tx);
        }
        assert!(e.peer_on, "60 ms lost: skipped");
        for _ in 0..2 {
            e.process_lost(160, &mut tx);
        }
        assert!(!e.peer_on, "100 ms lost: silence, the signal has ended");
    }

    #[test]
    fn a_v29_7200_tcf_of_random_bits_is_answered_with_ftt() {
        let (fcfs, log) = v29_tcf(7_200, &random_bits(10_800));
        let tcf_line = log.iter().find(|l| l.starts_with("TCF:")).cloned();
        assert!(
            fcfs.contains(&t30::fcf::FTT) && !fcfs.contains(&t30::fcf::CFR),
            "expected FTT ({tcf_line:?}); frames {fcfs:02x?}"
        );
    }
}
