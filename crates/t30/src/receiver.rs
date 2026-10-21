//! The T.30 answering (receiving) terminal, non-ECM.
//!
//! [`Receiver::on_event`] is a pure function of the machine state and the event: no clock, no
//! I/O. Time enters only as the `now_ms` argument (used for the call duration) and as
//! [`Event::Timer`] events answering [`Action::StartTimer`]. The states follow ITU-T T.30
//! section 5 (flow charts of the called terminal) at the level of detail the receiver needs:
//!
//! ```text
//!  Idle --Start--> SendingDis --TxComplete--> WaitCommand --DCS--> WaitTcf --TCF--> RxTcf
//!    WaitCommand: T4 expiry retransmits the DIS (dis_retries times), then fails
//!    RxTcf --end--> SendingCfr | SendingFtt
//!    SendingCfr --TxComplete--> WaitImage --data--> RxImage --end--> WaitPostPage
//!    SendingFtt --TxComplete--> WaitRetrain --DCS--> WaitTcf ...
//!    WaitPostPage --MPS/EOP/EOM--> SendingPostPage (MCF | RTP | RTN)
//!    SendingPostPage --TxComplete--> WaitImage (MPS) | WaitDcn (EOP) | SendingDis (EOM)
//!                                    | WaitRetrain (RTN / RTP)
//!    any --DCN--> Done;  failures --> SendingDcn --TxComplete--> Done
//! ```

use crate::caps::{Dcs, DcsError, Unsupported, build_dis_fif};
use crate::config::{Config, TcfMode};
use crate::event::{Action, Event, PageData, RxSignal, Timer, TxSignal};
use crate::frame::{self, Frame, decode_ident, encode_ident, fcf, fcf_name};
use crate::result::{ResultCode, T30Result};

/// Externally visible state of the receiver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum State {
    /// Before `Start`.
    Idle,
    /// Transmitting the (CED,) CSI and DIS burst; waiting for `TxComplete`.
    SendingDis,
    /// DIS sent; waiting for the first command (T4 retransmits the DIS).
    WaitCommand,
    /// DCS accepted, transferred TCF: waiting for the TCF to start (T2).
    WaitTcf,
    /// Receiving the TCF.
    RxTcf,
    /// Transmitting CFR.
    SendingCfr,
    /// Transmitting FTT.
    SendingFtt,
    /// After FTT / RTN / RTP: waiting for a new DCS (T2).
    WaitRetrain,
    /// After CFR or after MCF to an MPS: waiting for the page to start (T2).
    WaitImage,
    /// Receiving page data.
    RxImage,
    /// Page data ended: waiting for MPS, EOP or EOM (T2).
    WaitPostPage,
    /// Transmitting MCF, RTP or RTN.
    SendingPostPage,
    /// After MCF to an EOP: waiting for DCN (T2).
    WaitDcn,
    /// Transmitting DCN before ending the call with a failure code.
    SendingDcn,
    /// The call is over.
    Done,
}

impl State {
    /// Every state, in declaration order.
    pub const ALL: [State; 15] = [
        State::Idle,
        State::SendingDis,
        State::WaitCommand,
        State::WaitTcf,
        State::RxTcf,
        State::SendingCfr,
        State::SendingFtt,
        State::WaitRetrain,
        State::WaitImage,
        State::RxImage,
        State::WaitPostPage,
        State::SendingPostPage,
        State::WaitDcn,
        State::SendingDcn,
        State::Done,
    ];
}

/// What follows once the post-page response has been sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Next {
    /// Next page (after MCF to MPS).
    Image,
    /// Wait for DCN (after MCF to EOP).
    Dcn,
    /// Back to phase B (after MCF to EOM).
    PhaseB,
    /// Retrain (after RTN, or RTP).
    Retrain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Mcf,
    Rtp,
    Rtn,
}

/// The receiving T.30 terminal.
#[derive(Debug)]
pub struct Receiver {
    cfg: Config,
    state: State,
    started_ms: u64,
    now_ms: u64,
    armed: [bool; 4],
    /// Frame bursts handed out and not yet reported complete.
    tx_outstanding: u32,
    dis_retx: u32,
    unexpected: u32,
    ftt_count: u32,
    remote_ident: Option<String>,
    pending_tsi: Option<String>,
    dcs: Option<Dcs>,
    last_response: Vec<Vec<u8>>,
    /// FCF of the last post-page command that was answered, until new image data arrives.
    answered_cmd: Option<u8>,
    post_next: Next,
    /// The last accepted page ended the procedure (EOP) but the response asked for a retrain.
    eop_confirmed: bool,
    last_verdict: Option<Verdict>,
    /// A repeated response is on its way while the machine waits (F1): when it has left, the
    /// wait timer of the state starts over.
    rearm_after_tx: bool,
    tcf_bytes: u64,
    tcf_zero_bits: u64,
    decoder: Option<t4::Decoder>,
    page_rows: Vec<u8>,
    page_summary: Option<t4::Summary>,
    bad_lines: Vec<u32>,
    pending_code: ResultCode,
}

impl Receiver {
    /// Creates a receiver in [`State::Idle`].
    pub fn new(cfg: Config) -> Self {
        Self {
            cfg,
            state: State::Idle,
            started_ms: 0,
            now_ms: 0,
            armed: [false; 4],
            tx_outstanding: 0,
            dis_retx: 0,
            unexpected: 0,
            ftt_count: 0,
            remote_ident: None,
            pending_tsi: None,
            dcs: None,
            last_response: Vec::new(),
            answered_cmd: None,
            post_next: Next::Image,
            eop_confirmed: false,
            last_verdict: None,
            rearm_after_tx: false,
            tcf_bytes: 0,
            tcf_zero_bits: 0,
            decoder: None,
            page_rows: Vec::new(),
            page_summary: None,
            bad_lines: Vec::new(),
            pending_code: ResultCode::Ok,
        }
    }

    /// The current state.
    pub fn state(&self) -> State {
        self.state
    }

    /// True once the call is over.
    pub fn is_done(&self) -> bool {
        self.state == State::Done
    }

    /// The configuration in use.
    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// Pages confirmed so far.
    pub fn pages(&self) -> u32 {
        self.bad_lines.len() as u32
    }

    /// The DCS currently in force.
    pub fn dcs(&self) -> Option<&Dcs> {
        self.dcs.as_ref()
    }

    /// The frames of the DIS burst: CSI (if an identification is configured) and the final DIS.
    pub fn dis_burst(&self) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        if !self.cfg.local_id.is_empty() {
            frames.push(frame::build(
                false,
                fcf::CSI,
                &encode_ident(&self.cfg.local_id),
            ));
        }
        frames.push(frame::build(true, fcf::DIS, &build_dis_fif(&self.cfg)));
        frames
    }

    /// Feeds one event at time `now_ms` and returns the resulting actions.
    pub fn on_event(&mut self, now_ms: u64, event: Event) -> Vec<Action> {
        self.now_ms = self.now_ms.max(now_ms);
        let mut out = Vec::new();
        if self.state == State::Done {
            return out;
        }
        match event {
            Event::Abort => self.abort(&mut out),
            Event::Start => {
                if self.state == State::Idle {
                    self.start(&mut out);
                }
            }
            _ if self.state == State::Idle => {}
            Event::TxComplete => self.on_tx_complete(&mut out),
            Event::Timer(t) => self.on_timer(t, &mut out),
            Event::Frame { data, fcs_ok } => self.on_frame(&data, fcs_ok, &mut out),
            Event::Signal(s) => self.on_signal(s, &mut out),
            Event::ImageData(d) => self.on_image_data(&d, &mut out),
            Event::DataEnd => self.on_data_end(&mut out),
        }
        out
    }

    // ---- helpers -------------------------------------------------------------------------

    fn arm(&mut self, timer: Timer, ms: u32, out: &mut Vec<Action>) {
        if let Some(slot) = self.armed.get_mut(timer.index()) {
            *slot = true;
        }
        // A restart needs no preceding cancel.
        if out.last() == Some(&Action::CancelTimer(timer)) {
            out.pop();
        }
        out.push(Action::StartTimer { timer, ms });
    }

    fn cancel(&mut self, timer: Timer, out: &mut Vec<Action>) {
        if let Some(slot) = self.armed.get_mut(timer.index()) {
            if *slot {
                *slot = false;
                out.push(Action::CancelTimer(timer));
            }
        }
    }

    fn cancel_all(&mut self, out: &mut Vec<Action>) {
        for t in Timer::ALL {
            self.cancel(t, out);
        }
    }

    fn is_armed(&self, timer: Timer) -> bool {
        self.armed.get(timer.index()).copied().unwrap_or(false)
    }

    fn log(out: &mut Vec<Action>, msg: String) {
        out.push(Action::Log(msg));
    }

    /// Hands a burst to the front end and remembers it for CRP.
    fn send(&mut self, frames: Vec<Vec<u8>>, out: &mut Vec<Action>) {
        self.last_response = frames.clone();
        self.send_raw(frames, out);
    }

    /// Repeats the last response while waiting: the peer did not hear it. The wait timer
    /// stops while the response goes out and starts over once it has left.
    fn resend_last(&mut self, out: &mut Vec<Action>) {
        let resend = self.last_response.clone();
        // In phase B the T4 retransmission of the DIS keeps running on its own (pinned by R01).
        if self.state != State::WaitCommand {
            self.cancel(Timer::T2, out);
            self.rearm_after_tx = true;
        }
        self.send_raw(resend, out);
    }

    fn send_raw(&mut self, frames: Vec<Vec<u8>>, out: &mut Vec<Action>) {
        self.tx_outstanding += 1;
        out.push(Action::SendFrames(frames));
    }

    fn simple(fcf_value: u8) -> Vec<Vec<u8>> {
        vec![frame::build(true, fcf_value, &[])]
    }

    fn result(&self, code: ResultCode) -> T30Result {
        T30Result {
            code,
            pages: self.pages(),
            bit_rate: self.dcs.map_or(0, |d| d.bit_rate),
            modem: self.dcs.map(|d| d.modem),
            resolution: self.dcs.map(|d| d.resolution),
            compression: self.dcs.map(|d| d.compression),
            remote_ident: self.remote_ident.clone(),
            bad_lines_per_page: self.bad_lines.clone(),
            duration_ms: self.now_ms.saturating_sub(self.started_ms),
        }
    }

    fn finish(&mut self, code: ResultCode, out: &mut Vec<Action>) {
        self.cancel_all(out);
        self.state = State::Done;
        out.push(Action::Done(Box::new(self.result(code))));
    }

    /// Ends the call with a failure after telling the peer with a DCN.
    fn fail(&mut self, code: ResultCode, out: &mut Vec<Action>) {
        self.cancel_all(out);
        self.pending_code = code;
        self.state = State::SendingDcn;
        self.send_raw(Self::simple(fcf::DCN), out);
    }

    fn abort(&mut self, out: &mut Vec<Action>) {
        self.finish(ResultCode::Aborted, out);
    }

    // ---- start and transmit completion ---------------------------------------------------

    fn start(&mut self, out: &mut Vec<Action>) {
        self.started_ms = self.now_ms;
        self.state = State::SendingDis;
        self.arm(Timer::T1, self.cfg.timers.t1_ms, out);
        out.push(Action::SendSignal(TxSignal::Ced));
        let burst = self.dis_burst();
        self.send(burst, out);
    }

    fn on_tx_complete(&mut self, out: &mut Vec<Action>) {
        self.tx_outstanding = self.tx_outstanding.saturating_sub(1);
        if self.tx_outstanding > 0 {
            return;
        }
        let t2 = self.cfg.timers.t2_ms;
        if std::mem::take(&mut self.rearm_after_tx) {
            // A repeated response has left while we were waiting: the wait starts over, as
            // after the first response (T.30: the timer is restarted whenever a response is
            // sent). If the machine has moved on meanwhile, the flag is simply void.
            match self.state {
                State::WaitTcf
                | State::WaitRetrain
                | State::WaitImage
                | State::WaitPostPage
                | State::WaitDcn => {
                    self.arm(Timer::T2, t2, out);
                    return;
                }
                _ => {}
            }
        }
        match self.state {
            State::SendingDis => {
                self.state = State::WaitCommand;
                self.arm(Timer::T4, self.cfg.timers.t4_ms, out);
            }
            State::SendingCfr => {
                self.state = State::WaitImage;
                self.arm(Timer::T2, t2, out);
            }
            State::SendingFtt => {
                self.state = State::WaitRetrain;
                self.arm(Timer::T2, t2, out);
            }
            State::SendingPostPage => match self.post_next {
                Next::Image => {
                    self.state = State::WaitImage;
                    self.arm(Timer::T2, t2, out);
                }
                Next::Dcn => {
                    self.state = State::WaitDcn;
                    self.arm(Timer::T2, t2, out);
                }
                Next::Retrain => {
                    self.state = State::WaitRetrain;
                    self.arm(Timer::T2, t2, out);
                }
                Next::PhaseB => {
                    // EOM: back to phase B. Like the start of the call, minus the CED.
                    self.dis_retx = 0;
                    self.dcs = None;
                    self.state = State::SendingDis;
                    self.arm(Timer::T1, self.cfg.timers.t1_ms, out);
                    let burst = self.dis_burst();
                    self.send(burst, out);
                }
            },
            State::SendingDcn => {
                let code = self.pending_code;
                self.finish(code, out);
            }
            _ => {}
        }
    }

    // ---- timers ---------------------------------------------------------------------------

    fn on_timer(&mut self, timer: Timer, out: &mut Vec<Action>) {
        // Expiries of timers that are no longer running are stale and ignored.
        if !self.is_armed(timer) {
            return;
        }
        if let Some(slot) = self.armed.get_mut(timer.index()) {
            *slot = false;
        }
        match (timer, self.state) {
            (Timer::T1, State::SendingDis | State::WaitCommand) => {
                self.fail(ResultCode::T1Expired, out);
            }
            (Timer::T4, State::WaitCommand) => {
                if self.dis_retx >= self.cfg.dis_retries {
                    self.fail(ResultCode::NoResponseToDis, out);
                } else {
                    self.dis_retx += 1;
                    self.state = State::SendingDis;
                    let burst = self.dis_burst();
                    self.send(burst, out);
                }
            }
            (Timer::T2, State::WaitTcf) => {
                Self::log(out, "T2 expired: no TCF received".into());
                self.send_ftt(out);
            }
            (Timer::T2, State::WaitRetrain | State::WaitPostPage) => {
                self.fail(ResultCode::T2ExpiredCommand, out);
            }
            (Timer::T2, State::WaitImage) => self.fail(ResultCode::T2ExpiredImage, out),
            (Timer::T2, State::WaitDcn) => {
                Self::log(out, "T2 expired waiting for DCN; ending normally".into());
                self.finish(ResultCode::Ok, out);
            }
            (Timer::Data, State::RxTcf) => self.end_tcf(out),
            (Timer::Data, State::RxImage) => {
                Self::log(out, "image data stopped without a signal end".into());
                self.end_image(out);
            }
            _ => {}
        }
    }

    // ---- signals and data ----------------------------------------------------------------

    fn on_signal(&mut self, signal: RxSignal, out: &mut Vec<Action>) {
        if signal == RxSignal::NoSignal {
            match self.state {
                State::RxTcf => self.end_tcf(out),
                State::RxImage => self.end_image(out),
                _ => {}
            }
        }
    }

    fn on_data_end(&mut self, out: &mut Vec<Action>) {
        match self.state {
            State::WaitTcf | State::RxTcf => self.end_tcf(out),
            State::WaitImage => {
                // A signal end with no data at all: an empty page.
                if self.start_page(out) {
                    self.end_image(out);
                }
            }
            State::RxImage => self.end_image(out),
            _ => {}
        }
    }

    fn on_image_data(&mut self, data: &[u8], out: &mut Vec<Action>) {
        let idle = self.cfg.timers.data_idle_ms;
        match self.state {
            State::WaitTcf | State::RxTcf => {
                if self.state == State::WaitTcf {
                    self.cancel(Timer::T2, out);
                    self.state = State::RxTcf;
                }
                self.tcf_bytes += data.len() as u64;
                self.tcf_zero_bits += data.iter().map(|b| u64::from(b.count_zeros())).sum::<u64>();
                self.arm(Timer::Data, idle, out);
            }
            State::WaitImage | State::RxImage => {
                if self.state == State::WaitImage && !self.start_page(out) {
                    return;
                }
                self.push_image(data);
                self.arm(Timer::Data, idle, out);
            }
            _ => {}
        }
    }

    /// Creates the decoder for a new page and moves to `RxImage`.
    fn start_page(&mut self, out: &mut Vec<Action>) -> bool {
        let Some(dcs) = self.dcs else {
            self.fail(ResultCode::DecoderError, out);
            return false;
        };
        let mut dcfg = t4::DecoderConfig::new(dcs.width.pixels(), dcs.compression.coding());
        dcfg.max_rows = self.cfg.max_rows;
        match t4::Decoder::new(dcfg) {
            Ok(d) => {
                self.decoder = Some(d);
                self.page_rows.clear();
                self.page_summary = None;
                self.answered_cmd = None;
                self.cancel(Timer::T2, out);
                self.state = State::RxImage;
                true
            }
            Err(e) => {
                Self::log(out, format!("cannot start the T.4 decoder: {e}"));
                self.fail(ResultCode::DecoderError, out);
                false
            }
        }
    }

    fn push_image(&mut self, data: &[u8]) {
        if let Some(dec) = self.decoder.as_mut() {
            let rows = &mut self.page_rows;
            dec.push(data, &mut |row| rows.extend_from_slice(row));
        }
    }

    fn end_tcf(&mut self, out: &mut Vec<Action>) {
        self.cancel(Timer::Data, out);
        self.cancel(Timer::T2, out);
        let rate = self.dcs.map_or(0, |d| u64::from(d.bit_rate));
        let min_bytes = rate * u64::from(self.cfg.tcf_min_ms) / 8000;
        let total_bits = self.tcf_bytes * 8;
        let zero_ok = total_bits > 0
            && self.tcf_zero_bits * 100 >= total_bits * u64::from(self.cfg.tcf_zero_percent);
        let long_enough = self.tcf_bytes >= min_bytes && self.tcf_bytes > 0;
        Self::log(
            out,
            format!(
                "TCF: {} octets (need {min_bytes}), {} of {total_bits} bits zero",
                self.tcf_bytes, self.tcf_zero_bits
            ),
        );
        if zero_ok && long_enough {
            self.ftt_count = 0;
            self.state = State::SendingCfr;
            self.send(Self::simple(fcf::CFR), out);
        } else {
            self.send_ftt(out);
        }
    }

    fn send_ftt(&mut self, out: &mut Vec<Action>) {
        self.ftt_count += 1;
        self.state = State::SendingFtt;
        self.send(Self::simple(fcf::FTT), out);
    }

    /// Closes the page decoder and keeps the summary for the post-page command.
    fn close_page(&mut self, out: &mut Vec<Action>) {
        self.cancel(Timer::Data, out);
        let summary = match self.decoder.take() {
            Some(dec) => {
                let rows = &mut self.page_rows;
                dec.finish(&mut |row| rows.extend_from_slice(row))
            }
            None => t4::Summary {
                stats: t4::Stats::default(),
                end: t4::EndReason::Data,
                error: None,
            },
        };
        self.page_summary = Some(summary);
        self.state = State::WaitPostPage;
    }

    /// The image signal ended: wait for the post-page command.
    fn end_image(&mut self, out: &mut Vec<Action>) {
        self.close_page(out);
        self.arm(Timer::T2, self.cfg.timers.t2_ms, out);
    }

    // ---- frames ---------------------------------------------------------------------------

    fn on_frame(&mut self, data: &[u8], fcs_ok: bool, out: &mut Vec<Action>) {
        if !fcs_ok {
            Self::log(
                out,
                format!("frame with bad FCS ({} octets) ignored", data.len()),
            );
            if self.cfg.crp_on_bad_fcs
                && matches!(
                    self.state,
                    State::WaitCommand | State::WaitRetrain | State::WaitPostPage | State::WaitDcn
                )
            {
                self.send_raw(Self::simple(fcf::CRP), out);
            }
            return;
        }
        let frame = match Frame::parse(data) {
            Ok(f) => f,
            Err(e) => {
                Self::log(out, format!("malformed HDLC frame ignored: {e}"));
                return;
            }
        };
        if !frame.final_frame {
            if frame.fcf() == fcf::TSI {
                self.pending_tsi = Some(decode_ident(frame.fif));
            }
            return;
        }
        match frame.fcf() {
            fcf::DCN => self.on_dcn(out),
            fcf::CRP => self.on_crp(out),
            fcf::DCS => self.on_dcs(frame.fif, out),
            c @ (fcf::MPS | fcf::EOP | fcf::EOM | fcf::PRI_MPS | fcf::PRI_EOP | fcf::PRI_EOM) => {
                self.on_post_page(c, out);
            }
            other => self.unexpected_frame(other, out),
        }
    }

    fn unexpected_frame(&mut self, fcf_value: u8, out: &mut Vec<Action>) {
        self.unexpected += 1;
        Self::log(
            out,
            format!(
                "unexpected {} ({fcf_value:#04x}) in state {:?} ({} of {})",
                fcf_name(fcf_value),
                self.state,
                self.unexpected,
                self.cfg.max_unexpected_frames
            ),
        );
        let waiting = matches!(
            self.state,
            State::WaitCommand
                | State::WaitTcf
                | State::WaitRetrain
                | State::WaitImage
                | State::WaitPostPage
                | State::WaitDcn
        );
        if waiting && self.unexpected > self.cfg.max_unexpected_frames {
            self.fail(ResultCode::UnexpectedFrames, out);
        }
    }

    fn on_crp(&mut self, out: &mut Vec<Action>) {
        // Only while waiting: a response in flight already answers it, and a CRP in the
        // middle of the image signal makes no sense.
        let waiting = matches!(
            self.state,
            State::WaitCommand
                | State::WaitTcf
                | State::WaitRetrain
                | State::WaitImage
                | State::WaitPostPage
                | State::WaitDcn
        );
        if !waiting || self.last_response.is_empty() {
            return;
        }
        self.resend_last(out);
    }

    fn on_dcn(&mut self, out: &mut Vec<Action>) {
        let code = match self.state {
            State::SendingDcn => return,
            State::WaitDcn => ResultCode::Ok,
            State::WaitRetrain if self.eop_confirmed => ResultCode::Ok,
            _ if self.pages() > 0 => ResultCode::PartialDcn,
            _ if self.last_verdict == Some(Verdict::Rtn) => ResultCode::PageRejected,
            _ if self.ftt_count > 0 => ResultCode::TrainingFailed,
            _ => ResultCode::RemoteDisconnect,
        };
        self.finish(code, out);
    }

    fn on_dcs(&mut self, fif: &[u8], out: &mut Vec<Action>) {
        if !matches!(
            self.state,
            State::SendingDis
                | State::WaitCommand
                | State::WaitTcf
                | State::RxTcf
                | State::WaitRetrain
                | State::WaitImage
                | State::RxImage
                | State::WaitPostPage
        ) {
            // A DCS while a response is in flight: ours will answer it.
            Self::log(out, format!("DCS ignored in state {:?}", self.state));
            return;
        }
        let dcs = match Dcs::parse(fif) {
            Ok(d) => d,
            Err(DcsError::TooShort(n)) => {
                Self::log(out, format!("DCS with {n} octets ignored"));
                return;
            }
            Err(DcsError::ReservedModem(bits)) => {
                Self::log(out, format!("DCS with reserved modem bits {bits:04b}: FTT"));
                self.begin_training_reply(out);
                self.send_ftt(out);
                return;
            }
        };
        self.begin_training_reply(out);
        if let Some(u) = dcs.unsupported() {
            Self::log(
                out,
                format!("DCS requests {}: not supported", unsupported_name(u)),
            );
            self.dcs = Some(dcs);
            self.fail(ResultCode::IncompatibleDcs, out);
            return;
        }
        self.dcs = Some(dcs);
        if let Some(id) = self.pending_tsi.take() {
            self.remote_ident = Some(id);
        }
        self.decoder = None;
        self.page_rows.clear();
        self.page_summary = None;
        self.answered_cmd = None;
        match self.cfg.tcf {
            TcfMode::Transferred => {
                self.tcf_bytes = 0;
                self.tcf_zero_bits = 0;
                self.state = State::WaitTcf;
                self.arm(Timer::T2, self.cfg.timers.t2_ms, out);
            }
            TcfMode::Local => {
                self.ftt_count = 0;
                if self.cfg.local_tcf_sends_cfr {
                    self.state = State::SendingCfr;
                    self.send(Self::simple(fcf::CFR), out);
                } else {
                    // The image follows the DCS directly.
                    self.state = State::WaitImage;
                    self.arm(Timer::T2, self.cfg.timers.t2_ms, out);
                }
            }
        }
    }

    /// Common start of the handling of a usable DCS: a valid command arrived.
    fn begin_training_reply(&mut self, out: &mut Vec<Action>) {
        self.cancel(Timer::T1, out);
        self.cancel(Timer::T2, out);
        self.cancel(Timer::T4, out);
        self.cancel(Timer::Data, out);
    }

    fn on_post_page(&mut self, cmd: u8, out: &mut Vec<Action>) {
        let base = match cmd {
            fcf::PRI_MPS => fcf::MPS,
            fcf::PRI_EOP => fcf::EOP,
            fcf::PRI_EOM => fcf::EOM,
            other => other,
        };
        match self.state {
            State::RxImage => {
                // The signal end was lost: the command proves the image is over.
                self.close_page(out);
                self.answer_post_page(base, out);
            }
            State::WaitPostPage => self.answer_post_page(base, out),
            State::WaitImage | State::WaitDcn | State::WaitRetrain
                if self.answered_cmd == Some(base) && !self.last_response.is_empty() =>
            {
                // The sender repeats its command: our response was lost. Repeat it.
                Self::log(out, format!("{} repeated: response resent", fcf_name(base)));
                self.resend_last(out);
            }
            _ => self.unexpected_frame(base, out),
        }
    }

    fn judge(&self, summary: &t4::Summary) -> Verdict {
        let rows = u64::from(summary.stats.rows);
        let bad = u64::from(summary.stats.bad_lines);
        let y_dpi = u64::from(self.dcs.map_or(98, |d| d.y_dpi()));
        let run_limit = (u64::from(self.cfg.max_bad_run_tenth_mm) * y_dpi + 127) / 254;
        if rows == 0
            || summary.error.is_some()
            || bad * 100 > rows * u64::from(self.cfg.rtn_bad_percent)
            || u64::from(summary.stats.longest_bad_run) > run_limit
        {
            Verdict::Rtn
        } else if bad * 100 > rows * u64::from(self.cfg.rtp_bad_percent) {
            Verdict::Rtp
        } else {
            Verdict::Mcf
        }
    }

    fn answer_post_page(&mut self, cmd: u8, out: &mut Vec<Action>) {
        self.cancel(Timer::T2, out);
        let summary = self.page_summary.take().unwrap_or(t4::Summary {
            stats: t4::Stats::default(),
            end: t4::EndReason::Data,
            error: None,
        });
        let verdict = self.judge(&summary);
        self.last_verdict = Some(verdict);
        let accepted = verdict != Verdict::Rtn;
        if accepted {
            if let Some(dcs) = self.dcs {
                let page = PageData {
                    number: self.pages() + 1,
                    width: dcs.width.pixels(),
                    rows: summary.stats.rows,
                    resolution: dcs.resolution,
                    x_dpi: dcs.x_dpi(),
                    y_dpi: dcs.y_dpi(),
                    compression: dcs.compression,
                    modem: dcs.modem,
                    bitmap: std::mem::take(&mut self.page_rows),
                    bad_lines: summary.stats.bad_lines,
                    longest_bad_run: summary.stats.longest_bad_run,
                    end: summary.end,
                };
                self.bad_lines.push(summary.stats.bad_lines);
                out.push(Action::Page(Box::new(page)));
            }
        } else {
            Self::log(
                out,
                format!(
                    "page rejected: {} rows, {} bad, longest run {}",
                    summary.stats.rows, summary.stats.bad_lines, summary.stats.longest_bad_run
                ),
            );
        }
        self.page_rows.clear();
        self.eop_confirmed = accepted && cmd == fcf::EOP;
        self.post_next = match (verdict, cmd) {
            (Verdict::Mcf, fcf::MPS) => Next::Image,
            (Verdict::Mcf, fcf::EOP) => Next::Dcn,
            (Verdict::Mcf, _) => Next::PhaseB,
            (_, _) => Next::Retrain,
        };
        self.answered_cmd = Some(cmd);
        let response = match verdict {
            Verdict::Mcf => fcf::MCF,
            Verdict::Rtp => fcf::RTP,
            Verdict::Rtn => fcf::RTN,
        };
        self.state = State::SendingPostPage;
        self.send(Self::simple(response), out);
    }
}

fn unsupported_name(u: Unsupported) -> &'static str {
    match u {
        Unsupported::Ecm => "error correction mode",
        Unsupported::Uncompressed => "uncompressed mode",
        Unsupported::Resolution(_) => "a resolution above fine",
    }
}
