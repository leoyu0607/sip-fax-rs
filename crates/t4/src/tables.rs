//! ITU-T T.4 run-length code tables (white / black terminating and make-up codes, plus the
//! shared extended make-up codes) and the lookup structures derived from them.
//!
//! The tables are written as bit strings exactly as printed in the recommendation; the lookup
//! structures are built once on first use.

use std::sync::LazyLock;

/// White terminating codes for run lengths 0..=63.
const WHITE_TERM: [&str; 64] = [
    "00110101", "000111", "0111", "1000", "1011", "1100", "1110", "1111", "10011", "10100",
    "00111", "01000", "001000", "000011", "110100", "110101", "101010", "101011", "0100111",
    "0001100", "0001000", "0010111", "0000011", "0000100", "0101000", "0101011", "0010011",
    "0100100", "0011000", "00000010", "00000011", "00011010", "00011011", "00010010", "00010011",
    "00010100", "00010101", "00010110", "00010111", "00101000", "00101001", "00101010", "00101011",
    "00101100", "00101101", "00000100", "00000101", "00001010", "00001011", "01010010", "01010011",
    "01010100", "01010101", "00100100", "00100101", "01011000", "01011001", "01011010", "01011011",
    "01001010", "01001011", "00110010", "00110011", "00110100",
];

/// White make-up codes for run lengths 64, 128, ..., 1728.
const WHITE_MAKEUP: [&str; 27] = [
    "11011",
    "10010",
    "010111",
    "0110111",
    "00110110",
    "00110111",
    "01100100",
    "01100101",
    "01101000",
    "01100111",
    "011001100",
    "011001101",
    "011010010",
    "011010011",
    "011010100",
    "011010101",
    "011010110",
    "011010111",
    "011011000",
    "011011001",
    "011011010",
    "011011011",
    "010011000",
    "010011001",
    "010011010",
    "011000",
    "010011011",
];

/// Black terminating codes for run lengths 0..=63.
const BLACK_TERM: [&str; 64] = [
    "0000110111",
    "010",
    "11",
    "10",
    "011",
    "0011",
    "0010",
    "00011",
    "000101",
    "000100",
    "0000100",
    "0000101",
    "0000111",
    "00000100",
    "00000111",
    "000011000",
    "0000010111",
    "0000011000",
    "0000001000",
    "00001100111",
    "00001101000",
    "00001101100",
    "00000110111",
    "00000101000",
    "00000010111",
    "00000011000",
    "000011001010",
    "000011001011",
    "000011001100",
    "000011001101",
    "000001101000",
    "000001101001",
    "000001101010",
    "000001101011",
    "000011010010",
    "000011010011",
    "000011010100",
    "000011010101",
    "000011010110",
    "000011010111",
    "000001101100",
    "000001101101",
    "000011011010",
    "000011011011",
    "000001010100",
    "000001010101",
    "000001010110",
    "000001010111",
    "000001100100",
    "000001100101",
    "000001010010",
    "000001010011",
    "000000100100",
    "000000110111",
    "000000111000",
    "000000100111",
    "000000101000",
    "000001011000",
    "000001011001",
    "000000101011",
    "000000101100",
    "000001011010",
    "000001100110",
    "000001100111",
];

/// Black make-up codes for run lengths 64, 128, ..., 1728.
const BLACK_MAKEUP: [&str; 27] = [
    "0000001111",
    "000011001000",
    "000011001001",
    "000001011011",
    "000000110011",
    "000000110100",
    "000000110101",
    "0000001101100",
    "0000001101101",
    "0000001001010",
    "0000001001011",
    "0000001001100",
    "0000001001101",
    "0000001110010",
    "0000001110011",
    "0000001110100",
    "0000001110101",
    "0000001110110",
    "0000001110111",
    "0000001010010",
    "0000001010011",
    "0000001010100",
    "0000001010101",
    "0000001011010",
    "0000001011011",
    "0000001100100",
    "0000001100101",
];

/// Extended make-up codes (both colours) for run lengths 1792, 1856, ..., 2560.
const EXT_MAKEUP: [&str; 13] = [
    "00000001000",
    "00000001100",
    "00000001101",
    "000000010010",
    "000000010011",
    "000000010100",
    "000000010101",
    "000000010110",
    "000000010111",
    "000000011100",
    "000000011101",
    "000000011110",
    "000000011111",
];

/// Longest run-length code (black make-up codes are 13 bits).
pub(crate) const MAX_CODE_BITS: u32 = 13;

/// A code word: value right-aligned in `bits` bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Code {
    pub value: u16,
    pub bits: u8,
}

fn parse(s: &str) -> Code {
    let mut value = 0u16;
    for c in s.bytes() {
        value = (value << 1) | u16::from(c == b'1');
    }
    Code {
        value,
        bits: s.len() as u8,
    }
}

/// One entry of the 13-bit prefix lookup table; `bits == 0` means "not a valid code".
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Lookup {
    pub run: u16,
    pub bits: u8,
}

/// Encode and decode tables for one colour.
pub(crate) struct ColorTables {
    /// Terminating codes, index = run length (0..64).
    term: Vec<Code>,
    /// Make-up codes, index = run / 64 - 1 (64..=2560, 40 entries).
    makeup: Vec<Code>,
    /// Decode table indexed by the next 13 bits.
    pub lookup: Vec<Lookup>,
}

impl ColorTables {
    fn build(term: &[&str], makeup: &[&str]) -> Self {
        let term: Vec<Code> = term.iter().map(|s| parse(s)).collect();
        let mut mk: Vec<Code> = makeup.iter().map(|s| parse(s)).collect();
        mk.extend(EXT_MAKEUP.iter().map(|s| parse(s)));
        let mut lookup = vec![Lookup::default(); 1 << MAX_CODE_BITS];
        let all = term.iter().enumerate().map(|(i, c)| (i as u16, *c)).chain(
            mk.iter()
                .enumerate()
                .map(|(i, c)| ((i as u16 + 1) * 64, *c)),
        );
        for (run, c) in all {
            let shift = MAX_CODE_BITS - u32::from(c.bits);
            let base = usize::from(c.value) << shift;
            for slot in &mut lookup[base..base + (1usize << shift)] {
                *slot = Lookup { run, bits: c.bits };
            }
        }
        Self {
            term,
            makeup: mk,
            lookup,
        }
    }

    /// Terminating code for `run` (0..64).
    pub fn terminating(&self, run: u32) -> Code {
        self.term[(run & 63) as usize]
    }

    /// Make-up code for `run`, a multiple of 64 in 64..=2560.
    pub fn makeup(&self, run: u32) -> Code {
        self.makeup[(run / 64 - 1) as usize]
    }
}

/// Tables for white (index 0) and black (index 1) runs.
pub(crate) struct Tables {
    pub white: ColorTables,
    pub black: ColorTables,
}

impl Tables {
    /// Tables for the given colour.
    pub fn color(&self, black: bool) -> &ColorTables {
        if black { &self.black } else { &self.white }
    }
}

/// The shared tables.
pub(crate) static TABLES: LazyLock<Tables> = LazyLock::new(|| Tables {
    white: ColorTables::build(&WHITE_TERM, &WHITE_MAKEUP),
    black: ColorTables::build(&BLACK_TERM, &BLACK_MAKEUP),
});

#[cfg(test)]
mod tests;
