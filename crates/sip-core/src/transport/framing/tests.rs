use super::*;

// ---- tests carried over from Sipress (adapted to bytes / FrameError) ----

#[test]
fn p11_tcp_framing_follows_content_length() {
    let a = "INVITE sip:a@b SIP/2.0\r\nCall-ID: 1\r\nContent-Length: 5\r\n\r\nhello";
    let b = "SIP/2.0 200 OK\r\nl: 0\r\n\r\n";
    // keep-alive CRLF + two messages glued together
    let mut buf = format!("\r\n\r\n{}{}", a, b).into_bytes();
    assert_eq!(take_frame(&mut buf).unwrap().as_deref(), Some(a.as_bytes()));
    assert_eq!(take_frame(&mut buf).unwrap().as_deref(), Some(b.as_bytes()));
    assert!(take_frame(&mut buf).unwrap().is_none());
    assert!(buf.is_empty());

    // arrives in pieces: wait while header or body is incomplete
    let mut buf = a.as_bytes()[..20].to_vec();
    assert!(take_frame(&mut buf).unwrap().is_none());
    buf.extend_from_slice(&a.as_bytes()[20..a.len() - 2]);
    assert!(
        take_frame(&mut buf).unwrap().is_none(),
        "body is 2 bytes short"
    );
    buf.extend_from_slice(&a.as_bytes()[a.len() - 2..]);
    assert_eq!(take_frame(&mut buf).unwrap().as_deref(), Some(a.as_bytes()));

    // no Content-Length means no body
    let mut buf = b"BYE sip:a@b SIP/2.0\r\nCall-ID: 2\r\n\r\n".to_vec();
    assert!(take_frame(&mut buf).unwrap().is_some());
}

#[test]
fn p11_tcp_framing_rejects_oversized_messages() {
    let mut buf = format!(
        "SIP/2.0 200 OK\r\nContent-Length: {}\r\n\r\n",
        MAX_MESSAGE_SIZE
    )
    .into_bytes();
    assert_eq!(take_frame(&mut buf), Err(FrameError::MessageTooLarge));
    let mut buf = vec![b'a'; MAX_MESSAGE_SIZE + 1];
    assert_eq!(
        take_frame(&mut buf),
        Err(FrameError::HeaderTooLarge),
        "no end of headers and over the limit"
    );
}

// ---- additional tests ----

#[test]
fn p11_invalid_content_length_is_a_framing_error() {
    for bad in ["-999", "five", "", "1 2", "+5"] {
        let mut buf =
            format!("INVITE sip:a@b SIP/2.0\r\nContent-Length: {bad}\r\n\r\n").into_bytes();
        assert_eq!(
            take_frame(&mut buf),
            Err(FrameError::InvalidContentLength),
            "value {bad:?}"
        );
    }
}

#[test]
fn p11_huge_content_length_does_not_overflow() {
    let mut buf =
        b"INVITE sip:a@b SIP/2.0\r\nContent-Length: 99999999999999999999999\r\n\r\n".to_vec();
    assert_eq!(take_frame(&mut buf), Err(FrameError::MessageTooLarge));
}

#[test]
fn p11_exactly_max_size_message_is_accepted() {
    let head = "MESSAGE sip:a@b SIP/2.0\r\nContent-Length: ";
    // choose the body length so that head + digits + CRLFCRLF + body == MAX_MESSAGE_SIZE
    let digits = 5;
    let body = MAX_MESSAGE_SIZE - head.len() - digits - 4;
    let mut buf = format!("{head}{body:05}\r\n\r\n").into_bytes();
    buf.extend(std::iter::repeat_n(b'x', body));
    assert_eq!(buf.len(), MAX_MESSAGE_SIZE);
    let frame = take_frame(&mut buf).unwrap().unwrap();
    assert_eq!(frame.len(), MAX_MESSAGE_SIZE);
    assert!(buf.is_empty());
}
