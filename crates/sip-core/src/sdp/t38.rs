//! T.38 (`m=image <port> udptl t38`) attribute model and answer negotiation (ITU-T T.38
//! Annex D). This is the only fax-specific knowledge in the crate and it is part of SDP.

use super::{Attribute, MediaDescription, SdpError, invalid};

/// `T38FaxRateManagement`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateManagement {
    /// `localTCF`: the gateway generates TCF locally.
    LocalTcf,
    /// `transferredTCF`: TCF is transferred end to end.
    TransferredTcf,
}

impl RateManagement {
    fn as_str(self) -> &'static str {
        match self {
            RateManagement::LocalTcf => "localTCF",
            RateManagement::TransferredTcf => "transferredTCF",
        }
    }
}

/// `T38FaxUdpEC`: UDPTL error correction scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UdpEc {
    /// `t38UDPFEC`
    Fec,
    /// `t38UDPRedundancy`
    Redundancy,
    /// `t38UDPNoEC`
    NoEc,
}

impl UdpEc {
    fn as_str(self) -> &'static str {
        match self {
            UdpEc::Fec => "t38UDPFEC",
            UdpEc::Redundancy => "t38UDPRedundancy",
            UdpEc::NoEc => "t38UDPNoEC",
        }
    }
}

/// Default values applied when an offer omits an optional attribute. Where T.38 leaves the
/// default open these are the stack's choices (documented in `docs/test-reports/M1.md`).
pub mod defaults {
    use super::{RateManagement, UdpEc};
    /// `T38FaxVersion` default (T.38 Annex D: version 0).
    pub const VERSION: u8 = 0;
    /// `T38MaxBitRate` default.
    pub const MAX_BIT_RATE: u32 = 14_400;
    /// `T38FaxRateManagement` default.
    pub const RATE_MANAGEMENT: RateManagement = RateManagement::TransferredTcf;
    /// `T38FaxMaxBuffer` default.
    pub const MAX_BUFFER: u32 = 1_800;
    /// `T38FaxMaxDatagram` default.
    pub const MAX_DATAGRAM: u32 = 260;
    /// `T38FaxUdpEC` default when absent: redundancy (the T.38 baseline).
    pub const UDP_EC: UdpEc = UdpEc::Redundancy;
}

/// The T.38 attributes of one `m=image` section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct T38Params {
    /// `T38FaxVersion` (0..=3 in practice).
    pub version: u8,
    /// `T38MaxBitRate` in bit/s.
    pub max_bit_rate: u32,
    /// `T38FaxRateManagement`.
    pub rate_management: RateManagement,
    /// `T38FaxMaxBuffer`.
    pub max_buffer: u32,
    /// `T38FaxMaxDatagram`.
    pub max_datagram: u32,
    /// `T38FaxUdpEC`.
    pub udp_ec: UdpEc,
    /// `T38FaxFillBitRemoval` supported.
    pub fill_bit_removal: bool,
    /// `T38FaxTranscodingMMR` supported.
    pub transcoding_mmr: bool,
    /// `T38FaxTranscodingJBIG` supported.
    pub transcoding_jbig: bool,
    /// `T38VendorInfo` raw value.
    pub vendor_info: Option<String>,
}

impl Default for T38Params {
    fn default() -> Self {
        T38Params {
            version: defaults::VERSION,
            max_bit_rate: defaults::MAX_BIT_RATE,
            rate_management: defaults::RATE_MANAGEMENT,
            max_buffer: defaults::MAX_BUFFER,
            max_datagram: defaults::MAX_DATAGRAM,
            udp_ec: defaults::UDP_EC,
            fill_bit_removal: false,
            transcoding_mmr: false,
            transcoding_jbig: false,
            vendor_info: None,
        }
    }
}

fn num<T: std::str::FromStr>(a: &Attribute, field: &'static str) -> Result<T, SdpError> {
    a.value
        .as_deref()
        .map(str::trim)
        .and_then(|v| v.parse::<T>().ok())
        .ok_or_else(|| invalid(field, format!("bad numeric value {:?}", a.value)))
}

/// Boolean attribute: bare name or `:1` means true, `:0` means false.
fn flag(a: &Attribute, field: &'static str) -> Result<bool, SdpError> {
    match a.value.as_deref().map(str::trim) {
        None | Some("") | Some("1") => Ok(true),
        Some("0") => Ok(false),
        Some(other) => Err(invalid(field, format!("bad boolean value {other:?}"))),
    }
}

impl T38Params {
    /// Reads the T.38 attributes of a media section. Attribute names are matched
    /// case-insensitively; missing optional attributes get the [`defaults`].
    pub fn from_media(m: &MediaDescription) -> Result<Self, SdpError> {
        let mut p = T38Params::default();
        for a in &m.attributes {
            let name = a.name.to_ascii_lowercase();
            match name.as_str() {
                "t38faxversion" => p.version = num(a, "T38FaxVersion")?,
                "t38maxbitrate" => p.max_bit_rate = num(a, "T38MaxBitRate")?,
                "t38faxratemanagement" => {
                    p.rate_management = match a
                        .value
                        .as_deref()
                        .map(|v| v.trim().to_ascii_lowercase())
                        .as_deref()
                    {
                        Some("localtcf") => RateManagement::LocalTcf,
                        Some("transferredtcf") => RateManagement::TransferredTcf,
                        _ => return Err(invalid("T38FaxRateManagement", format!("{:?}", a.value))),
                    }
                }
                "t38faxmaxbuffer" => p.max_buffer = num(a, "T38FaxMaxBuffer")?,
                "t38faxmaxdatagram" => p.max_datagram = num(a, "T38FaxMaxDatagram")?,
                "t38faxudpec" => {
                    p.udp_ec = match a
                        .value
                        .as_deref()
                        .map(|v| v.trim().to_ascii_lowercase())
                        .as_deref()
                    {
                        Some("t38udpfec") => UdpEc::Fec,
                        Some("t38udpredundancy") => UdpEc::Redundancy,
                        Some("t38udpnoec") => UdpEc::NoEc,
                        _ => return Err(invalid("T38FaxUdpEC", format!("{:?}", a.value))),
                    }
                }
                "t38faxfillbitremoval" => p.fill_bit_removal = flag(a, "T38FaxFillBitRemoval")?,
                "t38faxtranscodingmmr" => p.transcoding_mmr = flag(a, "T38FaxTranscodingMMR")?,
                "t38faxtranscodingjbig" => p.transcoding_jbig = flag(a, "T38FaxTranscodingJBIG")?,
                "t38vendorinfo" => p.vendor_info = a.value.clone(),
                _ => {}
            }
        }
        Ok(p)
    }

    /// The attributes to place in an `m=image` section, in a fixed order. Boolean attributes are
    /// written as a bare name when true and omitted when false.
    pub fn to_attributes(&self) -> Vec<Attribute> {
        let mut v = vec![
            Attribute::value("T38FaxVersion", self.version.to_string()),
            Attribute::value("T38MaxBitRate", self.max_bit_rate.to_string()),
        ];
        if self.fill_bit_removal {
            v.push(Attribute::flag("T38FaxFillBitRemoval"));
        }
        if self.transcoding_mmr {
            v.push(Attribute::flag("T38FaxTranscodingMMR"));
        }
        if self.transcoding_jbig {
            v.push(Attribute::flag("T38FaxTranscodingJBIG"));
        }
        v.push(Attribute::value(
            "T38FaxRateManagement",
            self.rate_management.as_str(),
        ));
        v.push(Attribute::value(
            "T38FaxMaxBuffer",
            self.max_buffer.to_string(),
        ));
        v.push(Attribute::value(
            "T38FaxMaxDatagram",
            self.max_datagram.to_string(),
        ));
        v.push(Attribute::value("T38FaxUdpEC", self.udp_ec.as_str()));
        if let Some(info) = &self.vendor_info {
            v.push(Attribute::value("T38VendorInfo", info.clone()));
        }
        v
    }

    /// Computes the answer parameters for this *offer* (`self`) given local capabilities:
    ///
    /// * version = min(offer, local), max bit rate = min(offer, local);
    /// * rate management copied from the offer;
    /// * UDP EC: an offered FEC falls back to redundancy if we do not support FEC, otherwise
    ///   the offered scheme is kept;
    /// * max datagram / max buffer = our values;
    /// * boolean capabilities = intersection.
    pub fn answer(&self, local: &T38Capabilities) -> T38Params {
        T38Params {
            version: self.version.min(local.max_version),
            max_bit_rate: self.max_bit_rate.min(local.max_bit_rate),
            rate_management: self.rate_management,
            max_buffer: local.max_buffer,
            max_datagram: local.max_datagram,
            udp_ec: match self.udp_ec {
                UdpEc::Fec if !local.fec => UdpEc::Redundancy,
                other => other,
            },
            fill_bit_removal: self.fill_bit_removal && local.fill_bit_removal,
            transcoding_mmr: self.transcoding_mmr && local.transcoding_mmr,
            transcoding_jbig: self.transcoding_jbig && local.transcoding_jbig,
            vendor_info: None,
        }
    }
}

/// What the local side supports when answering a T.38 offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct T38Capabilities {
    /// Highest T.38 version supported.
    pub max_version: u8,
    /// Highest bit rate supported.
    pub max_bit_rate: u32,
    /// UDPTL FEC supported (redundancy and no-EC are always supported).
    pub fec: bool,
    /// Value advertised as `T38FaxMaxBuffer`.
    pub max_buffer: u32,
    /// Value advertised as `T38FaxMaxDatagram`.
    pub max_datagram: u32,
    /// Fill bit removal supported.
    pub fill_bit_removal: bool,
    /// MMR transcoding supported.
    pub transcoding_mmr: bool,
    /// JBIG transcoding supported.
    pub transcoding_jbig: bool,
}

impl Default for T38Capabilities {
    fn default() -> Self {
        T38Capabilities {
            max_version: 0,
            max_bit_rate: 14_400,
            fec: false,
            max_buffer: defaults::MAX_BUFFER,
            max_datagram: defaults::MAX_DATAGRAM,
            fill_bit_removal: false,
            transcoding_mmr: false,
            transcoding_jbig: false,
        }
    }
}
