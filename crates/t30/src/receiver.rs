//! The T.30 answering (receiving) terminal, non-ECM and ECM (T.30 Annex A, T.4 Annex A).
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
//!
//! # Error correction mode
//!
//! A DCS with bit 27 (and `Config::ecm`) switches the page procedure to ECM. TCF and CFR are
//! unchanged; after CFR the image is a sequence of *blocks* of up to 256 HDLC frames (FCD, frame
//! number + 64 or 256 octets) at the image rate, closed by three RCP frames and a V.21 post
//! message command:
//!
//! ```text
//!  SendingCfr --TxComplete--> EcmWaitBlock --FCD--> EcmRxBlock --RCP/signal end--> EcmWaitPps
//!    EcmWaitPps --PPS, every frame of the block present--> SendingPostPage (MCF)
//!                    NULL: --> EcmWaitBlock (next block)   MPS: page done, --> EcmWaitBlock
//!                    EOP: page done, --> WaitDcn           EOM: page done, --> phase B
//!    EcmWaitPps --PPS, frames missing--> SendingPostPage (PPR bitmap) --> EcmWaitBlock
//!                    (the block is kept; the sender resends only the missing frames)
//!    EcmWaitPps --CTC--> SendingPostPage (CTR) --> EcmWaitBlock   (rate fall-back, block kept)
//!    EcmWaitPps --EOR-Q--> SendingPostPage (ERR): the sender gave up on the missing frames,
//!                    which are zero filled; continues like the matching PPS-Q
//!    EcmWaitBlock --a repeated PPS / CTC / EOR--> the previous response again (it was lost)
//! ```
//!
//! A block is committed to the page decoder (the `t4` crate) when it is acknowledged, so the
//! decoder sees the coded data in order exactly once. A frame is "received" when its FCS was
//! good; the PPR bitmap marks every other frame number, and the unused numbers of a short block,
//! with 1 (Annex A.4.4).

use crate::caps::{Dcs, DcsError, Unsupported, build_dis_fif, parse_rate};

/// Frames in a block (T.30 A.3.3: the receiver must accept 256; frame numbers are 8 bits).
const ECM_BLOCK_FRAMES: usize = 256;
use crate::config::{Config, TcfMode};
use crate::event::{Action, Event, PageData, RxSignal, Timer, TxSignal};
use crate::frame::{self, Frame, decode_ident, encode_ident, fcf, fcf_name, fcf2, lsb_first};
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
    /// ECM: waiting for the first FCD frame of a block (T2). The frames of the block that are
    /// already held (after a PPR or CTR) are kept.
    EcmWaitBlock,
    /// ECM: receiving the FCD frames of a block (idle timer).
    EcmRxBlock,
    /// ECM: the block's frames have ended: waiting for PPS, CTC or EOR (T2).
    EcmWaitPps,
    /// Transmitting DCN before ending the call with a failure code.
    SendingDcn,
    /// The call is over.
    Done,
}

impl State {
    /// The ECM states.
    pub const ECM: [State; 3] = [State::EcmWaitBlock, State::EcmRxBlock, State::EcmWaitPps];

    /// Every state, in declaration order, including the ECM ones.
    pub const ALL_WITH_ECM: [State; 18] = [
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
        State::EcmWaitBlock,
        State::EcmRxBlock,
        State::EcmWaitPps,
    ];

    /// The states of the non-ECM procedure, in declaration order (the R01 matrix; the ECM
    /// states are in [`State::ECM`] and covered by the E05 matrix).
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
    /// ECM: wait for the next (or the retransmitted) FCD frames.
    EcmBlock,
    /// Retrain (after RTN, or RTP).
    Retrain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Mcf,
    Rtp,
    Rtn,
}

/// The post message command of a PPS or EOR (FCF2, T.30 Figure A.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Post {
    /// NULL: more blocks of this page.
    Null,
    /// MPS: page end, more pages.
    Mps,
    /// EOP: page end, last page.
    Eop,
    /// EOM: page end, back to phase B.
    Eom,
}

impl Post {
    /// FCF2 octet to command. The X bit is ignored for non-NULL values (some terminals do not
    /// set it); EOS and PRI-Q are taken for their base command.
    fn from_fcf2(octet: u8) -> Option<Self> {
        if octet == fcf2::NULL {
            return Some(Post::Null);
        }
        match octet & 0x7F {
            0x71 | 0x79 => Some(Post::Eom),
            0x72 | 0x7A => Some(Post::Mps),
            0x74 | 0x7C => Some(Post::Eop),
            _ => None,
        }
    }
}

/// ECM block state: the frames of the block being received.
#[derive(Debug)]
struct EcmBlock {
    /// Frame data by frame number (`None`: not received with a good FCS).
    frames: Vec<Option<Vec<u8>>>,
    /// Frames in the block as announced by the PPS (the largest value seen); 0 = not known.
    expected: usize,
    /// A good FCD frame arrived since the last response was sent.
    got_since_response: bool,
}

impl EcmBlock {
    fn new() -> Self {
        Self {
            frames: vec![None; ECM_BLOCK_FRAMES],
            expected: 0,
            got_since_response: false,
        }
    }

    fn clear(&mut self) {
        self.frames.iter_mut().for_each(|f| *f = None);
        self.expected = 0;
    }

    /// Frames of the block: the announced count, or one past the highest frame held.
    fn count(&self) -> usize {
        if self.expected > 0 {
            self.expected.min(ECM_BLOCK_FRAMES)
        } else {
            self.frames
                .iter()
                .rposition(Option::is_some)
                .map_or(0, |i| i + 1)
        }
    }

    fn missing(&self) -> Vec<usize> {
        (0..self.count())
            .filter(|&i| self.frames.get(i).is_none_or(Option::is_none))
            .collect()
    }
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
    /// The DCS in force selected ECM.
    ecm_on: bool,
    /// ECM frame size in octets (64 or 256).
    ecm_frame_octets: usize,
    ecm_block: EcmBlock,
    /// PPRs sent for the current block since the last CTC.
    ecm_ppr_round: u32,
    /// The last PPS / CTC / EOR (FCF + FIF) that was answered, to recognise a repeat.
    ecm_last_cmd: Option<Vec<u8>>,
    ecm_ppr_sent: u32,
    ecm_lost: u32,
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
            ecm_on: false,
            ecm_frame_octets: 256,
            ecm_block: EcmBlock::new(),
            ecm_ppr_round: 0,
            ecm_last_cmd: None,
            ecm_ppr_sent: 0,
            ecm_lost: 0,
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
        if let Some(slot) = self.armed.get_mut(timer.index())
            && *slot
        {
            *slot = false;
            out.push(Action::CancelTimer(timer));
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
        let code = if code == ResultCode::Ok && self.ecm_lost > 0 {
            ResultCode::EcmFramesLost
        } else {
            code
        };
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
            ecm: self.dcs.is_some_and(|d| d.ecm),
            ecm_ppr_sent: self.ecm_ppr_sent,
            ecm_frames_lost: self.ecm_lost,
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
                self.state = self.first_image_state();
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
                Next::EcmBlock => {
                    self.state = State::EcmWaitBlock;
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
            (Timer::T2, State::WaitImage | State::EcmWaitBlock) => {
                self.fail(ResultCode::T2ExpiredImage, out);
            }
            (Timer::T2, State::EcmWaitPps) => self.fail(ResultCode::T2ExpiredCommand, out),
            (Timer::Data, State::EcmRxBlock) => {
                Self::log(out, "ECM frames stopped without a signal end".into());
                self.ecm_end_block(out);
            }
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
        if signal == RxSignal::Training && self.state == State::EcmWaitBlock {
            self.ecm_block.got_since_response = true;
        }
        if signal == RxSignal::NoSignal {
            match self.state {
                State::RxTcf => self.end_tcf(out),
                State::RxImage => self.end_image(out),
                State::EcmRxBlock => self.ecm_end_block(out),
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
                    State::WaitCommand
                        | State::WaitRetrain
                        | State::WaitPostPage
                        | State::WaitDcn
                        | State::EcmWaitPps
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
        // ECM image frames: FCD frames are non-final, RCP is a one octet frame.
        match frame.fcf() {
            fcf::FCD => return self.on_fcd(frame.fif, out),
            fcf::RCP => return self.on_rcp(out),
            _ => {}
        }
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
            fcf::PPS => self.on_pps(frame.fif, out),
            fcf::CTC => self.on_ctc(frame.fif, out),
            fcf::EOR => self.on_eor(frame.fif, out),
            fcf::RR => self.on_rr(out),
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
                | State::EcmWaitBlock
                | State::EcmWaitPps
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
                | State::EcmWaitBlock
                | State::EcmWaitPps
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
                | State::EcmWaitBlock
                | State::EcmRxBlock
                | State::EcmWaitPps
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
        if let Some(u) = dcs.unsupported_with(self.cfg.ecm) {
            Self::log(
                out,
                format!("DCS requests {}: not supported", unsupported_name(u)),
            );
            self.dcs = Some(dcs);
            self.fail(ResultCode::IncompatibleDcs, out);
            return;
        }
        self.dcs = Some(dcs);
        self.ecm_on = dcs.ecm;
        self.ecm_frame_octets = dcs.frame_octets as usize;
        self.ecm_block.clear();
        self.ecm_block.got_since_response = false;
        self.ecm_ppr_round = 0;
        self.ecm_last_cmd = None;
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
                    self.state = self.first_image_state();
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

    /// Hands the decoded page (rows collected in `page_rows`) to the caller.
    fn push_page(&mut self, summary: &t4::Summary, out: &mut Vec<Action>) {
        let Some(dcs) = self.dcs else { return };
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
            self.push_page(&summary, out);
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

/// ECM (T.30 Annex A) handling.
impl Receiver {
    /// State after CFR (or after the DCS with local TCF): waiting for the first image data.
    fn first_image_state(&self) -> State {
        if self.ecm_on {
            State::EcmWaitBlock
        } else {
            State::WaitImage
        }
    }

    /// FCD: frame number (LSB first) and 64 or 256 octets of coded data.
    fn on_fcd(&mut self, fif: &[u8], out: &mut Vec<Action>) {
        if !self.ecm_on || !matches!(self.state, State::EcmWaitBlock | State::EcmRxBlock) {
            Self::log(out, format!("FCD ignored in state {:?}", self.state));
            return;
        }
        let Some((&number, data)) = fif.split_first() else {
            Self::log(out, "FCD without a frame number ignored".into());
            return;
        };
        if data.len() > 256 {
            Self::log(out, format!("FCD of {} data octets ignored", data.len()));
            return;
        }
        if self.state == State::EcmWaitBlock {
            self.cancel(Timer::T2, out);
            self.state = State::EcmRxBlock;
        }
        let n = usize::from(lsb_first(number));
        if let Some(slot) = self.ecm_block.frames.get_mut(n) {
            *slot = Some(data.to_vec());
        }
        self.ecm_block.got_since_response = true;
        self.arm(Timer::Data, self.cfg.timers.data_idle_ms, out);
    }

    /// RCP: the frames of the block are over. An RCP (like the training signal before it) also
    /// proves that the sender transmitted again since our last response, so a PPS that follows
    /// is not a repeat.
    fn on_rcp(&mut self, out: &mut Vec<Action>) {
        if self.ecm_on && matches!(self.state, State::EcmWaitBlock | State::EcmRxBlock) {
            self.ecm_block.got_since_response = true;
        }
        if self.state == State::EcmRxBlock {
            self.ecm_end_block(out);
        }
    }

    /// The block's frames are over (RCP, end of signal or silence): wait for the command.
    fn ecm_end_block(&mut self, out: &mut Vec<Action>) {
        self.cancel(Timer::Data, out);
        self.state = State::EcmWaitPps;
        self.arm(Timer::T2, self.cfg.timers.t2_ms, out);
    }

    fn ecm_cmd_key(fcf_value: u8, fif: &[u8]) -> Vec<u8> {
        let mut key = vec![fcf_value];
        key.extend_from_slice(fif);
        key
    }

    /// The sender repeats a command we already answered (no new frame since): our response was
    /// lost.
    fn ecm_is_repeat(&self, key: &[u8]) -> bool {
        self.ecm_last_cmd.as_deref() == Some(key)
            && !self.ecm_block.got_since_response
            && !self.last_response.is_empty()
    }

    /// Common entry of PPS, CTC and EOR. Returns false if the frame was dealt with (repeat or
    /// unexpected) and the caller must stop.
    fn ecm_enter_command(&mut self, fcf_value: u8, key: &[u8], out: &mut Vec<Action>) -> bool {
        if !self.ecm_on {
            self.unexpected_frame(fcf_value, out);
            return false;
        }
        match self.state {
            State::EcmRxBlock => {
                // The command itself proves that the block is over (RCP and the end of the
                // signal were lost); it is answered at once, so no T2 is started.
                self.cancel(Timer::Data, out);
                self.state = State::EcmWaitPps;
                true
            }
            State::EcmWaitPps => true,
            State::EcmWaitBlock | State::WaitDcn => {
                if self.ecm_is_repeat(key) {
                    Self::log(
                        out,
                        format!("{} repeated: response resent", fcf_name(fcf_value)),
                    );
                    self.resend_last(out);
                    false
                } else if self.state == State::EcmWaitBlock {
                    true
                } else {
                    self.unexpected_frame(fcf_value, out);
                    false
                }
            }
            _ => {
                self.unexpected_frame(fcf_value, out);
                false
            }
        }
    }

    fn on_pps(&mut self, fif: &[u8], out: &mut Vec<Action>) {
        let key = Self::ecm_cmd_key(fcf::PPS, fif);
        if !self.ecm_enter_command(fcf::PPS, &key, out) {
            return;
        }
        let [q, _page, _block, frames_minus_1, ..] = fif else {
            Self::log(out, format!("PPS with {} FIF octets ignored", fif.len()));
            return;
        };
        let Some(post) = Post::from_fcf2(*q) else {
            self.unexpected_frame(fcf::PPS, out);
            return;
        };
        self.cancel(Timer::T2, out);
        // The frame count is "number of frames minus one" (Figure A.1). Some senders put the
        // number of frames of the current burst there, which is smaller than the block during a
        // retransmission, so the largest value wins (as SpanDSP does).
        let announced = usize::from(lsb_first(*frames_minus_1)) + 1;
        self.ecm_block.expected = self.ecm_block.expected.max(announced);
        let missing = self.ecm_block.missing();
        if missing.is_empty() {
            self.ecm_acknowledge(post, key, fcf::MCF, out);
        } else {
            self.ecm_request(&missing, key, out);
        }
    }

    /// CTC: the sender continues to correct, possibly at another rate. Answered with CTR; the
    /// block is kept.
    fn on_ctc(&mut self, fif: &[u8], out: &mut Vec<Action>) {
        let key = Self::ecm_cmd_key(fcf::CTC, fif);
        if !self.ecm_enter_command(fcf::CTC, &key, out) {
            return;
        }
        self.cancel(Timer::T2, out);
        match parse_rate(fif) {
            Ok((modem, rate)) => {
                if let Some(d) = self.dcs.as_mut() {
                    d.modem = modem;
                    d.bit_rate = rate;
                }
            }
            Err(e) => Self::log(out, format!("CTC: {e}; rate unchanged")),
        }
        self.ecm_ppr_round = 0;
        self.ecm_last_cmd = Some(key);
        self.ecm_block.got_since_response = false;
        self.post_next = Next::EcmBlock;
        self.state = State::SendingPostPage;
        self.send(Self::simple(fcf::CTR), out);
    }

    /// EOR: the sender gives up on the frames still missing from the block.
    fn on_eor(&mut self, fif: &[u8], out: &mut Vec<Action>) {
        let key = Self::ecm_cmd_key(fcf::EOR, fif);
        if !self.ecm_enter_command(fcf::EOR, &key, out) {
            return;
        }
        let Some(post) = fif.first().and_then(|q| Post::from_fcf2(*q)) else {
            self.unexpected_frame(fcf::EOR, out);
            return;
        };
        self.cancel(Timer::T2, out);
        let lost = self.ecm_block.missing().len();
        self.ecm_lost += lost as u32;
        Self::log(
            out,
            format!("EOR: sender abandons {lost} frame(s); they stay zero filled"),
        );
        self.ecm_acknowledge(post, key, fcf::ERR, out);
    }

    /// RR: the sender asks whether we are ready; we never send RNR, so repeat the last answer.
    fn on_rr(&mut self, out: &mut Vec<Action>) {
        if self.ecm_on
            && matches!(self.state, State::EcmWaitBlock | State::WaitDcn)
            && !self.last_response.is_empty()
        {
            Self::log(out, "RR: last response resent".into());
            self.resend_last(out);
        } else {
            Self::log(out, format!("RR ignored in state {:?}", self.state));
        }
    }

    /// Commits the block (MCF after PPS, ERR after EOR) and answers with `response`.
    fn ecm_acknowledge(&mut self, post: Post, key: Vec<u8>, response: u8, out: &mut Vec<Action>) {
        self.ecm_commit_block(out);
        self.ecm_last_cmd = Some(key);
        self.ecm_block.got_since_response = false;
        self.ecm_ppr_round = 0;
        self.eop_confirmed = post == Post::Eop;
        self.post_next = match post {
            Post::Null => Next::EcmBlock,
            Post::Mps => Next::EcmBlock,
            Post::Eop => Next::Dcn,
            Post::Eom => Next::PhaseB,
        };
        if post != Post::Null {
            self.ecm_finish_page(out);
        }
        self.state = State::SendingPostPage;
        self.send(Self::simple(response), out);
    }

    /// Asks for the missing frames with a PPR (the block is kept).
    fn ecm_request(&mut self, missing: &[usize], key: Vec<u8>, out: &mut Vec<Action>) {
        self.ecm_ppr_round += 1;
        self.ecm_ppr_sent += 1;
        if self.ecm_ppr_round > self.cfg.ecm_max_ppr {
            Self::log(
                out,
                format!(
                    "{} PPR without CTC or EOR: giving up",
                    self.ecm_ppr_round - 1
                ),
            );
            self.fail(ResultCode::EcmFailed, out);
            return;
        }
        // 256 bits, the first bit is frame 0; 1 = send again. Bits of frame numbers beyond the
        // block are 1 (Annex A.4.4, Figure A.5).
        let count = self.ecm_block.count();
        let mut map = [0u8; 32];
        for n in 0..ECM_BLOCK_FRAMES {
            if (n >= count || missing.contains(&n))
                && let Some(octet) = map.get_mut(n / 8)
            {
                *octet |= 0x80 >> (n % 8);
            }
        }
        Self::log(
            out,
            format!(
                "PPR {} for this block: {} of {count} frames missing",
                self.ecm_ppr_round,
                missing.len()
            ),
        );
        self.ecm_last_cmd = Some(key);
        self.ecm_block.got_since_response = false;
        self.post_next = Next::EcmBlock;
        self.state = State::SendingPostPage;
        self.send(vec![frame::build(true, fcf::PPR, &map)], out);
    }

    /// Creates the page decoder if the page has none yet.
    fn ecm_ensure_decoder(&mut self, out: &mut Vec<Action>) -> bool {
        if self.decoder.is_some() {
            return true;
        }
        let Some(dcs) = self.dcs else { return false };
        let mut dcfg = t4::DecoderConfig::new(dcs.width.pixels(), dcs.compression.coding());
        dcfg.max_rows = self.cfg.max_rows;
        match t4::Decoder::new(dcfg) {
            Ok(d) => {
                self.decoder = Some(d);
                self.page_rows.clear();
                true
            }
            Err(e) => {
                Self::log(out, format!("cannot start the T.4 decoder: {e}"));
                false
            }
        }
    }

    /// Feeds the frames of the block, in order, to the page decoder. A frame that never arrived
    /// (EOR) is replaced by zero octets of the negotiated frame size.
    fn ecm_commit_block(&mut self, out: &mut Vec<Action>) {
        let count = self.ecm_block.count();
        let mut data = Vec::with_capacity(count * self.ecm_frame_octets);
        for slot in self.ecm_block.frames.iter().take(count) {
            match slot {
                Some(d) => data.extend_from_slice(d),
                None => data.resize(data.len() + self.ecm_frame_octets, 0),
            }
        }
        self.ecm_block.clear();
        if count > 0 && self.ecm_ensure_decoder(out) {
            self.push_image(&data);
        }
    }

    /// The page is complete (PPS / EOR with MPS, EOP or EOM): close the decoder and deliver it.
    fn ecm_finish_page(&mut self, out: &mut Vec<Action>) {
        let Some(dec) = self.decoder.take() else {
            Self::log(out, "ECM page end without any image data".into());
            return;
        };
        let rows = &mut self.page_rows;
        let summary = dec.finish(&mut |row| rows.extend_from_slice(row));
        if summary.stats.rows == 0 {
            Self::log(out, "ECM page without rows dropped".into());
            self.page_rows.clear();
            return;
        }
        self.push_page(&summary, out);
        self.page_rows.clear();
    }
}

fn unsupported_name(u: Unsupported) -> &'static str {
    match u {
        Unsupported::Ecm => "error correction mode",
        Unsupported::Uncompressed => "uncompressed mode",
        Unsupported::Resolution(_) => "a resolution above fine",
    }
}
