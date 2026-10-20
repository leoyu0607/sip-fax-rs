//! T.4 / T.6 encoder. MMR (T.6) is what the server stores; MH and MR are provided so the
//! decoder can be tested against streams with EOLs, fill bits and RTC.

use crate::bitio::BitWriter;
use crate::changes::{self, at, first_after};
use crate::tables::TABLES;
use crate::{Coding, Error, MAX_WIDTH, row_bytes};

const EOL: u32 = 1;
const EOL_BITS: u32 = 12;

/// Row-by-row encoder producing one page of T.4 / T.6 data.
#[derive(Debug)]
pub struct Encoder {
    width: u32,
    coding: Coding,
    fill_bits: bool,
    out: BitWriter,
    ref_changes: Vec<u32>,
    cur: Vec<u32>,
    rows: u32,
}

impl Encoder {
    /// Create an encoder for rows `width` pixels wide.
    pub fn new(width: u32, coding: Coding) -> Result<Self, Error> {
        if width == 0 || width > MAX_WIDTH {
            return Err(Error::InvalidWidth(width));
        }
        if let Coding::Mr { k } = coding {
            if k < 2 {
                return Err(Error::InvalidK(k));
            }
        }
        Ok(Self {
            width,
            coding,
            fill_bits: false,
            out: BitWriter::default(),
            ref_changes: Vec::new(),
            cur: Vec::new(),
            rows: 0,
        })
    }

    /// MH / MR: pad with zero fill bits so that every EOL ends on a byte boundary.
    pub fn with_fill_bits(mut self, on: bool) -> Self {
        self.fill_bits = on;
        self
    }

    /// Rows encoded so far.
    pub fn rows(&self) -> u32 {
        self.rows
    }

    fn put_eol(&mut self, tag_1d: bool) {
        if self.fill_bits {
            let after = (self.out.bit_len() + EOL_BITS as usize) % 8;
            if after != 0 {
                self.out.put(0, (8 - after) as u32);
            }
        }
        self.out.put(EOL, EOL_BITS);
        if matches!(self.coding, Coding::Mr { .. }) {
            self.out.put(u32::from(tag_1d), 1);
        }
    }

    fn put_run(&mut self, black: bool, mut run: u32) {
        let t = TABLES.color(black);
        while run >= 2560 + 64 {
            let c = t.makeup(2560);
            self.out.put(u32::from(c.value), u32::from(c.bits));
            run -= 2560;
        }
        if run >= 64 {
            let c = t.makeup(run / 64 * 64);
            self.out.put(u32::from(c.value), u32::from(c.bits));
        }
        let c = t.terminating(run % 64);
        self.out.put(u32::from(c.value), u32::from(c.bits));
    }

    /// Append one packed row (1 = black).
    pub fn push_row(&mut self, row: &[u8]) -> Result<(), Error> {
        let expected = row_bytes(self.width);
        if row.len() != expected {
            return Err(Error::RowSize {
                expected,
                got: row.len(),
            });
        }
        let mut cur = std::mem::take(&mut self.cur);
        changes::from_packed(row, self.width, &mut cur);
        match self.coding {
            Coding::Mh => {
                self.put_eol(true);
                self.encode_1d(&cur);
            }
            Coding::Mr { k } => {
                let one_d = self.rows % u32::from(k) == 0;
                self.put_eol(one_d);
                if one_d {
                    self.encode_1d(&cur);
                } else {
                    self.encode_2d(&cur);
                }
            }
            Coding::Mmr => self.encode_2d(&cur),
        }
        self.rows += 1;
        // The row just coded becomes the reference; recycle the old reference's allocation.
        std::mem::swap(&mut cur, &mut self.ref_changes);
        self.cur = cur;
        Ok(())
    }

    fn encode_1d(&mut self, ch: &[u32]) {
        let mut x = 0u32;
        let mut black = false;
        for &c in ch {
            self.put_run(black, c - x);
            x = c;
            black = !black;
        }
        self.put_run(black, self.width - x);
    }

    fn encode_2d(&mut self, cur: &[u32]) {
        let w = i64::from(self.width);
        let mut a0: i64 = -1;
        let mut black = false;
        let (mut hint_a, mut hint_b) = (0usize, 0usize);
        let reference = std::mem::take(&mut self.ref_changes);
        while a0 < w {
            let a1_idx = first_after(cur, a0, black, &mut hint_a);
            let a1 = i64::from(at(cur, self.width, a1_idx));
            let b1_idx = first_after(&reference, a0, black, &mut hint_b);
            let b1 = i64::from(at(&reference, self.width, b1_idx));
            let b2 = i64::from(at(&reference, self.width, b1_idx + 1));
            if b2 < a1 {
                self.out.put(0b0001, 4);
                a0 = b2;
            } else if (a1 - b1).abs() <= 3 {
                let (code, bits) = match a1 - b1 {
                    0 => (0b1, 1),
                    1 => (0b011, 3),
                    2 => (0b000011, 6),
                    3 => (0b0000011, 7),
                    -1 => (0b010, 3),
                    -2 => (0b000010, 6),
                    _ => (0b0000010, 7),
                };
                self.out.put(code, bits);
                a0 = a1;
                black = !black;
            } else {
                let a2 = i64::from(at(cur, self.width, a1_idx + 1));
                let start = a0.max(0);
                self.out.put(0b001, 3);
                self.put_run(black, (a1 - start) as u32);
                self.put_run(!black, (a2 - a1) as u32);
                a0 = a2;
            }
        }
        self.ref_changes = reference;
    }

    /// Terminate the page (RTC for MH / MR, EOFB for MMR) and return the bytes.
    pub fn finish(mut self) -> Vec<u8> {
        match self.coding {
            Coding::Mmr => {
                self.out.put(EOL, EOL_BITS);
                self.out.put(EOL, EOL_BITS);
            }
            _ => {
                for _ in 0..6 {
                    self.put_eol(true);
                }
            }
        }
        self.out.finish()
    }
}

/// Encode a whole bitmap (`rows * ceil(width / 8)` packed bytes) as one MMR (G4) stream.
pub fn encode_mmr(bitmap: &[u8], width: u32) -> Result<Vec<u8>, Error> {
    let mut enc = Encoder::new(width, Coding::Mmr)?;
    let rb = row_bytes(width);
    for row in bitmap.chunks(rb) {
        enc.push_row(row)?;
    }
    Ok(enc.finish())
}
