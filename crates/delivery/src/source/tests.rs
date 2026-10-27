use super::*;
use crate::error::FailureKind;
use crate::template::testutil::sample;

#[test]
fn d02_resolve_rejects_traversal() {
    let d = Path::new("/data");
    assert_eq!(
        resolve(d, "faxes/1.pdf").unwrap(),
        Path::new("/data/faxes/1.pdf")
    );
    for bad in ["../x", "a/../../x", "/etc/passwd", "", "C:\\x"] {
        let r = resolve(d, bad);
        // "C:\\x" is a single normal component on unix and a prefix on Windows; either way it
        // must not escape: accept only when it stays below d.
        match r {
            Ok(p) => assert!(p.starts_with(d), "{bad} -> {p:?}"),
            Err(e) => assert_eq!(e.kind, FailureKind::Permanent),
        }
    }
    assert!(resolve(d, "../x").is_err());
    assert!(resolve(d, "/etc/passwd").is_err());
    assert!(resolve(d, "").is_err());
}

#[tokio::test]
async fn d02_read_error_classes() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("faxes")).unwrap();
    let mut r = sample();
    std::fs::write(dir.path().join("faxes/42.pdf"), b"pdf!").unwrap();
    assert_eq!(read(dir.path(), &r, Format::Pdf).await.unwrap(), b"pdf!");
    // TIFF path set but file absent.
    let e = read(dir.path(), &r, Format::Tiff).await.unwrap_err();
    assert_eq!(e.kind, FailureKind::Permanent);
    assert!(e.message.contains("TIFF file missing"), "{e}");
    // A directory where a file is expected: other I/O error -> transient.
    std::fs::create_dir_all(dir.path().join("faxes/42.tif")).unwrap();
    let e = read(dir.path(), &r, Format::Tiff).await.unwrap_err();
    assert_eq!(e.kind, FailureKind::Transient, "{e}");
    r.tiff_path = None;
    let e = read(dir.path(), &r, Format::Tiff).await.unwrap_err();
    assert!(e.message.contains("no TIFF"), "{e}");
    r.pdf_path = Some("../evil.pdf".into());
    assert_eq!(
        read(dir.path(), &r, Format::Pdf).await.unwrap_err().kind,
        FailureKind::Permanent
    );
    r.pdf_path = Some("faxes/42.pdf".into());
    r.files_purged_at = Some(r.created_at);
    let e = read(dir.path(), &r, Format::Pdf).await.unwrap_err();
    assert!(e.message.contains("purged"), "{e}");
}
