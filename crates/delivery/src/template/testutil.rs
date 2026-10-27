
use chrono::{TimeZone, Utc};
use store::{FaxRecord, FaxStatus, HangupDirection};

/// 2026-10-21 08:30:15 UTC.
pub fn sample() -> FaxRecord {
    let t = Utc.with_ymd_and_hms(2026, 10, 21, 8, 30, 15).unwrap();
    FaxRecord {
        id: 42,
        call_id: "abc@host".into(),
        trunk: "audiocodes".into(),
        ani: Some("0227001234".into()),
        ani_source: Some("from".into()),
        dnis: Some("025570001".into()),
        dnis_source: Some("ruri".into()),
        remote_ident: Some("+886 2 2700".into()),
        started_at: t,
        answered_at: Some(t),
        ended_at: Some(t + chrono::Duration::seconds(90)),
        duration_sec: Some(90),
        hangup_direction: Some(HangupDirection::Local),
        hangup_code: Some("completed".into()),
        hangup_text: Some("ok".into()),
        sip_status: None,
        t30_code: Some(0),
        fax_status: FaxStatus::Received,
        pages: 3,
        resolution: Some("fine".into()),
        bit_rate: Some(14400),
        compression: Some("MMR".into()),
        ecm: true,
        tiff_path: Some("faxes/42.tif".into()),
        tiff_size: Some(1000),
        pdf_path: Some("faxes/42.pdf".into()),
        pdf_size: Some(2000),
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
