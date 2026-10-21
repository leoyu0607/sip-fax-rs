//! HDLC frame layout and facsimile control field (FCF) values (ITU-T T.30 section 5 and Table 3).
//!
//! # Octet convention
//!
//! A frame is `address (0xFF) | control | FCF | FIF...` **without** the FCS. Octets are written
//! the way T.38 carries them in `hdlc-data` and the way T.30 tabulates them: the first bit that
//! goes on the line is the most significant bit of the octet. For example a final DIS frame is
//! `FF C8 01 ...` and a final DCS is `FF C8 C1 ...`. SpanDSP keeps the same frames bit reversed
//! in memory (`FF 13 80 ...`); this crate never does.
//!
//! "Bit n" of a DIS / DTC / DCS information field (T.30 Table 2) is the n-th bit transmitted
//! after the FCF, i.e. `fif[(n - 1) / 8] & (0x80 >> ((n - 1) % 8))`.

/// HDLC address field.
pub const ADDRESS: u8 = 0xFF;
/// Control field of the final frame of a burst.
pub const CONTROL_FINAL: u8 = 0xC8;
/// Control field of a non-final frame of a burst.
pub const CONTROL_NON_FINAL: u8 = 0xC0;

/// Facsimile control field values with the `X` bit (the first transmitted bit) cleared.
///
/// Frames sent by the called terminal use `X = 0`; frames sent by the calling terminal carry
/// `X = 1` after a DIS was received (so a DCS goes out as `0xC1`).
pub mod fcf {
    /// Digital identification signal (called terminal capabilities).
    pub const DIS: u8 = 0x01;
    /// Called subscriber identification.
    pub const CSI: u8 = 0x02;
    /// Non-standard facilities.
    pub const NSF: u8 = 0x04;
    /// Digital transmit command (calling terminal wants to receive). Not `X`-maskable.
    pub const DTC: u8 = 0x81;
    /// Calling subscriber identification. Not `X`-maskable.
    pub const CIG: u8 = 0x82;
    /// Digital command signal.
    pub const DCS: u8 = 0x41;
    /// Transmitting subscriber identification.
    pub const TSI: u8 = 0x42;
    /// Sub-address.
    pub const SUB: u8 = 0x43;
    /// Non-standard facilities set-up.
    pub const NSS: u8 = 0x44;
    /// Sender identification.
    pub const SID: u8 = 0x45;
    /// Continue to correct (ECM).
    pub const CTC: u8 = 0x48;
    /// Confirmation to receive.
    pub const CFR: u8 = 0x21;
    /// Failure to train.
    pub const FTT: u8 = 0x22;
    /// Response for continue to correct (ECM).
    pub const CTR: u8 = 0x23;
    /// End of message.
    pub const EOM: u8 = 0x71;
    /// Multipage signal.
    pub const MPS: u8 = 0x72;
    /// End of retransmission (ECM).
    pub const EOR: u8 = 0x73;
    /// End of procedure.
    pub const EOP: u8 = 0x74;
    /// Receiver ready (ECM).
    pub const RR: u8 = 0x76;
    /// End of selection.
    pub const EOS: u8 = 0x78;
    /// Procedure interrupt, end of message.
    pub const PRI_EOM: u8 = 0x79;
    /// Procedure interrupt, multipage signal.
    pub const PRI_MPS: u8 = 0x7A;
    /// Procedure interrupt, end of procedure.
    pub const PRI_EOP: u8 = 0x7C;
    /// Partial page signal (ECM).
    pub const PPS: u8 = 0x7D;
    /// Message confirmation.
    pub const MCF: u8 = 0x31;
    /// Retrain negative.
    pub const RTN: u8 = 0x32;
    /// Retrain positive.
    pub const RTP: u8 = 0x33;
    /// Procedure interrupt negative.
    pub const PIN: u8 = 0x34;
    /// Procedure interrupt positive.
    pub const PIP: u8 = 0x35;
    /// Receive not ready.
    pub const RNR: u8 = 0x37;
    /// Response for end of retransmission (ECM).
    pub const ERR: u8 = 0x38;
    /// Partial page request (ECM).
    pub const PPR: u8 = 0x3D;
    /// Field not valid.
    pub const FNV: u8 = 0x53;
    /// Command repeat.
    pub const CRP: u8 = 0x58;
    /// Disconnect.
    pub const DCN: u8 = 0x5F;
    /// Facsimile coded data (ECM image frame; T.4 Annex A.3.5 "0110 0000").
    pub const FCD: u8 = 0x60;
    /// Return to control for partial page (ECM; T.4 Annex A.3.5 "0110 0001").
    pub const RCP: u8 = 0x61;
}

/// FCF2 values of PPS / EOR (T.30 Figure A.1 / A.2): the post-message command a partial page
/// signal announces. The first transmitted bit is the most significant one (octet convention of
/// [`crate::frame`]); FCF2 for a command other than NULL has its first bit set (`1111 0010` for
/// MPS).
pub mod fcf2 {
    /// NULL: a partial page boundary, more of the same page follows.
    pub const NULL: u8 = 0x00;
    /// EOM.
    pub const EOM: u8 = 0xF1;
    /// MPS.
    pub const MPS: u8 = 0xF2;
    /// EOP.
    pub const EOP: u8 = 0xF4;
    /// EOS.
    pub const EOS: u8 = 0xF8;
    /// PRI-EOM.
    pub const PRI_EOM: u8 = 0xF9;
    /// PRI-MPS.
    pub const PRI_MPS: u8 = 0xFA;
    /// PRI-EOP.
    pub const PRI_EOP: u8 = 0xFC;
}

/// An 8-bit counter or frame number as T.30 / T.4 Annex A transmit it: **least significant bit
/// first** (PPS page / block / frame counters, the FCD frame number), which in the octet
/// convention of this crate (first transmitted bit = most significant bit) is the bit reversed
/// value. The same function converts both ways.
pub fn lsb_first(value: u8) -> u8 {
    value.reverse_bits()
}

/// Why a byte string is not an HDLC frame of this crate's shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// Fewer than the three octets address, control and FCF.
    TooShort,
    /// The address field is not `0xFF`.
    BadAddress(u8),
    /// The control field is neither `0xC8` (final) nor `0xC0` (non-final).
    BadControl(u8),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::TooShort => write!(f, "frame shorter than address, control and FCF"),
            FrameError::BadAddress(a) => write!(f, "bad HDLC address {a:#04x}"),
            FrameError::BadControl(c) => write!(f, "bad HDLC control {c:#04x}"),
        }
    }
}

impl std::error::Error for FrameError {}

/// A borrowed, validated HDLC frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame<'a> {
    /// True for control `0xC8` (the last frame of a burst).
    pub final_frame: bool,
    /// The FCF octet exactly as received (including the `X` bit).
    pub raw_fcf: u8,
    /// The facsimile information field.
    pub fif: &'a [u8],
}

impl<'a> Frame<'a> {
    /// Validates the address and control octets of `bytes` (FCS already removed).
    ///
    /// # Errors
    /// [`FrameError`] for a short frame or a bad address / control field.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, FrameError> {
        let [addr, ctrl, raw_fcf, fif @ ..] = bytes else {
            return Err(FrameError::TooShort);
        };
        if *addr != ADDRESS {
            return Err(FrameError::BadAddress(*addr));
        }
        let final_frame = match *ctrl {
            CONTROL_FINAL => true,
            CONTROL_NON_FINAL => false,
            other => return Err(FrameError::BadControl(other)),
        };
        Ok(Self {
            final_frame,
            raw_fcf: *raw_fcf,
            fif,
        })
    }

    /// The FCF with the `X` bit cleared (see [`fcf`]). The DTC family `0x81..=0x88` has a fixed
    /// first bit and is returned unchanged.
    pub fn fcf(&self) -> u8 {
        if (0x81..=0x88).contains(&self.raw_fcf) {
            self.raw_fcf
        } else {
            self.raw_fcf & 0x7F
        }
    }

    /// The `X` bit of the FCF.
    pub fn x_bit(&self) -> bool {
        self.raw_fcf & 0x80 != 0
    }
}

/// Builds `FF control FCF fif...` with the `X` bit clear.
pub fn build(final_frame: bool, fcf: u8, fif: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(3 + fif.len());
    v.push(ADDRESS);
    v.push(if final_frame {
        CONTROL_FINAL
    } else {
        CONTROL_NON_FINAL
    });
    v.push(fcf);
    v.extend_from_slice(fif);
    v
}

/// Bit `n` (1-based, T.30 Table 2 numbering) of an information field. Bits beyond the end of
/// the field read as 0, which is also how T.30 treats a DIS / DCS that stops early.
pub fn fif_bit(fif: &[u8], n: usize) -> bool {
    if n == 0 {
        return false;
    }
    fif.get((n - 1) / 8)
        .is_some_and(|b| b & (0x80 >> ((n - 1) % 8)) != 0)
}

/// Sets bit `n` (1-based) of `fif`, growing it with zero octets as needed.
pub(crate) fn set_fif_bit(fif: &mut Vec<u8>, n: usize) {
    if n == 0 {
        return;
    }
    let idx = (n - 1) / 8;
    if fif.len() <= idx {
        fif.resize(idx + 1, 0);
    }
    if let Some(b) = fif.get_mut(idx) {
        *b |= 0x80 >> ((n - 1) % 8);
    }
}

/// Human readable name of a masked FCF, for log lines.
pub fn fcf_name(fcf: u8) -> &'static str {
    match fcf {
        fcf::DIS => "DIS",
        fcf::CSI => "CSI",
        fcf::NSF => "NSF",
        fcf::DTC => "DTC",
        fcf::CIG => "CIG",
        fcf::DCS => "DCS",
        fcf::TSI => "TSI",
        fcf::SUB => "SUB",
        fcf::NSS => "NSS",
        fcf::SID => "SID",
        fcf::CTC => "CTC",
        fcf::CFR => "CFR",
        fcf::FTT => "FTT",
        fcf::CTR => "CTR",
        fcf::EOM => "EOM",
        fcf::MPS => "MPS",
        fcf::EOR => "EOR",
        fcf::EOP => "EOP",
        fcf::RR => "RR",
        fcf::EOS => "EOS",
        fcf::PRI_EOM => "PRI-EOM",
        fcf::PRI_MPS => "PRI-MPS",
        fcf::PRI_EOP => "PRI-EOP",
        fcf::PPS => "PPS",
        fcf::MCF => "MCF",
        fcf::RTN => "RTN",
        fcf::RTP => "RTP",
        fcf::PIN => "PIN",
        fcf::PIP => "PIP",
        fcf::RNR => "RNR",
        fcf::ERR => "ERR",
        fcf::PPR => "PPR",
        fcf::FNV => "FNV",
        fcf::CRP => "CRP",
        fcf::DCN => "DCN",
        fcf::FCD => "FCD",
        fcf::RCP => "RCP",
        _ => "unknown",
    }
}

/// Length of the subscriber identification (CSI / TSI / CIG) information field.
pub const IDENT_LEN: usize = 20;

/// Encodes an identification string for a CSI / TSI frame: the characters in reverse order,
/// padded with spaces to 20 octets (T.30 5.3.6.2.4), each octet as it appears on the line in
/// the octet convention of this module, i.e. the ASCII code **bit reversed** (the digit `1` is
/// `0x8C`, a space `0x04`; confirmed against SpanDSP's frames). Only printable ASCII is kept;
/// anything else becomes a space. Longer strings keep their last 20 characters.
pub fn encode_ident(ident: &str) -> [u8; IDENT_LEN] {
    let clean: Vec<u8> = ident
        .bytes()
        .map(|b| if (0x20..=0x7E).contains(&b) { b } else { b' ' })
        .collect();
    let keep = clean.len().saturating_sub(IDENT_LEN);
    let mut out = [b' '.reverse_bits(); IDENT_LEN];
    for (slot, b) in out.iter_mut().zip(clean.iter().skip(keep).rev()) {
        *slot = b.reverse_bits();
    }
    out
}

/// Decodes the information field of a CSI / TSI / CIG frame. Reverses the order, drops
/// non-printable octets and trims surrounding spaces. Never fails: an empty or garbled field
/// yields an empty (or garbled) string.
pub fn decode_ident(fif: &[u8]) -> String {
    let take = &fif[..fif.len().min(IDENT_LEN)];
    let s: String = take
        .iter()
        .rev()
        .map(|&b| {
            let b = b.reverse_bits();
            if (0x20..=0x7E).contains(&b) {
                b as char
            } else {
                ' '
            }
        })
        .collect();
    s.trim().to_string()
}
