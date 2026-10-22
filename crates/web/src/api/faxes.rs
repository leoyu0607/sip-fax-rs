//! `/api/v1/faxes/*` (spec 3): the list, one fax with its deliveries, read flag, note, page
//! previews, the files, a redelivery.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use accounts::Capability;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use store::{
    Cursor, DeliveryAttempt, FaxRecord, FaxStatus, ListFilter, PageRequest, PngKind, PreviewError,
};

use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::session::Session;
use crate::state::AppState;

/// The largest page size of the list.
pub const MAX_LIMIT: u32 = 200;

/// The routes.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/faxes", get(list))
        .route("/faxes/{id}", get(show))
        .route("/faxes/{id}/read", put(set_read))
        .route("/faxes/{id}/note", put(set_note))
        .route("/faxes/{id}/pages/{page}", get(page_png))
        .route("/faxes/{id}/{file}", get(download))
        .route("/faxes/{id}/redeliver", post(redeliver))
}

// ------------------------------------------------------------------------------ values

/// A fax as the API shows it. File locations stay on the server: the client only learns which
/// files there are.
pub fn fax_json(r: &FaxRecord) -> Value {
    let purged = r.files_purged_at.is_some();
    json!({
        "id": r.id,
        "call_id": r.call_id,
        "trunk": r.trunk,
        "ani": r.ani,
        "ani_source": r.ani_source,
        "dnis": r.dnis,
        "dnis_source": r.dnis_source,
        "remote_ident": r.remote_ident,
        "started_at": r.started_at,
        "answered_at": r.answered_at,
        "ended_at": r.ended_at,
        "duration_sec": r.duration_sec,
        "hangup_direction": r.hangup_direction.map(|d| d.as_str()),
        "hangup_code": r.hangup_code,
        "hangup_text": r.hangup_text,
        "sip_status": r.sip_status,
        "t30_code": r.t30_code,
        "fax_status": r.fax_status.to_string(),
        "pages": r.pages,
        "resolution": r.resolution,
        "bit_rate": r.bit_rate,
        "compression": r.compression,
        "ecm": r.ecm,
        "has_tiff": r.tiff_path.is_some() && !purged,
        "has_pdf": r.pdf_path.is_some() && !purged,
        "tiff_size": r.tiff_size,
        "pdf_size": r.pdf_size,
        "files_purged_at": r.files_purged_at,
        "is_read": r.is_read,
        "read_changed_by": r.read_changed_by,
        "read_changed_at": r.read_changed_at,
        "note": r.note,
        "note_updated_by": r.note_updated_by,
        "note_updated_at": r.note_updated_at,
        "created_at": r.created_at,
        "updated_at": r.updated_at,
    })
}

/// A delivery attempt; secrets in a webhook target are masked.
pub fn delivery_json(d: &DeliveryAttempt) -> Value {
    let target = match d.target.split_once(':') {
        Some(("webhook", url)) => format!("webhook:{}", delivery::mask::mask_http_url(url)),
        _ => d.target.clone(),
    };
    json!({
        "id": d.id,
        "target": target,
        "kind": d.kind,
        "attempt": d.attempt,
        "status": d.status.to_string(),
        "last_error": d.last_error,
        "next_try_at": d.next_try_at,
        "created_at": d.created_at,
        "updated_at": d.updated_at,
    })
}

/// A fax id from the path: a positive integer, anything else is 404 (no fax can have it).
fn fax_id(raw: &str) -> Result<i64, ApiError> {
    raw.parse::<i64>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| ApiError::not_found("fax"))
}

fn store_error(e: store::Error) -> ApiError {
    match e {
        store::Error::NotFound(_) => ApiError::not_found("fax"),
        store::Error::Unavailable(_) => ApiError::unavailable(),
        store::Error::TooLong { field, max, .. } => ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "too_long",
            format!("{field} is longer than {max} characters"),
        ),
        other => ApiError::internal(other),
    }
}

async fn record(st: &AppState, id: i64) -> Result<FaxRecord, ApiError> {
    st.store()
        .await?
        .get(id)
        .await
        .map_err(store_error)?
        .ok_or_else(|| ApiError::not_found("fax"))
}

// ------------------------------------------------------------------------------ list

fn opt_time(q: &HashMap<String, String>, k: &str) -> Result<Option<DateTime<Utc>>, ApiError> {
    q.get(k)
        .filter(|v| !v.is_empty())
        .map(|v| {
            DateTime::parse_from_rfc3339(v)
                .map(|t| t.to_utc())
                .map_err(|_| ApiError::bad_request(format!("{k} must be an RFC 3339 time")))
        })
        .transpose()
}

fn opt_text(q: &HashMap<String, String>, k: &str) -> Option<String> {
    q.get(k)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// The filter and page of `GET /faxes` (unknown parameters are refused, so a typo does not
/// silently list everything).
fn list_query(q: &HashMap<String, String>) -> Result<(ListFilter, PageRequest), ApiError> {
    const KNOWN: &[&str] = &[
        "is_read", "status", "from", "to", "ani", "dnis", "trunk", "cursor", "limit",
    ];
    if let Some(k) = q.keys().find(|k| !KNOWN.contains(&k.as_str())) {
        return Err(ApiError::bad_request(format!("unknown parameter {k:?}")));
    }
    let is_read = match q.get("is_read").map(String::as_str) {
        None | Some("") => None,
        Some("true") => Some(true),
        Some("false") => Some(false),
        Some(_) => return Err(ApiError::bad_request("is_read must be true or false")),
    };
    let fax_status = match opt_text(q, "status") {
        None => None,
        Some(s) => Some(s.parse::<FaxStatus>().map_err(|_| {
            ApiError::bad_request("status must be receiving, received, partial, failed or no_fax")
        })?),
    };
    let cursor = match opt_text(q, "cursor") {
        None => None,
        Some(c) => Some(
            c.parse::<Cursor>()
                .map_err(|_| ApiError::bad_request("the cursor is not valid"))?,
        ),
    };
    let limit = match opt_text(q, "limit") {
        None => 0,
        Some(l) => match l.parse::<u32>() {
            Ok(n) if (1..=MAX_LIMIT).contains(&n) => n,
            _ => return Err(ApiError::bad_request("limit must be 1 to 200")),
        },
    };
    let filter = ListFilter {
        is_read,
        fax_status,
        from: opt_time(q, "from")?,
        to: opt_time(q, "to")?,
        ani_prefix: opt_text(q, "ani"),
        dnis_prefix: opt_text(q, "dnis"),
        trunk: opt_text(q, "trunk"),
    };
    Ok((filter, PageRequest { cursor, limit }))
}

async fn list(
    State(st): State<AppState>,
    s: Session,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    s.need(Capability::ViewFaxes)?;
    let (filter, page) = list_query(&q)?;
    let p = st
        .store()
        .await?
        .list(&filter, page)
        .await
        .map_err(store_error)?;
    Ok(Json(json!({
        "items": p.items.iter().map(fax_json).collect::<Vec<_>>(),
        "next_cursor": p.next.map(|c| c.to_string()),
    })))
}

// ------------------------------------------------------------------------------ one fax

async fn show(
    State(st): State<AppState>,
    s: Session,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    s.need(Capability::ViewFaxes)?;
    let id = fax_id(&id)?;
    let r = record(&st, id).await?;
    let mut v = fax_json(&r);
    if s.0.require(Capability::ViewDeliveries).is_ok() {
        let d = st
            .store()
            .await?
            .list_deliveries(id)
            .await
            .map_err(store_error)?;
        v["deliveries"] = Value::Array(d.iter().map(delivery_json).collect());
    }
    Ok(Json(v))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadBody {
    read: bool,
}

async fn set_read(
    State(st): State<AppState>,
    s: Session,
    UrlPath(id): UrlPath<String>,
    ApiJson(b): ApiJson<ReadBody>,
) -> Result<Json<Value>, ApiError> {
    s.need(Capability::EditFaxMeta)?;
    let id = fax_id(&id)?;
    let r = st
        .store()
        .await?
        .set_read(id, b.read, s.username())
        .await
        .map_err(store_error)?;
    Ok(Json(fax_json(&r)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoteBody {
    text: String,
    /// `note_updated_at` the client last saw (`null`: the note was never edited).
    expected_updated_at: Option<DateTime<Utc>>,
}

async fn set_note(
    State(st): State<AppState>,
    s: Session,
    UrlPath(id): UrlPath<String>,
    ApiJson(b): ApiJson<NoteBody>,
) -> Result<Json<Value>, ApiError> {
    s.need(Capability::EditFaxMeta)?;
    let id = fax_id(&id)?;
    let store = st.store().await?;
    match store
        .set_note(id, &b.text, s.username(), b.expected_updated_at)
        .await
    {
        Ok(r) => Ok(Json(fax_json(&r))),
        Err(store::Error::Conflict(_)) => {
            // W04: the current note, who wrote it and when, so the client can merge
            let cur = store
                .get(id)
                .await
                .map_err(store_error)?
                .ok_or_else(|| ApiError::not_found("fax"))?;
            let mut f = Map::new();
            f.insert("note".into(), json!(cur.note));
            f.insert("note_updated_by".into(), json!(cur.note_updated_by));
            f.insert("note_updated_at".into(), json!(cur.note_updated_at));
            Err(ApiError::new(
                StatusCode::CONFLICT,
                "conflict",
                "the note was changed by someone else",
            )
            .with_fields(f))
        }
        Err(e) => Err(store_error(e)),
    }
}

// ------------------------------------------------------------------------------ files

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PngQuery {
    kind: Option<String>,
}

fn preview_error(e: PreviewError) -> ApiError {
    match e {
        PreviewError::NotFound(_) => ApiError::not_found("fax"),
        PreviewError::Purged { .. } => purged(),
        PreviewError::NoFile(_)
        | PreviewError::TiffMissing(_)
        | PreviewError::PageOutOfRange { .. } => ApiError::not_found("page"),
        PreviewError::Store(e) => store_error(e),
        other => ApiError::internal(other),
    }
}

fn purged() -> ApiError {
    ApiError::new(
        StatusCode::GONE,
        "purged",
        "the files of this fax were removed by the retention policy",
    )
}

/// A strong ETag of the bytes.
fn etag(bytes: &[u8]) -> String {
    let d = ring::digest::digest(&ring::digest::SHA256, bytes);
    let hex: String = d.as_ref()[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("\"{hex}\"")
}

fn not_modified(headers: &HeaderMap, tag: &str) -> bool {
    headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|t| t.trim() == tag || t.trim() == "*"))
}

async fn page_png(
    State(st): State<AppState>,
    s: Session,
    UrlPath((id, page)): UrlPath<(String, String)>,
    Query(q): Query<PngQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    s.need(Capability::ViewFaxes)?;
    let id = fax_id(&id)?;
    let page: u32 = page
        .strip_suffix(".png")
        .and_then(|n| n.parse().ok())
        .filter(|n| *n > 0)
        .ok_or_else(|| ApiError::not_found("page"))?;
    let kind = match q.kind.as_deref() {
        None | Some("full") => PngKind::Full,
        Some("thumb") => PngKind::Thumb,
        Some(_) => return Err(ApiError::bad_request("kind must be full or thumb")),
    };
    let path = st
        .preview()
        .await?
        .page_png(id, page, kind)
        .await
        .map_err(preview_error)?;
    let bytes = tokio::fs::read(&path).await.map_err(|e| {
        // the cache file was trimmed between the conversion and the read: rare, try again
        ApiError::internal(format!(
            "cannot read the cached page {}: {e}",
            path.display()
        ))
    })?;
    let tag = etag(&bytes);
    let mut r = if not_modified(&headers, &tag) {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        ([(header::CONTENT_TYPE, "image/png")], bytes).into_response()
    };
    let h = r.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&tag) {
        h.insert(header::ETAG, v);
    }
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=3600"),
    );
    Ok(r)
}

/// `relative` (from the record) under `data_dir`, refusing anything that could leave it.
fn inside(data_dir: &Path, relative: &str) -> Option<PathBuf> {
    let rel = Path::new(relative);
    if rel.as_os_str().is_empty()
        || rel.is_absolute()
        || rel.components().any(|c| !matches!(c, Component::Normal(_)))
    {
        return None;
    }
    let full = data_dir.join(rel);
    // a symbolic link inside the data directory that points out of it counts as outside
    let real = full.canonicalize().ok()?;
    let root = data_dir.canonicalize().ok()?;
    (real.starts_with(&root) && real.is_file()).then_some(real)
}

/// `fax-<id>[_<ani>].<ext>`: ASCII only (digits, letters, `+`, `-`; anything else becomes `_`).
pub fn download_name(r: &FaxRecord, ext: &str) -> String {
    let ani: String = r
        .ani
        .as_deref()
        .unwrap_or("")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '+' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(32)
        .collect();
    if ani.is_empty() {
        format!("fax-{}.{ext}", r.id)
    } else {
        format!("fax-{}_{ani}.{ext}", r.id)
    }
}

/// `Content-Disposition` with the plain name and its RFC 5987 form.
pub fn content_disposition(name: &str) -> String {
    let encoded: String = name
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    let plain: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() && c != '"' && c != '\\' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("attachment; filename=\"{plain}\"; filename*=UTF-8''{encoded}")
}

async fn download(
    State(st): State<AppState>,
    s: Session,
    UrlPath((id, file)): UrlPath<(String, String)>,
) -> Result<Response, ApiError> {
    s.need(Capability::ViewFaxes)?;
    let (ext, mime) = match file.as_str() {
        "file.pdf" => ("pdf", "application/pdf"),
        "file.tif" => ("tif", "image/tiff"),
        _ => return Err(ApiError::not_found("resource")),
    };
    let id = fax_id(&id)?;
    let r = record(&st, id).await?;
    if r.files_purged_at.is_some() {
        return Err(purged());
    }
    let rel = if ext == "pdf" {
        &r.pdf_path
    } else {
        &r.tiff_path
    };
    let rel = rel.as_deref().ok_or_else(|| ApiError::not_found("file"))?;
    let Some(path) = inside(&st.data_dir, rel) else {
        // a record that names a file outside the data directory, or a file that is gone
        tracing::warn!(
            fax = id,
            "web: the {ext} of fax {id} is missing or outside data_dir"
        );
        return Err(ApiError::not_found("file"));
    };
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_| ApiError::not_found("file"))?;
    let disposition = content_disposition(&download_name(&r, ext));
    let mut resp = ([(header::CONTENT_TYPE, mime)], bytes).into_response();
    if let Ok(v) = HeaderValue::from_str(&disposition) {
        resp.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    Ok(resp)
}

// ------------------------------------------------------------------------------ redelivery

async fn redeliver(
    State(st): State<AppState>,
    s: Session,
    UrlPath(id): UrlPath<String>,
    ApiJson(_): ApiJson<Value>,
) -> Result<Json<Value>, ApiError> {
    s.need(Capability::Redeliver)?;
    let id = fax_id(&id)?;
    record(&st, id).await?;
    let store = st.store().await?;
    let queued = store
        .redeliver(id, st.clock.now())
        .await
        .map_err(store_error)?;
    tracing::info!(
        fax = id,
        by = s.username(),
        attempts = queued.len(),
        "web: redelivery"
    );
    let d = store.list_deliveries(id).await.map_err(store_error)?;
    Ok(Json(json!({
        "queued": queued,
        "deliveries": d.iter().map(delivery_json).collect::<Vec<_>>(),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn w07_paths_that_could_leave_the_data_directory_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        std::fs::create_dir_all(data.join("faxes")).unwrap();
        std::fs::write(data.join("faxes/a.pdf"), b"x").unwrap();
        std::fs::write(dir.path().join("secret.txt"), b"s").unwrap();
        assert!(inside(&data, "faxes/a.pdf").is_some());
        for bad in ["../secret.txt", "faxes/../../secret.txt", "/etc/passwd", ""] {
            assert!(inside(&data, bad).is_none(), "{bad:?} must be refused");
        }
        assert!(inside(&data, "faxes/missing.pdf").is_none());
        // `.` in the middle is dropped by `Path::components`: the same file inside data_dir
        assert!(inside(&data, "faxes/./a.pdf").is_some());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.path().join("secret.txt"), data.join("faxes/link.pdf"))
                .unwrap();
            assert!(
                inside(&data, "faxes/link.pdf").is_none(),
                "a link out of data_dir"
            );
        }
    }

    #[test]
    fn w07_fax_ids_are_positive_integers() {
        assert_eq!(fax_id("12").unwrap(), 12);
        for bad in [
            "0",
            "-1",
            "..",
            "1.5",
            "99999999999999999999999",
            "1%2F2",
            "abc",
            "",
        ] {
            assert_eq!(
                fax_id(bad).unwrap_err().status,
                StatusCode::NOT_FOUND,
                "{bad}"
            );
        }
    }

    #[test]
    fn w06_download_names_are_safe_and_carry_their_rfc_5987_form() {
        let mut r = test_record();
        r.ani = Some("+886 2-1234\"5678/..".into());
        assert_eq!(download_name(&r, "pdf"), "fax-7_+886_2-1234_5678___.pdf");
        r.ani = None;
        assert_eq!(download_name(&r, "tif"), "fax-7.tif");
        let d = content_disposition("傳真-7.pdf");
        assert!(d.starts_with("attachment; filename=\"__-7.pdf\""), "{d}");
        assert!(
            d.contains("filename*=UTF-8''%E5%82%B3%E7%9C%9F-7.pdf"),
            "{d}"
        );
    }

    #[test]
    fn w01_list_parameters_are_checked() {
        let q = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<HashMap<_, _>>()
        };
        let (f, p) = list_query(&q(&[
            ("is_read", "false"),
            ("status", "partial"),
            ("ani", " 0912 "),
            ("limit", "20"),
            ("from", "2026-10-01T00:00:00Z"),
        ]))
        .unwrap();
        assert_eq!(f.is_read, Some(false));
        assert_eq!(f.fax_status, Some(FaxStatus::Partial));
        assert_eq!(f.ani_prefix.as_deref(), Some("0912"));
        assert_eq!(p.limit, 20);
        assert!(f.from.is_some() && f.to.is_none());
        for bad in [
            ("is_read", "yes"),
            ("status", "lost"),
            ("cursor", "garbage"),
            ("cursor", "1.2.x"),
            ("limit", "0"),
            ("limit", "201"),
            ("from", "yesterday"),
            ("bogus", "1"),
        ] {
            let e = list_query(&q(&[bad])).unwrap_err();
            assert_eq!(e.status, StatusCode::BAD_REQUEST, "{bad:?}");
        }
    }

    #[test]
    fn w02_webhook_targets_are_masked_and_no_file_path_is_shown() {
        let mut d = DeliveryAttempt {
            id: 1,
            fax_id: 7,
            target: "webhook:https://u:pw@hook.example/x?token=s3cret".into(),
            kind: "webhook".into(),
            attempt: 1,
            status: store::DeliveryStatus::Failed,
            last_error: None,
            next_try_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let v = delivery_json(&d);
        let t = v["target"].as_str().unwrap();
        assert!(!t.contains("pw@") && !t.contains("s3cret"), "{t}");
        d.target = "email:fax@example.com".into();
        assert_eq!(delivery_json(&d)["target"], "email:fax@example.com");
        let v = fax_json(&test_record());
        let text = v.to_string();
        assert!(!text.contains("faxes/2026"), "no file path in {text}");
        assert_eq!(v["has_pdf"], true);
    }

    fn test_record() -> FaxRecord {
        let t = Utc::now();
        FaxRecord {
            id: 7,
            call_id: "c@gw".into(),
            trunk: "gw".into(),
            ani: Some("0212345678".into()),
            ani_source: None,
            dnis: Some("2557".into()),
            dnis_source: None,
            remote_ident: None,
            started_at: t,
            answered_at: None,
            ended_at: None,
            duration_sec: None,
            hangup_direction: None,
            hangup_code: None,
            hangup_text: None,
            sip_status: None,
            t30_code: None,
            fax_status: FaxStatus::Received,
            pages: 1,
            resolution: None,
            bit_rate: None,
            compression: None,
            ecm: false,
            tiff_path: Some("faxes/2026/10/22/c.tif".into()),
            tiff_size: Some(10),
            pdf_path: Some("faxes/2026/10/22/c.pdf".into()),
            pdf_size: Some(20),
            files_purged_at: None,
            is_read: false,
            read_changed_by: None,
            read_changed_at: None,
            note: None,
            note_updated_by: None,
            note_updated_at: None,
            created_at: t,
            updated_at: t,
        }
    }
}
