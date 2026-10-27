use super::*;

#[test]
fn m00_requests_parse_with_defaults() {
    let p = |s: &str| serde_json::from_str::<Request>(s).unwrap();
    assert_eq!(p(r#"{"cmd":"status"}"#), Request::Status);
    assert_eq!(
        p(r#"{"cmd":"stop"}"#),
        Request::Stop {
            force: false,
            timeout: None
        }
    );
    assert_eq!(
        p(r#"{"cmd":"stop","force":true,"timeout":7}"#),
        Request::Stop {
            force: true,
            timeout: Some(7)
        }
    );
    assert!(serde_json::from_str::<Request>(r#"{"cmd":"nope"}"#).is_err());
    assert!(serde_json::from_str::<Request>("not json").is_err());
}

#[test]
fn m00_replies_round_trip_and_are_single_lines() {
    let replies = [
        Reply::Stopped,
        Reply::Waiting {
            active: 2,
            remaining_secs: 295,
        },
        Reply::Error {
            message: "a\nb".into(),
        },
        Reply::Event {
            event: Event::FaxProgress {
                call_id: "c".into(),
                pages: 3,
            },
        },
    ];
    for r in replies {
        let line = r.to_line();
        assert!(!line.contains('\n'), "{line}");
        assert_eq!(serde_json::from_str::<Reply>(&line).unwrap(), r);
    }
    assert_eq!(Reply::Stopped.to_line(), r#"{"type":"stopped"}"#);
}
