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
mod tests;
