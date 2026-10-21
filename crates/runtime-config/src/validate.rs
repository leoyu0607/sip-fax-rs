//! Validation of one section, reported per field (docs/design/m4-web.md section 1; the REST
//! layer turns the list into the 422 body `{"error": {"code": "invalid", "fields": {...}}}`).
//!
//! The Rust types decide what is well formed (unknown keys, wrong types); the checks below add
//! the ranges and the cross-field rules. Messages are English and for logs; the interface
//! translates by `code`.

use fax_in::FaxInConfig;
use fax_in::config::{
    CallConfig, CaptureConfig, MediaConfig, PortRange, SipConfig, TrunkConfig, resolve_peer,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use sip_core::transport::Cidr;

use crate::model::{DeliverySection, RetentionSection, Section};

/// One problem with one field of a section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FieldError {
    /// The key, dotted from the section root: `port_range`, `t38.max_version`,
    /// `resolutions[0]`, `routes[1].deliveries[0].url`. Empty when the problem concerns the
    /// section as a whole.
    pub field: String,
    /// Fixed code the interface translates: `required`, `invalid`, `wrong_type`,
    /// `unknown_field`, `out_of_range`, `empty`, `too_long`, `duplicate`, `not_last`,
    /// `unknown_preset`, `mismatch`, `removed`.
    pub code: String,
    /// English description.
    pub message: String,
}

impl FieldError {
    pub(crate) fn new(field: &str, code: &str, message: impl Into<String>) -> FieldError {
        FieldError {
            field: field.to_string(),
            code: code.to_string(),
            message: message.into(),
        }
    }
}

/// A validated section.
#[derive(Debug, Clone)]
pub enum SectionValue {
    /// `[sip]`
    Sip(SipConfig),
    /// `[media]`
    Media(MediaConfig),
    /// `[call]`
    Call(CallConfig),
    /// `[capture]`
    Capture(CaptureConfig),
    /// `[delivery]`
    Delivery(DeliverySection),
    /// `[retention]`
    Retention(RetentionSection),
    /// `[[routes]]`
    Routes(delivery::Routes),
    /// One trunk.
    Trunk(Box<TrunkConfig>),
}

fn serde_error(e: &serde_path_to_error::Error<serde_json::Error>) -> FieldError {
    let path = e.path().to_string();
    let field = if path == "." { String::new() } else { path };
    let msg = e.inner().to_string();
    let code = if msg.contains("unknown field") {
        "unknown_field"
    } else if msg.contains("missing field") {
        "required"
    } else if msg.contains("invalid type") {
        "wrong_type"
    } else {
        "invalid"
    };
    FieldError::new(&field, code, msg)
}

fn typed<T: DeserializeOwned>(body: &Value) -> Result<T, Vec<FieldError>> {
    serde_path_to_error::deserialize(body.clone()).map_err(|e| vec![serde_error(&e)])
}

/// Parses and validates the JSON body of a section (secrets in plain text or as a placeholder
/// string; presence is what matters here).
///
/// # Errors
/// Every problem found, per field.
pub fn parse_section(section: &Section, body: &Value) -> Result<SectionValue, Vec<FieldError>> {
    let mut errs = Vec::new();
    let v = match section {
        Section::Sip => {
            let c: SipConfig = typed(body)?;
            check_sip(&c, &mut errs);
            SectionValue::Sip(c)
        }
        Section::Media => {
            let c: MediaConfig = typed(body)?;
            check_media(&c, &mut errs);
            SectionValue::Media(c)
        }
        Section::Call => {
            let c: CallConfig = typed(body)?;
            check_call(&c, &mut errs);
            SectionValue::Call(c)
        }
        Section::Capture => {
            let c: CaptureConfig = typed(body)?;
            if c.max_bytes_per_call == 0 {
                errs.push(FieldError::new(
                    "max_bytes_per_call",
                    "out_of_range",
                    "max_bytes_per_call must be greater than 0",
                ));
            }
            SectionValue::Capture(c)
        }
        Section::Delivery => {
            let c: DeliverySection = typed(body)?;
            if !(1..=100).contains(&c.max_attempts) {
                errs.push(FieldError::new(
                    "max_attempts",
                    "out_of_range",
                    "max_attempts must be between 1 and 100",
                ));
            }
            if c.catch_up_hours > 24 * 365 {
                errs.push(FieldError::new(
                    "catch_up_hours",
                    "out_of_range",
                    "catch_up_hours must be at most 8760 (one year)",
                ));
            }
            SectionValue::Delivery(c)
        }
        Section::Retention => {
            let c: RetentionSection = typed(body)?;
            check_retention(&c, &mut errs);
            SectionValue::Retention(c)
        }
        Section::Routes => {
            let r: delivery::Routes = typed(body)?;
            if let Err(list) = r.validate() {
                errs.extend(list.iter().map(|m| route_error(m)));
            }
            SectionValue::Routes(r)
        }
        Section::Trunk(name) => {
            let merged = fax_in::config::trunk_json_with_preset(body.clone()).map_err(|e| {
                let msg = e.to_string();
                let field = if msg.contains("preset") { "preset" } else { "" };
                let code = if msg.contains("unknown preset") {
                    "unknown_preset"
                } else {
                    "invalid"
                };
                vec![FieldError::new(field, code, msg)]
            })?;
            let t: TrunkConfig = typed(&merged)?;
            check_trunk(name, &t, &mut errs);
            SectionValue::Trunk(Box::new(t))
        }
    };
    if errs.is_empty() { Ok(v) } else { Err(errs) }
}

fn check_sip(c: &SipConfig, errs: &mut Vec<FieldError>) {
    if c.user_agent.trim().is_empty() {
        errs.push(FieldError::new(
            "user_agent",
            "empty",
            "user_agent must not be empty",
        ));
    } else if c.user_agent.len() > 128 {
        errs.push(FieldError::new(
            "user_agent",
            "too_long",
            "user_agent is longer than 128 bytes",
        ));
    }
    if let Some(t) = &c.transports
        && t.is_empty()
    {
        errs.push(FieldError::new(
            "transports",
            "empty",
            "transports must list at least one transport (or be left out)",
        ));
    }
    if let Some(a) = &c.advertised
        && fax_in::config::split_host_port(a).0.trim().is_empty()
    {
        errs.push(FieldError::new(
            "advertised",
            "invalid",
            "advertised must be `host` or `host:port`",
        ));
    }
}

fn check_media(c: &MediaConfig, errs: &mut Vec<FieldError>) {
    if let Err(e) = PortRange::parse(&c.port_range) {
        let msg = e.to_string();
        let code = if msg.contains("above the high port") {
            "reversed"
        } else if msg.contains("no even port") || msg.contains("port 0") {
            "out_of_range"
        } else {
            "invalid"
        };
        errs.push(FieldError::new("port_range", code, msg));
    }
    if c.t38_port.is_some() {
        errs.push(FieldError::new(
            "t38_port",
            "removed",
            "t38_port is no longer supported, use port_range",
        ));
    }
}

fn check_call(c: &CallConfig, errs: &mut Vec<FieldError>) {
    if c.max_duration_secs == 0 {
        errs.push(FieldError::new(
            "max_duration_secs",
            "out_of_range",
            "max_duration_secs must be greater than 0",
        ));
    }
    if c.reinvite_timeout_secs == 0 {
        errs.push(FieldError::new(
            "reinvite_timeout_secs",
            "out_of_range",
            "reinvite_timeout_secs must be at least 1",
        ));
    }
    if c.answer_delay_ms > 60_000 {
        errs.push(FieldError::new(
            "answer_delay_ms",
            "out_of_range",
            "answer_delay_ms must be at most 60000",
        ));
    }
    if c.station_id.chars().count() > 20 {
        errs.push(FieldError::new(
            "station_id",
            "too_long",
            "station_id is at most 20 characters (T.30 CSI)",
        ));
    } else if !c
        .station_id
        .chars()
        .all(|ch| ch.is_ascii() && !ch.is_ascii_control())
    {
        errs.push(FieldError::new(
            "station_id",
            "invalid",
            "station_id may only contain printable ASCII characters (T.30 CSI)",
        ));
    }
}

fn check_retention(c: &RetentionSection, errs: &mut Vec<FieldError>) {
    if let Err(e) = c.settings().validate() {
        let msg = e.to_string();
        if msg.contains("run_at") {
            errs.push(FieldError::new("run_at", "invalid", msg));
        } else {
            errs.push(FieldError::new("records_days", "out_of_range", msg));
        }
    }
}

fn route_error(msg: &str) -> FieldError {
    // "routes[1] (sales).deliveries[0]: text" -> field "routes[1].deliveries[0]"
    let (who, text) = msg.split_once(": ").unwrap_or(("", msg));
    let mut field = String::new();
    let mut depth = 0u32;
    let mut skip = false;
    for ch in who.chars() {
        match ch {
            '(' => {
                depth += 1;
                skip = true;
            }
            ')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    skip = false;
                }
            }
            ' ' if !skip => {}
            c if !skip => field.push(c),
            _ => {}
        }
    }
    let code = if text.contains("must be last") {
        "not_last"
    } else if text.contains("no deliveries") {
        "empty"
    } else if text.contains("duplicate target") {
        "duplicate"
    } else if text.contains("needs ") {
        "required"
    } else {
        "invalid"
    };
    FieldError::new(&field, code, text)
}

/// Characters allowed in a trunk name (it is part of the section name `trunk:<name>`).
pub fn valid_trunk_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 56
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn check_trunk(section_name: &str, t: &TrunkConfig, errs: &mut Vec<FieldError>) {
    if t.name != section_name {
        errs.push(FieldError::new(
            "name",
            "mismatch",
            format!(
                "name \"{}\" does not match the section trunk:{section_name}",
                t.name
            ),
        ));
    }
    if !valid_trunk_name(&t.name) {
        errs.push(FieldError::new(
            "name",
            "invalid",
            "name must be 1-56 characters of letters, digits, `.`, `_` or `-`",
        ));
    }
    if !(t.initiate_after_secs.is_finite() && t.initiate_after_secs >= 0.0) {
        errs.push(FieldError::new(
            "initiate_after_secs",
            "out_of_range",
            "initiate_after_secs must be >= 0",
        ));
    }
    if t.audio_codecs.is_empty() {
        errs.push(FieldError::new(
            "audio_codecs",
            "empty",
            "audio_codecs must not be empty",
        ));
    }
    for (i, c) in t.audio_codecs.iter().enumerate() {
        if !matches!(c.to_ascii_uppercase().as_str(), "PCMU" | "PCMA") {
            errs.push(FieldError::new(
                &format!("audio_codecs[{i}]"),
                "invalid",
                format!("unsupported audio codec \"{c}\" (supported: PCMU, PCMA)"),
            ));
        }
    }
    let n = &t.t38;
    for (f, v) in [
        ("t38.max_datagram", n.max_datagram),
        ("t38.max_buffer", n.max_buffer),
        ("t38.max_bit_rate", n.max_bit_rate),
    ] {
        if v == 0 {
            errs.push(FieldError::new(
                f,
                "out_of_range",
                format!("{f} must be greater than 0"),
            ));
        }
    }
    if n.max_version > 3 {
        errs.push(FieldError::new(
            "t38.max_version",
            "out_of_range",
            "t38.max_version must be 0..=3",
        ));
    }
    if n.redundancy_depth > 8 {
        errs.push(FieldError::new(
            "t38.redundancy_depth",
            "out_of_range",
            "redundancy_depth must be 0..=8",
        ));
    }
    for (f, v) in [
        ("t38.fec_span", n.fec_span),
        ("t38.fec_entries", n.fec_entries),
    ] {
        if !(1..=8).contains(&v) {
            errs.push(FieldError::new(
                f,
                "out_of_range",
                format!("{f} must be 1..=8"),
            ));
        }
    }
    if let Err(m) = t.fax.check() {
        errs.push(FieldError::new(
            "fax.resolutions",
            if t.fax.resolutions.is_empty() {
                "empty"
            } else {
                "invalid"
            },
            m,
        ));
    }
    if let Err(m) = t.numbers.check() {
        let key = if m.starts_with("dnis_sources") {
            "numbers.dnis_sources"
        } else {
            "numbers.ani_sources"
        };
        errs.push(FieldError::new(
            key,
            if m.contains("is empty") {
                "empty"
            } else {
                "invalid"
            },
            m,
        ));
    }
    let peer = match &t.peer {
        Some(p) => match resolve_peer(p) {
            Ok(a) => Some(a),
            Err(e) => {
                errs.push(FieldError::new("peer", "invalid", e.to_string()));
                None
            }
        },
        None => None,
    };
    for (i, s) in t.allowed_sources.iter().enumerate() {
        if let Err(e) = Cidr::parse(s) {
            errs.push(FieldError::new(
                &format!("allowed_sources[{i}]"),
                "invalid",
                e.to_string(),
            ));
        }
    }
    if t.allowed_sources.is_empty() && t.peer.is_none() {
        errs.push(FieldError::new(
            "allowed_sources",
            "required",
            "set allowed_sources or peer (the source filter needs at least one address)",
        ));
    }
    let r = &t.register;
    if r.enabled {
        if r.username.as_deref().is_none_or(str::is_empty) {
            errs.push(FieldError::new(
                "register.username",
                "required",
                "register.enabled needs register.username",
            ));
        }
        if r.password.is_none() {
            errs.push(FieldError::new(
                "register.password",
                "required",
                "register.enabled needs register.password",
            ));
        }
        if r.registrar.is_none() && peer.is_none() && t.peer.is_none() {
            errs.push(FieldError::new(
                "register.registrar",
                "required",
                "register.enabled needs register.registrar or peer",
            ));
        }
        if r.expires == 0 {
            errs.push(FieldError::new(
                "register.expires",
                "out_of_range",
                "register.expires must be greater than 0",
            ));
        }
        if !(r.backoff_initial_secs.is_finite()
            && r.backoff_initial_secs > 0.0
            && r.backoff_max_secs.is_finite()
            && r.backoff_max_secs >= r.backoff_initial_secs)
        {
            errs.push(FieldError::new(
                "register.backoff_initial_secs",
                "out_of_range",
                "register backoff needs 0 < backoff_initial_secs <= backoff_max_secs",
            ));
        }
        if let Some(reg) = &r.registrar
            && let Err(e) = resolve_peer(reg)
        {
            errs.push(FieldError::new(
                "register.registrar",
                "invalid",
                e.to_string(),
            ));
        }
    }
    if errs.is_empty() {
        // the library's own rules as a safety net for whatever the checks above do not cover
        let one = FaxInConfig {
            trunks: vec![t.clone()],
            ..FaxInConfig::default()
        };
        if let Err(e) = fax_in::trunk::TrunkTable::build(&one) {
            errs.push(FieldError::new("", "invalid", e.to_string()));
        }
    }
}

/// Problems that only show when the sections are put together (a trunk on a transport that
/// `[sip]` does not open, registration with a wildcard port, ...). Empty when consistent.
pub fn cross_check(cfg: &crate::RuntimeConfig) -> Vec<String> {
    let fi = cfg.to_fax_in(&fax_in::config::StorageConfig::default());
    match fi.validate() {
        Ok(()) => Vec::new(),
        Err(e) => vec![e.to_string()],
    }
}

/// Things worth a warning that do not stop a change.
pub fn warnings(cfg: &crate::RuntimeConfig) -> Vec<String> {
    let mut w = cfg
        .to_fax_in(&fax_in::config::StorageConfig::default())
        .warnings();
    for (i, r) in cfg.routes.routes.iter().enumerate() {
        if let Some(t) = &r.trunk
            && !cfg.trunks.iter().any(|x| &x.name == t)
        {
            w.push(format!(
                "routes[{i}] refers to trunk \"{t}\" which is not configured"
            ));
        }
    }
    w
}
