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
mod tests {
    use super::*;

    const V: [T38Version; 4] = [
        T38Version::V0,
        T38Version::V1,
        T38Version::V2,
        T38Version::V3,
    ];

    fn hdlc(data: &[u8]) -> Ifp {
        Ifp::Data {
            data_type: DataType::V21,
            fields: vec![
                Field::new(FieldType::HdlcData, data),
                Field::new(FieldType::HdlcFcsOkSigEnd, vec![]),
            ],
        }
    }

    #[test]
    fn indicator_bytes_match_asn1() {
        // dfp=0, choice=0, ext=0, value<<1: matches SpanDSP's `indicator << 1`.
        assert_eq!(
            Ifp::Indicator(Indicator::Cng)
                .encode(T38Version::V0)
                .unwrap(),
            [0x02]
        );
        assert_eq!(
            Ifp::Indicator(Indicator::Ced)
                .encode(T38Version::V3)
                .unwrap(),
            [0x04]
        );
        assert_eq!(
            Ifp::Indicator(Indicator::V21Preamble)
                .encode(T38Version::V3)
                .unwrap(),
            [0x06]
        );
        assert_eq!(
            Ifp::Indicator(Indicator::V17_14400LongTraining)
                .encode(T38Version::V3)
                .unwrap(),
            [0x1E]
        );
        // Addition: ext=1, small-number flag 0, 6 bits index => 0x20 | index>>2, index<<6.
        assert_eq!(
            Ifp::Indicator(Indicator::V8Ansam)
                .encode(T38Version::V3)
                .unwrap(),
            [0x20, 0x00]
        );
        assert_eq!(
            Ifp::Indicator(Indicator::V33_14400Training)
                .encode(T38Version::V3)
                .unwrap(),
            [0x21, 0x80]
        );
    }

    #[test]
    fn data_bytes_match_asn1() {
        // v21 + [hdlc-data(0xFF 0x03), hdlc-fcs-OK-sig-end(no data)], v3:
        // 0x80|0x40 (dfp,data) 0<<1 ; count 2 ; field1: 1,0,000 -> 0x80 ; len-1 = 0x0001 ; FF 03 ;
        // field2: 0,0,100 -> 0x20
        assert_eq!(
            hdlc(&[0xFF, 0x03]).encode(T38Version::V3).unwrap(),
            [0xC0, 0x02, 0x80, 0x00, 0x01, 0xFF, 0x03, 0x20]
        );
        // v0: field type has no extension bit: 1,000 -> 0x80 ; sig-end no-data: 0,100 -> 0x40.
        assert_eq!(
            hdlc(&[0xFF, 0x03]).encode(T38Version::V0).unwrap(),
            [0xC0, 0x02, 0x80, 0x00, 0x01, 0xFF, 0x03, 0x40]
        );
        // v0 packs two data-less fields into one octet (4 bits each).
        let two = Ifp::Data {
            data_type: DataType::V29_9600,
            fields: vec![
                Field::new(FieldType::HdlcSigEnd, vec![]),
                Field::new(FieldType::T4NonEcmSigEnd, vec![]),
            ],
        };
        assert_eq!(two.encode(T38Version::V0).unwrap(), [0xC8, 0x02, 0x17]);
        // v1+: 5 bits each => 0 0 001 | 0 0 111 packed -> 0b0000_1001, 0b1100_0000.
        assert_eq!(
            two.encode(T38Version::V3).unwrap(),
            [0xC8, 0x02, 0x09, 0xC0]
        );
        assert_eq!(
            Ifp::decode(&[0xC8, 0x02, 0x09, 0xC0], T38Version::V3).unwrap(),
            two
        );
        assert_eq!(
            Ifp::decode(&[0xC8, 0x02, 0x17], T38Version::V0).unwrap(),
            two
        );
    }

    #[test]
    fn field_type_additions_use_pure_per() {
        let f = |ft| Ifp::Data {
            data_type: DataType::V21,
            fields: vec![Field::new(ft, vec![0xAA])],
        };
        // present=1, ext=1, flag=0, index 0 in 5+1 bits, padded; then len-1 = 0, data.
        assert_eq!(
            f(FieldType::CmMessage).encode(T38Version::V3).unwrap(),
            [0xC0, 0x01, 0xC0, 0x00, 0x00, 0x00, 0xAA]
        );
        // index 1 -> bit0 of the 6-bit number lands in the MSB of the next octet.
        assert_eq!(
            f(FieldType::JmMessage).encode(T38Version::V3).unwrap(),
            [0xC0, 0x01, 0xC0, 0x80, 0x00, 0x00, 0xAA]
        );
        // index 2 -> bit 1 of the 6-bit number is the last bit of the first octet.
        assert_eq!(
            f(FieldType::CiMessage).encode(T38Version::V3).unwrap(),
            [0xC0, 0x01, 0xC1, 0x00, 0x00, 0x00, 0xAA]
        );
    }

    #[test]
    fn roundtrip_every_combination() {
        for v in V {
            for ind in Indicator::ALL {
                let pkt = Ifp::Indicator(*ind);
                match pkt.encode(v) {
                    Ok(b) => assert_eq!(Ifp::decode(&b, v).unwrap(), pkt, "{ind:?} {v:?}"),
                    Err(e) => {
                        assert!(
                            ind.is_addition() && !v.allows_additions(),
                            "{ind:?} {v:?} {e}"
                        )
                    }
                }
            }
            for dt in DataType::ALL {
                for ft in FieldType::ALL {
                    for payload in [vec![], vec![1u8, 2, 3]] {
                        let pkt = Ifp::Data {
                            data_type: *dt,
                            fields: vec![
                                Field::new(*ft, payload.clone()),
                                Field::new(FieldType::HdlcSigEnd, vec![]),
                                Field::new(*ft, vec![9]),
                            ],
                        };
                        let allowed =
                            v.allows_additions() || !(dt.is_addition() || ft.is_addition());
                        match pkt.encode(v) {
                            Ok(b) => {
                                assert!(allowed, "{dt:?} {ft:?} {v:?} should be rejected");
                                assert_eq!(Ifp::decode(&b, v).unwrap(), pkt);
                            }
                            Err(e) => {
                                assert!(!allowed, "{dt:?} {ft:?} {v:?}: {e}");
                                assert!(matches!(e, IfpError::NotInVersion(_)));
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn v0_decoder_rejects_additions() {
        // v8-ansam on the wire, decoded as v0 / v1.
        for v in [T38Version::V0, T38Version::V1] {
            assert!(matches!(
                Ifp::decode(&[0x20, 0x00], v),
                Err(IfpError::NotInVersion(_))
            ));
            // t30-data v8 addition.
            assert!(matches!(
                Ifp::decode(&[0x60, 0x00], v),
                Err(IfpError::NotInVersion(_))
            ));
        }
        assert_eq!(
            Ifp::decode(&[0x20, 0x00], T38Version::V2).unwrap(),
            Ifp::Indicator(Indicator::V8Ansam)
        );
        // field-type addition under v1 (extension bit exists but additions are v2+).
        let cm = [0xC0, 0x01, 0xC0, 0x00, 0x00, 0x00, 0xAA];
        assert!(matches!(
            Ifp::decode(&cm, T38Version::V1),
            Err(IfpError::NotInVersion(_))
        ));
        // Under v0 the same octets parse as 3-bit field types and fail differently.
        assert!(Ifp::decode(&cm, T38Version::V0).is_err());
    }

    #[test]
    fn malformed_inputs() {
        use IfpError::*;
        let v = T38Version::V3;
        assert!(matches!(Ifp::decode(&[], v), Err(Per(PerError::Truncated))));
        // indicator with data-field bit
        assert_eq!(Ifp::decode(&[0x82], v), Err(DataFieldWithIndicator));
        // t30-data root value 9 is undefined (root has 9 values: 0..=8)
        assert!(matches!(
            Ifp::decode(&[0x40 | (9 << 1)], v),
            Err(UndefinedRoot { .. })
        ));
        // unknown addition index 63 and the >=64 form
        assert!(matches!(
            Ifp::decode(&[0x23, 0xC0], v),
            Err(UnknownExtension { .. })
        ));
        assert!(matches!(
            Ifp::decode(&[0x30, 0x00], v),
            Err(UnknownExtension { .. })
        ));
        // trailing octet
        assert_eq!(Ifp::decode(&[0x02, 0x00], v), Err(TrailingData(1)));
        assert_eq!(
            Ifp::decode_padded(&[0x02, 0x00, 0x00], v).unwrap(),
            Ifp::Indicator(Indicator::Cng)
        );
        assert_eq!(
            Ifp::decode_padded(&[0x02, 0x00, 0x01], v),
            Err(TrailingData(2))
        );
        // count larger than the input
        assert!(matches!(
            Ifp::decode(&[0xC0, 0x7F, 0x80], v),
            Err(Per(PerError::Truncated))
        ));
        // field data truncated
        assert!(matches!(
            Ifp::decode(&[0xC0, 0x01, 0x80, 0x00, 0x05, 0xFF], v),
            Err(Per(PerError::Truncated))
        ));
        // invalid fragment header
        assert!(matches!(
            Ifp::decode(&[0xC0, 0xC9], v),
            Err(Per(PerError::InvalidFragment(0xC9)))
        ));
        // data with no data field decodes to an empty field list
        assert_eq!(
            Ifp::decode(&[0x42], v).unwrap(),
            Ifp::Data {
                data_type: DataType::V27ter2400,
                fields: vec![]
            }
        );
    }

    #[test]
    fn field_too_long_is_rejected() {
        let pkt = Ifp::Data {
            data_type: DataType::V21,
            fields: vec![Field::new(FieldType::HdlcData, vec![0u8; 65536])],
        };
        assert_eq!(
            pkt.encode(T38Version::V3),
            Err(IfpError::FieldTooLong(65536))
        );
        let ok = Ifp::Data {
            data_type: DataType::V21,
            fields: vec![Field::new(FieldType::HdlcData, vec![7u8; 65535])],
        };
        let b = ok.encode(T38Version::V3).unwrap();
        assert_eq!(&b[..5], [0xC0, 0x01, 0x80, 0xFF, 0xFE]);
        assert_eq!(Ifp::decode(&b, T38Version::V3).unwrap(), ok);
    }

    /// Builds a packet with `n` data-less fields.
    fn many(n: usize) -> Ifp {
        Ifp::Data {
            data_type: DataType::V29_9600,
            fields: (0..n)
                .map(|_| Field::new(FieldType::HdlcSigEnd, vec![]))
                .collect(),
        }
    }

    #[test]
    fn i05_length_determinant_boundaries() {
        for v in [T38Version::V0, T38Version::V3] {
            for (n, header) in [
                (1usize, vec![0x01u8]),
                (127, vec![0x7F]),
                (128, vec![0x80, 0x80]),
                (16383, vec![0xBF, 0xFF]),
                (16384, vec![0xC1]),
                (16385, vec![0xC1]),
                (65536, vec![0xC4]),
            ] {
                let pkt = many(n);
                let b = pkt.encode(v).unwrap();
                assert_eq!(&b[1..1 + header.len()], &header[..], "count {n} {v:?}");
                assert_eq!(Ifp::decode(&b, v).unwrap(), pkt, "count {n} {v:?}");
            }
        }
        // Exact multiple of 16384 ends with an empty chunk.
        let b = many(16384).encode(T38Version::V3).unwrap();
        let bits: usize = 16384 * 5;
        assert_eq!(b.len(), 1 + 1 + bits.div_ceil(8) + 1);
        assert_eq!(b.last(), Some(&0x00));
    }

    #[test]
    fn decode_prefix_reports_used_length() {
        let enc = hdlc(&[1, 2, 3]).encode(T38Version::V3).unwrap();
        let mut padded = enc.clone();
        padded.extend_from_slice(&[0, 0]);
        let (ifp, used) = Ifp::decode_prefix(&padded, T38Version::V3).unwrap();
        assert_eq!(used, enc.len());
        assert_eq!(ifp, hdlc(&[1, 2, 3]));
    }

    #[test]
    fn version_helpers() {
        for n in 0..4 {
            assert_eq!(T38Version::from_number(n).unwrap().number(), n);
        }
        assert_eq!(T38Version::from_number(4), None);
        assert!(!T38Version::V0.has_field_type_extension_bit());
        assert!(T38Version::V1.has_field_type_extension_bit());
        assert!(!T38Version::V1.allows_additions());
        assert!(T38Version::V2.allows_additions());
    }
}
