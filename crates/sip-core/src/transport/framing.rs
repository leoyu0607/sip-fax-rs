// Derived from Sipress (MIT, Copyright (c) 2025 LeoYu) core/src/sip/transport.rs (`take_frame`
// and its tests). See docs/sipress-extraction.md for the list of changes.

//! Message framing on a byte stream (TCP / TLS), RFC 3261 section 18.3.
//!
//! [`take_frame`] cuts one complete SIP message out of a receive buffer using `Content-Length`.
//! The cut bytes are then handed to [`crate::message::parse_message`].

use crate::message::MAX_MESSAGE_SIZE;

/// Framing failure. The stream cannot be re-synchronised afterwards; the connection must be
/// closed (RFC 4475 3.1.2.3).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    /// No end of headers within [`MAX_MESSAGE_SIZE`] bytes.
    #[error("SIP message header block exceeds {MAX_MESSAGE_SIZE} bytes")]
    HeaderTooLarge,
    /// Headers plus `Content-Length` exceed [`MAX_MESSAGE_SIZE`].
    #[error("SIP message exceeds {MAX_MESSAGE_SIZE} bytes")]
    MessageTooLarge,
    /// A `Content-Length` header is present but is not a non-negative decimal number.
    #[error("invalid Content-Length value")]
    InvalidContentLength,
}

/// Cuts one complete SIP message out of a TCP byte stream, framed by `Content-Length`.
///
/// Returns `Ok(None)` while the data is incomplete. CRLFs between messages (keep-alives) are
/// skipped. A message without `Content-Length` is treated as having no body (the strict
/// "Content-Length is mandatory on streams" rule is enforced by the message parser).
pub fn take_frame(buf: &mut Vec<u8>) -> Result<Option<Vec<u8>>, FrameError> {
    let skip = buf
        .iter()
        .take_while(|&&b| b == b'\r' || b == b'\n')
        .count();
    buf.drain(..skip);
    let Some(head_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
        if buf.len() > MAX_MESSAGE_SIZE {
            return Err(FrameError::HeaderTooLarge);
        }
        return Ok(None);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]);
    let mut body_len = 0usize;
    for line in head.lines().skip(1) {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if name.eq_ignore_ascii_case("content-length") || name.eq_ignore_ascii_case("l") {
            let value = value.trim();
            if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err(FrameError::InvalidContentLength);
            }
            body_len = value
                .parse::<usize>()
                .map_err(|_| FrameError::MessageTooLarge)?;
            break;
        }
    }
    let total = head_end.saturating_add(4).saturating_add(body_len);
    if total > MAX_MESSAGE_SIZE {
        return Err(FrameError::MessageTooLarge);
    }
    if buf.len() < total {
        return Ok(None);
    }
    let msg = buf[..total].to_vec();
    buf.drain(..total);
    Ok(Some(msg))
}

#[cfg(test)]
mod tests {
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
}
