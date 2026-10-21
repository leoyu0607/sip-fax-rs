//! DIS generation and DIS / DCS parsing (ITU-T T.30 Table 2).
//!
//! Bit numbers are those of Table 2 (see [`crate::frame`] for the octet convention). The bit
//! positions were cross-checked against SpanDSP 3.1.1 (`t30_dis_dtc_dcs_bits.h`,
//! `t30_build_dis_or_dtc`, `analyze_rx_dcs`) and against the DIS frames real SpanDSP sessions put
//! on the wire (`DIS` test `r02_*` in `tests/receiver.rs`), and in M3d against the text of
//! ITU-T Rec. T.30 (09/2005) itself (Table 2 and notes; the comparison table is in
//! docs/test-reports/M3d.md).

use crate::config::{Config, MinScanTime, PaperLength, PaperWidth};
use crate::frame::{fif_bit, set_fif_bit};

/// DIS / DCS bit numbers used by this crate (T.30 Table 2).
pub mod bits {
    /// Internet-aware fax over T.38 (`T38`).
    pub const T38: usize = 3;
    /// V.8 capability.
    pub const V8: usize = 6;
    /// DIS: ready to transmit a facsimile document (polling).
    pub const READY_TO_TRANSMIT: usize = 9;
    /// DIS: receiver capability. DCS: receive facsimile document.
    pub const RECEIVE: usize = 10;
    /// Data signalling rate, first of four bits (11..=14).
    pub const MODEM_FIRST: usize = 11;
    /// `R8 x 7.7 lines/mm` (fine) or 200 x 200 pels/25.4 mm.
    pub const FINE: usize = 15;
    /// DIS: `0` = 256 octet ECM frames preferred, `1` = 64 octets (Table 2 bit 7, note 42).
    pub const FRAME_SIZE_PREF: usize = 7;
    /// Two-dimensional coding (MR).
    pub const TWO_D: usize = 16;
    /// Recording width, first bit (17; 18 is the second).
    pub const WIDTH_1: usize = 17;
    /// Recording width, second bit.
    pub const WIDTH_2: usize = 18;
    /// Recording length, first bit (19; 20 is the second).
    pub const LENGTH_1: usize = 19;
    /// Recording length, second bit.
    pub const LENGTH_2: usize = 20;
    /// Minimum scan line time, first of three bits (21..=23).
    pub const MIN_SCAN_FIRST: usize = 21;
    /// Extend bit of the third octet.
    pub const EXTEND_24: usize = 24;
    /// Uncompressed mode.
    pub const UNCOMPRESSED: usize = 26;
    /// Error correction mode.
    pub const ECM: usize = 27;
    /// DCS: ECM frame size, `0` = 256 octets, `1` = 64 octets (valid only with bit 27, note 7).
    /// In a DIS this bit is "set to 0".
    pub const ECM_64: usize = 28;
    /// T.6 coding (MMR).
    pub const T6: usize = 31;
    /// R8 x 15.4 lines/mm or 200 x 400 pels/25.4 mm.
    pub const SUPERFINE_200_400: usize = 41;
    /// 300 x 300 pels/25.4 mm.
    pub const RES_300_300: usize = 42;
    /// R16 x 15.4 lines/mm or 400 x 400 pels/25.4 mm.
    pub const RES_400_400: usize = 43;
    /// Inch based resolution (DCS) / preferred (DIS).
    pub const INCH: usize = 44;
    /// Metric based resolution preferred (DIS).
    pub const METRIC: usize = 45;
}

/// Image data modulation chosen by a DCS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modem {
    /// V.27ter.
    V27ter,
    /// V.29.
    V29,
    /// V.33 (not offered by this crate; accepted from legacy senders, T.30 Table 2 note 31).
    V33,
    /// V.17.
    V17,
}

/// Vertical resolution selected by a DCS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// 3.85 lines/mm (98 dpi metric, 100 dpi inch).
    Standard,
    /// 7.7 lines/mm (196 dpi metric, 200 dpi inch).
    Fine,
    /// 15.4 lines/mm / 400 dpi vertical. Not supported by this crate.
    Superfine,
    /// 300 x 300. Not supported.
    Dpi300,
    /// 400 x 400. Not supported.
    Dpi400,
}

/// Coding scheme selected by a DCS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    /// T.4 one-dimensional.
    Mh,
    /// T.4 two-dimensional.
    Mr,
    /// T.6.
    Mmr,
}

impl Compression {
    /// The matching T.4 decoder coding.
    pub fn coding(self) -> t4::Coding {
        match self {
            Compression::Mh => t4::Coding::Mh,
            Compression::Mr => t4::Coding::Mr { k: 4 },
            Compression::Mmr => t4::Coding::Mmr,
        }
    }
}

/// Builds the information field of our DIS from the configuration.
///
/// Only the octets that carry information are emitted: three octets (bits 1..=24) normally, four
/// (bits 25..=32) if MMR is offered; the extend bit (the last bit of each octet) links them.
pub fn build_dis_fif(cfg: &Config) -> Vec<u8> {
    let mut fif = vec![0u8; 3];
    let mut set = |n: usize| set_fif_bit(&mut fif, n);
    if cfg.t38_iaf {
        set(bits::T38);
    }
    set(bits::RECEIVE);
    // Data signalling rate, bits 11..=14 (T.30 Table 2 note 4 combinations).
    let m = &cfg.modems;
    if m.v29 || m.v17 {
        set(bits::MODEM_FIRST); // bit 11: V.29
    }
    if m.v27ter || m.v17 {
        set(bits::MODEM_FIRST + 1); // bit 12: V.27ter
    }
    if m.v17 {
        set(bits::MODEM_FIRST + 3); // bit 14: V.17 (always together with 11 and 12)
    }
    if cfg.fine {
        set(bits::FINE);
    }
    if cfg.mr {
        set(bits::TWO_D);
    }
    match cfg.max_width {
        PaperWidth::A4 => {}
        PaperWidth::B4 => set(bits::WIDTH_1),
        PaperWidth::A3 => set(bits::WIDTH_2),
    }
    match cfg.max_length {
        PaperLength::A4 => {}
        PaperLength::B4 => set(bits::LENGTH_1),
        PaperLength::Unlimited => set(bits::LENGTH_2),
    }
    let code = cfg.min_scan.code();
    for i in 0..3 {
        if code & (1 << i) != 0 {
            set(bits::MIN_SCAN_FIRST + i);
        }
    }
    if cfg.ecm {
        set(bits::ECM);
        // Table 2 notes 9 and 17: T.6 is only valid together with ECM.
        if cfg.mmr {
            set(bits::T6);
        }
    }
    // Extend bits (bit 24, 32, ...): the last bit of every octet from the third on that is
    // followed by another octet. The first two octets have no extend bit.
    let octets = fif.len();
    for o in 3..octets {
        set_fif_bit(&mut fif, o * 8);
    }
    fif
}

/// A parsed DIS (the capabilities of the called terminal).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dis {
    /// Bit 3.
    pub t38_iaf: bool,
    /// Bit 6.
    pub v8: bool,
    /// Bit 9.
    pub ready_to_transmit: bool,
    /// Bit 10.
    pub can_receive: bool,
    /// Bits 11 to 14 in transmission order (bit 11 is the most significant of the four).
    pub modem_bits: u8,
    /// Bit 15.
    pub fine: bool,
    /// Bit 16.
    pub mr: bool,
    /// Bits 17 and 18.
    pub width: PaperWidth,
    /// Bits 19 and 20.
    pub length: PaperLength,
    /// Bits 21 to 23.
    pub min_scan: MinScanTime,
    /// Bit 26.
    pub uncompressed: bool,
    /// Bit 27.
    pub ecm: bool,
    /// Bit 31.
    pub mmr: bool,
}

impl Dis {
    /// V.29 offered (bit 11).
    pub fn v29(&self) -> bool {
        self.modem_bits & 0b1000 != 0
    }
    /// V.27ter offered (bit 12).
    pub fn v27ter(&self) -> bool {
        self.modem_bits & 0b0100 != 0
    }
    /// V.33 offered (bit 13).
    pub fn v33(&self) -> bool {
        self.modem_bits & 0b0010 != 0
    }
    /// V.17 offered (bit 14).
    pub fn v17(&self) -> bool {
        self.modem_bits & 0b0001 != 0
    }

    /// Parses the information field of a DIS / DTC. Total: bits past the end read as 0.
    pub fn parse(fif: &[u8]) -> Self {
        let b = |n| fif_bit(fif, n);
        let modem_bits = (0..4).fold(0u8, |acc, i| {
            (acc << 1) | u8::from(b(bits::MODEM_FIRST + i))
        });
        Self {
            t38_iaf: b(bits::T38),
            v8: b(bits::V8),
            ready_to_transmit: b(bits::READY_TO_TRANSMIT),
            can_receive: b(bits::RECEIVE),
            modem_bits,
            fine: b(bits::FINE),
            mr: b(bits::TWO_D),
            width: parse_width(b(bits::WIDTH_1), b(bits::WIDTH_2)),
            length: parse_length(b(bits::LENGTH_1), b(bits::LENGTH_2)),
            min_scan: parse_min_scan(fif),
            uncompressed: b(bits::UNCOMPRESSED),
            ecm: b(bits::ECM),
            mmr: b(bits::T6),
        }
    }
}

fn parse_width(b17: bool, b18: bool) -> PaperWidth {
    // T.30 Table 2 note 6: the invalid combination 11 is treated like 10.
    match (b17, b18) {
        (false, false) => PaperWidth::A4,
        (true, false) => PaperWidth::B4,
        (_, true) => PaperWidth::A3,
    }
}

fn parse_length(b19: bool, b20: bool) -> PaperLength {
    match (b19, b20) {
        (false, false) => PaperLength::A4,
        (true, false) => PaperLength::B4,
        (_, true) => PaperLength::Unlimited,
    }
}

fn parse_min_scan(fif: &[u8]) -> MinScanTime {
    let code = (0..3).fold(0u8, |acc, i| {
        acc | (u8::from(fif_bit(fif, bits::MIN_SCAN_FIRST + i)) << i)
    });
    MinScanTime::from_code(code)
}

/// A parsed DCS (the parameters the calling terminal selected).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dcs {
    /// Modulation of the image (and of the TCF).
    pub modem: Modem,
    /// Image data rate in bit/s.
    pub bit_rate: u32,
    /// Vertical resolution.
    pub resolution: Resolution,
    /// True if the resolution is inch based (bit 44), false for metric.
    pub inch_based: bool,
    /// Coding scheme.
    pub compression: Compression,
    /// Page width.
    pub width: PaperWidth,
    /// Page length class.
    pub length: PaperLength,
    /// Minimum scan line time.
    pub min_scan: MinScanTime,
    /// Error correction mode (bit 27).
    pub ecm: bool,
    /// ECM frame size in octets: 64 if bit 28 is set, else 256 (only meaningful with `ecm`).
    pub frame_octets: u32,
    /// Uncompressed mode (bit 26).
    pub uncompressed: bool,
    /// Bit 3: sender is Internet-aware.
    pub t38_iaf: bool,
    /// Bit 10: the DCS says "receive a facsimile document".
    pub receive: bool,
}

/// Why a DCS information field is unusable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DcsError {
    /// The field has fewer than the three mandatory octets.
    TooShort(usize),
    /// Bits 11 to 14 hold a reserved data signalling rate (the four bits are given).
    ReservedModem(u8),
}

impl std::fmt::Display for DcsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DcsError::TooShort(n) => write!(f, "DCS information field of {n} octets"),
            DcsError::ReservedModem(b) => write!(f, "reserved DCS data signalling rate {b:04b}"),
        }
    }
}

impl std::error::Error for DcsError {}

/// The data signalling rate selected by bits 11 to 14 of a DCS (or of a CTC, whose two octet
/// information field is bits 1 to 16 of a DCS).
///
/// Table 2 of T.30 (09/2005) lists 2400, 4800, 7200 and 9600 bit/s (V.27ter, V.29) and 7200 to
/// 14400 (V.17); `0010` and `0110` are "Invalid" in a DCS. Note 31 explains that terminals of the
/// 1994 edition and older used them for V.33 (14400 / 12000 bit/s) and that a V.17 receiver "may
/// optionally support" it, so they are accepted here. Everything else is reserved.
///
/// # Errors
/// [`DcsError::ReservedModem`] for a reserved combination.
pub fn parse_rate(fif: &[u8]) -> Result<(Modem, u32), DcsError> {
    let sel = (0..4).fold(0u8, |acc, i| {
        (acc << 1) | u8::from(fif_bit(fif, bits::MODEM_FIRST + i))
    });
    Ok(match sel {
        0b0000 => (Modem::V27ter, 2400),
        0b0100 => (Modem::V27ter, 4800),
        0b1000 => (Modem::V29, 9600),
        0b1100 => (Modem::V29, 7200),
        0b0010 => (Modem::V33, 14400),
        0b0110 => (Modem::V33, 12000),
        0b1001 => (Modem::V17, 9600),
        0b0001 => (Modem::V17, 14400),
        0b0101 => (Modem::V17, 12000),
        0b1101 => (Modem::V17, 7200),
        other => return Err(DcsError::ReservedModem(other)),
    })
}

/// A feature of a valid DCS this receiver cannot handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unsupported {
    /// Error correction mode is requested but not enabled.
    Ecm,
    /// Uncompressed mode.
    Uncompressed,
    /// A resolution above fine.
    Resolution(Resolution),
}

impl Dcs {
    /// Parses the information field of a DCS.
    ///
    /// # Errors
    /// [`DcsError`] if the field is shorter than three octets or the data signalling rate is
    /// one of the reserved combinations. Total: never panics.
    pub fn parse(fif: &[u8]) -> Result<Self, DcsError> {
        if fif.len() < 3 {
            return Err(DcsError::TooShort(fif.len()));
        }
        let b = |n| fif_bit(fif, n);
        let (modem, bit_rate) = parse_rate(fif)?;
        let resolution = if b(bits::RES_400_400) {
            Resolution::Dpi400
        } else if b(bits::RES_300_300) {
            Resolution::Dpi300
        } else if b(bits::SUPERFINE_200_400) {
            Resolution::Superfine
        } else if b(bits::FINE) {
            Resolution::Fine
        } else {
            Resolution::Standard
        };
        let compression = if b(bits::T6) {
            Compression::Mmr
        } else if b(bits::TWO_D) {
            Compression::Mr
        } else {
            Compression::Mh
        };
        Ok(Self {
            modem,
            bit_rate,
            resolution,
            inch_based: b(bits::INCH),
            compression,
            width: parse_width(b(bits::WIDTH_1), b(bits::WIDTH_2)),
            length: parse_length(b(bits::LENGTH_1), b(bits::LENGTH_2)),
            min_scan: parse_min_scan(fif),
            ecm: b(bits::ECM),
            frame_octets: if b(bits::ECM_64) { 64 } else { 256 },
            uncompressed: b(bits::UNCOMPRESSED),
            t38_iaf: b(bits::T38),
            receive: b(bits::RECEIVE),
        })
    }

    /// The first feature of this DCS a receiver without ECM cannot handle, if any.
    pub fn unsupported(&self) -> Option<Unsupported> {
        self.unsupported_with(false)
    }

    /// The first feature of this DCS the receiver cannot handle, if any; `ecm_ok` says whether
    /// the receiver implements the error correction mode.
    pub fn unsupported_with(&self, ecm_ok: bool) -> Option<Unsupported> {
        if self.ecm && !ecm_ok {
            Some(Unsupported::Ecm)
        } else if self.uncompressed {
            Some(Unsupported::Uncompressed)
        } else if matches!(
            self.resolution,
            Resolution::Superfine | Resolution::Dpi300 | Resolution::Dpi400
        ) {
            Some(Unsupported::Resolution(self.resolution))
        } else {
            None
        }
    }

    /// Horizontal resolution in dpi (8 pels/mm is 203.2, rounded to the usual 204 of Class F).
    pub fn x_dpi(&self) -> u32 {
        if self.inch_based { 200 } else { 204 }
    }

    /// Vertical resolution in dpi, rounded the way Class F files do (98 / 196, or 100 / 200).
    pub fn y_dpi(&self) -> u32 {
        match (self.resolution, self.inch_based) {
            (Resolution::Standard, false) => 98,
            (Resolution::Standard, true) => 100,
            (Resolution::Fine, false) => 196,
            (Resolution::Fine, true) => 200,
            (Resolution::Superfine, false) => 391,
            (Resolution::Superfine, true) => 400,
            (Resolution::Dpi300, _) => 300,
            (Resolution::Dpi400, _) => 400,
        }
    }
}
