//! Configuration model of the fax-in call controller (docs/design/sip-core.md section 9).
//!
//! The file is TOML. `[[trunks]]` entries start from a built-in preset (`generic`, `cht-cs210`)
//! and override single fields, so a trunk can be as short as
//!
//! ```toml
//! [[trunks]]
//! name = "cht"
//! preset = "cht-cs210"
//! peer = "203.0.113.10:5060"
//! ```
//!
//! Every section rejects unknown keys, so a misspelled option is an error instead of being
//! silently ignored. `conf/sip-faxd.toml.example` documents every option.

use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sip_core::sdp::negotiate::AudioCodec;
use sip_core::sdp::{RateManagement, T38Capabilities, T38Params, UdpEc};
use sip_core::transport::{Cidr, PolicyMode, TransportKind};

/// A configuration problem.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The text is not valid TOML or a field has the wrong type / an unknown key.
    #[error("{0}")]
    Toml(String),
    /// A value is out of range or inconsistent.
    #[error("{0}")]
    Invalid(String),
}

fn invalid<T>(msg: impl Into<String>) -> Result<T, ConfigError> {
    Err(ConfigError::Invalid(msg.into()))
}

/// SIP transport of a trunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransportName {
    /// UDP (the expected transport of the Chunghwa Telecom CS210).
    Udp,
    /// TCP.
    Tcp,
}

impl TransportName {
    /// The sip-core transport.
    pub fn kind(self) -> TransportKind {
        match self {
            TransportName::Udp => TransportKind::Udp,
            TransportName::Tcp => TransportKind::Tcp,
        }
    }
}

/// What to do with a request from a source that belongs to no trunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UnknownSource {
    /// Discard silently (default).
    #[default]
    Drop,
    /// Answer 403 Forbidden.
    Reject,
    /// Process the call like a `generic` trunk and log a warning.
    Log,
}

impl UnknownSource {
    /// The matching sip-core source policy mode.
    pub fn policy_mode(self) -> PolicyMode {
        match self {
            UnknownSource::Drop => PolicyMode::Drop,
            UnknownSource::Reject => PolicyMode::Reject,
            UnknownSource::Log => PolicyMode::LogOnly,
        }
    }
}

/// How T.38 gets established (docs/design/sip-core.md section 9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum T38Policy {
    /// T.38 in the initial INVITE, a T.38 re-INVITE from the peer, or our own re-INVITE
    /// `initiate_after_secs` after the ACK (all three paths).
    #[default]
    Auto,
    /// Initial INVITE and peer re-INVITE only; we never send a T.38 re-INVITE.
    WaitPeer,
    /// Like `auto`, with the emphasis on our own re-INVITE (see ADR-013: identical on the wire
    /// until CNG detection arrives in M2).
    Initiate,
    /// Stay on G.711: T.38 offers are answered with 488 (or the audio line is chosen when the
    /// INVITE offers both).
    AudioOnly,
}

impl T38Policy {
    /// Do we send our own T.38 re-INVITE?
    pub fn initiates(self) -> bool {
        matches!(self, T38Policy::Auto | T38Policy::Initiate)
    }

    /// Do we accept T.38 at all?
    pub fn accepts_t38(self) -> bool {
        !matches!(self, T38Policy::AudioOnly)
    }
}

/// Which captured calls are kept on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeepMode {
    /// Keep every call (default).
    #[default]
    All,
    /// Keep only calls that ended with a failure.
    Failed,
}

/// `T38FaxRateManagement` as written in the config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum RateManagementName {
    /// `localTCF`
    #[serde(rename = "localTCF")]
    LocalTcf,
    /// `transferredTCF`
    #[default]
    #[serde(rename = "transferredTCF")]
    TransferredTcf,
}

/// `T38FaxUdpEC` as written in the config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UdpEcName {
    /// `t38UDPRedundancy`
    #[default]
    Redundancy,
    /// `t38UDPFEC`
    Fec,
    /// `t38UDPNoEC`
    None,
}

/// `[sip]`
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SipConfig {
    /// Address to bind (UDP and/or TCP, see `transports`).
    pub bind: SocketAddr,
    /// Transports to open. Default: the union of the trunks' transports.
    pub transports: Option<Vec<TransportName>>,
    /// `host` or `host:port` written into Via and Contact (NAT / multi-homed hosts).
    pub advertised: Option<String>,
    /// `User-Agent` / `Server` value.
    pub user_agent: String,
    /// `Allow` header we send.
    pub allow: Option<String>,
    /// `Supported` header we send.
    pub supported: Option<String>,
    /// Handling of sources outside every trunk.
    pub unknown_source: UnknownSource,
}

impl Default for SipConfig {
    fn default() -> Self {
        SipConfig {
            bind: SocketAddr::from(([0, 0, 0, 0], 5060)),
            transports: None,
            advertised: None,
            user_agent: format!("sip-faxd/{}", env!("CARGO_PKG_VERSION")),
            allow: None,
            supported: None,
            unknown_source: UnknownSource::Drop,
        }
    }
}

/// An inclusive UDP port range, `"low-high"` in the configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortRange {
    /// First port of the range.
    pub low: u16,
    /// Last port of the range (inclusive).
    pub high: u16,
}

impl PortRange {
    /// Parses `"30000-30999"`.
    pub fn parse(text: &str) -> Result<PortRange, ConfigError> {
        let bad = || {
            ConfigError::Invalid(format!(
                "[media] port_range \"{text}\" is not of the form \"low-high\" (e.g. \"30000-30999\")"
            ))
        };
        let (a, b) = text.split_once('-').ok_or_else(bad)?;
        let low: u16 = a.trim().parse().map_err(|_| bad())?;
        let high: u16 = b.trim().parse().map_err(|_| bad())?;
        if low == 0 {
            return invalid("[media] port_range must not include port 0");
        }
        if low > high {
            return invalid(format!(
                "[media] port_range \"{text}\": the low port is above the high port"
            ));
        }
        let r = PortRange { low, high };
        if r.ports().next().is_none() {
            return invalid(format!(
                "[media] port_range \"{text}\" contains no even port (UDPTL ports are allocated from the even ports of the range)"
            ));
        }
        Ok(r)
    }

    /// The usable ports: the even ports of the range, ascending. A call gets one of them, so
    /// `"30000-30999"` allows 500 simultaneous T.38 calls.
    pub fn ports(&self) -> impl Iterator<Item = u16> {
        (self.low..=self.high).filter(|p| p % 2 == 0)
    }

    /// Number of usable (even) ports.
    pub fn capacity(&self) -> usize {
        self.ports().count()
    }
}

/// `[media]`
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MediaConfig {
    /// Address put into `c=` / `o=` of our SDP. Default: the advertised host, else the bind IP.
    pub address: Option<String>,
    /// RTP port in the audio answer (placeholder until G.711 passthrough, M5).
    pub audio_port: u16,
    /// UDPTL ports for T.38 calls, `"low-high"`. Every call is given one even port of the
    /// range for its whole duration; when none is free a new call is answered with 503.
    pub port_range: String,
    /// Removed option, kept only to give a helpful error: `t38_port` was replaced by
    /// `port_range` in M2.
    pub t38_port: Option<u16>,
}

impl MediaConfig {
    /// The parsed [`MediaConfig::port_range`].
    pub fn port_range(&self) -> Result<PortRange, ConfigError> {
        PortRange::parse(&self.port_range)
    }
}

impl Default for MediaConfig {
    fn default() -> Self {
        MediaConfig {
            address: None,
            audio_port: 20000,
            port_range: "30000-30999".to_string(),
            t38_port: None,
        }
    }
}

/// `[call]`
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CallConfig {
    /// Send `180 Ringing` for every call.
    pub send_ringing: bool,
    /// Wait this long (after the 180) before answering with 200.
    pub answer_delay_ms: u64,
    /// The stub fax session (and an audio-only call) is ended with BYE after this long.
    pub max_duration_secs: u64,
    /// Our re-INVITE gets this long to receive a final answer.
    pub reinvite_timeout_secs: u64,
    /// How often a re-INVITE answered with 491 is retried.
    pub max_491_retries: u32,
    /// What runs once T.38 is established: the real fax session or a stub that only holds
    /// the call (signalling tests).
    pub fax_session: FaxSessionKind,
    /// Our station identification (CSI in the DIS). Empty sends no CSI.
    pub station_id: String,
}

impl Default for CallConfig {
    fn default() -> Self {
        CallConfig {
            send_ringing: true,
            answer_delay_ms: 0,
            max_duration_secs: 3600,
            reinvite_timeout_secs: 32,
            max_491_retries: 3,
            fax_session: FaxSessionKind::T38,
            station_id: String::new(),
        }
    }
}

/// `[call] fax_session`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FaxSessionKind {
    /// Receive the fax over T.38 and write TIFF / PDF (default).
    T38,
    /// Only hold the call until the peer hangs up or `max_duration_secs` passes.
    Hold,
}

/// `[storage]`
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StorageConfig {
    /// Received faxes are written to `<data_dir>/faxes/<YYYY>/<MM>/<DD>/<call-id>.tif|.pdf`.
    /// A relative path is relative to the executable directory.
    pub data_dir: PathBuf,
}

impl Default for StorageConfig {
    fn default() -> Self {
        StorageConfig {
            data_dir: PathBuf::from("data"),
        }
    }
}

/// `[capture]`
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CaptureConfig {
    /// Write per-call SIP captures.
    pub enabled: bool,
    /// Directory; a relative path is relative to the executable directory.
    pub dir: PathBuf,
    /// Keep every call or only failed ones.
    pub keep: KeepMode,
    /// Upper bound of the captured text per call (further messages are counted, not stored).
    pub max_bytes_per_call: usize,
    /// What `udptl.log` records.
    pub udptl: UdptlLogMode,
}

/// `[capture] udptl`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UdptlLogMode {
    /// No `udptl.log`.
    Off,
    /// One line per datagram: time, direction, seq, length and the decoded IFP summary. T.30
    /// control frames are included in full; image data is never recorded (default).
    Summary,
    /// Every datagram in full hex. This stores the fax content.
    Full,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        CaptureConfig {
            enabled: true,
            dir: PathBuf::from("logs/calls"),
            keep: KeepMode::All,
            max_bytes_per_call: 1 << 20,
            udptl: UdptlLogMode::Summary,
        }
    }
}

/// `[trunks.t38]`: the T.38 parameters we advertise.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct T38Section {
    /// Highest `T38FaxVersion` we support (0..=3).
    pub max_version: u8,
    /// `T38MaxBitRate`.
    pub max_bit_rate: u32,
    /// `T38FaxRateManagement` of our own offers (answers copy the peer's).
    pub rate_management: RateManagementName,
    /// `T38FaxMaxBuffer`.
    pub max_buffer: u32,
    /// `T38FaxMaxDatagram`.
    pub max_datagram: u32,
    /// `T38FaxUdpEC` of our own offers.
    pub udp_ec: UdpEcName,
    /// We can do UDPTL FEC (otherwise an offered FEC is answered with redundancy).
    pub fec: bool,
    /// UDPTL redundancy we send when `t38UDPRedundancy` is in force: how many previous
    /// packets each datagram repeats (0 = none).
    pub redundancy_depth: u8,
    /// UDPTL FEC we send when `t38UDPFEC` is in force: packets per parity entry.
    pub fec_span: u8,
    /// UDPTL FEC we send when `t38UDPFEC` is in force: parity entries per datagram.
    pub fec_entries: u8,
    /// `T38FaxFillBitRemoval`
    pub fill_bit_removal: bool,
    /// `T38FaxTranscodingMMR`
    pub transcoding_mmr: bool,
    /// `T38FaxTranscodingJBIG`
    pub transcoding_jbig: bool,
}

impl Default for T38Section {
    fn default() -> Self {
        let d = T38Capabilities::default();
        T38Section {
            max_version: d.max_version,
            max_bit_rate: d.max_bit_rate,
            rate_management: RateManagementName::TransferredTcf,
            max_buffer: d.max_buffer,
            max_datagram: d.max_datagram,
            udp_ec: UdpEcName::Redundancy,
            fec: false,
            redundancy_depth: 3,
            fec_span: 3,
            fec_entries: 3,
            fill_bit_removal: false,
            transcoding_mmr: false,
            transcoding_jbig: false,
        }
    }
}

/// `[trunks.fax]`: what the DIS offers (docs/design/m3-store.md sections 8 and 10).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FaxSection {
    /// Vertical resolutions advertised: `"standard"` (3.85 lines/mm) and / or `"fine"`
    /// (7.7 lines/mm). Default both. T.30 cannot express "fine only": standard is the base
    /// resolution every terminal supports, so `["fine"]` advertises fine (DIS bit 15) and a
    /// DCS for standard is still accepted.
    pub resolutions: Vec<String>,
    /// Offer the error correction mode (T.30 Annex A, DIS bit 27) and, with it, MMR (T.6, bit
    /// 31). Default off until ECM has been verified against a real gateway (ADR-035).
    pub ecm: bool,
}

impl Default for FaxSection {
    fn default() -> Self {
        FaxSection {
            resolutions: vec!["standard".into(), "fine".into()],
            ecm: false,
        }
    }
}

impl FaxSection {
    /// The resolution names this build can advertise.
    pub const SUPPORTED_RESOLUTIONS: [&'static str; 2] = ["standard", "fine"];

    /// Fine resolution is listed (DIS bit 15).
    pub fn fine(&self) -> bool {
        self.resolutions.iter().any(|r| r == "fine")
    }

    /// Checks the list: not empty, only supported names.
    ///
    /// # Errors
    /// A message naming the offending value.
    pub fn check(&self) -> Result<(), String> {
        if self.resolutions.is_empty() {
            return Err(format!(
                "resolutions is empty (use any of {:?})",
                Self::SUPPORTED_RESOLUTIONS
            ));
        }
        if let Some(bad) = self
            .resolutions
            .iter()
            .find(|r| !Self::SUPPORTED_RESOLUTIONS.contains(&r.as_str()))
        {
            return Err(format!(
                "resolutions: unsupported value \"{bad}\" (supported: {:?})",
                Self::SUPPORTED_RESOLUTIONS
            ));
        }
        Ok(())
    }
}

/// `[trunks.numbers]`: where the calling / called number comes from (docs/design/m3-store.md
/// section 2).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NumbersSection {
    /// Where to look for the calling number, in order: `pai` (P-Asserted-Identity), `rpid`
    /// (Remote-Party-ID), `from`.
    pub ani_sources: Vec<String>,
    /// Where to look for the called number, in order: `ruri` (Request-URI), `to`, `diversion`.
    pub dnis_sources: Vec<String>,
    /// Drop a leading `+` from both numbers.
    pub strip_plus: bool,
    /// Remove this prefix from the calling number (for example a country code).
    pub ani_strip_prefix: Option<String>,
    /// Remove this prefix from the called number.
    pub dnis_strip_prefix: Option<String>,
}

impl Default for NumbersSection {
    fn default() -> Self {
        NumbersSection {
            ani_sources: vec!["pai".into(), "rpid".into(), "from".into()],
            dnis_sources: vec!["ruri".into(), "to".into()],
            strip_plus: false,
            ani_strip_prefix: None,
            dnis_strip_prefix: None,
        }
    }
}

impl NumbersSection {
    /// Names accepted in `ani_sources`.
    pub const ANI_SOURCES: [&'static str; 3] = ["pai", "rpid", "from"];
    /// Names accepted in `dnis_sources`.
    pub const DNIS_SOURCES: [&'static str; 3] = ["ruri", "to", "diversion"];

    /// Checks that every listed source is known and no list is empty.
    ///
    /// # Errors
    /// A message naming the offending key and value.
    pub fn check(&self) -> Result<(), String> {
        for (key, list, allowed) in [
            ("ani_sources", &self.ani_sources, &Self::ANI_SOURCES),
            ("dnis_sources", &self.dnis_sources, &Self::DNIS_SOURCES),
        ] {
            if list.is_empty() {
                return Err(format!("{key} is empty (use any of {allowed:?})"));
            }
            if let Some(bad) = list.iter().find(|s| !allowed.contains(&s.as_str())) {
                return Err(format!(
                    "{key}: unsupported value \"{bad}\" (supported: {allowed:?})"
                ));
            }
        }
        Ok(())
    }
}

/// `[trunks.compat]`: tolerance switches for peers without a specification.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CompatSection {
    /// Accept INVITEs whose `Require` lists option tags we do not know (default: 420).
    pub accept_unknown_require: bool,
    /// Floor for the peer's `T38FaxMaxDatagram`; smaller offered values are raised to this
    /// (with a warning).
    pub min_max_datagram: u32,
}

impl Default for CompatSection {
    fn default() -> Self {
        CompatSection {
            accept_unknown_require: false,
            min_max_datagram: 72,
        }
    }
}

/// `[trunks.register]`
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RegisterSection {
    /// Register this trunk at the peer.
    pub enabled: bool,
    /// Registrar `host:port`. Default: `peer`.
    pub registrar: Option<String>,
    /// Domain of the registrar URI and of the default AoR. Default: the registrar host.
    pub domain: Option<String>,
    /// Address-of-record, e.g. `sip:fax@example.com`. Default: `sip:<username>@<domain>`.
    pub aor: Option<String>,
    /// Digest user name.
    pub username: Option<String>,
    /// Digest password.
    pub password: Option<String>,
    /// Requested binding lifetime in seconds.
    pub expires: u32,
    /// First retry delay after a failed REGISTER.
    pub backoff_initial_secs: f64,
    /// Upper bound of the retry delay.
    pub backoff_max_secs: f64,
}

impl Default for RegisterSection {
    fn default() -> Self {
        RegisterSection {
            enabled: false,
            registrar: None,
            domain: None,
            aor: None,
            username: None,
            password: None,
            expires: 3600,
            backoff_initial_secs: 5.0,
            backoff_max_secs: 300.0,
        }
    }
}

/// One `[[trunks]]` entry after the preset has been applied.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TrunkConfig {
    /// Unique name (shown in logs and call results).
    pub name: String,
    /// Preset the entry started from (`generic` or `cht-cs210`).
    pub preset: String,
    /// The peer `host:port` (a fixed IP is expected).
    pub peer: Option<String>,
    /// Source IPs / CIDRs calls may come from. Default: the peer's IP.
    pub allowed_sources: Vec<String>,
    /// Transport of this trunk.
    pub transport: TransportName,
    /// T.38 establishment policy.
    pub t38_policy: T38Policy,
    /// Wait this long after the ACK before our own T.38 re-INVITE (`auto` / `initiate`).
    pub initiate_after_secs: f64,
    /// Audio codecs we answer with, in preference order (`PCMU`, `PCMA`).
    pub audio_codecs: Vec<String>,
    /// T.38 parameters.
    pub t38: T38Section,
    /// Tolerance switches.
    pub compat: CompatSection,
    /// Registration.
    pub register: RegisterSection,
    /// Fax capabilities advertised in the DIS.
    pub fax: FaxSection,
    /// Where the calling / called number is taken from.
    pub numbers: NumbersSection,
}

impl Default for TrunkConfig {
    fn default() -> Self {
        TrunkConfig {
            name: String::new(),
            preset: "generic".into(),
            peer: None,
            allowed_sources: Vec::new(),
            transport: TransportName::Udp,
            t38_policy: T38Policy::Auto,
            initiate_after_secs: 3.0,
            audio_codecs: vec!["PCMU".into(), "PCMA".into()],
            t38: T38Section::default(),
            compat: CompatSection::default(),
            register: RegisterSection::default(),
            fax: FaxSection::default(),
            numbers: NumbersSection::default(),
        }
    }
}

/// Names of the built-in presets.
pub const PRESETS: [&str; 2] = ["generic", "cht-cs210"];

/// The built-in presets as TOML (a trunk's own keys are merged on top).
pub fn preset_toml(name: &str) -> Option<&'static str> {
    match name {
        "generic" => Some(
            r#"
t38_policy = "auto"
initiate_after_secs = 3
transport = "udp"
"#,
        ),
        // Chunghwa Telecom CS210: fixed IP, transport expected to be UDP, no specification.
        // Today this is `generic` plus an explicit UDP; it will be tuned from real captures
        // (docs/design/sip-core.md section 9, milestone M6).
        "cht-cs210" => Some(
            r#"
t38_policy = "auto"
initiate_after_secs = 3
transport = "udp"
"#,
        ),
        _ => None,
    }
}

/// The complete fax-in configuration.
#[derive(Debug, Clone, Default)]
pub struct FaxInConfig {
    /// `[sip]`
    pub sip: SipConfig,
    /// `[media]`
    pub media: MediaConfig,
    /// `[call]`
    pub call: CallConfig,
    /// `[capture]`
    pub capture: CaptureConfig,
    /// `[storage]`
    pub storage: StorageConfig,
    /// `[[trunks]]`
    pub trunks: Vec<TrunkConfig>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Sections {
    #[serde(default)]
    sip: SipConfig,
    #[serde(default)]
    media: MediaConfig,
    #[serde(default)]
    call: CallConfig,
    #[serde(default)]
    capture: CaptureConfig,
    #[serde(default)]
    storage: StorageConfig,
}

fn merge(base: &mut toml::Table, over: toml::Table) {
    for (k, v) in over {
        match (base.get_mut(&k), v) {
            (Some(toml::Value::Table(b)), toml::Value::Table(o)) => merge(b, o),
            (_, v) => {
                base.insert(k, v);
            }
        }
    }
}

fn toml_err(context: &str, e: impl std::fmt::Display) -> ConfigError {
    let text = e.to_string();
    // toml's multi-line diagnostics are noisy; the first line names the key / reason.
    let first = text
        .lines()
        .find(|l| !l.trim().is_empty() && !l.trim_start().starts_with("TOML parse error"))
        .unwrap_or(text.as_str());
    let detail = text
        .lines()
        .filter(|l| !l.contains('|') && !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join("; ");
    let detail = if detail.is_empty() {
        first.to_string()
    } else {
        detail
    };
    ConfigError::Toml(format!("{context}{detail}"))
}

impl FaxInConfig {
    /// Parses and validates configuration text.
    pub fn parse(text: &str) -> Result<FaxInConfig, ConfigError> {
        let table: toml::Table = text.parse().map_err(|e| toml_err("invalid TOML: ", e))?;
        Self::from_table(table)
    }

    /// Like [`FaxInConfig::parse`] for an already parsed table (the application removes its own
    /// sections, e.g. `[log]`, first).
    pub fn from_table(mut table: toml::Table) -> Result<FaxInConfig, ConfigError> {
        let trunks = match table.remove("trunks") {
            None => Vec::new(),
            Some(toml::Value::Array(a)) => a,
            Some(_) => return invalid("`trunks` must be an array of tables ([[trunks]])"),
        };
        let sections: Sections = toml::Value::Table(table)
            .try_into()
            .map_err(|e| toml_err("", e))?;
        let mut out = FaxInConfig {
            sip: sections.sip,
            media: sections.media,
            call: sections.call,
            capture: sections.capture,
            storage: sections.storage,
            trunks: Vec::new(),
        };
        for (i, t) in trunks.into_iter().enumerate() {
            let toml::Value::Table(user) = t else {
                return invalid(format!("trunks[{i}] must be a table"));
            };
            let label = user
                .get("name")
                .and_then(|n| n.as_str())
                .map_or_else(|| format!("trunks[{i}]"), |n| format!("trunk \"{n}\""));
            let preset = match user.get("preset") {
                None => "generic".to_string(),
                Some(toml::Value::String(s)) => s.clone(),
                Some(_) => return invalid(format!("{label}: `preset` must be a string")),
            };
            let Some(preset_text) = preset_toml(&preset) else {
                return invalid(format!(
                    "{label}: unknown preset \"{preset}\" (available: {})",
                    PRESETS.join(", ")
                ));
            };
            let mut merged: toml::Table = preset_text
                .parse()
                .map_err(|e| ConfigError::Invalid(format!("built-in preset {preset}: {e}")))?;
            merge(&mut merged, user);
            merged.insert("preset".into(), toml::Value::String(preset));
            let cfg: TrunkConfig = toml::Value::Table(merged)
                .try_into()
                .map_err(|e| toml_err(&format!("{label}: "), e))?;
            out.trunks.push(cfg);
        }
        out.validate()?;
        Ok(out)
    }

    /// Checks cross-field rules and resolves every trunk (so `parse` fails on bad CIDRs or
    /// unresolvable peers).
    pub fn validate(&self) -> Result<(), ConfigError> {
        let _ = crate::trunk::TrunkTable::build(self)?;
        if let Some(p) = self.media.t38_port {
            return invalid(format!(
                "[media] t38_port = {p} is no longer supported: every call now gets its own UDPTL port. \
                 Replace it with port_range = \"{p}-{}\" (a range of even ports, one per simultaneous call)",
                p.saturating_add(999)
            ));
        }
        self.media.port_range()?;
        let t = &self.sip.bind;
        if t.port() == 0 && self.trunks.iter().any(|t| t.register.enabled) {
            return invalid("[sip] bind port 0 cannot be combined with registration");
        }
        if self.call.max_duration_secs == 0 {
            return invalid("[call] max_duration_secs must be greater than 0");
        }
        for t in &self.trunks {
            let c = &t.t38;
            if c.redundancy_depth > 8 {
                return invalid(format!(
                    "trunk \"{}\": [trunks.t38] redundancy_depth {} is above 8",
                    t.name, c.redundancy_depth
                ));
            }
            if !(1..=8).contains(&c.fec_span) || !(1..=8).contains(&c.fec_entries) {
                return invalid(format!(
                    "trunk \"{}\": [trunks.t38] fec_span and fec_entries must be 1..=8",
                    t.name
                ));
            }
            if let Err(e) = t.fax.check() {
                return invalid(format!("trunk \"{}\": [trunks.fax] {e}", t.name));
            }
            if let Err(e) = t.numbers.check() {
                return invalid(format!("trunk \"{}\": [trunks.numbers] {e}", t.name));
            }
        }
        Ok(())
    }

    /// Non-fatal observations (printed by `config check`).
    pub fn warnings(&self) -> Vec<String> {
        let mut w = Vec::new();
        if self.trunks.is_empty() {
            w.push(match self.sip.unknown_source {
                UnknownSource::Log => "no [[trunks]] defined: every call is handled as an unknown source (unknown_source = \"log\")".to_string(),
                _ => "no [[trunks]] defined: every inbound call will be dropped or rejected".to_string(),
            });
        }
        if self.sip.bind.ip().is_unspecified()
            && self.sip.advertised.is_none()
            && self.media.address.is_none()
        {
            w.push("[sip] bind is a wildcard address and neither [sip] advertised nor [media] address is set: our SDP and Contact will contain the wildcard address; set `advertised`".to_string());
        }
        for t in &self.trunks {
            if t.compat.accept_unknown_require {
                w.push(format!(
                    "trunk \"{}\": accept_unknown_require = true (RFC 3261 says to answer 420)",
                    t.name
                ));
            }
            if t.t38.max_version > 3 {
                w.push(format!(
                    "trunk \"{}\": t38.max_version {} is above 3",
                    t.name, t.t38.max_version
                ));
            }
        }
        w
    }

    /// The transports the endpoint opens.
    pub fn transports(&self) -> Vec<TransportName> {
        if let Some(t) = &self.sip.transports {
            return dedup(t.clone());
        }
        let mut v: Vec<TransportName> = self.trunks.iter().map(|t| t.transport).collect();
        if v.is_empty() {
            v.push(TransportName::Udp);
        }
        dedup(v)
    }

    /// Host part used for SDP `c=` / `o=`.
    pub fn media_address(&self) -> String {
        if let Some(a) = &self.media.address {
            return a.clone();
        }
        if let Some(adv) = &self.sip.advertised {
            return split_host_port(adv).0.to_string();
        }
        self.sip.bind.ip().to_string()
    }
}

fn dedup(mut v: Vec<TransportName>) -> Vec<TransportName> {
    let mut seen = Vec::new();
    v.retain(|t| {
        if seen.contains(t) {
            false
        } else {
            seen.push(*t);
            true
        }
    });
    v
}

/// Splits `host`, `host:port`, `[v6]` or `[v6]:port` (the port is `None` when absent).
pub fn split_host_port(s: &str) -> (&str, Option<u16>) {
    if let Some(rest) = s.strip_prefix('[')
        && let Some((host, tail)) = rest.split_once(']')
    {
        return (host, tail.strip_prefix(':').and_then(|p| p.parse().ok()));
    }
    if s.matches(':').count() == 1
        && let Some((h, p)) = s.split_once(':')
        && let Ok(p) = p.parse()
    {
        return (h, Some(p));
    }
    (s, None)
}

/// Resolves `host[:port]` (default port 5060); IP literals never touch DNS.
pub fn resolve_peer(s: &str) -> Result<SocketAddr, ConfigError> {
    if let Ok(a) = s.parse::<SocketAddr>() {
        return Ok(a);
    }
    if let Ok(ip) = s.parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, 5060));
    }
    let (host, port) = split_host_port(s);
    (host, port.unwrap_or(5060))
        .to_socket_addrs()
        .map_err(|e| ConfigError::Invalid(format!("cannot resolve \"{s}\": {e}")))?
        .next()
        .ok_or_else(|| ConfigError::Invalid(format!("\"{s}\" resolves to no address")))
}

impl TrunkConfig {
    /// `initiate_after_secs` as a duration.
    pub fn initiate_after(&self) -> Duration {
        Duration::from_secs_f64(self.initiate_after_secs.max(0.0))
    }

    /// The audio codecs in preference order (validated at parse time).
    pub fn codecs(&self) -> Vec<AudioCodec> {
        self.audio_codecs
            .iter()
            .filter_map(|c| match c.to_ascii_uppercase().as_str() {
                "PCMU" => Some(AudioCodec::PCMU),
                "PCMA" => Some(AudioCodec::PCMA),
                _ => None,
            })
            .collect()
    }

    /// Capabilities used when answering a T.38 offer.
    pub fn t38_capabilities(&self) -> T38Capabilities {
        T38Capabilities {
            max_version: self.t38.max_version,
            max_bit_rate: self.t38.max_bit_rate,
            fec: self.t38.fec || self.t38.udp_ec == UdpEcName::Fec,
            max_buffer: self.t38.max_buffer,
            max_datagram: self.t38.max_datagram,
            fill_bit_removal: self.t38.fill_bit_removal,
            transcoding_mmr: self.t38.transcoding_mmr,
            transcoding_jbig: self.t38.transcoding_jbig,
        }
    }

    /// The attributes of a T.38 offer we send ourselves.
    pub fn t38_offer_params(&self) -> T38Params {
        T38Params {
            version: self.t38.max_version,
            max_bit_rate: self.t38.max_bit_rate,
            rate_management: match self.t38.rate_management {
                RateManagementName::LocalTcf => RateManagement::LocalTcf,
                RateManagementName::TransferredTcf => RateManagement::TransferredTcf,
            },
            max_buffer: self.t38.max_buffer,
            max_datagram: self.t38.max_datagram,
            udp_ec: match self.t38.udp_ec {
                UdpEcName::Redundancy => UdpEc::Redundancy,
                UdpEcName::Fec => UdpEc::Fec,
                UdpEcName::None => UdpEc::NoEc,
            },
            fill_bit_removal: self.t38.fill_bit_removal,
            transcoding_mmr: self.t38.transcoding_mmr,
            transcoding_jbig: self.t38.transcoding_jbig,
            vendor_info: None,
        }
    }

    pub(crate) fn validate(&self) -> Result<(Vec<Cidr>, Option<SocketAddr>), ConfigError> {
        let label = format!("trunk \"{}\"", self.name);
        if self.name.trim().is_empty() {
            return invalid("every [[trunks]] entry needs a non-empty `name`");
        }
        if !(self.initiate_after_secs.is_finite() && self.initiate_after_secs >= 0.0) {
            return invalid(format!("{label}: initiate_after_secs must be >= 0"));
        }
        if self.t38.max_version > 3 {
            // accepted with a warning, but 255 is certainly a typo
        }
        if self.audio_codecs.is_empty() {
            return invalid(format!("{label}: audio_codecs must not be empty"));
        }
        for c in &self.audio_codecs {
            if !matches!(c.to_ascii_uppercase().as_str(), "PCMU" | "PCMA") {
                return invalid(format!(
                    "{label}: unsupported audio codec \"{c}\" (supported: PCMU, PCMA)"
                ));
            }
        }
        if self.t38.max_datagram == 0 || self.t38.max_buffer == 0 || self.t38.max_bit_rate == 0 {
            return invalid(format!(
                "{label}: t38.max_datagram / max_buffer / max_bit_rate must be greater than 0"
            ));
        }
        let peer = match &self.peer {
            Some(p) => Some(
                resolve_peer(p).map_err(|e| ConfigError::Invalid(format!("{label}: peer: {e}")))?,
            ),
            None => None,
        };
        let mut cidrs = Vec::new();
        for s in &self.allowed_sources {
            cidrs.push(
                Cidr::parse(s)
                    .map_err(|e| ConfigError::Invalid(format!("{label}: allowed_sources: {e}")))?,
            );
        }
        if cidrs.is_empty() {
            match peer {
                Some(p) => cidrs.push(
                    Cidr::parse(&p.ip().to_string())
                        .map_err(|e| ConfigError::Invalid(e.to_string()))?,
                ),
                None => {
                    return invalid(format!(
                        "{label}: set `allowed_sources` or `peer` (the source filter needs at least one address)"
                    ));
                }
            }
        }
        let r = &self.register;
        if r.enabled {
            if r.username.as_deref().is_none_or(str::is_empty) {
                return invalid(format!("{label}: register.enabled needs register.username"));
            }
            if r.password.is_none() {
                return invalid(format!("{label}: register.enabled needs register.password"));
            }
            if r.registrar.is_none() && peer.is_none() {
                return invalid(format!(
                    "{label}: register.enabled needs register.registrar or peer"
                ));
            }
            if r.expires == 0 {
                return invalid(format!("{label}: register.expires must be greater than 0"));
            }
            if !(r.backoff_initial_secs.is_finite()
                && r.backoff_initial_secs > 0.0
                && r.backoff_max_secs.is_finite()
                && r.backoff_max_secs >= r.backoff_initial_secs)
            {
                return invalid(format!(
                    "{label}: register backoff needs 0 < backoff_initial_secs <= backoff_max_secs"
                ));
            }
            if let Some(reg) = &r.registrar {
                resolve_peer(reg).map_err(|e| {
                    ConfigError::Invalid(format!("{label}: register.registrar: {e}"))
                })?;
            }
        }
        Ok((cidrs, peer))
    }
}
