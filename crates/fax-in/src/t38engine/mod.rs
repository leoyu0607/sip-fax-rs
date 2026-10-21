//! Sans-IO T.38 receive engine: IFP packets in, IFP packets out, a virtual clock in between.
//!
//! [`T38Engine`] glues the three protocol crates:
//!
//! * `t38` supplies the [`Ifp`] packet model (and, in [`UdptlEngine`], UDPTL framing and loss
//!   recovery);
//! * `t30` is the receiving fax terminal ([`t30::Receiver`]), driven with [`t30::Event`]s;
//! * `t4` decodes the pages (inside `t30`) and re-encodes the accepted ones as MMR here.
//!
//! The engine owns no socket and no clock. The caller says what time it is:
//!
//! ```text
//! engine.start(now_ms);                 // the call is answered: CED, CSI, DIS
//! loop {
//!     engine.advance(now_ms);           // release due packets, run due T.30 timers
//!     for ifp in engine.poll_output() { send(ifp); }   // or poll_timed() for the instants
//!     engine.on_ifp(now_ms, &received); // a packet arrived
//!     sleep_until(engine.next_wakeup_ms());
//! }
//! ```
//!
//! # Pacing
//!
//! Outgoing packets are spaced the way a real fax machine would send the same signal; see
//! [`tx`] for the table of delays and where each number comes from (SpanDSP's `t38_terminal`).
//! Indicators go out [`EngineConfig::indicator_repeat`] times (default 3) and the final
//! `sig-end` of a burst [`EngineConfig::data_end_repeat`] times, as SpanDSP does, to survive
//! UDP loss even without redundancy.
//!
//! # Output
//!
//! Accepted pages are re-encoded as MMR (G4) strips ready for `t4::write_tiff` /
//! `t4::write_pdf` ([`ReceivedPage`]); writing files is the job of the session layer.

mod rx;
mod tx;
mod udptl;

pub use tx::Pacing;
pub use udptl::{TimedDatagram, UdptlEngine};

use std::collections::VecDeque;
use t30::{Action, Event, Timer};
use t38::{Ifp, IfpError, T38Version};
use tx::{Item, Scheduler};

/// Engine settings.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// T.30 capabilities, thresholds and timers.
    pub t30: t30::Config,
    /// Negotiated T.38 version (selects the IFP encoding).
    pub version: T38Version,
    /// Transmit pacing.
    pub pacing: Pacing,
    /// Copies of every indicator (default 3).
    pub indicator_repeat: u8,
    /// Copies of the final `sig-end` packet of a burst (default 3).
    pub data_end_repeat: u8,
    /// Diagnostic lines kept for [`T38Engine::drain_log`] (older ones are dropped).
    pub max_log_lines: usize,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            t30: t30::Config::default(),
            version: T38Version::V0,
            pacing: Pacing::default(),
            indicator_repeat: 3,
            data_end_repeat: 3,
            max_log_lines: 512,
        }
    }
}

/// An outgoing packet with the millisecond it is due.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimedIfp {
    /// Due time on the caller's clock, in milliseconds (rounded down).
    pub at_ms: u64,
    /// The packet.
    pub ifp: Ifp,
}

impl TimedIfp {
    /// The PER encoding of the packet.
    ///
    /// # Errors
    /// [`IfpError`] if the packet is not representable in `version`.
    pub fn encode(&self, version: T38Version) -> Result<Vec<u8>, IfpError> {
        self.ifp.encode(version)
    }
}

/// A received page, re-encoded for storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedPage {
    /// 1-based page number.
    pub number: u32,
    /// Pixels per row.
    pub width: u32,
    /// Rows.
    pub height: u32,
    /// Horizontal resolution in dpi (Class F: 204).
    pub x_dpi: u32,
    /// Vertical resolution in dpi (98 standard, 196 fine).
    pub y_dpi: u32,
    /// The page as one T.6 (MMR / G4) strip; empty only if encoding failed.
    pub g4: Vec<u8>,
    /// Rows that were replaced because they did not decode.
    pub bad_lines: u32,
    /// Longest run of consecutive bad rows.
    pub longest_bad_run: u32,
    /// Coding the page arrived in.
    pub source_coding: t30::Compression,
    /// Modem it arrived over.
    pub modem: t30::Modem,
}

impl ReceivedPage {
    /// The page as a `t4::Page` for the TIFF / PDF writers.
    pub fn as_t4_page(&self) -> t4::Page<'_> {
        t4::Page {
            width: self.width,
            height: self.height,
            x_dpi: self.x_dpi,
            y_dpi: self.y_dpi,
            g4: &self.g4,
            bad_lines: self.bad_lines,
            longest_bad_run: self.longest_bad_run,
        }
    }
}

/// The receive engine.
#[derive(Debug)]
pub struct T38Engine {
    cfg: EngineConfig,
    t30: t30::Receiver,
    sched: Scheduler,
    rx: rx::RxAdapter,
    /// Deadlines of the T.30 timers in microseconds, indexed like `Timer::ALL`.
    timers: [Option<u64>; 4],
    clock_us: u64,
    out: VecDeque<TimedIfp>,
    pages: Vec<ReceivedPage>,
    result: Option<t30::T30Result>,
    log: VecDeque<String>,
    events_in: u64,
}

fn timer_index(t: Timer) -> usize {
    Timer::ALL.iter().position(|x| *x == t).unwrap_or(0)
}

impl T38Engine {
    /// Creates an engine; nothing happens until [`T38Engine::start`].
    pub fn new(cfg: EngineConfig) -> Self {
        Self {
            t30: t30::Receiver::new(cfg.t30.clone()),
            sched: Scheduler::new(cfg.pacing, cfg.indicator_repeat, cfg.data_end_repeat),
            rx: rx::RxAdapter::default(),
            timers: [None; 4],
            clock_us: 0,
            out: VecDeque::new(),
            pages: Vec::new(),
            result: None,
            log: VecDeque::new(),
            events_in: 0,
            cfg,
        }
    }

    /// The configuration.
    pub fn config(&self) -> &EngineConfig {
        &self.cfg
    }

    /// The T.30 state, for diagnostics.
    pub fn state(&self) -> t30::State {
        self.t30.state()
    }

    /// The call is answered at `now_ms`: send CED, CSI and DIS.
    pub fn start(&mut self, now_ms: u64) {
        self.advance(now_ms);
        self.feed(self.clock_us, Event::Start);
        self.advance(now_ms);
    }

    /// Moves the clock to `now_ms` (milliseconds on the caller's clock): releases the packets
    /// that fall due and runs the T.30 timers and transmit completions in time order.
    pub fn advance(&mut self, now_ms: u64) {
        let limit = now_ms.saturating_mul(1_000).saturating_add(999);
        loop {
            let item_at = self.sched.next_at().filter(|t| *t <= limit);
            let timer = self
                .timers
                .iter()
                .enumerate()
                .filter_map(|(i, d)| d.map(|d| (d, i)))
                .filter(|(d, _)| *d <= limit)
                .min();
            match (item_at, timer) {
                (None, None) => break,
                (Some(t), tm) if tm.is_none_or(|(d, _)| t <= d) => {
                    if let Some((at, item)) = self.sched.pop() {
                        self.clock_us = self.clock_us.max(at);
                        match item {
                            Item::Packet(ifp) => self.out.push_back(TimedIfp {
                                at_ms: at / 1_000,
                                ifp,
                            }),
                            Item::TxComplete => self.feed(at, Event::TxComplete),
                        }
                    }
                }
                (_, Some((d, i))) => {
                    if let Some(slot) = self.timers.get_mut(i) {
                        *slot = None;
                    }
                    self.clock_us = self.clock_us.max(d);
                    if let Some(timer) = Timer::ALL.get(i) {
                        self.feed(d, Event::Timer(*timer));
                    }
                }
                _ => break,
            }
        }
        self.clock_us = self.clock_us.max(now_ms.saturating_mul(1_000));
    }

    /// A packet arrived at `now_ms`.
    pub fn on_ifp(&mut self, now_ms: u64, ifp: &Ifp) {
        self.advance(now_ms);
        if self.t30.is_done() {
            return;
        }
        self.events_in += 1;
        let mut events = Vec::new();
        self.rx.on_ifp(ifp, &mut events);
        for ev in events {
            self.feed(self.clock_us, ev);
        }
        // Responses that are due immediately (no pacing) leave now.
        self.advance(now_ms);
    }

    /// Decodes and handles a packet (PER encoded with the configured version). Trailing zero
    /// octets (a FEC-recovered packet is padded) are tolerated.
    ///
    /// # Errors
    /// The decode error; the packet is then counted as lost data.
    pub fn on_ifp_bytes(&mut self, now_ms: u64, bytes: &[u8]) -> Result<(), IfpError> {
        match Ifp::decode_prefix(bytes, self.cfg.version) {
            Ok((ifp, _)) => {
                self.on_ifp(now_ms, &ifp);
                Ok(())
            }
            Err(e) => {
                self.on_loss(now_ms);
                Err(e)
            }
        }
    }

    /// The transport lost (or could not decode) data: a frame in progress becomes unusable.
    pub fn on_loss(&mut self, now_ms: u64) {
        self.advance(now_ms);
        self.rx.on_loss();
    }

    /// Ends the call at `now_ms` (hangup, error). Pending output is discarded.
    pub fn abort(&mut self, now_ms: u64) {
        self.advance(now_ms);
        self.feed(self.clock_us, Event::Abort);
        self.sched.clear();
    }

    /// Packets that fell due since the last call, in order.
    pub fn poll_output(&mut self) -> Vec<Ifp> {
        self.out.drain(..).map(|t| t.ifp).collect()
    }

    /// Like [`T38Engine::poll_output`] with the due times.
    pub fn poll_timed(&mut self) -> Vec<TimedIfp> {
        self.out.drain(..).collect()
    }

    /// The next millisecond at which [`T38Engine::advance`] has something to do.
    pub fn next_wakeup_ms(&self) -> Option<u64> {
        let item = self.sched.next_at();
        let timer = self.timers.iter().flatten().min().copied();
        [item, timer]
            .into_iter()
            .flatten()
            .min()
            .map(|us| us / 1_000)
    }

    /// True once the call is over (result available).
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

    /// Pages accepted so far (without taking them).
    pub fn pages(&self) -> &[ReceivedPage] {
        &self.pages
    }

    /// Takes the diagnostic lines collected so far.
    pub fn drain_log(&mut self) -> Vec<String> {
        self.log.drain(..).collect()
    }

    /// Counters of the receive adapter: (duplicate packets dropped, packets ignored).
    pub fn rx_counters(&self) -> (u64, u64) {
        (self.rx.duplicates, self.rx.ignored)
    }

    fn note(&mut self, line: String) {
        tracing::debug!(target: "fax_in::t38engine", "{line}");
        if self.log.len() >= self.cfg.max_log_lines.max(1) {
            self.log.pop_front();
        }
        self.log.push_back(line);
    }

    /// Hands one event to T.30 at `at_us` and carries out the resulting actions.
    fn feed(&mut self, at_us: u64, event: Event) {
        let actions = self.t30.on_event(at_us / 1_000, event);
        for action in actions {
            match action {
                Action::SendSignal(t30::TxSignal::Ced) => self.sched.plan_ced(at_us),
                Action::SendFrames(frames) => {
                    self.sched.plan_frames(at_us, &frames);
                }
                Action::StartTimer { timer, ms } => {
                    if let Some(slot) = self.timers.get_mut(timer_index(timer)) {
                        *slot = Some(at_us + u64::from(ms) * 1_000);
                    }
                }
                Action::CancelTimer(timer) => {
                    if let Some(slot) = self.timers.get_mut(timer_index(timer)) {
                        *slot = None;
                    }
                }
                Action::Page(page) => self.store_page(&page),
                Action::Log(line) => self.note(line),
                Action::Done(result) => {
                    self.note(format!(
                        "T.30 done: {} after {} page(s), {} ms",
                        result.code.name(),
                        result.pages,
                        result.duration_ms
                    ));
                    self.result = Some(*result);
                    // Nothing more is sent after the end of the call.
                    self.sched.clear();
                }
            }
        }
    }

    fn store_page(&mut self, p: &t30::PageData) {
        let g4 = match t4::encode_mmr(&p.bitmap, p.width) {
            Ok(g4) => g4,
            Err(e) => {
                self.note(format!("page {}: MMR encoding failed: {e}", p.number));
                Vec::new()
            }
        };
        self.pages.push(ReceivedPage {
            number: p.number,
            width: p.width,
            height: p.rows,
            x_dpi: p.x_dpi,
            y_dpi: p.y_dpi,
            g4,
            bad_lines: p.bad_lines,
            longest_bad_run: p.longest_bad_run,
            source_coding: p.compression,
            modem: p.modem,
        });
    }
}
