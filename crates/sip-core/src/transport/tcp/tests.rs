use super::*;

const A: &str = "OPTIONS sip:a@b SIP/2.0\r\nVia: SIP/2.0/TCP h;branch=z9hG4bK1\r\nTo: <sip:a@b>\r\nFrom: <sip:c@d>;tag=1\r\nCall-ID: 1\r\nCSeq: 1 OPTIONS\r\nMax-Forwards: 70\r\nContent-Length: 4\r\n\r\nabcd";
const B: &str = "SIP/2.0 200 OK\r\nVia: SIP/2.0/TCP h;branch=z9hG4bK1\r\nTo: <sip:a@b>;tag=2\r\nFrom: <sip:c@d>;tag=1\r\nCall-ID: 1\r\nCSeq: 1 OPTIONS\r\nContent-Length: 0\r\n\r\n";

fn methods(evs: &[StreamEvent]) -> Vec<String> {
    evs.iter()
        .map(|e| match e {
            StreamEvent::Message(SipMessage::Request(r)) => r.method.to_string(),
            StreamEvent::Message(SipMessage::Response(r)) => r.status.to_string(),
            StreamEvent::Ping => "PING".into(),
            StreamEvent::Malformed(_) => "MALFORMED".into(),
            StreamEvent::Fatal(_) => "FATAL".into(),
        })
        .collect()
}

#[test]
fn n04_two_messages_in_one_segment() {
    let mut d = StreamDecoder::new();
    let evs = d.push(format!("{A}{B}").as_bytes());
    assert_eq!(methods(&evs), ["OPTIONS", "200"]);
    assert_eq!(d.buffered(), 0);
}

#[test]
fn n04_every_split_point_yields_the_same_messages() {
    let all = format!("{A}{B}");
    for cut in 0..=all.len() {
        let mut d = StreamDecoder::new();
        let mut evs = d.push(&all.as_bytes()[..cut]);
        evs.extend(d.push(&all.as_bytes()[cut..]));
        assert_eq!(methods(&evs), ["OPTIONS", "200"], "split at {cut}");
    }
}

#[test]
fn n04_one_message_in_three_segments() {
    let mut d = StreamDecoder::new();
    let b = A.as_bytes();
    assert!(d.push(&b[..30]).is_empty());
    assert!(d.push(&b[30..b.len() - 2]).is_empty());
    let evs = d.push(&b[b.len() - 2..]);
    let StreamEvent::Message(SipMessage::Request(r)) = &evs[0] else {
        panic!("{evs:?}")
    };
    assert_eq!(&r.body[..], b"abcd");
}

#[test]
fn n04_byte_by_byte() {
    let all = format!("{A}\r\n\r\n{B}");
    let mut d = StreamDecoder::new();
    let mut evs = vec![];
    for b in all.as_bytes() {
        evs.extend(d.push(&[*b]));
    }
    assert_eq!(methods(&evs), ["OPTIONS", "PING", "200"]);
}

#[test]
fn n05_keepalives() {
    let mut d = StreamDecoder::new();
    assert_eq!(methods(&d.push(b"\r\n\r\n")), ["PING"]);
    assert!(
        d.push(b"\r\n").is_empty(),
        "lone CRLF: wait, nothing to report"
    );
    assert_eq!(d.buffered(), 2);
    // a lone CRLF followed by a message is ignored
    assert_eq!(methods(&d.push(A.as_bytes())), ["OPTIONS"]);
    assert_eq!(methods(&d.push(b"\r\n\r\n\r\n\r\n")), ["PING", "PING"]);
    // "\r\n\r" then "\n" completes a ping
    assert!(d.push(b"\r\n\r").is_empty());
    assert_eq!(methods(&d.push(b"\n")), ["PING"]);
}

#[test]
fn n04_bad_content_length_is_fatal_and_bad_message_is_not() {
    let mut d = StreamDecoder::new();
    let evs = d.push(b"INVITE sip:a@b SIP/2.0\r\nContent-Length: nope\r\n\r\n");
    assert_eq!(methods(&evs), ["FATAL"]);
    // framed but unparseable (no mandatory headers): stream continues
    let mut d = StreamDecoder::new();
    let evs = d.push(format!("BYE sip:a@b SIP/2.0\r\nContent-Length: 0\r\n\r\n{B}").as_bytes());
    assert_eq!(methods(&evs), ["MALFORMED", "200"]);
}
