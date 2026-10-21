//! Streaming T.4 / T.6 decoder.
//!
//! Data is pushed in arbitrary chunks; every completed row is handed to a sink as a packed
//! 1-bit row (1 = black). The decoder never buffers more than a few KiB of undecoded input
//! and never emits more than [`DecoderConfig::max_rows`] rows.
//!
//! Error handling follows the spec:
//!
//! * MH / MR (non-ECM): a row that fails to decode is replaced by the previous row, counted in
//!   [`Stats::bad_lines`], and decoding resynchronises at the next EOL.
//! * MMR has no EOL to resynchronise on: the first error abandons the rest of the page
//!   ([`EndReason::Aborted`] with the error in [`Summary::error`]).

use crate::bitio::Bits;
use crate::changes::{self, at, first_after};
use crate::tables::{MAX_CODE_BITS, TABLES};
use crate::{Coding, Error, MAX_WIDTH, row_bytes};

/// Default hard limit on rows per page (a long A4 page at 196 dpi is about 2300 rows).
pub const DEFAULT_MAX_ROWS: u32 = 16_384;

/// EOLs in a row that end the page (RTC).
const RTC_EOLS: u32 = 6;
/// Bytes handed to the bit buffer at once, bounding buffered input.
const CHUNK: usize = 4096;

/// Decoder parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecoderConfig {
    /// Line width in pixels (1728, 2048 or 2432 for fax; any 1..=[`MAX_WIDTH`] is accepted).
    pub width: u32,
    /// Coding scheme.
    pub coding: Coding,
    /// MH/MR: require an EOL after every row. A row that is not followed by an EOL is then
    /// treated as bad. Set to false for EOL-less strips. Ignored for MMR.
    pub eol_required: bool,
    /// Hard limit on rows; decoding stops with [`EndReason::RowLimit`] when reached.
    pub max_rows: u32,
}

impl DecoderConfig {
    /// Configuration with EOLs required and the default row limit.
    pub fn new(width: u32, coding: Coding) -> Self {
        Self {
            width,
            coding,
            eol_required: true,
            max_rows: DEFAULT_MAX_ROWS,
        }
    }
}

/// Why decoding stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    /// The input ended (no RTC / EOFB seen).
    Data,
    /// Return To Control: six consecutive EOLs (MH / MR).
    Rtc,
    /// End Of Facsimile Block (MMR).
    Eofb,
    /// [`DecoderConfig::max_rows`] was reached.
    RowLimit,
    /// MMR error: the rest of the page was abandoned.
    Aborted,
}

/// Row statistics for the T.30 layer (MCF versus RTN).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// Rows delivered, including substituted ones.
    pub rows: u32,
    /// Rows that failed to decode and were replaced by the previous row.
    pub bad_lines: u32,
    /// Longest run of consecutive bad rows.
    pub longest_bad_run: u32,
}

/// Final result of [`Decoder::finish`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    /// Row statistics.
    pub stats: Stats,
    /// Why decoding stopped.
    pub end: EndReason,
    /// The error that aborted an MMR page, if any.
    pub error: Option<Error>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// MH/MR: consume EOLs (and tag bits) before a row.
    AwaitEol,
    /// MH/MR: decode a row.
    RowData,
    /// MH/MR: skip to the next EOL after an error.
    Resync,
    /// MMR: decode rows back to back.
    Mmr,
    Done,
}

enum Flow {
    Progress,
    NeedMore,
    Stop,
}

enum Fail {
    More,
    Bad(Error),
}

type Res<T> = Result<T, Fail>;

/// Streaming decoder. See the module documentation.
#[derive(Debug)]
pub struct Decoder {
    cfg: DecoderConfig,
    bits: Bits,
    state: State,
    /// Changing elements of the reference (previous) row.
    ref_changes: Vec<u32>,
    cur: Vec<u32>,
    prev_row: Vec<u8>,
    row_buf: Vec<u8>,
    /// MR: the row being decoded is one-dimensional.
    tag_1d: bool,
    consec_eols: u32,
    /// MH / MR: an EOL or a good row has been seen, so what follows is page data. Before that,
    /// undecodable bits are line-up noise ahead of the first EOL (a gateway's marking idle,
    /// field case F1) and are skipped without counting a row.
    synced: bool,
    /// Noise was skipped before any EOL: if no EOL ever follows, it is counted as a bad row
    /// after all (data without any EOL is not a clean page).
    noise_pending: bool,
    bad_run: u32,
    stats: Stats,
    end: Option<EndReason>,
    error: Option<Error>,
}

impl Decoder {
    /// Create a decoder.
    pub fn new(cfg: DecoderConfig) -> Result<Self, Error> {
        if cfg.width == 0 || cfg.width > MAX_WIDTH {
            return Err(Error::InvalidWidth(cfg.width));
        }
        let rb = row_bytes(cfg.width);
        Ok(Self {
            cfg,
            bits: Bits::default(),
            state: if cfg.coding == Coding::Mmr {
                State::Mmr
            } else {
                State::AwaitEol
            },
            ref_changes: Vec::new(),
            cur: Vec::new(),
            prev_row: vec![0; rb],
            row_buf: vec![0; rb],
            tag_1d: true,
            consec_eols: 0,
            synced: false,
            noise_pending: false,
            bad_run: 0,
            stats: Stats::default(),
            end: None,
            error: None,
        })
    }

    /// Row statistics so far.
    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// True once the page has ended; further input is ignored.
    pub fn is_finished(&self) -> bool {
        self.state == State::Done
    }

    /// Feed more data. `sink` is called once per completed row.
    pub fn push(&mut self, data: &[u8], sink: &mut dyn FnMut(&[u8])) {
        for chunk in data.chunks(CHUNK) {
            if self.state == State::Done {
                return;
            }
            self.bits.extend(chunk);
            self.run(sink);
        }
    }

    /// Signal the end of input, flush what can be decoded and return the summary.
    pub fn finish(mut self, sink: &mut dyn FnMut(&[u8])) -> Summary {
        self.bits.eof = true;
        self.run(sink);
        if self.state != State::Done {
            self.stop(EndReason::Data);
        }
        if self.noise_pending && !self.synced {
            self.bad_row();
            sink(&self.prev_row);
        }
        Summary {
            stats: self.stats,
            end: self.end.unwrap_or(EndReason::Data),
            error: self.error,
        }
    }

    fn run(&mut self, sink: &mut dyn FnMut(&[u8])) {
        while let Flow::Progress = self.step(sink) {}
        self.bits.compact();
    }

    fn stop(&mut self, why: EndReason) -> Flow {
        self.state = State::Done;
        if self.end.is_none() {
            self.end = Some(why);
        }
        Flow::Stop
    }

    fn step(&mut self, sink: &mut dyn FnMut(&[u8])) -> Flow {
        if self.state != State::Done && self.stats.rows >= self.cfg.max_rows {
            return self.stop(EndReason::RowLimit);
        }
        match self.state {
            State::Done => Flow::Stop,
            State::AwaitEol => self.step_await_eol(),
            State::RowData => self.step_row(sink),
            State::Resync => self.step_resync(),
            State::Mmr => self.step_mmr(sink),
        }
    }

    // ---- MH / MR ----

    /// Try to read `EOL [tag]` at the current position.
    fn parse_eol(&mut self) -> Res<bool> {
        let start = self.bits.pos;
        let total = self.bits.total();
        let mut p = start;
        while p < total && self.bits.bit_at(p) == 0 {
            p += 1;
        }
        let zeros = p - start;
        if p >= total {
            // Only zeros so far. Fill bits beyond the 11 needed carry no information.
            if zeros > 11 {
                self.bits.pos = start + zeros - 11;
            }
            return Err(Fail::More);
        }
        if zeros < 11 {
            return Ok(false);
        }
        let mut next = p + 1;
        if matches!(self.cfg.coding, Coding::Mr { .. }) {
            if next >= total {
                self.bits.pos = start + zeros - 11;
                return Err(Fail::More);
            }
            self.tag_1d = self.bits.bit_at(next) == 1;
            next += 1;
        }
        self.bits.pos = next;
        Ok(true)
    }

    fn step_await_eol(&mut self) -> Flow {
        match self.parse_eol() {
            Ok(true) => {
                self.synced = true;
                self.consec_eols = self.consec_eols.saturating_add(1);
                if self.consec_eols >= RTC_EOLS {
                    return self.stop(EndReason::Rtc);
                }
                Flow::Progress
            }
            Ok(false) => {
                self.state = State::RowData;
                Flow::Progress
            }
            // Only fill bits / a partial EOL are left: that is a clean end of the data.
            Err(_) if self.bits.eof => self.stop(EndReason::Data),
            Err(_) => Flow::NeedMore,
        }
    }

    fn step_row(&mut self, sink: &mut dyn FnMut(&[u8])) -> Flow {
        let start = self.bits.pos;
        let one_d = match self.cfg.coding {
            Coding::Mh => true,
            _ => self.tag_1d,
        };
        let r = if one_d {
            decode_1d(&mut self.bits, self.cfg.width, &mut self.cur)
        } else {
            decode_2d(
                &mut self.bits,
                self.cfg.width,
                &self.ref_changes,
                &mut self.cur,
            )
        };
        match r {
            Ok(()) => {
                if self.cfg.eol_required {
                    match self.verify_eol_follows() {
                        Ok(true) => {}
                        Ok(false) if !self.synced => {
                            // Marking idle ahead of the first EOL can decode as a "row" of
                            // 7-pixel white and 2-pixel black runs (all ones): noise too.
                            self.noise_pending = true;
                            self.bits.pos = start;
                            self.state = State::Resync;
                            return Flow::Progress;
                        }
                        Ok(false) => {
                            // The row decoded but is not followed by an EOL: not a real row.
                            self.bad_row();
                            sink(&self.prev_row);
                            self.state = State::Resync;
                            return Flow::Progress;
                        }
                        Err(_) => {
                            self.bits.pos = start;
                            return Flow::NeedMore;
                        }
                    }
                }
                self.good_row(sink);
                self.consec_eols = 0;
                self.state = State::AwaitEol;
                Flow::Progress
            }
            Err(Fail::More) => {
                self.bits.pos = start;
                Flow::NeedMore
            }
            Err(Fail::Bad(_)) if !self.synced => {
                // Noise before the first EOL is not a row of the page.
                self.noise_pending = true;
                // Look for the EOL from where the noise began: the failed attempt may have
                // read into the zeros of the EOL.
                self.bits.pos = start;
                self.state = State::Resync;
                Flow::Progress
            }
            Err(Fail::Bad(_)) => {
                self.bad_row();
                sink(&self.prev_row);
                self.consec_eols = 0;
                self.state = State::Resync;
                Flow::Progress
            }
        }
    }

    /// After a row: fill zeros then an EOL must follow (or the data must end).
    fn verify_eol_follows(&self) -> Res<bool> {
        let mut p = self.bits.pos;
        let total = self.bits.total();
        let mut zeros = 0;
        while p < total {
            if self.bits.bit_at(p) == 1 {
                return Ok(zeros >= 11);
            }
            zeros += 1;
            if zeros >= 11 {
                return Ok(true);
            }
            p += 1;
        }
        if self.bits.eof {
            Ok(true)
        } else {
            Err(Fail::More)
        }
    }

    fn step_resync(&mut self) -> Flow {
        let total = self.bits.total();
        let mut p = self.bits.pos;
        let mut zeros = 0usize;
        while p < total {
            if self.bits.bit_at(p) == 0 {
                zeros += 1;
            } else {
                if zeros >= 11 {
                    self.bits.pos = p - zeros;
                    self.state = State::AwaitEol;
                    return Flow::Progress;
                }
                zeros = 0;
            }
            p += 1;
        }
        if self.bits.eof {
            self.bits.pos = total;
            return self.stop(EndReason::Data);
        }
        // Keep a trailing zero run: it may be the start of an EOL.
        self.bits.pos = total - zeros.min(11);
        Flow::NeedMore
    }

    // ---- MMR ----

    fn step_mmr(&mut self, sink: &mut dyn FnMut(&[u8])) -> Flow {
        let avail = self.bits.avail();
        if avail == 0 {
            return if self.bits.eof {
                self.stop(EndReason::Data)
            } else {
                Flow::NeedMore
            };
        }
        let n = avail.min(12) as u32;
        let head = self.bits.peek(n);
        if head == 0 {
            // Zeros: either padding or the start of an EOL (EOFB).
            if self.bits.eof && self.bits.rest_is_zero() {
                return self.stop(EndReason::Data);
            }
            if n < 12 {
                // At end of input this would have been padding (handled above).
                return Flow::NeedMore;
            }
        } else if n == 12 && head == 1 {
            self.bits.pos += 12;
            return self.stop(EndReason::Eofb);
        }
        let start = self.bits.pos;
        match decode_2d(
            &mut self.bits,
            self.cfg.width,
            &self.ref_changes,
            &mut self.cur,
        ) {
            Ok(()) => {
                self.good_row(sink);
                Flow::Progress
            }
            Err(Fail::More) => {
                self.bits.pos = start;
                Flow::NeedMore
            }
            Err(Fail::Bad(e)) => self.abort(e),
        }
    }

    fn abort(&mut self, e: Error) -> Flow {
        self.error = Some(e);
        self.stop(EndReason::Aborted)
    }

    // ---- row output ----

    fn good_row(&mut self, sink: &mut dyn FnMut(&[u8])) {
        self.synced = true;
        changes::normalize(&mut self.cur);
        changes::to_packed(&self.cur, self.cfg.width, &mut self.row_buf);
        sink(&self.row_buf);
        std::mem::swap(&mut self.row_buf, &mut self.prev_row);
        std::mem::swap(&mut self.cur, &mut self.ref_changes);
        self.stats.rows += 1;
        self.bad_run = 0;
    }

    /// Account for a bad row; the caller emits `prev_row` (borrowing prevents doing it here).
    fn bad_row(&mut self) {
        self.stats.rows += 1;
        self.stats.bad_lines += 1;
        self.bad_run += 1;
        self.consec_eols = 0;
        self.stats.longest_bad_run = self.stats.longest_bad_run.max(self.bad_run);
    }
}

// ---- row decoding ----

fn short(bits: &Bits) -> Fail {
    if bits.eof {
        Fail::Bad(Error::Truncated)
    } else {
        Fail::More
    }
}

/// Read one complete run (make-up codes plus terminating code) of the given colour.
fn read_run(bits: &mut Bits, black: bool, limit: u32) -> Res<u32> {
    let table = &TABLES.color(black).lookup;
    let mut total = 0u32;
    loop {
        let avail = bits.avail();
        let idx = bits.peek(MAX_CODE_BITS) as usize;
        let e = table[idx];
        if e.bits == 0 {
            return Err(if avail < MAX_CODE_BITS as usize {
                short(bits)
            } else {
                Fail::Bad(Error::InvalidCode)
            });
        }
        if usize::from(e.bits) > avail {
            return Err(short(bits));
        }
        bits.pos += usize::from(e.bits);
        total += u32::from(e.run);
        if total > limit {
            return Err(Fail::Bad(Error::LineOverrun));
        }
        if e.run < 64 {
            return Ok(total);
        }
    }
}

/// One-dimensional row: alternating white / black runs summing to `width`.
fn decode_1d(bits: &mut Bits, width: u32, out: &mut Vec<u32>) -> Res<()> {
    out.clear();
    let mut x = 0u32;
    let mut black = false;
    while x < width {
        let before = bits.pos;
        let run = match read_run(bits, black, width - x) {
            Ok(r) => r,
            Err(f) => {
                // Report an invalid code at its own start so resync does not skip an EOL.
                if matches!(f, Fail::Bad(_)) {
                    bits.pos = before;
                }
                return Err(f);
            }
        };
        x += run;
        if x < width {
            out.push(x);
        }
        if out.len() > width as usize + 2 {
            return Err(Fail::Bad(Error::TooManyChanges));
        }
        black = !black;
    }
    Ok(())
}

/// Two-dimensional row coded against `reference`.
fn decode_2d(bits: &mut Bits, width: u32, reference: &[u32], out: &mut Vec<u32>) -> Res<()> {
    out.clear();
    let w = i64::from(width);
    let mut a0: i64 = -1;
    let mut black = false;
    let mut hint = 0usize;
    while a0 < w {
        let mode_start = bits.pos;
        let avail = bits.avail();
        let v = bits.peek(7);
        // (code length, kind) where kind: 0 = pass, 1 = horizontal, 2.. = vertical offset + 5.
        let (len, kind): (usize, u8) = if v >> 6 == 1 {
            (1, 5)
        } else if v >> 4 == 0b011 {
            (3, 6)
        } else if v >> 4 == 0b010 {
            (3, 4)
        } else if v >> 4 == 0b001 {
            (3, 1)
        } else if v >> 3 == 0b0001 {
            (4, 0)
        } else if v >> 1 == 0b000011 {
            (6, 7)
        } else if v >> 1 == 0b000010 {
            (6, 3)
        } else if v == 0b0000011 {
            (7, 8)
        } else if v == 0b0000010 {
            (7, 2)
        } else if v >> 4 == 0 && avail < 7 {
            return Err(short(bits));
        } else if v == 0b0000001 {
            return Err(Fail::Bad(Error::Unsupported));
        } else {
            return Err(Fail::Bad(Error::InvalidCode));
        };
        if len > avail {
            return Err(short(bits));
        }
        bits.pos += len;

        let b1_idx = first_after(reference, a0, black, &mut hint);
        let b1 = i64::from(at(reference, width, b1_idx));
        match kind {
            0 => {
                a0 = i64::from(at(reference, width, b1_idx + 1));
            }
            1 => {
                let start = a0.max(0);
                let r1 = match read_run(bits, black, (w - start) as u32) {
                    Ok(r) => r,
                    Err(f) => return Err(restore(bits, mode_start, f)),
                };
                let r2 = match read_run(bits, !black, (w - start - i64::from(r1)) as u32) {
                    Ok(r) => r,
                    Err(f) => return Err(restore(bits, mode_start, f)),
                };
                let a1 = start + i64::from(r1);
                let a2 = a1 + i64::from(r2);
                if a1 < w {
                    out.push(a1 as u32);
                }
                if a2 < w {
                    out.push(a2 as u32);
                }
                a0 = a2;
            }
            _ => {
                let a1 = b1 + i64::from(kind) - 5;
                if a1 > w || a1 < 0 || (a1 <= a0 && a0 >= 0) {
                    bits.pos = mode_start;
                    return Err(Fail::Bad(Error::InvalidCode));
                }
                if a1 < w {
                    out.push(a1 as u32);
                }
                a0 = a1;
                black = !black;
            }
        }
        if out.len() > width as usize + 2 {
            return Err(Fail::Bad(Error::TooManyChanges));
        }
    }
    Ok(())
}

/// A failure inside a horizontal-mode run: rewind to the mode code so the position is stable.
fn restore(bits: &mut Bits, mode_start: usize, f: Fail) -> Fail {
    if matches!(f, Fail::Bad(_)) {
        bits.pos = mode_start;
    }
    f
}

// ---- convenience ----

/// A fully decoded page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageData {
    /// Packed rows, `rows * ceil(width / 8)` bytes.
    pub bitmap: Vec<u8>,
    /// Summary of the decode.
    pub summary: Summary,
}

/// Decode a complete page held in memory.
pub fn decode_page(data: &[u8], cfg: DecoderConfig) -> Result<PageData, Error> {
    let mut dec = Decoder::new(cfg)?;
    let mut bitmap = Vec::new();
    dec.push(data, &mut |row| bitmap.extend_from_slice(row));
    let summary = dec.finish(&mut |row| bitmap.extend_from_slice(row));
    Ok(PageData { bitmap, summary })
}
