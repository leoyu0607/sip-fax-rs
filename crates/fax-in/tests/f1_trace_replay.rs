//! F1-04: replay of the AudioCodes Mediant 1000 (v6.80A) field trace into the engine, on the
//! virtual clock.
//!
//! Two replays:
//!
//! * `f1_04_summary_trace_*` (always on) replays the committed *summary* trace
//!   `tools/fax-sim/tests/traces/audiocodes-m1000-6.80A-8page-fail.udptl.log`. The summary has
//!   no image data, so:
//!   REAL: every received packet's kind, contents (indicators, V.21 octets and frames), its
//!   millisecond instant and the way the fields are split into packets (FCS-OK and sig-end are
//!   separate packets 120-141 ms apart), the 255 `t4-non-ecm-data` packets of page 1 with their
//!   lengths and spacing, and both MPS commands (the second is the AudioCodes repeat).
//!   SUBSTITUTED: the *octets* of `t4-non-ecm-data` (TCF: zeros; pages: a known MR-coded test
//!   page, followed by zero fill to the length the trace's packets carried, fed with the trace's
//!   chunking: 72 octets per packet, a shorter last one).
//!   SYNTHESIZED: everything after the second MPS, because the real sender had given up
//!   there: pages 2..8, built by reacting to our MCF with the page-1 pattern of the trace
//!   (training indicator after the same gap the trace shows between our CFR and its training,
//!   the data packets at the offsets of page 1, MPS commands with the FCS-OK / sig-end split of
//!   the trace; the last page ends with EOP, then a DCN in the same shape).
//! * `f1_04_full_trace_*` replays an exact capture (`[capture] udptl = "full"`) when the
//!   environment variable `F1_FULL_TRACE` names its `udptl.log`; skipped otherwise (the file is
//!   not part of the repository).

use fax_in::t38engine::{EngineConfig, T38Engine, UdptlEngine};
use std::collections::BTreeSet;
use t38::{DataType, ErrorRecovery, Field, FieldType, Ifp, Indicator, ReceiverConfig, UdptlPacket};

const SUMMARY: &str =
    include_str!("../../../tools/fax-sim/tests/traces/audiocodes-m1000-6.80A-8page-fail.udptl.log");

// ------------------------------------------------------------------ the summary-log parser

#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
    Ind(Indicator),
    /// (data type, [(field type, length, octets if the log shows them)])
    Data(DataType, Vec<(FieldType, usize, Vec<u8>)>),
}

#[derive(Debug, Clone)]
struct Line {
    /// Milliseconds since midnight.
    t_ms: u64,
    tx: bool,
    seq: u32,
    kind: Kind,
}

fn indicator(name: &str) -> Indicator {
    use Indicator::*;
    match name {
        "NoSignal" => NoSignal,
        "Cng" => Cng,
        "Ced" => Ced,
        "V21Preamble" => V21Preamble,
        "V27ter2400Training" => V27ter2400Training,
        "V27ter4800Training" => V27ter4800Training,
        "V29_7200Training" => V29_7200Training,
        "V29_9600Training" => V29_9600Training,
        "V17_7200ShortTraining" => V17_7200ShortTraining,
        "V17_7200LongTraining" => V17_7200LongTraining,
        "V17_9600ShortTraining" => V17_9600ShortTraining,
        "V17_9600LongTraining" => V17_9600LongTraining,
        "V17_12000ShortTraining" => V17_12000ShortTraining,
        "V17_12000LongTraining" => V17_12000LongTraining,
        "V17_14400ShortTraining" => V17_14400ShortTraining,
        "V17_14400LongTraining" => V17_14400LongTraining,
        other => panic!("unknown indicator {other}"),
    }
}

fn data_type(name: &str) -> DataType {
    use DataType::*;
    match name {
        "V21" => V21,
        "V27ter2400" => V27ter2400,
        "V27ter4800" => V27ter4800,
        "V29_7200" => V29_7200,
        "V29_9600" => V29_9600,
        "V17_7200" => V17_7200,
        "V17_9600" => V17_9600,
        "V17_12000" => V17_12000,
        "V17_14400" => V17_14400,
        other => panic!("unknown data type {other}"),
    }
}

fn field_type(name: &str) -> FieldType {
    use FieldType::*;
    match name {
        "HdlcData" => HdlcData,
        "HdlcSigEnd" => HdlcSigEnd,
        "HdlcFcsOk" => HdlcFcsOk,
        "HdlcFcsBad" => HdlcFcsBad,
        "HdlcFcsOkSigEnd" => HdlcFcsOkSigEnd,
        "HdlcFcsBadSigEnd" => HdlcFcsBadSigEnd,
        "T4NonEcmData" => T4NonEcmData,
        "T4NonEcmSigEnd" => T4NonEcmSigEnd,
        other => panic!("unknown field type {other}"),
    }
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("hex"))
        .collect()
}

/// Parses `<ts> RX|TX <seq> len=<n> ind=<Name>` and
/// `<ts> RX|TX <seq> len=<n> data=<Type> <Field>(<len>)[=<hex>]... [frame=<hex>]`.
fn parse_summary(text: &str) -> Vec<Line> {
    let mut out = Vec::new();
    for raw in text.lines().filter(|l| !l.trim().is_empty()) {
        let mut it = raw.split_whitespace();
        let ts = it.next().expect("timestamp");
        let dir = it.next().expect("direction");
        let seq: u32 = it.next().expect("seq").parse().expect("seq number");
        let _len = it.next().expect("len=");
        let what = it.next().expect("ind= or data=");
        // 2026-10-21T03:42:05.790Z
        let time = ts.split('T').nth(1).expect("time").trim_end_matches('Z');
        let mut p = time.split([':', '.']);
        let mut n = |what: &str| -> u64 { p.next().expect(what).parse().expect(what) };
        let t_ms = ((n("h") * 60 + n("m")) * 60 + n("s")) * 1000 + n("ms");
        let kind = if let Some(name) = what.strip_prefix("ind=") {
            Kind::Ind(indicator(name))
        } else {
            let dt = data_type(what.strip_prefix("data=").expect("data="));
            let mut fields = Vec::new();
            for tok in it {
                if tok.starts_with("frame=") {
                    continue; // derived from the fields
                }
                let (head, hex) = tok.split_once('=').map_or((tok, ""), |(h, x)| (h, x));
                let (name, len) = head.split_once('(').expect("Field(len)");
                let len: usize = len.trim_end_matches(')').parse().expect("len");
                fields.push((field_type(name), len, unhex(hex)));
            }
            Kind::Data(dt, fields)
        };
        out.push(Line {
            t_ms,
            tx: dir == "TX",
            seq,
            kind,
        });
    }
    out
}

#[test]
fn f1_04_parser_reads_the_documented_formats() {
    let l = parse_summary(
        "2026-10-21T03:42:25.649Z RX 315 len=31 data=V21 HdlcFcsOk(0) frame=ffc8f2\n\
         2026-10-21T03:42:14.269Z RX 54 len=82 data=V17_14400 T4NonEcmData(72)\n\
         2026-10-21T03:42:08.550Z RX 4 len=29 data=V21 HdlcData(1)=c1\n\
         2026-10-21T03:42:02.267Z TX 0 len=6 ind=NoSignal\n",
    );
    assert_eq!(l.len(), 4);
    assert_eq!((l[0].t_ms, l[0].tx, l[0].seq), (13_345_649, false, 315));
    assert_eq!(
        l[0].kind,
        Kind::Data(DataType::V21, vec![(FieldType::HdlcFcsOk, 0, vec![])])
    );
    assert_eq!(
        l[1].kind,
        Kind::Data(
            DataType::V17_14400,
            vec![(FieldType::T4NonEcmData, 72, vec![])]
        )
    );
    assert_eq!(
        l[2].kind,
        Kind::Data(DataType::V21, vec![(FieldType::HdlcData, 1, vec![0xC1])])
    );
    assert!(l[3].tx);
    assert_eq!(l[3].kind, Kind::Ind(Indicator::NoSignal));
    // and the whole committed trace
    let all = parse_summary(SUMMARY);
    assert_eq!(all.len(), 441);
    assert_eq!(all.iter().filter(|l| !l.tx).count(), 369);
}

// ------------------------------------------------------------------ the harness

#[derive(Clone)]
enum Input {
    Ifp(Ifp),
    Datagram(Vec<u8>),
}

#[allow(clippy::large_enum_variant)]
enum Eng {
    Raw(T38Engine),
    Udptl(Box<UdptlEngine>),
}

impl Eng {
    fn engine(&self) -> &T38Engine {
        match self {
            Eng::Raw(e) => e,
            Eng::Udptl(u) => u.engine(),
        }
    }
    fn engine_mut(&mut self) -> &mut T38Engine {
        match self {
            Eng::Raw(e) => e,
            Eng::Udptl(u) => u.engine_mut(),
        }
    }
    fn input(&mut self, t: u64, i: &Input) {
        match (self, i) {
            (Eng::Raw(e), Input::Ifp(p)) => e.on_ifp(t, p),
            (Eng::Udptl(u), Input::Datagram(d)) => {
                let _ = u.on_datagram(t, d);
            }
            _ => panic!("input kind does not match the engine"),
        }
    }
    fn advance(&mut self, t: u64) {
        match self {
            Eng::Raw(e) => e.advance(t),
            Eng::Udptl(u) => u.advance(t),
        }
    }
    /// What we sent, as IFPs with their instants.
    fn polled(&mut self) -> Vec<(u64, Ifp)> {
        match self {
            Eng::Raw(e) => e
                .poll_timed()
                .into_iter()
                .map(|t| (t.at_ms, t.ifp))
                .collect(),
            Eng::Udptl(u) => u
                .poll_datagrams()
                .into_iter()
                .map(|d| {
                    let p = UdptlPacket::decode(&d.bytes).expect("our datagram decodes");
                    let (ifp, _) = Ifp::decode_prefix(&p.primary, t38::T38Version::V0).unwrap();
                    (d.at_ms, ifp)
                })
                .collect(),
        }
    }
}

/// T.30 view of what we sent.
#[derive(Default)]
struct Heard {
    hdlc: Vec<u8>,
    /// (instant, frame)
    frames: Vec<(u64, Vec<u8>)>,
    /// instants of our `v21-preamble` packets
    preambles: Vec<u64>,
    /// instants of our `no-signal` packets
    no_signals: Vec<u64>,
}

impl Heard {
    fn take(&mut self, at: u64, ifp: &Ifp) {
        match ifp {
            Ifp::Indicator(Indicator::V21Preamble) => self.preambles.push(at),
            Ifp::Indicator(Indicator::NoSignal) => self.no_signals.push(at),
            Ifp::Indicator(_) => {}
            Ifp::Data { fields, .. } => {
                for f in fields {
                    match f.field_type {
                        FieldType::HdlcData => self.hdlc.extend(&f.data),
                        FieldType::HdlcFcsOk | FieldType::HdlcFcsOkSigEnd
                            if !self.hdlc.is_empty() =>
                        {
                            self.frames.push((at, std::mem::take(&mut self.hdlc)));
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

struct Replay {
    eng: Eng,
    heard: Heard,
    /// (instant, packet) of everything we sent
    sent: Vec<(u64, Ifp)>,
    inbox: Vec<(u64, Input)>,
    now: u64,
    done_at: Option<u64>,
}

impl Replay {
    fn new(eng: Eng) -> Self {
        let mut eng = eng;
        eng.engine_mut().start(0);
        let mut r = Replay {
            eng,
            heard: Heard::default(),
            sent: Vec::new(),
            inbox: Vec::new(),
            now: 0,
            done_at: None,
        };
        r.collect();
        r
    }

    fn collect(&mut self) {
        for (at, ifp) in self.eng.polled() {
            self.heard.take(at, &ifp);
            self.sent.push((at, ifp));
        }
    }

    fn schedule(&mut self, at: u64, i: Input) {
        self.inbox.push((at, i));
    }

    /// One millisecond: deliver what is due, advance, collect.
    fn step(&mut self) {
        self.now += 1;
        let now = self.now;
        let mut due: Vec<(u64, Input)> = Vec::new();
        self.inbox.retain(|(t, i)| {
            if *t <= now {
                due.push((*t, i.clone()));
                false
            } else {
                true
            }
        });
        for (t, i) in due {
            self.eng.input(t.max(now - 1).min(now), &i);
        }
        self.eng.advance(now);
        self.collect();
        if self.done_at.is_none() && self.eng.engine().is_done() {
            self.done_at = Some(now);
        }
    }

    fn log(&mut self) -> String {
        self.eng.engine_mut().drain_log().join("\n")
    }
}

fn engine_cfg() -> EngineConfig {
    // result.json of the trace: T.38 version 0, transferredTCF, 14400, no local identification
    // (the trace's DIS is `ffc80100771e`, without CSI); pacing is the shipped default.
    EngineConfig::default()
}

// ------------------------------------------------------------------ test pages

/// Text-like test page (`seed` makes the pages differ), `rows` rows of 1728 pixels.
fn page_bitmap(rows: usize, seed: u32) -> Vec<u8> {
    let mut v = vec![0u8; 216 * rows];
    for y in 0..rows {
        for x in 0..216usize {
            let h =
                (y as u32).wrapping_mul(131) ^ (x as u32).wrapping_mul(17) ^ seed.wrapping_mul(7);
            v[y * 216 + x] = if h % 23 == 0 {
                0xF0
            } else if y % 20 < 9 && (y / 20 + x / 9 + seed as usize) % 4 == 0 {
                0x3C
            } else {
                0
            };
        }
    }
    v
}

fn mr(bm: &[u8], rows: usize) -> Vec<u8> {
    let mut e = t4::Encoder::new(1728, t4::Coding::Mr { k: 2 }).unwrap();
    for y in 0..rows {
        e.push_row(&bm[y * 216..(y + 1) * 216]).unwrap();
    }
    e.finish()
}

/// The largest page that fits `budget` octets: (bitmap, rows, MR data zero-padded to `budget`).
fn page_for(seed: u32, budget: usize) -> (Vec<u8>, Vec<u8>) {
    let mut rows = 1200;
    loop {
        let bm = page_bitmap(rows, seed);
        let mut data = mr(&bm, rows);
        if data.len() <= budget {
            data.resize(budget, 0); // zero fill after the RTC
            return (bm, data);
        }
        rows -= 10;
        assert!(rows > 100, "no page fits");
    }
}

// ------------------------------------------------------------------ the summary replay

/// The trace's receive side, de-duplicated by sequence number (the log shows every datagram,
/// including the repeats the UDPTL layer discards), times relative to the start of the call.
struct Trace {
    rx: Vec<Line>,
    start: u64,
    all: Vec<Line>,
}

fn trace() -> Trace {
    let all = parse_summary(SUMMARY);
    let start = all[0].t_ms;
    let mut seen = BTreeSet::new();
    let rx = all
        .iter()
        .filter(|l| !l.tx && seen.insert(l.seq))
        .map(|l| Line {
            t_ms: l.t_ms - start,
            ..l.clone()
        })
        .collect();
    Trace { rx, start, all }
}

fn data_ifp(dt: DataType, fields: Vec<(FieldType, Vec<u8>)>) -> Ifp {
    Ifp::Data {
        data_type: dt,
        fields: fields
            .into_iter()
            .map(|(ft, d)| Field::new(ft, d))
            .collect(),
    }
}

/// Feeds `T4NonEcmData` octets: TCF is zeros, the pages come from `pages` in order.
struct Substitute {
    pages: Vec<Vec<u8>>,
    section: i32,
    pos: usize,
}

impl Substitute {
    fn new(pages: Vec<Vec<u8>>) -> Self {
        Substitute {
            pages,
            section: -1,
            pos: 0,
        }
    }
    fn on_training(&mut self) {
        self.section += 1;
        self.pos = 0;
    }
    fn octets(&mut self, len: usize) -> Vec<u8> {
        if self.section == 0 {
            return vec![0; len]; // TCF
        }
        let page = &self.pages[(self.section - 1) as usize];
        let out = page[self.pos..self.pos + len].to_vec();
        self.pos += len;
        out
    }
}

fn to_ifp(l: &Line, sub: &mut Substitute) -> Ifp {
    match &l.kind {
        Kind::Ind(i) => {
            if format!("{i:?}").contains("Training") {
                sub.on_training();
            }
            Ifp::Indicator(*i)
        }
        Kind::Data(dt, fields) => data_ifp(
            *dt,
            fields
                .iter()
                .map(|(ft, len, bytes)| {
                    if *ft == FieldType::T4NonEcmData {
                        (*ft, sub.octets(*len))
                    } else {
                        assert_eq!(bytes.len(), *len, "log shows all non-image octets");
                        (*ft, bytes.clone())
                    }
                })
                .collect(),
        ),
    }
}

/// Octets of page-1 data in the trace (sum of the packets of the second training section).
fn page1_budget(rx: &[Line]) -> usize {
    let mut section = -1;
    let mut total = 0;
    for l in rx {
        match &l.kind {
            Kind::Ind(i) if format!("{i:?}").contains("Training") => section += 1,
            Kind::Data(_, f) if section == 1 => {
                total += f
                    .iter()
                    .filter(|(ft, _, _)| *ft == FieldType::T4NonEcmData)
                    .map(|(_, n, _)| *n)
                    .sum::<usize>();
            }
            _ => {}
        }
    }
    total
}

/// Synthesizes pages after the second MPS by reacting to our MCF.
struct Continuation {
    /// Template: the trace's page-1 events relative to its ShortTraining indicator, from that
    /// indicator to the `no-signal` after the MPS (`(dt offset, line)`).
    template: Vec<(u64, Line)>,
    /// Index into `template` of the `t4-non-ecm-data` packets.
    train_gap_ms: u64,
    pages: Vec<Vec<u8>>,
    /// next page number to send (0-based; 1 = page 2)
    next: usize,
    total_pages: usize,
    after_ms: u64,
    mcf_seen: Option<u64>,
    dcn_sent: bool,
    log: Vec<String>,
}

impl Continuation {
    fn new(t: &Trace, pages: Vec<Vec<u8>>, last_real_rx: u64) -> Self {
        // template: from the first ShortTraining to the first NoSignal after the first MPS
        let i0 =
            t.rx.iter()
                .position(|l| l.kind == Kind::Ind(Indicator::V17_14400ShortTraining))
                .unwrap();
        let mps = t
            .rx
            .iter()
            .position(|l| {
                matches!(&l.kind, Kind::Data(DataType::V21, f) if f[0].0 == FieldType::HdlcFcsOk)
                    && l.t_ms > t.rx[i0].t_ms
            })
            .unwrap();
        let end = mps
            + t.rx[mps..]
                .iter()
                .position(|l| l.kind == Kind::Ind(Indicator::NoSignal))
                .unwrap();
        let base = t.rx[i0].t_ms;
        let template = t.rx[i0..=end]
            .iter()
            .map(|l| (l.t_ms - base, l.clone()))
            .collect();
        // the gap between our CFR burst's last `no-signal` and the peer's ShortTraining
        let train_gap_ms = train_gap(t);
        let total_pages = pages.len();
        Continuation {
            template,
            train_gap_ms,
            pages,
            next: 1,
            total_pages,
            after_ms: last_real_rx,
            mcf_seen: None,
            dcn_sent: false,
            log: Vec::new(),
        }
    }

    /// Looks at what we sent and schedules the peer's next burst.
    fn react(&mut self, r: &mut Replay) {
        // our first MCF after the last thing the peer did
        if self.mcf_seen.is_none() {
            if let Some((t, _)) = r
                .heard
                .frames
                .iter()
                .find(|(t, f)| *t > self.after_ms && f.get(2) == Some(&0x31))
            {
                self.mcf_seen = Some(*t);
            }
        }
        let Some(mcf_at) = self.mcf_seen else { return };
        let Some(&end) = r.heard.no_signals.iter().find(|t| **t >= mcf_at) else {
            return;
        };
        self.mcf_seen = None;
        if self.next < self.total_pages {
            let page = self.next;
            let last = page + 1 == self.total_pages;
            let t0 = end + self.train_gap_ms;
            self.log.push(format!(
                "synthesized page {} from {} ms (our MCF ended at {end} ms)",
                page + 1,
                t0
            ));
            let mut sub = Substitute::new(vec![self.pages[page].clone()]);
            sub.section = 0; // the template starts at the training indicator, which bumps it to 1
            let mut last_t = t0;
            for (off, line) in self.template.clone() {
                let mut ifp = to_ifp(&line, &mut sub);
                if last {
                    // EOP instead of MPS: the F2 octet of the command
                    if let Ifp::Data { fields, .. } = &mut ifp {
                        if fields[0].field_type == FieldType::HdlcData && fields[0].data == [0xF2] {
                            fields[0].data = vec![0xF4];
                        }
                    }
                }
                last_t = t0 + off;
                r.schedule(last_t, Input::Ifp(ifp));
            }
            self.after_ms = last_t;
            self.next += 1;
        } else if !self.dcn_sent {
            // the peer hangs up: the command burst of the template with a DCN
            self.dcn_sent = true;
            let t0 = end + 540;
            let i0 = self
                .template
                .iter()
                .position(|(_, l)| l.kind == Kind::Ind(Indicator::V21Preamble))
                .unwrap();
            let base = self.template[i0].0;
            self.log.push(format!("synthesized DCN from {t0} ms"));
            let mut sub = Substitute::new(vec![]);
            for (off, line) in &self.template[i0..] {
                let mut ifp = to_ifp(line, &mut sub);
                if let Ifp::Data { fields, .. } = &mut ifp {
                    if fields[0].field_type == FieldType::HdlcData && fields[0].data == [0xF2] {
                        fields[0].data = vec![0xDF]; // DCN with the X bit, as the peer writes it
                    }
                }
                r.schedule(t0 + off - base, Input::Ifp(ifp));
            }
        }
    }
}

/// Gap between the end of our CFR (last `no-signal` after it) and the peer's ShortTraining.
fn train_gap(t: &Trace) -> u64 {
    let short =
        t.rx.iter()
            .find(|l| l.kind == Kind::Ind(Indicator::V17_14400ShortTraining))
            .unwrap()
            .t_ms;
    let our_end = t
        .all
        .iter()
        .filter(|l| l.tx && l.kind == Kind::Ind(Indicator::NoSignal) && l.t_ms - t.start < short)
        .map(|l| l.t_ms - t.start)
        .max()
        .unwrap();
    short - our_end
}

/// (bitmap, MR data zero padded to the trace's page-1 length)
type TestPage = (Vec<u8>, Vec<u8>);

fn run_summary_replay() -> (Replay, Trace, Vec<TestPage>, Continuation) {
    let t = trace();
    let budget = page1_budget(&t.rx);
    // eight different pages, each fitting the page-1 budget
    let pages: Vec<TestPage> = (1..=8).map(|k| page_for(k, budget)).collect();
    let mut sub = Substitute::new(pages.iter().map(|(_, d)| d.clone()).collect());
    let mut r = Replay::new(Eng::Raw(T38Engine::new(engine_cfg())));
    for l in &t.rx {
        let ifp = to_ifp(l, &mut sub);
        r.schedule(l.t_ms, Input::Ifp(ifp));
    }
    let last_real = t.rx.last().unwrap().t_ms;
    let mut cont = Continuation::new(
        &t,
        pages.iter().map(|(_, d)| d.clone()).collect(),
        last_real,
    );
    let limit = last_real + 120_000;
    while r.now < limit && r.done_at.is_none() {
        r.step();
        cont.react(&mut r);
    }
    (r, t, pages, cont)
}

/// Relative instants (ms) of the peer's FCS-OK / sig-end of its two real MPS commands.
fn mps_instants(t: &Trace) -> Vec<(u64, u64)> {
    let mut v = Vec::new();
    let mut fcs = None;
    // (the DCS is also FCS-OK + sig-end; the post-page commands come after the page training)
    let page =
        t.rx.iter()
            .find(|l| l.kind == Kind::Ind(Indicator::V17_14400ShortTraining))
            .unwrap()
            .t_ms;
    for l in t.rx.iter().filter(|l| l.t_ms > page) {
        if let Kind::Data(DataType::V21, f) = &l.kind {
            match f[0].0 {
                FieldType::HdlcFcsOk if f[0].2.is_empty() => fcs = Some(l.t_ms),
                FieldType::HdlcSigEnd => {
                    if let Some(c) = fcs.take() {
                        v.push((c, l.t_ms));
                    }
                }
                _ => {}
            }
        }
    }
    v
}

#[test]
fn f1_04_summary_trace_the_engine_completes_all_eight_pages() {
    let (mut r, t, pages, cont) = run_summary_replay();
    let log = r.log();
    let ctx = format!("{log}\n{:#?}", cont.log);

    // our first DIS is the trace's DIS
    assert_eq!(
        r.heard.frames.first().map(|(_, f)| hex(f)),
        Some("ffc80100771e".to_string()),
        "{ctx}"
    );

    // Real part 1: we never start talking before the peer's signal has ended. The trace's two
    // MPS commands (FCS-OK, then sig-end 141 / 119 ms later): the first preamble after each
    // FCS-OK is exactly 75 ms after the sig-end (the field failure: 75 ms after the FCS-OK).
    let mps = mps_instants(&t);
    assert_eq!(mps.len(), 2, "two real MPS commands in the trace");
    for (n, (fcs, sig_end)) in mps.iter().enumerate() {
        let first = r
            .heard
            .preambles
            .iter()
            .find(|p| **p > *fcs)
            .copied()
            .expect("a response");
        assert_eq!(
            first,
            sig_end + 75,
            "MPS #{}: FCS-OK at {fcs} ms, sig-end at {sig_end} ms; {ctx}",
            n + 1
        );
    }

    // Real part 2: no t2_expired_image; the call ends with the synthesized DCN.
    let res = r.eng.engine().result().cloned().expect("call finished");
    assert_eq!(res.code, t30::ResultCode::Ok, "{ctx}");
    assert_eq!(res.pages, 8, "{ctx}");
    assert_eq!(res.bad_lines_per_page, vec![0; 8], "{ctx}");
    assert!(cont.log.len() >= 8, "{ctx}");

    // Every page is pixel-exact the page we substituted for the trace's image data.
    let got = r.eng.engine_mut().take_pages();
    assert_eq!(got.len(), 8);
    for (i, p) in got.iter().enumerate() {
        let dec = t4::decode_page(&p.g4, t4::DecoderConfig::new(p.width, t4::Coding::Mmr)).unwrap();
        let (bm, _) = &pages[i];
        assert_eq!(dec.bitmap.len(), bm.len(), "page {} height", i + 1);
        assert_eq!(&dec.bitmap, bm, "page {} pixel exact", i + 1);
    }
}

#[test]
fn f1_04_summary_trace_t2_restarts_with_the_repeated_mps() {
    // Same replay, cut off after the second real MPS: nothing follows (the real sender gave
    // up). The engine must answer the repeat and then run T2 from the end of *that* response.
    let t = trace();
    let budget = page1_budget(&t.rx);
    let (_, d1) = page_for(1, budget);
    let mut sub = Substitute::new(vec![d1]);
    let mut r = Replay::new(Eng::Raw(T38Engine::new(engine_cfg())));
    for l in &t.rx {
        let ifp = to_ifp(l, &mut sub);
        r.schedule(l.t_ms, Input::Ifp(ifp));
    }
    let last_real = t.rx.last().unwrap().t_ms;
    // The field failure: the call was over at the trace's `duration_ms` (32140).
    while r.now < 33_000 {
        r.step();
    }
    let log = r.log();
    assert!(
        !r.eng.engine().is_done(),
        "ended early (the field failure): {log}"
    );
    while r.now < last_real + 30_000 && r.done_at.is_none() {
        r.step();
    }
    let res = r
        .eng
        .engine()
        .result()
        .cloned()
        .expect("T2 eventually expires");
    assert_eq!(res.code, t30::ResultCode::T2ExpiredImage);
    assert_eq!(res.pages, 1);
    assert_eq!(res.bad_lines_per_page, vec![0]);
    // T2 ran from the end of our last MCF (the response to the repeated MPS): the DCN burst
    // starts 6000 ms + 75 ms turnaround after the `no-signal` that ends it.
    let mcf_ends: Vec<u64> = r
        .heard
        .frames
        .iter()
        .filter(|(_, f)| f.get(2) == Some(&0x31))
        .map(|(at, _)| *r.heard.no_signals.iter().find(|n| **n >= *at).unwrap())
        .collect();
    assert!(mcf_ends.len() >= 2, "MCF and its repeat: {mcf_ends:?}");
    let last_mcf_end = *mcf_ends.last().unwrap();
    let dcn_preamble = r
        .heard
        .preambles
        .iter()
        .find(|p| **p > last_mcf_end)
        .copied()
        .unwrap();
    assert_eq!(dcn_preamble, last_mcf_end + 6_000 + 75, "{log}");
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

// ------------------------------------------------------------------ the exact replay

fn full_trace_path() -> Option<String> {
    std::env::var("F1_FULL_TRACE")
        .ok()
        .filter(|p| !p.is_empty())
}

/// `<ts> RX|TX <seq> <hex of the whole UDPTL datagram>`
fn parse_full(text: &str) -> Vec<(u64, bool, Vec<u8>)> {
    let mut v = Vec::new();
    let mut start = None;
    for l in text.lines().filter(|l| !l.trim().is_empty()) {
        let f: Vec<&str> = l.split_whitespace().collect();
        let time = f[0].split('T').nth(1).unwrap().trim_end_matches('Z');
        let mut p = time.split([':', '.']).map(|x| x.parse::<u64>().unwrap());
        let (h, m, s, ms) = (
            p.next().unwrap(),
            p.next().unwrap(),
            p.next().unwrap(),
            p.next().unwrap(),
        );
        let t = ((h * 60 + m) * 60 + s) * 1000 + ms;
        let t0 = *start.get_or_insert(t);
        v.push((t - t0, f[1] == "TX", unhex(f[3])));
    }
    v
}

#[test]
fn f1_04_full_trace_exact_replay() {
    let Some(path) = full_trace_path() else {
        eprintln!(
            "SKIPPED f1_04_full_trace_exact_replay: set F1_FULL_TRACE=<udptl.log of a [capture] udptl = \"full\" call> to run it"
        );
        return;
    };
    let text = std::fs::read_to_string(&path).expect("F1_FULL_TRACE is readable");
    let lines = parse_full(&text);
    let udptl = UdptlEngine::new(
        engine_cfg(),
        ErrorRecovery::Redundancy { depth: 3 },
        ReceiverConfig::default(),
    );
    let mut r = Replay::new(Eng::Udptl(Box::new(udptl)));
    // the peer's V.21 windows (FCS-OK .. sig-end/no-signal), from the datagrams themselves
    let mut windows: Vec<(u64, Option<u64>)> = Vec::new();
    let mut last_rx = 0;
    let mut seen = BTreeSet::new();
    for (t, tx, bytes) in &lines {
        if *tx {
            continue;
        }
        r.schedule(*t, Input::Datagram(bytes.clone()));
        last_rx = *t;
        let p = UdptlPacket::decode(bytes).unwrap();
        if !seen.insert(p.seq) {
            continue;
        }
        let (ifp, _) = Ifp::decode_prefix(&p.primary, t38::T38Version::V0).unwrap();
        match &ifp {
            Ifp::Data {
                data_type: DataType::V21,
                fields,
            } => {
                for f in fields {
                    match f.field_type {
                        FieldType::HdlcFcsOk => windows.push((*t, None)),
                        FieldType::HdlcSigEnd => {
                            if let Some(w) = windows.last_mut() {
                                w.1.get_or_insert(*t);
                            }
                        }
                        _ => {}
                    }
                }
            }
            Ifp::Indicator(Indicator::NoSignal) => {
                if let Some(w) = windows.last_mut() {
                    w.1.get_or_insert(*t);
                }
            }
            _ => {}
        }
    }
    // The field failure ended the call at `duration_ms` (33085 ms in the capture).
    while r.now < last_rx + 30_000 && r.done_at.is_none() {
        r.step();
        if r.now == 33_100 {
            let log = r.log();
            assert!(!r.eng.engine().is_done(), "ended early: {log}");
        }
    }
    let log = r.log();
    // 1. We never put a preamble on the air inside the peer's V.21 signal.
    assert_eq!(windows.len(), 3, "DCS and two MPS: {windows:?}");
    for (fcs, end) in &windows {
        let end = end.expect("every window ends");
        assert!(
            !r.heard.preambles.iter().any(|p| *p > *fcs && *p < end),
            "talked over the peer {fcs}..{end} ms; {log}"
        );
    }
    // The two MPS commands are answered exactly the turnaround after the peer's sig-end.
    for (fcs, end) in &windows[1..] {
        let first = r
            .heard
            .preambles
            .iter()
            .find(|p| **p > *fcs)
            .copied()
            .unwrap();
        assert_eq!(first, end.unwrap() + 75, "MPS at {fcs} ms; {log}");
    }
    // 2. Page 1 arrived without a bad line.
    let res = r
        .eng
        .engine()
        .result()
        .cloned()
        .expect("T2 eventually expires");
    assert_eq!(res.pages, 1, "{log}");
    assert_eq!(res.bad_lines_per_page, vec![0], "{log}");
    // 3. T2 started over with the response to the repeated MPS.
    assert_eq!(res.code, t30::ResultCode::T2ExpiredImage, "{log}");
    let mcf_ends: Vec<u64> = r
        .heard
        .frames
        .iter()
        .filter(|(_, f)| f.get(2) == Some(&0x31))
        .map(|(at, _)| *r.heard.no_signals.iter().find(|n| **n >= *at).unwrap())
        .collect();
    assert!(mcf_ends.len() >= 2, "{mcf_ends:?}");
    let last = *mcf_ends.last().unwrap();
    let dcn = r
        .heard
        .preambles
        .iter()
        .find(|p| **p > last)
        .copied()
        .unwrap();
    assert_eq!(dcn, last + 6_000 + 75, "{log}");
}
