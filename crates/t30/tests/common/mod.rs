//! Shared helpers for the t30 tests. Everything here is independent of the crate's own
//! bit-twiddling helpers: frames are assembled from hand-written bit lists.
#![allow(dead_code)]

use t30::{Action, Config, Event, Receiver, State, Timer};

/// Builds an information field with the given Table 2 bit numbers set (MSB first within each
/// octet: bit 1 is `0x80` of octet 0). `octets` is the field length.
pub fn fif_with_bits(octets: usize, set: &[usize]) -> Vec<u8> {
    let mut v = vec![0u8; octets];
    for &n in set {
        assert!(
            n >= 1 && (n - 1) / 8 < octets,
            "bit {n} outside {octets} octets"
        );
        v[(n - 1) / 8] |= 0x80 >> ((n - 1) % 8);
    }
    v
}

pub fn final_frame(fcf: u8, fif: &[u8]) -> Vec<u8> {
    let mut v = vec![0xFF, 0xC8, fcf];
    v.extend_from_slice(fif);
    v
}

pub fn non_final_frame(fcf: u8, fif: &[u8]) -> Vec<u8> {
    let mut v = vec![0xFF, 0xC0, fcf];
    v.extend_from_slice(fif);
    v
}

/// Modem selector bits (11..=14) of a DCS for the common rates.
pub fn modem_bits(rate: u32, v17: bool) -> Vec<usize> {
    match (rate, v17) {
        (2400, false) => vec![],
        (4800, false) => vec![12],
        (9600, false) => vec![11],
        (7200, false) => vec![11, 12],
        (14400, true) => vec![14],
        (12000, true) => vec![12, 14],
        (9600, true) => vec![11, 14],
        (7200, true) => vec![11, 12, 14],
        other => panic!("no such rate {other:?}"),
    }
}

/// A plausible DCS: receive bit, the rate, optional fine / MR / MMR, 0 ms scan time, unlimited.
pub struct DcsSpec {
    pub rate: u32,
    pub v17: bool,
    pub fine: bool,
    pub mr: bool,
    pub mmr: bool,
    pub width_bits: Vec<usize>,
    pub extra: Vec<usize>,
}

impl Default for DcsSpec {
    fn default() -> Self {
        Self {
            rate: 14400,
            v17: true,
            fine: false,
            mr: false,
            mmr: false,
            width_bits: vec![],
            extra: vec![],
        }
    }
}

impl DcsSpec {
    pub fn fif(&self) -> Vec<u8> {
        let mut bits = vec![10];
        bits.extend(modem_bits(self.rate, self.v17));
        if self.fine {
            bits.push(15);
        }
        if self.mr {
            bits.push(16);
        }
        bits.extend(&self.width_bits);
        bits.extend([20, 21, 22, 23]);
        if self.mmr {
            bits.push(31);
        }
        bits.extend(&self.extra);
        let octets = if bits.iter().any(|&b| b > 24) {
            let max = *bits.iter().max().unwrap();
            max.div_ceil(8).max(4)
        } else {
            3
        };
        let mut b = bits;
        for o in 3..octets {
            b.push(o * 8); // extend bit of octet o (bit 24, 32, ...)
        }
        fif_with_bits(octets, &b)
    }

    /// The final DCS frame with X = 1 (0xC1).
    pub fn frame(&self) -> Vec<u8> {
        final_frame(0xC1, &self.fif())
    }
}

pub fn tsi_frame(ident: &str) -> Vec<u8> {
    // reversed order, every octet bit reversed (0x04 is a space, 0x8C the digit 1)
    let mut f = vec![0x04u8; 20];
    for (i, c) in ident.bytes().rev().enumerate() {
        f[i] = c.reverse_bits();
    }
    non_final_frame(0xC2, &f)
}

pub fn frame_ev(data: Vec<u8>) -> Event {
    Event::Frame { data, fcs_ok: true }
}

pub fn bad_frame_ev(data: Vec<u8>) -> Event {
    Event::Frame {
        data,
        fcs_ok: false,
    }
}

pub fn simple_cmd(fcf: u8) -> Event {
    frame_ev(final_frame(fcf, &[]))
}

/// Compact description of an action for table comparisons. Logs are dropped.
pub fn summarize(actions: &[Action]) -> Vec<String> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::SendSignal(s) => Some(format!("signal {s:?}")),
            Action::SendFrames(frames) => {
                let names: Vec<String> = frames
                    .iter()
                    .map(|f| format!("{:02X}", f.get(2).copied().unwrap_or(0)))
                    .collect();
                Some(format!("tx[{}]", names.join(",")))
            }
            Action::StartTimer { timer, ms } => Some(format!("start {timer:?}={ms}")),
            Action::CancelTimer(t) => Some(format!("cancel {t:?}")),
            Action::Page(p) => Some(format!(
                "page#{} rows={} bad={}",
                p.number, p.rows, p.bad_lines
            )),
            Action::Log(_) => None,
            Action::Done(r) => Some(format!("done {}", r.code.name())),
        })
        .collect()
}

/// A virtual-time harness: executes `StartTimer`/`CancelTimer` against a clock, so timer tests
/// can assert exact millisecond instants.
pub struct Sim {
    pub rx: Receiver,
    pub now: u64,
    /// (deadline, timer) of running timers.
    pub timers: Vec<(u64, Timer)>,
    /// Every action with the instant it was produced.
    pub trace: Vec<(u64, Action)>,
    pub pages: Vec<t30::PageData>,
    pub result: Option<t30::T30Result>,
}

impl Sim {
    pub fn new(cfg: Config) -> Self {
        Self {
            rx: Receiver::new(cfg),
            now: 0,
            timers: Vec::new(),
            trace: Vec::new(),
            pages: Vec::new(),
            result: None,
        }
    }

    pub fn state(&self) -> State {
        self.rx.state()
    }

    /// Delivers an event at the current instant.
    pub fn send(&mut self, ev: Event) -> Vec<Action> {
        let actions = self.rx.on_event(self.now, ev);
        for a in &actions {
            match a {
                Action::StartTimer { timer, ms } => {
                    self.timers.retain(|(_, t)| t != timer);
                    self.timers.push((self.now + u64::from(*ms), *timer));
                }
                Action::CancelTimer(t) => self.timers.retain(|(_, x)| x != t),
                Action::Page(p) => self.pages.push((**p).clone()),
                Action::Done(r) => self.result = Some((**r).clone()),
                _ => {}
            }
            self.trace.push((self.now, a.clone()));
        }
        actions
    }

    /// Moves the clock forward by `ms`, firing timers in order.
    pub fn advance(&mut self, ms: u64) -> Vec<Action> {
        let target = self.now + ms;
        let mut all = Vec::new();
        loop {
            self.timers.sort_by_key(|(d, _)| *d);
            match self.timers.first().copied() {
                Some((d, t)) if d <= target => {
                    self.now = d;
                    self.timers.remove(0);
                    all.extend(self.send(Event::Timer(t)));
                }
                _ => break,
            }
        }
        self.now = target;
        all
    }

    pub fn at(&mut self, t: u64) {
        assert!(t >= self.now, "time runs forward");
        let d = t - self.now;
        self.advance(d);
    }

    pub fn deadline(&self, timer: Timer) -> Option<u64> {
        self.timers
            .iter()
            .find(|(_, t)| *t == timer)
            .map(|(d, _)| *d)
    }

    /// Frames of the form `[FCF, ...]` sent so far, with instants.
    pub fn sent_fcfs(&self) -> Vec<(u64, Vec<u8>)> {
        self.trace
            .iter()
            .filter_map(|(t, a)| match a {
                Action::SendFrames(fs) => Some((*t, fs.iter().map(|f| f[2]).collect())),
                _ => None,
            })
            .collect()
    }
}

pub fn started(cfg: Config) -> Sim {
    let mut s = Sim::new(cfg);
    s.send(Event::Start);
    s
}

/// Start, DIS burst out, TxComplete.
pub fn after_dis(cfg: Config) -> Sim {
    let mut s = started(cfg);
    s.send(Event::TxComplete);
    s
}

/// Test page: `rows` rows of 1728 pixels with a repeating pattern; 1 = black.
pub fn test_bitmap(rows: usize, seed: u32) -> Vec<u8> {
    let rb = 216;
    let mut v = vec![0u8; rb * rows];
    for y in 0..rows {
        for x in 0..rb {
            let k = (y as u32 * 31 + x as u32 * 7 + seed) % 11;
            v[y * rb + x] = match k {
                0 => 0xFF,
                1 | 2 => 0xF0,
                3 => 0x0F,
                _ => 0x00,
            };
        }
    }
    v
}

pub fn encode_mh(bitmap: &[u8], rows: usize) -> Vec<u8> {
    let mut e = t4::Encoder::new(1728, t4::Coding::Mh).unwrap();
    for y in 0..rows {
        e.push_row(&bitmap[y * 216..(y + 1) * 216]).unwrap();
    }
    e.finish()
}

pub fn encode_mr(bitmap: &[u8], rows: usize, k: u8) -> Vec<u8> {
    let mut e = t4::Encoder::new(1728, t4::Coding::Mr { k }).unwrap();
    for y in 0..rows {
        e.push_row(&bitmap[y * 216..(y + 1) * 216]).unwrap();
    }
    e.finish()
}

pub fn encode_mmr(bitmap: &[u8]) -> Vec<u8> {
    t4::encode_mmr(bitmap, 1728).unwrap()
}

/// Drives a Sim through the happy-path prefix up to and including the TxComplete of the CFR,
/// leaving it in WaitImage.
pub fn to_wait_image(cfg: Config, spec: &DcsSpec) -> Sim {
    let mut s = after_dis(cfg);
    s.send(frame_ev(tsi_frame("12345")));
    s.send(frame_ev(spec.frame()));
    assert_eq!(s.state(), State::WaitTcf);
    let rate_bytes = (spec.rate as usize) * 3 / 16; // 1.5 s of TCF
    s.send(Event::ImageData(vec![0u8; rate_bytes]));
    s.send(Event::DataEnd);
    assert_eq!(s.state(), State::SendingCfr);
    s.send(Event::TxComplete);
    assert_eq!(s.state(), State::WaitImage);
    s
}
