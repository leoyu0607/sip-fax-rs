use super::*;
use crate::error::FailureKind;
use crate::template::testutil::sample;

fn utc() -> FixedOffset {
    FixedOffset::east_opt(0).unwrap()
}

fn setup() -> (tempfile::TempDir, FaxRecord) {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("faxes")).unwrap();
    std::fs::write(d.path().join("faxes/42.pdf"), b"%PDF-1.4 test").unwrap();
    std::fs::write(d.path().join("faxes/42.tif"), b"II*\0tiff").unwrap();
    (d, sample())
}

fn cfg(dir: &Path) -> FolderConfig {
    FolderConfig {
        dir: dir.to_string_lossy().into_owned(),
        filename: "{date}_{ani}_{id}.pdf".into(),
        formats: vec![Format::Pdf, Format::Tiff],
        create_dir: true,
    }
}

#[tokio::test]
async fn d02_folder_copies_both_formats_with_template_name() {
    let (d, rec) = setup();
    let out = tempfile::tempdir().unwrap();
    let paths = deliver(&cfg(out.path()), &rec, d.path(), utc())
        .await
        .unwrap();
    assert_eq!(paths.len(), 2);
    let pdf = out.path().join("20261021_0227001234_42.pdf");
    let tif = out.path().join("20261021_0227001234_42.tif");
    assert_eq!(std::fs::read(pdf).unwrap(), b"%PDF-1.4 test");
    assert_eq!(std::fs::read(tif).unwrap(), b"II*\0tiff");
    // No temp files are left behind.
    let names: Vec<String> = std::fs::read_dir(out.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names.len(), 2, "{names:?}");
    assert!(names.iter().all(|n| !n.ends_with(".part")));
}

#[tokio::test]
async fn d02_folder_format_selection() {
    let (d, rec) = setup();
    let out = tempfile::tempdir().unwrap();
    let mut c = cfg(out.path());
    c.formats = vec![Format::Tiff];
    c.filename = "f{id}".into();
    deliver(&c, &rec, d.path(), utc()).await.unwrap();
    let names: Vec<_> = std::fs::read_dir(out.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["f42.tif"]);
}

#[tokio::test]
async fn d02_folder_creates_missing_directory() {
    let (d, rec) = setup();
    let out = tempfile::tempdir().unwrap();
    let sub = out.path().join("a/b/c");
    deliver(&cfg(&sub), &rec, d.path(), utc()).await.unwrap();
    assert!(sub.join("20261021_0227001234_42.pdf").is_file());
}

#[tokio::test]
async fn d02_folder_missing_directory_is_transient_when_not_created() {
    let (d, rec) = setup();
    let out = tempfile::tempdir().unwrap();
    let sub = out.path().join("not-yet");
    let mut c = cfg(&sub);
    c.create_dir = false;
    let e = deliver(&c, &rec, d.path(), utc()).await.unwrap_err();
    assert_eq!(e.kind, FailureKind::Transient);
    assert!(e.message.contains("does not exist"), "{e}");
    assert!(!sub.exists(), "must not be created");
    // Once the directory appears, the retry succeeds.
    std::fs::create_dir(&sub).unwrap();
    deliver(&c, &rec, d.path(), utc()).await.unwrap();
}

#[tokio::test]
async fn d02_folder_dir_is_a_file_is_permanent() {
    let (d, rec) = setup();
    let out = tempfile::tempdir().unwrap();
    let f = out.path().join("file");
    std::fs::write(&f, b"x").unwrap();
    let e = deliver(&cfg(&f), &rec, d.path(), utc()).await.unwrap_err();
    assert_eq!(e.kind, FailureKind::Permanent);
}

#[tokio::test]
async fn d02_folder_unwritable_target_is_transient_and_leaves_no_part_file() {
    // A directory squatting on the target name makes the rename fail on every platform
    // (permission problems are simulated portably this way; a real read-only directory test
    // lives in tests/delivery.rs and runs on unix).
    let (d, rec) = setup();
    let out = tempfile::tempdir().unwrap();
    let mut c = cfg(out.path());
    c.formats = vec![Format::Pdf];
    c.filename = "x".into();
    let target = out.path().join("x.pdf");
    std::fs::create_dir(&target).unwrap();
    let e = deliver(&c, &rec, d.path(), utc()).await.unwrap_err();
    assert_eq!(e.kind, FailureKind::Transient, "{e}");
    let names: Vec<_> = std::fs::read_dir(out.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["x.pdf"], "no .part left behind");
}

#[tokio::test]
async fn d02_folder_is_idempotent_and_never_overwrites_different_content() {
    let (d, rec) = setup();
    let out = tempfile::tempdir().unwrap();
    let mut c = cfg(out.path());
    c.formats = vec![Format::Pdf];
    c.filename = "same".into();
    deliver(&c, &rec, d.path(), utc()).await.unwrap();
    // Same content again: no new file.
    deliver(&c, &rec, d.path(), utc()).await.unwrap();
    assert_eq!(std::fs::read_dir(out.path()).unwrap().count(), 1);
    // A different fax with the same name gets a suffix; the first file is untouched.
    std::fs::write(d.path().join("faxes/42.pdf"), b"different").unwrap();
    deliver(&c, &rec, d.path(), utc()).await.unwrap();
    assert_eq!(
        std::fs::read(out.path().join("same.pdf")).unwrap(),
        b"%PDF-1.4 test"
    );
    assert_eq!(
        std::fs::read(out.path().join("same_1.pdf")).unwrap(),
        b"different"
    );
}

#[tokio::test]
async fn d02_folder_hostile_values_stay_inside_directory() {
    let (d, mut rec) = setup();
    rec.ani = Some("../../../evil".into());
    let out = tempfile::tempdir().unwrap();
    let inner = out.path().join("inbox");
    let mut c = cfg(&inner);
    c.filename = "{ani}/{ani}".into();
    let paths = deliver(&c, &rec, d.path(), utc()).await.unwrap();
    for p in &paths {
        assert_eq!(p.parent().unwrap(), inner, "{p:?}");
    }
    // Nothing appeared next to the inbox directory.
    let siblings: Vec<_> = std::fs::read_dir(out.path()).unwrap().collect();
    assert_eq!(siblings.len(), 1);
}

#[tokio::test]
async fn d02_folder_source_problems_are_permanent_and_write_nothing() {
    let (d, mut rec) = setup();
    let out = tempfile::tempdir().unwrap();
    std::fs::remove_file(d.path().join("faxes/42.tif")).unwrap();
    let e = deliver(&cfg(out.path()), &rec, d.path(), utc())
        .await
        .unwrap_err();
    assert_eq!(e.kind, FailureKind::Permanent);
    assert_eq!(
        std::fs::read_dir(out.path()).unwrap().count(),
        0,
        "PDF must not be half-delivered"
    );
    rec.pdf_path = None;
    let mut c = cfg(out.path());
    c.formats = vec![Format::Pdf];
    assert_eq!(
        deliver(&c, &rec, d.path(), utc()).await.unwrap_err().kind,
        FailureKind::Permanent
    );
    c.filename = "{bogus}".into();
    assert!(
        deliver(&c, &rec, d.path(), utc())
            .await
            .unwrap_err()
            .message
            .contains("template")
    );
}

#[test]
fn d02_strip_ext() {
    assert_eq!(strip_ext("a.PDF"), "a");
    assert_eq!(strip_ext("a.tiff"), "a");
    assert_eq!(strip_ext("a.tif"), "a");
    assert_eq!(strip_ext("a.txt"), "a.txt");
}
