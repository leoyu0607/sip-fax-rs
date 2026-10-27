//! The [`FaxStore`] trait: everything fax-in, the web UI and the purge job need from the database.

use crate::db::BackendKind;
use crate::error::Result;
use crate::migrate::MigrateReport;
use crate::model::*;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

/// Backend independent store API. Implemented once on top of the four drivers
/// ([`crate::SqlStore`]); the same behaviour suite runs against each.
#[async_trait]
pub trait FaxStore: crate::admin::AdminStore + Send + Sync {
    /// Database family.
    fn backend(&self) -> BackendKind;
    /// Current schema version (0 = empty database).
    async fn schema_version(&self) -> Result<i32>;
    /// Apply pending migrations (`sip-faxd db migrate`). Refuses a database newer than this binary.
    async fn migrate(&self) -> Result<MigrateReport>;
    /// Close the pool.
    async fn close(&self);

    /// Insert a `receiving` record when the call is answered. Returns the new id, or
    /// `Error::Duplicate` when the Call-ID already exists.
    async fn insert_receiving(&self, new: &NewFax) -> Result<i64>;
    /// Record that `pages` pages have been received so far. Only moves forward, and only while the
    /// record is still `receiving`. Returns whether a row changed.
    async fn update_pages(&self, id: i64, pages: i32, at: DateTime<Utc>) -> Result<bool>;
    /// Write the final values. User fields (read flag, note) are preserved.
    async fn finalize(&self, id: i64, fin: &FaxFinal) -> Result<FaxRecord>;
    /// Record for backfill from `result.json`: insert if the Call-ID is unknown, otherwise
    /// finalize the existing row. Returns `(id, inserted)`.
    async fn upsert_finished(&self, new: &NewFax, fin: &FaxFinal) -> Result<(i64, bool)>;
    /// Fetch by id.
    async fn get(&self, id: i64) -> Result<Option<FaxRecord>>;
    /// Fetch by SIP Call-ID.
    async fn get_by_call_id(&self, call_id: &str) -> Result<Option<FaxRecord>>;
    /// Filtered, keyset-paginated listing, newest first (`started_at DESC, id DESC`).
    async fn list(&self, filter: &ListFilter, page: PageRequest) -> Result<Page<FaxRecord>>;
    /// The change feed (M6a, ADR-070): records by `(updated_at, id)` ascending, from `from`,
    /// changed no later than `until` (the caller leaves a settling margin, so that a write
    /// still in flight with an earlier time is not skipped), at most `limit` (1 to 200).
    async fn changes(
        &self,
        from: ChangesFrom,
        until: DateTime<Utc>,
        limit: u32,
    ) -> Result<Vec<FaxRecord>>;
    /// Number of unread records (any status), for the badge of the web interface.
    async fn count_unread(&self) -> Result<u64>;
    /// Number of records with `started_at >= since` (any status): "received today" of the status
    /// page, the day being the viewer's.
    async fn count_since(&self, since: DateTime<Utc>) -> Result<u64>;
    /// Number of delivery attempts in `status` (the status page counts `failed` and `giving_up`).
    async fn count_deliveries(&self, status: DeliveryStatus) -> Result<u64>;
    /// Set the shared read flag, recording who and when.
    async fn set_read(&self, id: i64, read: bool, by: &str) -> Result<FaxRecord>;
    /// Replace the note with an optimistic lock on `note_updated_at`. `expected_updated_at` is the
    /// value the caller last saw (`None` = the note was never edited). A mismatch returns
    /// `Error::Conflict`.
    async fn set_note(
        &self,
        id: i64,
        text: &str,
        by: &str,
        expected_updated_at: Option<DateTime<Utc>>,
    ) -> Result<FaxRecord>;
    /// Startup repair: finish every record still `receiving` (spec section 4). Returns the count.
    async fn fix_stale_receiving(&self) -> Result<u64>;

    /// Read one setting.
    async fn get_setting(&self, name: &str) -> Result<Option<Setting>>;
    /// All settings.
    async fn list_settings(&self) -> Result<Vec<Setting>>;
    /// Create or overwrite a setting.
    async fn set_setting(&self, name: &str, value: &str, by: Option<&str>) -> Result<()>;
    /// Insert the given defaults only for keys that do not exist yet (config seeding: the database
    /// wins afterwards). Returns the number of keys inserted.
    async fn seed_settings(&self, defaults: &[(&str, &str)]) -> Result<u64>;

    /// Records whose files are due for deletion: `ended_at < cutoff`, files not yet purged, not
    /// receiving, with at least one file path, `id > after_id`, ascending id.
    async fn files_due(
        &self,
        cutoff: DateTime<Utc>,
        after_id: i64,
        limit: u32,
    ) -> Result<Vec<PurgeRow>>;
    /// Number of records [`FaxStore::files_due`] would eventually return.
    async fn count_files_due(&self, cutoff: DateTime<Utc>) -> Result<u64>;
    /// Mark files as purged (record kept).
    async fn mark_files_purged(&self, ids: &[i64], at: DateTime<Utc>) -> Result<u64>;
    /// Records due for deletion: `ended_at < cutoff`, not receiving, `id > after_id`.
    async fn records_due(
        &self,
        cutoff: DateTime<Utc>,
        after_id: i64,
        limit: u32,
    ) -> Result<Vec<PurgeRow>>;
    /// Number of records [`FaxStore::records_due`] would eventually return.
    async fn count_records_due(&self, cutoff: DateTime<Utc>) -> Result<u64>;
    /// Delete records and their delivery attempts.
    async fn delete_records(&self, ids: &[i64]) -> Result<u64>;

    /// Insert a delivery attempt (used by M3c).
    async fn add_delivery(&self, d: &NewDelivery) -> Result<i64>;
    /// Delivery attempts of a fax, oldest first.
    async fn list_deliveries(&self, fax_id: i64) -> Result<Vec<DeliveryAttempt>>;
    /// Attempts that are due: `status = pending` and `next_try_at` is NULL or `<= now`, ordered by
    /// `next_try_at, id`, at most `limit` rows (M3c worker queue).
    async fn due_deliveries(&self, now: DateTime<Utc>, limit: u32) -> Result<Vec<DeliveryAttempt>>;
    /// Record the outcome of an attempt: new status, error text and next retry time. Returns
    /// whether a row changed.
    async fn update_delivery(
        &self,
        id: i64,
        status: DeliveryStatus,
        last_error: Option<&str>,
        next_try_at: Option<DateTime<Utc>>,
    ) -> Result<bool>;
}

/// Where a long-lived component gets its store from. The daemon's database handle connects
/// lazily (a database that is down at start must not stop the server), so components ask for the
/// store when they need it; tests and one-shot commands pass an already open store.
#[async_trait]
pub trait StoreSource: Send + Sync {
    /// The store, connecting first when necessary.
    async fn store(&self) -> Result<std::sync::Arc<dyn FaxStore>>;
}

#[async_trait]
impl StoreSource for std::sync::Arc<dyn FaxStore> {
    async fn store(&self) -> Result<std::sync::Arc<dyn FaxStore>> {
        Ok(self.clone())
    }
}
