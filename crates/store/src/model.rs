//! Store model types. These are deliberately independent of fax-in: M3b maps `CallResult` /
//! `CallEvent` into them.

use crate::error::{Error, Result};
use chrono::{DateTime, TimeZone, Utc};
use std::fmt;
use std::str::FromStr;

/// Maximum note length in characters (spec section 1.1).
pub const NOTE_MAX_CHARS: usize = 4000;
/// Maximum page size for [`crate::FaxStore::list`].
pub const MAX_PAGE: u32 = 200;
/// Page size used when the caller passes 0.
pub const DEFAULT_PAGE: u32 = 50;

/// Truncate to millisecond precision (the precision every backend stores).
pub fn trunc_ms(t: DateTime<Utc>) -> DateTime<Utc> {
    Utc.timestamp_millis_opt(t.timestamp_millis())
        .single()
        .expect("valid timestamp")
}

macro_rules! text_enum {
    ($(#[$m:meta])* $name:ident { $($(#[$vm:meta])* $v:ident => $s:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name { $($(#[$vm])* $v),+ }
        impl $name {
            /// Stable text stored in the database.
            pub fn as_str(&self) -> &'static str { match self { $(Self::$v => $s),+ } }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(self.as_str()) }
        }
        impl FromStr for $name {
            type Err = Error;
            fn from_str(s: &str) -> Result<Self> {
                match s { $($s => Ok(Self::$v),)+ other => Err(Error::Corrupt(format!(concat!("unknown ", stringify!($name), " {:?}"), other))) }
            }
        }
    };
}

text_enum! {
    /// Fax reception state (`faxes.fax_status`).
    FaxStatus {
        /// Call in progress.
        Receiving => "receiving",
        /// Complete fax received.
        Received => "received",
        /// Some pages received.
        Partial => "partial",
        /// Failed without pages.
        Failed => "failed",
        /// No fax signal (voice call).
        NoFax => "no_fax",
    }
}

text_enum! {
    /// Who ended the call (`faxes.hangup_direction`).
    HangupDirection {
        /// Remote side hung up.
        Remote => "remote",
        /// We hung up.
        Local => "local",
        /// Timeout, error or rejection.
        System => "system",
    }
}

text_enum! {
    /// Delivery attempt state (`delivery_attempts.status`).
    DeliveryStatus {
        /// Waiting for (re)try.
        Pending => "pending",
        /// Delivered.
        Done => "done",
        /// Last attempt failed, will retry.
        Failed => "failed",
        /// Gave up after max attempts.
        GivingUp => "giving_up",
    }
}

/// Data known when the call is answered (`CallEvent::Started`).
#[derive(Debug, Clone, PartialEq)]
pub struct NewFax {
    /// SIP Call-ID (unique).
    pub call_id: String,
    /// Trunk name.
    pub trunk: String,
    /// Calling number.
    pub ani: Option<String>,
    /// Header the ANI came from (`pai`/`rpid`/`from`/`anonymous`).
    pub ani_source: Option<String>,
    /// Called number.
    pub dnis: Option<String>,
    /// Header the DNIS came from (`ruri`/`to`/`diversion`).
    pub dnis_source: Option<String>,
    /// Time the INVITE arrived.
    pub started_at: DateTime<Utc>,
    /// Time the 200 OK was sent, if already known.
    pub answered_at: Option<DateTime<Utc>>,
}

/// Final values written when the call ends (`CallEvent::Finished`).
#[derive(Debug, Clone, PartialEq)]
pub struct FaxFinal {
    /// Hang-up time.
    pub ended_at: DateTime<Utc>,
    /// Overrides `answered_at` when `Some`.
    pub answered_at: Option<DateTime<Utc>>,
    /// Call length in seconds; computed from `answered_at`/`ended_at` when `None`.
    pub duration_sec: Option<i32>,
    /// Who hung up.
    pub hangup_direction: HangupDirection,
    /// Fixed reason code (spec section 3).
    pub hangup_code: String,
    /// Human readable reason.
    pub hangup_text: Option<String>,
    /// SIP status for rejections.
    pub sip_status: Option<i32>,
    /// T.30 result code.
    pub t30_code: Option<i32>,
    /// Final fax status (must not be `Receiving`).
    pub fax_status: FaxStatus,
    /// Total pages.
    pub pages: i32,
    /// Remote station identifier (TSI).
    pub remote_ident: Option<String>,
    /// Resolution label.
    pub resolution: Option<String>,
    /// Negotiated bit rate.
    pub bit_rate: Option<i32>,
    /// Compression label (MH/MR/MMR).
    pub compression: Option<String>,
    /// ECM used.
    pub ecm: bool,
    /// TIFF path relative to `data_dir`.
    pub tiff_path: Option<String>,
    /// TIFF size in bytes.
    pub tiff_size: Option<i64>,
    /// PDF path relative to `data_dir`.
    pub pdf_path: Option<String>,
    /// PDF size in bytes.
    pub pdf_size: Option<i64>,
}

/// One row of `faxes`.
#[derive(Debug, Clone, PartialEq)]
#[allow(missing_docs)] // fields mirror docs/database.md
pub struct FaxRecord {
    pub id: i64,
    pub call_id: String,
    pub trunk: String,
    pub ani: Option<String>,
    pub ani_source: Option<String>,
    pub dnis: Option<String>,
    pub dnis_source: Option<String>,
    pub remote_ident: Option<String>,
    pub started_at: DateTime<Utc>,
    pub answered_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub duration_sec: Option<i32>,
    pub hangup_direction: Option<HangupDirection>,
    pub hangup_code: Option<String>,
    pub hangup_text: Option<String>,
    pub sip_status: Option<i32>,
    pub t30_code: Option<i32>,
    pub fax_status: FaxStatus,
    pub pages: i32,
    pub resolution: Option<String>,
    pub bit_rate: Option<i32>,
    pub compression: Option<String>,
    pub ecm: bool,
    pub tiff_path: Option<String>,
    pub tiff_size: Option<i64>,
    pub pdf_path: Option<String>,
    pub pdf_size: Option<i64>,
    pub files_purged_at: Option<DateTime<Utc>>,
    pub is_read: bool,
    pub read_changed_by: Option<String>,
    pub read_changed_at: Option<DateTime<Utc>>,
    pub note: Option<String>,
    pub note_updated_by: Option<String>,
    pub note_updated_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Filters for [`crate::FaxStore::list`]. All set fields are ANDed.
#[derive(Debug, Clone, Default)]
pub struct ListFilter {
    /// Read / unread.
    pub is_read: Option<bool>,
    /// Fax status.
    pub fax_status: Option<FaxStatus>,
    /// `started_at >= from`.
    pub from: Option<DateTime<Utc>>,
    /// `started_at < to`.
    pub to: Option<DateTime<Utc>>,
    /// ANI prefix (literal; `%` and `_` are not wildcards).
    pub ani_prefix: Option<String>,
    /// DNIS prefix.
    pub dnis_prefix: Option<String>,
    /// Exact trunk name.
    pub trunk: Option<String>,
}

/// Opaque keyset cursor: position after (`started_at`, `id`) in `started_at DESC, id DESC` order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    /// `started_at` of the last row of the previous page, unix milliseconds.
    pub started_ms: i64,
    /// `id` of the last row of the previous page.
    pub id: i64,
}

impl Cursor {
    pub(crate) fn started_at(&self) -> DateTime<Utc> {
        Utc.timestamp_millis_opt(self.started_ms)
            .single()
            .expect("valid cursor time")
    }
}

impl fmt::Display for Cursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.started_ms, self.id)
    }
}

impl FromStr for Cursor {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        let bad = || Error::Invalid(format!("bad cursor {s:?}"));
        let (a, b) = s.rsplit_once('.').ok_or_else(bad)?;
        let started_ms: i64 = a.parse().map_err(|_| bad())?;
        let id: i64 = b.parse().map_err(|_| bad())?;
        if Utc.timestamp_millis_opt(started_ms).single().is_none() {
            return Err(bad());
        }
        Ok(Cursor { started_ms, id })
    }
}

/// Page request.
#[derive(Debug, Clone, Copy, Default)]
pub struct PageRequest {
    /// Continue after this cursor (`None` = first page).
    pub cursor: Option<Cursor>,
    /// Page size; 0 = default (50), capped at 200.
    pub limit: u32,
}

/// One page of results.
#[derive(Debug, Clone)]
pub struct Page<T> {
    /// Items, newest first.
    pub items: Vec<T>,
    /// Cursor for the next page, `None` at the end.
    pub next: Option<Cursor>,
}

/// Retention settings (spec section 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionPolicy {
    /// Delete TIFF/PDF/PNG after N days; 0 = never.
    pub files_days: u32,
    /// Delete the record after N days; 0 = never.
    pub records_days: u32,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            files_days: 90,
            records_days: 365,
        }
    }
}

impl RetentionPolicy {
    /// `records_days` must be >= `files_days` unless one of them is 0 (never).
    pub fn validate(&self) -> Result<()> {
        if self.files_days != 0 && self.records_days != 0 && self.records_days < self.files_days {
            return Err(Error::Invalid(format!(
                "retention.records_days ({}) must be >= retention.files_days ({})",
                self.records_days, self.files_days
            )));
        }
        Ok(())
    }
}

/// A row due for purging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PurgeRow {
    /// Fax id.
    pub id: i64,
    /// TIFF path (relative to `data_dir`) as stored.
    pub tiff_path: Option<String>,
    /// PDF path as stored.
    pub pdf_path: Option<String>,
    /// Already marked as files purged.
    pub files_purged: bool,
}

/// A stored setting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setting {
    /// Key (for example `retention.files_days`).
    pub name: String,
    /// Value.
    pub value: String,
    /// Last writer.
    pub updated_by: Option<String>,
    /// Last write time.
    pub updated_at: DateTime<Utc>,
}

/// One row of `delivery_attempts`.
#[derive(Debug, Clone, PartialEq)]
#[allow(missing_docs)]
pub struct DeliveryAttempt {
    pub id: i64,
    pub fax_id: i64,
    pub target: String,
    pub kind: String,
    pub attempt: i32,
    pub status: DeliveryStatus,
    pub last_error: Option<String>,
    pub next_try_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// New delivery attempt (M3c fills the delivery logic; the table is created now).
#[derive(Debug, Clone)]
pub struct NewDelivery {
    /// Fax id.
    pub fax_id: i64,
    /// Target description (folder path, address, URL).
    pub target: String,
    /// `folder` / `email` / `webhook`.
    pub kind: String,
    /// Attempt number.
    pub attempt: i32,
    /// Initial status.
    pub status: DeliveryStatus,
    /// Next retry time.
    pub next_try_at: Option<DateTime<Utc>>,
}
