//! Folder delivery: copy the PDF / TIFF into a directory, atomically.
//!
//! * The file is written to `<name>.part`, flushed, then renamed, so a watcher never sees a
//!   half-written fax.
//! * Missing directory: created (`create_dir = true`, default) or a transient failure so the attempt
//!   is retried (`create_dir = false`, for example an unmounted share).
//! * Permission denied and other I/O errors are transient (retried); an invalid template or a
//!   missing source file is permanent.
//! * An existing file with identical content counts as already delivered (retry after a partial
//!   success is idempotent); a different file with the same name is never overwritten, the new
//!   file gets a `_1`, `_2`, ... suffix.

use crate::config::{FolderConfig, Format};
use crate::error::DeliveryError;
use crate::source;
use crate::template::{Mode, Template, sanitize_name};
use chrono::FixedOffset;
use std::path::{Path, PathBuf};
use store::FaxRecord;
use tokio::io::AsyncWriteExt;

/// Deliver `rec` according to `cfg`. Returns the paths written (or already present).
pub async fn deliver(
    cfg: &FolderConfig,
    rec: &FaxRecord,
    data_dir: &Path,
    offset: FixedOffset,
) -> Result<Vec<PathBuf>, DeliveryError> {
    let tpl = Template::parse(&cfg.filename)
        .map_err(|e| DeliveryError::permanent(format!("filename template: {e}")))?;
    let stem = strip_ext(&tpl.render(rec, offset, Mode::FileName));
    let stem = sanitize_name(&stem);

    // Read every source first: nothing is written when a file is missing.
    let mut files = Vec::new();
    for f in &cfg.formats {
        files.push((*f, source::read(data_dir, rec, *f).await?));
    }

    let dir = PathBuf::from(&cfg.dir);
    match tokio::fs::metadata(&dir).await {
        Ok(m) if m.is_dir() => {}
        Ok(_) => {
            return Err(DeliveryError::permanent(format!(
                "{} exists and is not a directory",
                dir.display()
            )));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if !cfg.create_dir {
                return Err(DeliveryError::transient(format!(
                    "directory {} does not exist",
                    dir.display()
                )));
            }
            tokio::fs::create_dir_all(&dir).await.map_err(|e| {
                DeliveryError::transient(format!("creating {}: {e}", dir.display()))
            })?;
        }
        Err(e) => {
            return Err(DeliveryError::transient(format!("{}: {e}", dir.display())));
        }
    }

    let mut out = Vec::new();
    for (f, bytes) in files {
        out.push(write_unique(&dir, &stem, f, &bytes).await?);
    }
    Ok(out)
}

fn strip_ext(s: &str) -> String {
    let lower = s.to_ascii_lowercase();
    for ext in [".pdf", ".tiff", ".tif"] {
        if lower.ends_with(ext) {
            return s[..s.len() - ext.len()].to_string();
        }
    }
    s.to_string()
}

async fn write_unique(
    dir: &Path,
    stem: &str,
    f: Format,
    bytes: &[u8],
) -> Result<PathBuf, DeliveryError> {
    for n in 0..1000u32 {
        let name = if n == 0 {
            format!("{stem}.{}", f.ext())
        } else {
            format!("{stem}_{n}.{}", f.ext())
        };
        let path = dir.join(&name);
        match tokio::fs::read(&path).await {
            Ok(existing) if existing == bytes => return Ok(path),
            Ok(_) => continue,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(DeliveryError::transient(format!("{}: {e}", path.display())));
            }
        }
        let part = dir.join(format!("{name}.part"));
        let res = async {
            let mut file = tokio::fs::File::create(&part).await?;
            file.write_all(bytes).await?;
            file.flush().await?;
            file.sync_all().await?;
            drop(file);
            tokio::fs::rename(&part, &path).await
        }
        .await;
        return match res {
            Ok(()) => Ok(path),
            Err(e) => {
                let _ = tokio::fs::remove_file(&part).await;
                Err(DeliveryError::transient(format!(
                    "writing {}: {e}",
                    path.display()
                )))
            }
        };
    }
    Err(DeliveryError::transient(format!(
        "no free file name for {stem}.{} in {}",
        f.ext(),
        dir.display()
    )))
}

#[cfg(test)]
mod tests;
