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
        modem: None,
        media: None,
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
