//! The runtime configuration as one value, and its division into the sections that are stored
//! (docs/design/m4-web.md section 1.5).

use fax_in::FaxInConfig;
use fax_in::config::{
    CallConfig, CaptureConfig, MediaConfig, SipConfig, StorageConfig, TrunkConfig,
};
use serde::{Deserialize, Serialize};
use store::{RetentionPolicy, RetentionSettings};

/// `[delivery]`: how the delivery engine runs. The routes themselves are `[[routes]]`.
#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct DeliverySection {
    /// At start, queue the deliveries of finished faxes that started at most this many hours ago
    /// and have none yet (repairs a crash between the database write and the queueing, and
    /// delivers what `db backfill` restored). 0 = off.
    pub catch_up_hours: u32,
    /// Attempts per delivery before it is given up (`giving_up`).
    pub max_attempts: u32,
}

impl Default for DeliverySection {
    fn default() -> Self {
        DeliverySection {
            catch_up_hours: 24,
            max_attempts: delivery::DEFAULT_MAX_ATTEMPTS,
        }
    }
}

/// `[retention]`: how long files and records are kept.
#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct RetentionSection {
    /// Delete TIFF / PDF / PNG files after this many days (0 = never).
    pub files_days: u32,
    /// Delete the records after this many days (0 = never; at least `files_days`).
    pub records_days: u32,
    /// Daily run time `HH:MM`, server local time.
    pub run_at: String,
}

impl Default for RetentionSection {
    fn default() -> Self {
        let d = RetentionSettings::default();
        RetentionSection {
            files_days: d.policy.files_days,
            records_days: d.policy.records_days,
            run_at: d.run_at,
        }
    }
}

impl RetentionSection {
    /// As the store's settings type.
    pub fn settings(&self) -> RetentionSettings {
        RetentionSettings {
            policy: RetentionPolicy {
                files_days: self.files_days,
                records_days: self.records_days,
            },
            run_at: self.run_at.clone(),
        }
    }
}

/// Everything that is configured at run time (the sections of `config_sections`).
#[derive(Debug, Clone, Default, Serialize)]
pub struct RuntimeConfig {
    /// `[sip]`
    pub sip: SipConfig,
    /// `[media]`
    pub media: MediaConfig,
    /// `[call]`
    pub call: CallConfig,
    /// `[capture]`
    pub capture: CaptureConfig,
    /// `[[trunks]]`, in match order.
    pub trunks: Vec<TrunkConfig>,
    /// `[[routes]]`, in match order.
    pub routes: delivery::Routes,
    /// `[delivery]`
    pub delivery: DeliverySection,
    /// `[retention]`
    pub retention: RetentionSection,
}

impl RuntimeConfig {
    /// The part fax-in reads, with the file's `[storage]` (a startup setting) attached.
    pub fn to_fax_in(&self, storage: &StorageConfig) -> FaxInConfig {
        FaxInConfig {
            sip: self.sip.clone(),
            media: self.media.clone(),
            call: self.call.clone(),
            capture: self.capture.clone(),
            storage: storage.clone(),
            trunks: self.trunks.clone(),
        }
    }

    /// Takes the runtime parts out of a parsed configuration file.
    pub fn from_file_parts(
        fax_in: &FaxInConfig,
        routes: delivery::Routes,
        delivery: DeliverySection,
        retention: RetentionSection,
    ) -> RuntimeConfig {
        RuntimeConfig {
            sip: fax_in.sip.clone(),
            media: fax_in.media.clone(),
            call: fax_in.call.clone(),
            capture: fax_in.capture.clone(),
            trunks: fax_in.trunks.clone(),
            routes,
            delivery,
            retention,
        }
    }

    /// Whether two configurations are identical (compared as JSON, the stored form).
    pub fn same_as(&self, other: &RuntimeConfig) -> bool {
        serde_json::to_value(self).ok() == serde_json::to_value(other).ok()
    }
}

/// Names of the sections that are not trunks.
pub const FIXED_SECTIONS: [&str; 7] = [
    "sip",
    "media",
    "call",
    "capture",
    "delivery",
    "retention",
    "routes",
];

/// The section that records the order of the trunks (the first matching trunk wins, so the
/// order is part of the configuration). Body: a JSON array of trunk names.
pub const TRUNK_ORDER: &str = "trunk_order";

/// One stored section.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Section {
    /// `[sip]`
    Sip,
    /// `[media]`
    Media,
    /// `[call]`
    Call,
    /// `[capture]`
    Capture,
    /// `[delivery]`
    Delivery,
    /// `[retention]`
    Retention,
    /// `[[routes]]` as one ordered list.
    Routes,
    /// One `[[trunks]]` entry, `trunk:<name>`.
    Trunk(String),
}

impl Section {
    /// Parses `sip`, `routes`, `trunk:<name>`, ...
    pub fn parse(name: &str) -> Option<Section> {
        Some(match name {
            "sip" => Section::Sip,
            "media" => Section::Media,
            "call" => Section::Call,
            "capture" => Section::Capture,
            "delivery" => Section::Delivery,
            "retention" => Section::Retention,
            "routes" => Section::Routes,
            other => {
                let n = other.strip_prefix("trunk:")?;
                if n.is_empty() {
                    return None;
                }
                Section::Trunk(n.to_string())
            }
        })
    }

    /// The name used in the database and in the API.
    pub fn name(&self) -> String {
        match self {
            Section::Sip => "sip".into(),
            Section::Media => "media".into(),
            Section::Call => "call".into(),
            Section::Capture => "capture".into(),
            Section::Delivery => "delivery".into(),
            Section::Retention => "retention".into(),
            Section::Routes => "routes".into(),
            Section::Trunk(n) => format!("trunk:{n}"),
        }
    }
}
