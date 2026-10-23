//! The one [`FaxStore`] implementation, generic over the four [`Backend`] drivers.

use crate::clock::Clock;
use crate::db::*;
use crate::error::{Error, Result};
use crate::migrate;
use crate::model::*;
use crate::store::FaxStore;
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use std::sync::Arc;

const FAX_COLS: &str = "id, call_id, trunk, ani, ani_source, dnis, dnis_source, remote_ident, started_at, answered_at, \
ended_at, duration_sec, hangup_direction, hangup_code, hangup_text, sip_status, t30_code, fax_status, pages, resolution, \
bit_rate, compression, ecm, tiff_path, tiff_size, pdf_path, pdf_size, files_purged_at, is_read, read_changed_by, \
read_changed_at, note, note_updated_by, note_updated_at, created_at, updated_at";

use Kind::{Bool as B, Int as I, Str as S, Time as T};
const FAX_KINDS: &[Kind] = &[
    I, S, S, S, S, S, S, S, T, T, T, I, S, S, S, I, I, S, I, S, I, S, B, S, I, S, I, T, B, S, T, S,
    S, T, T, T,
];

const DELIVERY_COLS: &str =
    "id, fax_id, target, kind, attempt, status, last_error, next_try_at, created_at, updated_at";
const DELIVERY_KINDS: &[Kind] = &[I, I, S, S, I, S, S, T, T, T];

/// Store over any [`Backend`].
pub struct SqlStore {
    be: Box<dyn Backend>,
    clock: Arc<dyn Clock>,
}

pub(crate) fn tv(t: DateTime<Utc>) -> Val {
    Val::T(Some(trunc_ms(t)))
}
pub(crate) fn otv(t: Option<DateTime<Utc>>) -> Val {
    Val::T(t.map(trunc_ms))
}

pub(crate) fn check_len(field: &'static str, v: &str, max: usize) -> Result<()> {
    // UTF-16 units: the strictest measure among the backends (NVARCHAR counts code units).
    let len = v.encode_utf16().count();
    if len > max {
        Err(Error::TooLong { field, len, max })
    } else {
        Ok(())
    }
}
pub(crate) fn check_opt(field: &'static str, v: &Option<String>, max: usize) -> Result<()> {
    v.as_deref().map_or(Ok(()), |s| check_len(field, s, max))
}

fn read_fax(vals: Vec<Val>) -> Result<FaxRecord> {
    let mut r = RowReader::new(vals);
    let parse_dir = |s: Option<String>| s.map(|s| s.parse::<HangupDirection>()).transpose();
    Ok(FaxRecord {
        id: r.i64(),
        call_id: r.str(),
        trunk: r.str(),
        ani: r.opt_str(),
        ani_source: r.opt_str(),
        dnis: r.opt_str(),
        dnis_source: r.opt_str(),
        remote_ident: r.opt_str(),
        started_at: r.time(),
        answered_at: r.opt_time(),
        ended_at: r.opt_time(),
        duration_sec: r.opt_i32(),
        hangup_direction: parse_dir(r.opt_str())?,
        hangup_code: r.opt_str(),
        hangup_text: r.opt_str(),
        sip_status: r.opt_i32(),
        t30_code: r.opt_i32(),
        fax_status: r.str().parse()?,
        pages: r.i32(),
        resolution: r.opt_str(),
        bit_rate: r.opt_i32(),
        compression: r.opt_str(),
        ecm: r.bool(),
        tiff_path: r.opt_str(),
        tiff_size: r.int(),
        pdf_path: r.opt_str(),
        pdf_size: r.int(),
        files_purged_at: r.opt_time(),
        is_read: r.bool(),
        read_changed_by: r.opt_str(),
        read_changed_at: r.opt_time(),
        note: r.opt_str(),
        note_updated_by: r.opt_str(),
        note_updated_at: r.opt_time(),
        created_at: r.time(),
        updated_at: r.time(),
    })
}

fn duration_between(answered: Option<DateTime<Utc>>, ended: DateTime<Utc>) -> i32 {
    match answered {
        Some(a) => {
            let ms = (ended - a).num_milliseconds().max(0);
            ((ms + 500) / 1000) as i32
        }
        None => 0,
    }
}

/// Escape LIKE wildcards with `!` (portable `ESCAPE '!'`).
fn like_escape(p: &str) -> String {
    let mut s = String::with_capacity(p.len() + 2);
    for c in p.chars() {
        if matches!(c, '%' | '_' | '!' | '[') {
            s.push('!');
        }
        s.push(c);
    }
    s
}

fn like_prefix(p: &str) -> String {
    format!("{}%", like_escape(p))
}

/// `%text%`, lower case (compared with `LOWER(column)`: PostgreSQL's LIKE is case sensitive).
fn like_contains(p: &str) -> String {
    format!("%{}%", like_escape(&p.to_lowercase()))
}

fn id_list(ids: &[i64]) -> String {
    ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
}

impl SqlStore {
    pub(crate) fn now(&self) -> DateTime<Utc> {
        self.clock.now()
    }

    pub(crate) fn new(be: Box<dyn Backend>, clock: Arc<dyn Clock>) -> Self {
        Self { be, clock }
    }

    /// Raw driver access (tests and diagnostics only).
    #[doc(hidden)]
    pub fn raw(&self) -> &dyn Backend {
        self.be.as_ref()
    }

    /// `(select_prefix, limit_suffix)` for a row limit: `TOP (n)` on MSSQL, `LIMIT n` elsewhere.
    pub(crate) fn limit(&self, n: u32) -> (String, String) {
        if self.be.kind() == BackendKind::Mssql {
            (format!("TOP ({n})"), String::new())
        } else {
            (String::new(), format!("LIMIT {n}"))
        }
    }

    async fn one_fax(&self, where_sql: &str, params: &[Val]) -> Result<Option<FaxRecord>> {
        let sql = format!("SELECT {FAX_COLS} FROM faxes WHERE {where_sql}");
        let rows = self.be.query(&sql, params, FAX_KINDS).await?;
        rows.into_iter().next().map(read_fax).transpose()
    }

    async fn require(&self, id: i64) -> Result<FaxRecord> {
        self.get(id)
            .await?
            .ok_or_else(|| Error::NotFound(format!("fax {id}")))
    }

    fn purge_rows(rows: Vec<Vec<Val>>) -> Vec<PurgeRow> {
        rows.into_iter()
            .map(|v| {
                let mut r = RowReader::new(v);
                PurgeRow {
                    id: r.i64(),
                    tiff_path: r.opt_str(),
                    pdf_path: r.opt_str(),
                    files_purged: r.opt_time().is_some(),
                }
            })
            .collect()
    }

    async fn count(&self, sql: &str, params: &[Val]) -> Result<u64> {
        let rows = self.be.query(sql, params, &[Kind::Int]).await?;
        Ok(match rows.first().and_then(|r| r.first()) {
            Some(Val::I(Some(n))) => *n as u64,
            _ => 0,
        })
    }
}

const PURGE_COLS: &str = "id, tiff_path, pdf_path, files_purged_at";
const PURGE_KINDS: &[Kind] = &[I, S, S, T];
const RECEIVING: &str = "receiving";

#[async_trait]
impl FaxStore for SqlStore {
    fn backend(&self) -> BackendKind {
        self.be.kind()
    }

    async fn schema_version(&self) -> Result<i32> {
        migrate::current_version(self.be.as_ref()).await
    }

    async fn migrate(&self) -> Result<migrate::MigrateReport> {
        migrate::migrate(self.be.as_ref(), self.clock.as_ref()).await
    }

    async fn close(&self) {
        self.be.close().await;
    }

    async fn insert_receiving(&self, n: &NewFax) -> Result<i64> {
        check_len("call_id", &n.call_id, 255)?;
        check_len("trunk", &n.trunk, 64)?;
        check_opt("ani", &n.ani, 64)?;
        check_opt("ani_source", &n.ani_source, 16)?;
        check_opt("dnis", &n.dnis, 64)?;
        check_opt("dnis_source", &n.dnis_source, 16)?;
        let now = self.clock.now();
        self.be
            .insert_id(
                "INSERT INTO faxes (call_id, trunk, ani, ani_source, dnis, dnis_source, started_at, answered_at, \
                 fax_status, pages, ecm, is_read, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                &[
                    n.call_id.clone().into(),
                    n.trunk.clone().into(),
                    n.ani.clone().into(),
                    n.ani_source.clone().into(),
                    n.dnis.clone().into(),
                    n.dnis_source.clone().into(),
                    tv(n.started_at),
                    otv(n.answered_at),
                    RECEIVING.into(),
                    0.into(),
                    false.into(),
                    false.into(),
                    tv(now),
                    tv(now),
                ],
            )
            .await
            .map_err(|e| match e {
                Error::Duplicate(_) => Error::Duplicate(format!("call_id {:?} already exists", n.call_id)),
                other => other,
            })
    }

    async fn update_pages(&self, id: i64, pages: i32, at: DateTime<Utc>) -> Result<bool> {
        let n = self
            .be
            .execute(
                "UPDATE faxes SET pages = ?, updated_at = ? WHERE id = ? AND fax_status = ? AND pages < ?",
                &[pages.into(), tv(at), id.into(), RECEIVING.into(), pages.into()],
            )
            .await?;
        Ok(n > 0)
    }

    async fn finalize(&self, id: i64, f: &FaxFinal) -> Result<FaxRecord> {
        if f.fax_status == FaxStatus::Receiving {
            return Err(Error::Invalid("finalize with fax_status receiving".into()));
        }
        check_len("hangup_code", &f.hangup_code, 32)?;
        check_opt("hangup_text", &f.hangup_text, 255)?;
        check_opt("remote_ident", &f.remote_ident, 32)?;
        check_opt("resolution", &f.resolution, 12)?;
        check_opt("compression", &f.compression, 8)?;
        check_opt("tiff_path", &f.tiff_path, 1024)?;
        check_opt("pdf_path", &f.pdf_path, 1024)?;
        let cur = self.require(id).await?;
        let answered = f.answered_at.or(cur.answered_at).map(trunc_ms);
        let ended = trunc_ms(f.ended_at);
        let duration = f
            .duration_sec
            .unwrap_or_else(|| duration_between(answered, ended));
        let affected = self
            .be
            .execute(
                "UPDATE faxes SET answered_at = ?, ended_at = ?, duration_sec = ?, hangup_direction = ?, hangup_code = ?, \
                 hangup_text = ?, sip_status = ?, t30_code = ?, fax_status = ?, pages = ?, remote_ident = ?, resolution = ?, \
                 bit_rate = ?, compression = ?, ecm = ?, tiff_path = ?, tiff_size = ?, pdf_path = ?, pdf_size = ?, \
                 updated_at = ? WHERE id = ?",
                &[
                    otv(answered),
                    tv(ended),
                    duration.into(),
                    f.hangup_direction.as_str().into(),
                    f.hangup_code.clone().into(),
                    f.hangup_text.clone().into(),
                    f.sip_status.into(),
                    f.t30_code.into(),
                    f.fax_status.as_str().into(),
                    f.pages.into(),
                    f.remote_ident.clone().into(),
                    f.resolution.clone().into(),
                    f.bit_rate.into(),
                    f.compression.clone().into(),
                    f.ecm.into(),
                    f.tiff_path.clone().into(),
                    f.tiff_size.into(),
                    f.pdf_path.clone().into(),
                    f.pdf_size.into(),
                    tv(self.clock.now()),
                    id.into(),
                ],
            )
            .await?;
        if affected == 0 {
            return Err(Error::NotFound(format!("fax {id}")));
        }
        self.require(id).await
    }

    async fn upsert_finished(&self, new: &NewFax, fin: &FaxFinal) -> Result<(i64, bool)> {
        match self.insert_receiving(new).await {
            Ok(id) => {
                self.finalize(id, fin).await?;
                Ok((id, true))
            }
            Err(Error::Duplicate(_)) => {
                let rec = self
                    .get_by_call_id(&new.call_id)
                    .await?
                    .ok_or_else(|| Error::NotFound(format!("call_id {}", new.call_id)))?;
                self.finalize(rec.id, fin).await?;
                Ok((rec.id, false))
            }
            Err(e) => Err(e),
        }
    }

    async fn get(&self, id: i64) -> Result<Option<FaxRecord>> {
        self.one_fax("id = ?", &[id.into()]).await
    }

    async fn get_by_call_id(&self, call_id: &str) -> Result<Option<FaxRecord>> {
        self.one_fax("call_id = ?", &[call_id.into()]).await
    }

    async fn list(&self, filter: &ListFilter, page: PageRequest) -> Result<Page<FaxRecord>> {
        let limit = if page.limit == 0 {
            DEFAULT_PAGE
        } else {
            page.limit.min(MAX_PAGE)
        };
        let mut conds: Vec<String> = Vec::new();
        let mut params: Vec<Val> = Vec::new();
        if let Some(r) = filter.is_read {
            conds.push("is_read = ?".into());
            params.push(r.into());
        }
        if let Some(s) = filter.fax_status {
            conds.push("fax_status = ?".into());
            params.push(s.as_str().into());
        }
        if let Some(t) = filter.from {
            conds.push("started_at >= ?".into());
            params.push(tv(t));
        }
        if let Some(t) = filter.to {
            conds.push("started_at < ?".into());
            params.push(tv(t));
        }
        if let Some(p) = &filter.ani_prefix {
            conds.push("ani LIKE ? ESCAPE '!'".into());
            params.push(like_prefix(p).into());
        }
        if let Some(p) = &filter.dnis_prefix {
            conds.push("dnis LIKE ? ESCAPE '!'".into());
            params.push(like_prefix(p).into());
        }
        if let Some(t) = &filter.trunk {
            conds.push("trunk = ?".into());
            params.push(t.clone().into());
        }
        if let Some(q) = &filter.q {
            let cols = ["ani", "dnis", "remote_ident", "note"];
            conds.push(format!(
                "({})",
                cols.map(|c| format!("LOWER({c}) LIKE ? ESCAPE '!'"))
                    .join(" OR ")
            ));
            for _ in cols {
                params.push(like_contains(q).into());
            }
        }
        if let Some(c) = page.cursor {
            conds.push("(started_at < ? OR (started_at = ? AND id < ?))".into());
            params.push(tv(c.started_at()));
            params.push(tv(c.started_at()));
            params.push(c.id.into());
        }
        let where_sql = if conds.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conds.join(" AND "))
        };
        let (top, lim) = self.limit(limit + 1);
        let sql = format!(
            "SELECT {top} {FAX_COLS} FROM faxes {where_sql} ORDER BY started_at DESC, id DESC {lim}"
        );
        let rows = self.be.query(&sql, &params, FAX_KINDS).await?;
        let mut items = rows.into_iter().map(read_fax).collect::<Result<Vec<_>>>()?;
        let next = if items.len() > limit as usize {
            items.truncate(limit as usize);
            items.last().map(|l| Cursor {
                started_ms: l.started_at.timestamp_millis(),
                id: l.id,
            })
        } else {
            None
        };
        Ok(Page { items, next })
    }

    async fn count_unread(&self) -> Result<u64> {
        self.count(
            "SELECT COUNT(*) FROM faxes WHERE is_read = ?",
            &[false.into()],
        )
        .await
    }

    async fn count_since(&self, since: DateTime<Utc>) -> Result<u64> {
        self.count(
            "SELECT COUNT(*) FROM faxes WHERE started_at >= ?",
            &[tv(since)],
        )
        .await
    }

    async fn count_deliveries(&self, status: DeliveryStatus) -> Result<u64> {
        self.count(
            "SELECT COUNT(*) FROM delivery_attempts WHERE status = ?",
            &[status.as_str().into()],
        )
        .await
    }

    async fn set_read(&self, id: i64, read: bool, by: &str) -> Result<FaxRecord> {
        check_len("by", by, 64)?;
        let now = self.clock.now();
        self.be
            .execute(
                "UPDATE faxes SET is_read = ?, read_changed_by = ?, read_changed_at = ?, updated_at = ? WHERE id = ?",
                &[read.into(), by.into(), tv(now), tv(now), id.into()],
            )
            .await?;
        self.require(id).await
    }

    async fn set_note(
        &self,
        id: i64,
        text: &str,
        by: &str,
        expected: Option<DateTime<Utc>>,
    ) -> Result<FaxRecord> {
        check_len("by", by, 64)?;
        let chars = text.chars().count();
        if chars > NOTE_MAX_CHARS {
            return Err(Error::TooLong {
                field: "note",
                len: chars,
                max: NOTE_MAX_CHARS,
            });
        }
        let cur = self.require(id).await?;
        if cur.note_updated_at != expected.map(trunc_ms) {
            return Err(Error::Conflict(format!(
                "note of fax {id} was changed by {}",
                cur.note_updated_by.as_deref().unwrap_or("someone else")
            )));
        }
        // Strictly newer than the value being replaced, so two writers holding the same
        // `expected` can never both succeed even under a frozen clock.
        let mut new_ts = trunc_ms(self.clock.now());
        if let Some(c) = cur.note_updated_at {
            new_ts = new_ts.max(c + Duration::milliseconds(1));
        }
        let (guard, mut params): (&str, Vec<Val>) = match cur.note_updated_at {
            Some(c) => ("note_updated_at = ?", vec![tv(c)]),
            None => ("note_updated_at IS NULL", vec![]),
        };
        let mut all = vec![text.into(), by.into(), tv(new_ts), tv(new_ts), id.into()];
        all.append(&mut params);
        let sql = format!(
            "UPDATE faxes SET note = ?, note_updated_by = ?, note_updated_at = ?, updated_at = ? WHERE id = ? AND {guard}"
        );
        if self.be.execute(&sql, &all).await? == 0 {
            return Err(Error::Conflict(format!(
                "note of fax {id} was changed concurrently"
            )));
        }
        self.require(id).await
    }

    async fn fix_stale_receiving(&self) -> Result<u64> {
        let rows = self
            .be
            .query(
                "SELECT id, pages, answered_at FROM faxes WHERE fax_status = ?",
                &[RECEIVING.into()],
                &[I, I, T],
            )
            .await?;
        let now = trunc_ms(self.clock.now());
        let mut fixed = 0;
        for v in rows {
            let mut r = RowReader::new(v);
            let (id, pages, answered) = (r.i64(), r.i32(), r.opt_time());
            let status = if pages >= 1 {
                FaxStatus::Partial
            } else {
                FaxStatus::Failed
            };
            let n = self
                .be
                .execute(
                    "UPDATE faxes SET fax_status = ?, hangup_code = ?, hangup_direction = ?, hangup_text = ?, ended_at = ?, \
                     duration_sec = ?, updated_at = ? WHERE id = ? AND fax_status = ?",
                    &[
                        status.as_str().into(),
                        "shutdown".into(),
                        HangupDirection::System.as_str().into(),
                        "服務中斷時仍在接收".into(),
                        tv(now),
                        duration_between(answered, now).into(),
                        tv(now),
                        id.into(),
                        RECEIVING.into(),
                    ],
                )
                .await?;
            fixed += n;
        }
        Ok(fixed)
    }

    async fn get_setting(&self, name: &str) -> Result<Option<Setting>> {
        let rows = self
            .be
            .query(
                "SELECT name, value, updated_by, updated_at FROM settings WHERE name = ?",
                &[name.into()],
                &[S, S, S, T],
            )
            .await?;
        Ok(rows.into_iter().next().map(read_setting))
    }

    async fn list_settings(&self) -> Result<Vec<Setting>> {
        let rows = self
            .be
            .query(
                "SELECT name, value, updated_by, updated_at FROM settings ORDER BY name",
                &[],
                &[S, S, S, T],
            )
            .await?;
        Ok(rows.into_iter().map(read_setting).collect())
    }

    async fn set_setting(&self, name: &str, value: &str, by: Option<&str>) -> Result<()> {
        check_len("setting name", name, 64)?;
        if let Some(b) = by {
            check_len("by", b, 64)?;
        }
        let now = tv(self.clock.now());
        let n = self
            .be
            .execute(
                "UPDATE settings SET value = ?, updated_by = ?, updated_at = ? WHERE name = ?",
                &[value.into(), by.into(), now.clone(), name.into()],
            )
            .await?;
        if n == 0 {
            match self
                .be
                .execute("INSERT INTO settings (name, value, updated_by, updated_at) VALUES (?, ?, ?, ?)", &[name.into(), value.into(), by.into(), now.clone()])
                .await
            {
                Ok(_) => {}
                // Lost an insert race: the other writer created it, so update.
                Err(Error::Duplicate(_)) => {
                    self.be
                        .execute("UPDATE settings SET value = ?, updated_by = ?, updated_at = ? WHERE name = ?", &[value.into(), by.into(), now, name.into()])
                        .await?;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    async fn seed_settings(&self, defaults: &[(&str, &str)]) -> Result<u64> {
        let mut inserted = 0;
        for (k, v) in defaults {
            check_len("setting name", k, 64)?;
            if self.get_setting(k).await?.is_some() {
                continue;
            }
            match self
                .be
                .execute(
                    "INSERT INTO settings (name, value, updated_by, updated_at) VALUES (?, ?, ?, ?)",
                    &[(*k).into(), (*v).into(), "config".into(), tv(self.clock.now())],
                )
                .await
            {
                Ok(n) => inserted += n,
                Err(Error::Duplicate(_)) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(inserted)
    }

    async fn files_due(
        &self,
        cutoff: DateTime<Utc>,
        after_id: i64,
        limit: u32,
    ) -> Result<Vec<PurgeRow>> {
        let (top, lim) = self.limit(limit);
        let sql = format!(
            "SELECT {top} {PURGE_COLS} FROM faxes WHERE ended_at < ? AND files_purged_at IS NULL AND fax_status <> ? \
             AND (tiff_path IS NOT NULL OR pdf_path IS NOT NULL) AND id > ? ORDER BY id {lim}"
        );
        Ok(Self::purge_rows(
            self.be
                .query(
                    &sql,
                    &[tv(cutoff), RECEIVING.into(), after_id.into()],
                    PURGE_KINDS,
                )
                .await?,
        ))
    }

    async fn count_files_due(&self, cutoff: DateTime<Utc>) -> Result<u64> {
        self.count(
            "SELECT COUNT(*) FROM faxes WHERE ended_at < ? AND files_purged_at IS NULL AND fax_status <> ? \
             AND (tiff_path IS NOT NULL OR pdf_path IS NOT NULL)",
            &[tv(cutoff), RECEIVING.into()],
        )
        .await
    }

    async fn mark_files_purged(&self, ids: &[i64], at: DateTime<Utc>) -> Result<u64> {
        if ids.is_empty() {
            return Ok(0);
        }
        let sql = format!(
            "UPDATE faxes SET files_purged_at = ?, updated_at = ? WHERE id IN ({})",
            id_list(ids)
        );
        self.be.execute(&sql, &[tv(at), tv(at)]).await
    }

    async fn records_due(
        &self,
        cutoff: DateTime<Utc>,
        after_id: i64,
        limit: u32,
    ) -> Result<Vec<PurgeRow>> {
        let (top, lim) = self.limit(limit);
        let sql = format!(
            "SELECT {top} {PURGE_COLS} FROM faxes WHERE ended_at < ? AND fax_status <> ? AND id > ? ORDER BY id {lim}"
        );
        Ok(Self::purge_rows(
            self.be
                .query(
                    &sql,
                    &[tv(cutoff), RECEIVING.into(), after_id.into()],
                    PURGE_KINDS,
                )
                .await?,
        ))
    }

    async fn count_records_due(&self, cutoff: DateTime<Utc>) -> Result<u64> {
        self.count(
            "SELECT COUNT(*) FROM faxes WHERE ended_at < ? AND fax_status <> ?",
            &[tv(cutoff), RECEIVING.into()],
        )
        .await
    }

    async fn delete_records(&self, ids: &[i64]) -> Result<u64> {
        if ids.is_empty() {
            return Ok(0);
        }
        let list = id_list(ids);
        self.be
            .execute(
                &format!("DELETE FROM delivery_attempts WHERE fax_id IN ({list})"),
                &[],
            )
            .await?;
        self.be
            .execute(&format!("DELETE FROM faxes WHERE id IN ({list})"), &[])
            .await
    }

    async fn add_delivery(&self, d: &NewDelivery) -> Result<i64> {
        check_len("target", &d.target, 255)?;
        check_len("kind", &d.kind, 16)?;
        let now = tv(self.clock.now());
        self.be
            .insert_id(
                "INSERT INTO delivery_attempts (fax_id, target, kind, attempt, status, last_error, next_try_at, created_at, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                &[
                    d.fax_id.into(),
                    d.target.clone().into(),
                    d.kind.clone().into(),
                    d.attempt.into(),
                    d.status.as_str().into(),
                    Val::S(None),
                    otv(d.next_try_at),
                    now.clone(),
                    now,
                ],
            )
            .await
    }

    async fn list_deliveries(&self, fax_id: i64) -> Result<Vec<DeliveryAttempt>> {
        let sql =
            format!("SELECT {DELIVERY_COLS} FROM delivery_attempts WHERE fax_id = ? ORDER BY id");
        let rows = self
            .be
            .query(&sql, &[fax_id.into()], DELIVERY_KINDS)
            .await?;
        rows.into_iter().map(read_delivery).collect()
    }

    async fn due_deliveries(&self, now: DateTime<Utc>, limit: u32) -> Result<Vec<DeliveryAttempt>> {
        let (top, lim) = self.limit(limit);
        let sql = format!(
            "SELECT {top} {DELIVERY_COLS} FROM delivery_attempts WHERE status = ?              AND (next_try_at IS NULL OR next_try_at <= ?) ORDER BY CASE WHEN next_try_at IS NULL THEN 0 ELSE 1 END, next_try_at, id {lim}"
        );
        let rows = self
            .be
            .query(
                &sql,
                &[DeliveryStatus::Pending.as_str().into(), tv(now)],
                DELIVERY_KINDS,
            )
            .await?;
        rows.into_iter().map(read_delivery).collect()
    }

    async fn update_delivery(
        &self,
        id: i64,
        status: DeliveryStatus,
        last_error: Option<&str>,
        next_try_at: Option<DateTime<Utc>>,
    ) -> Result<bool> {
        if let Some(e) = last_error {
            check_len("last_error", e, 4000)?;
        }
        let n = self
            .be
            .execute(
                "UPDATE delivery_attempts SET status = ?, last_error = ?, next_try_at = ?, updated_at = ? WHERE id = ?",
                &[
                    status.as_str().into(),
                    Val::S(last_error.map(str::to_string)),
                    otv(next_try_at),
                    tv(self.clock.now()),
                    id.into(),
                ],
            )
            .await?;
        Ok(n > 0)
    }
}

fn read_delivery(v: Vec<Val>) -> Result<DeliveryAttempt> {
    let mut r = RowReader::new(v);
    Ok(DeliveryAttempt {
        id: r.i64(),
        fax_id: r.i64(),
        target: r.str(),
        kind: r.str(),
        attempt: r.i32(),
        status: r.str().parse()?,
        last_error: r.opt_str(),
        next_try_at: r.opt_time(),
        created_at: r.time(),
        updated_at: r.time(),
    })
}

fn read_setting(v: Vec<Val>) -> Setting {
    let mut r = RowReader::new(v);
    Setting {
        name: r.str(),
        value: r.str(),
        updated_by: r.opt_str(),
        updated_at: r.time(),
    }
}
