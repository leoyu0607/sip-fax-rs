//! IFP packets (ITU-T T.38 clause 7 and the ASN.1 annex) and their ALIGNED PER codec.
//!
//! The relevant ASN.1 (module `T38(2002)`, AUTOMATIC TAGS):
//!
//! ```text
//! IFPPacket ::= SEQUENCE { type-of-msg Type-of-msg, data-field Data-Field OPTIONAL }
//! Type-of-msg ::= CHOICE { t30-indicator ENUMERATED {..16 root.., ..., ..7 additions..},
//!                          t30-data      ENUMERATED {..9 root..,  ..., ..6 additions..} }
//! Data-Field ::= SEQUENCE OF SEQUENCE {
//!     field-type  ENUMERATED {..8 root.., ..., cm-message, jm-message, ci-message, v34rate},
//!     field-data  OCTET STRING (SIZE (1..65535)) OPTIONAL }
//! ```
//!
//! Resulting bit layout (aligned PER):
//!
//! * octet 0: `data-field-present`, `type-of-msg` choice bit, then the enumeration (extension
//!   bit, 4 bits for a root value, or the 6-bit "normally small" index for an addition, which
//!   makes the type 2 octets long).
//! * if a data field is present: an unconstrained length determinant (octet aligned) with the
//!   number of fields, each field being `field-data-present`, the field-type enumeration and,
//!   if present, a 16-bit `length - 1` followed by the octets.
//!
//! T.38 version 0 corresponds to the 1998 ASN.1 in which the `field-type` enumeration was not
//! extensible (3 bits, no extension bit). Version 1 and later use the extensible form; the
//! additions (V.8, V.34, V.33 indicators and data types, cm/jm/ci/v34rate fields) exist from
//! version 2 on.

use crate::per::{BitReader, BitWriter, PerError, plan_fragments};
use thiserror::Error;

/// A negotiated T.38 version (the `T38FaxVersion` SDP attribute).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum T38Version {
    /// Version 0 (1998 ASN.1: `field-type` is not extensible).
    V0,
    /// Version 1 (extensible `field-type`, root values only).
    V1,
    /// Version 2 (adds V.8 / V.34 indicators, data types and field types).
    V2,
    /// Version 3.
    V3,
}

impl T38Version {
    /// Maps the numeric `T38FaxVersion` to a version; `None` for anything above 3.
    pub fn from_number(n: u32) -> Option<Self> {
        match n {
            0 => Some(Self::V0),
            1 => Some(Self::V1),
            2 => Some(Self::V2),
            3 => Some(Self::V3),
            _ => None,
        }
    }

    /// The numeric `T38FaxVersion`.
    pub fn number(self) -> u32 {
        match self {
            Self::V0 => 0,
            Self::V1 => 1,
            Self::V2 => 2,
            Self::V3 => 3,
        }
    }

    /// True if `field-type` carries an extension bit (version 1 and later).
    pub fn has_field_type_extension_bit(self) -> bool {
        self >= Self::V1
    }

    /// True if the extension additions (V.8, V.34, cm/jm/ci/v34rate, V.33) may be used.
    pub fn allows_additions(self) -> bool {
        self >= Self::V2
    }
}

/// Errors from IFP encoding and decoding.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum IfpError {
    /// The packet ended early or contains an illegal length determinant.
    #[error("malformed PER: {0}")]
    Per(#[from] PerError),
    /// The value is an extension addition that the T.38 version does not have.
    #[error("{0} requires T.38 version 2 or later")]
    NotInVersion(&'static str),
    /// An extension addition index outside the table of this crate / the ASN.1.
    #[error("unknown {kind} extension index {index}")]
    UnknownExtension {
        /// Which enumeration.
        kind: &'static str,
        /// The addition index found on the wire.
        index: u32,
    },
    /// A root enumeration value that the ASN.1 does not define (e.g. t30-data value 12).
    #[error("undefined {kind} root value {value}")]
    UndefinedRoot {
        /// Which enumeration.
        kind: &'static str,
        /// The value found on the wire.
        value: u32,
    },
    /// `field-data` is longer than 65535 octets.
    #[error("field-data of {0} octets exceeds 65535")]
    FieldTooLong(usize),
    /// A `t30-indicator` message that also carries a data field.
    #[error("indicator packet carries a data field")]
    DataFieldWithIndicator,
    /// Octets remain after the packet.
    #[error("{0} trailing octet(s) after the IFP packet")]
    TrailingData(usize),
}

/// Writes an enumeration with `root_bits` bits for the root and an extension marker.
fn write_enum(w: &mut BitWriter, ext: bool, index: u8, root_bits: u8) {
    if ext {
        w.write_bit(true);
        // Normally small non-negative whole number: 0 + 6 bits (additions are few).
        w.write_bit(false);
        w.write_bits(u32::from(index), 6);
    } else {
        w.write_bit(false);
        w.write_bits(u32::from(index), root_bits);
    }
}

/// Reads an enumeration written by [`write_enum`]; returns `(is_addition, index)`.
fn read_enum(
    r: &mut BitReader<'_>,
    root_bits: u8,
    kind: &'static str,
) -> Result<(bool, u8), IfpError> {
    if r.read_bit()? {
        // Normally small non-negative whole number.
        if r.read_bit()? {
            // 64 or more: length-prefixed form. No such addition exists.
            return Err(IfpError::UnknownExtension { kind, index: 64 });
        }
        let idx = r.read_bits(6)?;
        Ok((true, idx as u8))
    } else {
        Ok((false, r.read_bits(root_bits)? as u8))
    }
}

macro_rules! ifp_enum {
    (
        $(#[$meta:meta])*
        $name:ident, $kind:literal, root_bits = $rb:literal,
        root { $($rvar:ident = $rdoc:literal),+ $(,)? },
        ext { $($evar:ident = $edoc:literal),+ $(,)? }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name {
            $(#[doc = $rdoc] $rvar,)+
            $(#[doc = $edoc] $evar,)+
        }

        impl $name {
            /// Every value, root values first, in wire-index order.
            pub const ALL: &'static [$name] = &[$($name::$rvar,)+ $($name::$evar,)+];
            const ROOT: &'static [$name] = &[$($name::$rvar,)+];
            const ADDITIONS: &'static [$name] = &[$($name::$evar,)+];

            /// True if the value is an extension addition (needs T.38 version 2 or later).
            pub fn is_addition(self) -> bool {
                Self::ADDITIONS.contains(&self)
            }

            /// `(is_addition, index)` as used on the wire: index within the root, or within
            /// the additions.
            pub fn wire_index(self) -> (bool, u8) {
                if let Some(i) = Self::ROOT.iter().position(|v| *v == self) {
                    (false, i as u8)
                } else {
                    let i = Self::ADDITIONS.iter().position(|v| *v == self).unwrap_or(0);
                    (true, i as u8)
                }
            }

            fn from_wire(ext: bool, idx: u8) -> Result<Self, IfpError> {
                if ext {
                    Self::ADDITIONS.get(usize::from(idx)).copied().ok_or(
                        IfpError::UnknownExtension { kind: $kind, index: u32::from(idx) },
                    )
                } else {
                    Self::ROOT.get(usize::from(idx)).copied().ok_or(
                        IfpError::UndefinedRoot { kind: $kind, value: u32::from(idx) },
                    )
                }
            }

            #[allow(dead_code)]
            const ROOT_BITS: u8 = $rb;
        }
    };
}

ifp_enum!(
    /// `t30-indicator`: a signal the sender is about to start (or stop) sending.
    Indicator, "t30-indicator", root_bits = 4,
    root {
        NoSignal = "no-signal",
        Cng = "cng",
        Ced = "ced",
        V21Preamble = "v21-preamble",
        V27ter2400Training = "v27-2400-training",
        V27ter4800Training = "v27-4800-training",
        V29_7200Training = "v29-7200-training",
        V29_9600Training = "v29-9600-training",
        V17_7200ShortTraining = "v17-7200-short-training",
        V17_7200LongTraining = "v17-7200-long-training",
        V17_9600ShortTraining = "v17-9600-short-training",
        V17_9600LongTraining = "v17-9600-long-training",
        V17_12000ShortTraining = "v17-12000-short-training",
        V17_12000LongTraining = "v17-12000-long-training",
        V17_14400ShortTraining = "v17-14400-short-training",
        V17_14400LongTraining = "v17-14400-long-training",
    },
    ext {
        V8Ansam = "v8-ansam",
        V8Signal = "v8-signal",
        V34CntlChannel1200 = "v34-cntl-channel-1200",
        V34PriChannel = "v34-pri-channel",
        V34CcRetrain = "v34-CC-retrain",
        V33_12000Training = "v33-12000-training",
        V33_14400Training = "v33-14400-training",
    }
);

ifp_enum!(
    /// `t30-data`: the modulation of the data carried in the fields.
    DataType, "t30-data", root_bits = 4,
    root {
        V21 = "v21",
        V27ter2400 = "v27-2400",
        V27ter4800 = "v27-4800",
        V29_7200 = "v29-7200",
        V29_9600 = "v29-9600",
        V17_7200 = "v17-7200",
        V17_9600 = "v17-9600",
        V17_12000 = "v17-12000",
        V17_14400 = "v17-14400",
    },
    ext {
        V8 = "v8",
        V34PriRate = "v34-pri-rate",
        V34Cc1200 = "v34-CC-1200",
        V34PriCh = "v34-pri-ch",
        V33_12000 = "v33-12000",
        V33_14400 = "v33-14400",
    }
);

ifp_enum!(
    /// `field-type`: what the octets of a data field are.
    FieldType, "field-type", root_bits = 3,
    root {
        HdlcData = "hdlc-data",
        HdlcSigEnd = "hdlc-sig-end",
        HdlcFcsOk = "hdlc-fcs-OK",
        HdlcFcsBad = "hdlc-fcs-BAD",
        HdlcFcsOkSigEnd = "hdlc-fcs-OK-sig-end",
        HdlcFcsBadSigEnd = "hdlc-fcs-BAD-sig-end",
        T4NonEcmData = "t4-non-ecm-data",
        T4NonEcmSigEnd = "t4-non-ecm-sig-end",
    },
    ext {
        CmMessage = "cm-message",
        JmMessage = "jm-message",
        CiMessage = "ci-message",
        V34Rate = "v34rate",
    }
);

/// One element of the IFP data field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// What the octets are.
    pub field_type: FieldType,
    /// `field-data`; empty means the OPTIONAL component is absent. At most 65535 octets.
    pub data: Vec<u8>,
}

impl Field {
    /// Convenience constructor.
    pub fn new(field_type: FieldType, data: impl Into<Vec<u8>>) -> Self {
        Self {
            field_type,
            data: data.into(),
        }
    }
}

/// An IFP packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ifp {
    /// `t30-indicator` (never has a data field).
    Indicator(Indicator),
    /// `t30-data`; an empty `fields` list means the OPTIONAL data field is absent.
    Data {
        /// Modulation.
        data_type: DataType,
        /// The data field elements.
        fields: Vec<Field>,
    },
}

impl Ifp {
    /// Encodes the packet for `version`.
    ///
    /// # Errors
    /// [`IfpError::NotInVersion`] for an addition that `version` does not have and
    /// [`IfpError::FieldTooLong`] for more than 65535 octets of field data.
    pub fn encode(&self, version: T38Version) -> Result<Vec<u8>, IfpError> {
        let mut w = BitWriter::new();
        match self {
            Ifp::Indicator(ind) => {
                if ind.is_addition() && !version.allows_additions() {
                    return Err(IfpError::NotInVersion("indicator addition"));
                }
                w.write_bit(false); // no data field
                w.write_bit(false); // t30-indicator
                let (ext, idx) = ind.wire_index();
                write_enum(&mut w, ext, idx, Indicator::ROOT_BITS);
            }
            Ifp::Data { data_type, fields } => {
                if data_type.is_addition() && !version.allows_additions() {
                    return Err(IfpError::NotInVersion("data type addition"));
                }
                w.write_bit(!fields.is_empty());
                w.write_bit(true); // t30-data
                let (ext, idx) = data_type.wire_index();
                write_enum(&mut w, ext, idx, DataType::ROOT_BITS);
                if !fields.is_empty() {
                    encode_fields(&mut w, fields, version)?;
                }
            }
        }
        Ok(w.into_bytes())
    }

    /// Decodes exactly one packet; any trailing octet is an error.
    ///
    /// # Errors
    /// Any [`IfpError`]; never panics.
    pub fn decode(buf: &[u8], version: T38Version) -> Result<Self, IfpError> {
        let (ifp, used) = Self::decode_prefix(buf, version)?;
        if used != buf.len() {
            return Err(IfpError::TrailingData(buf.len() - used));
        }
        Ok(ifp)
    }

    /// Like [`Ifp::decode`] but tolerates trailing zero octets. A packet recovered from
    /// parity FEC has the length of the longest protected packet, so shorter packets come back
    /// zero padded (the receiver cannot know the original length).
    ///
    /// # Errors
    /// Any [`IfpError`], including [`IfpError::TrailingData`] for non-zero trailing octets.
    pub fn decode_padded(buf: &[u8], version: T38Version) -> Result<Self, IfpError> {
        let (ifp, used) = Self::decode_prefix(buf, version)?;
        if !buf.get(used..).unwrap_or(&[]).iter().all(|&b| b == 0) {
            return Err(IfpError::TrailingData(buf.len() - used));
        }
        Ok(ifp)
    }

    /// Decodes one packet from the start of `buf`, returning it with the number of octets used.
    ///
    /// # Errors
    /// Any [`IfpError`] except [`IfpError::TrailingData`].
    pub fn decode_prefix(buf: &[u8], version: T38Version) -> Result<(Self, usize), IfpError> {
        let mut r = BitReader::new(buf);
        let data_field_present = r.read_bit()?;
        let is_data = r.read_bit()?;
        if !is_data {
            let (ext, idx) = read_enum(&mut r, Indicator::ROOT_BITS, "t30-indicator")?;
            let ind = Indicator::from_wire(ext, idx)?;
            if ind.is_addition() && !version.allows_additions() {
                return Err(IfpError::NotInVersion("indicator addition"));
            }
            if data_field_present {
                return Err(IfpError::DataFieldWithIndicator);
            }
            return Ok((Ifp::Indicator(ind), r.consumed_bytes()));
        }
        let (ext, idx) = read_enum(&mut r, DataType::ROOT_BITS, "t30-data")?;
        let data_type = DataType::from_wire(ext, idx)?;
        if data_type.is_addition() && !version.allows_additions() {
            return Err(IfpError::NotInVersion("data type addition"));
        }
        let fields = if data_field_present {
            decode_fields(&mut r, version)?
        } else {
            Vec::new()
        };
        Ok((Ifp::Data { data_type, fields }, r.consumed_bytes()))
    }
}

fn encode_fields(w: &mut BitWriter, fields: &[Field], version: T38Version) -> Result<(), IfpError> {
    let mut idx = 0usize;
    for chunk in plan_fragments(fields.len()) {
        w.write_length_chunk(chunk);
        for f in fields.get(idx..idx + chunk).unwrap_or(&[]) {
            encode_field(w, f, version)?;
        }
        idx += chunk;
    }
    Ok(())
}

fn encode_field(w: &mut BitWriter, f: &Field, version: T38Version) -> Result<(), IfpError> {
    if f.data.len() > usize::from(u16::MAX) {
        return Err(IfpError::FieldTooLong(f.data.len()));
    }
    if f.field_type.is_addition() && !version.allows_additions() {
        return Err(IfpError::NotInVersion("field type addition"));
    }
    w.write_bit(!f.data.is_empty());
    let (ext, idx) = f.field_type.wire_index();
    if version.has_field_type_extension_bit() {
        write_enum(w, ext, idx, FieldType::ROOT_BITS);
    } else {
        // 1998 ASN.1: plain 3-bit enumeration without extension marker.
        w.write_bits(u32::from(idx), FieldType::ROOT_BITS);
    }
    if !f.data.is_empty() {
        w.write_u16((f.data.len() - 1) as u16);
        w.write_bytes(&f.data);
    }
    Ok(())
}

fn decode_fields(r: &mut BitReader<'_>, version: T38Version) -> Result<Vec<Field>, IfpError> {
    // The smallest possible field is 4 bits (v0, no data) so a count that cannot fit in the
    // remaining input is rejected before any allocation.
    let min_bits = if version.has_field_type_extension_bit() {
        5
    } else {
        4
    };
    let mut out = Vec::new();
    loop {
        let (count, more) = r.read_length_chunk()?;
        if count.saturating_mul(min_bits) > r.remaining_bits() {
            return Err(PerError::Truncated.into());
        }
        for _ in 0..count {
            out.push(decode_field(r, version)?);
        }
        if !more {
            return Ok(out);
        }
    }
}

fn decode_field(r: &mut BitReader<'_>, version: T38Version) -> Result<Field, IfpError> {
    let has_data = r.read_bit()?;
    let field_type = if version.has_field_type_extension_bit() {
        let (ext, idx) = read_enum(r, FieldType::ROOT_BITS, "field-type")?;
        let ft = FieldType::from_wire(ext, idx)?;
        if ft.is_addition() && !version.allows_additions() {
            return Err(IfpError::NotInVersion("field type addition"));
        }
        ft
    } else {
        FieldType::from_wire(false, r.read_bits(FieldType::ROOT_BITS)? as u8)?
    };
    let data = if has_data {
        let len = usize::from(r.read_u16()?) + 1;
        r.read_bytes(len)?.to_vec()
    } else {
        Vec::new()
    };
    Ok(Field { field_type, data })
}

#[cfg(test)]
mod tests;
