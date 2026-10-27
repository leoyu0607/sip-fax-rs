//! Access to the fax files of a record (paths are relative to `data_dir`).

use crate::config::Format;
use crate::error::DeliveryError;
use std::path::{Component, Path, PathBuf};
use store::FaxRecord;

/// Resolve `rel` under `data_dir`, refusing absolute paths and `..` components.
pub fn resolve(data_dir: &Path, rel: &str) -> Result<PathBuf, DeliveryError> {
    let p = Path::new(rel);
    let ok = !rel.is_empty()
        && p.components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
    if !ok {
        return Err(DeliveryError::permanent(format!(
            "unsafe file path in record: {rel:?}"
        )));
    }
    Ok(data_dir.join(p))
}

/// Read one file of the record. Missing path, purged files and missing files are permanent
/// failures (nothing to send); other I/O errors are transient.
pub async fn read(
    data_dir: &Path,
    rec: &FaxRecord,
    format: Format,
) -> Result<Vec<u8>, DeliveryError> {
    let (rel, what) = match format {
        Format::Pdf => (rec.pdf_path.as_deref(), "PDF"),
        Format::Tiff => (rec.tiff_path.as_deref(), "TIFF"),
    };
    if rec.files_purged_at.is_some() {
        return Err(DeliveryError::permanent(format!(
            "{what} no longer available: files were purged"
        )));
    }
    let Some(rel) = rel else {
        return Err(DeliveryError::permanent(format!(
            "record has no {what} file"
        )));
    };
    let path = resolve(data_dir, rel)?;
    match tokio::fs::read(&path).await {
        Ok(b) => Ok(b),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(DeliveryError::permanent(
            format!("{what} file missing: {}", path.display()),
        )),
        Err(e) => Err(DeliveryError::transient(format!(
            "reading {}: {e}",
            path.display()
        ))),
    }
}

#[cfg(test)]
mod tests;
