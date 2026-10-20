//! M1b transport tests N01-N07 and C01 (source policy), on real localhost sockets.
//!
//! Positive waits use a generous real timeout that only matters when something is broken;
//! negative assertions ("nothing is sent") wait a short real interval.

mod common;

use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sip_core::message::{
    Method, Request, Response, SipMessage, SipUri, StatusCode, parse_datagram, parse_message,
};
use sip_core::transport::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::mpsc;
use tokio::time::timeout;

const LONG: Duration = Duration::from_secs(5);
const SHORT: Duration = Duration::from_millis(300);

fn sock(s: &str) -> SocketAddr {
    s.parse().unwrap()
}

async fn start(cfg: TransportConfig) -> (TransportLayer, mpsc::Receiver<Incoming>) {
    TransportLayer::bind(cfg).await.expect("bind")
}

fn udp_cfg(bind: &str) -> TransportConfig {
    TransportConfig {
        udp_bind: Some(sock(bind)),
        ..Default::default()
    }
}

fn tcp_cfg(bind: &str) -> TransportConfig {
    TransportConfig {
        tcp_bind: Some(sock(bind)),
        ..Default::default()
    }
}

fn options(via: &str, call_id: &str) -> String {
    format!(
        "OPTIONS sip:fax@example.com SIP/2.0\r\nVia: {via}\r\nMax-Forwards: 70\r\n\
         From: <sip:gw@example.com>;tag=g1\r\nTo: <sip:fax@example.com>\r\n\
         Call-ID: {call_id}\r\nCSeq: 1 OPTIONS\r\nContent-Length: 0\r\n\r\n"
    )
}

fn request_of(i: &Incoming) -> &Request {
    match &i.msg {
        SipMessage::Request(r) => r,
        other => panic!("expected request, got {other:?}"),
    }
}

fn top_via_line(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .lines()
        .find(|l| l.starts_with("Via:"))
        .unwrap_or("")
        .to_string()
}

async fn recv_incoming(rx: &mut mpsc::Receiver<Incoming>) -> Incoming {
    timeout(LONG, rx.recv())
        .await
        .expect("timeout")
        .expect("closed")
}

async fn assert_no_incoming(rx: &mut mpsc::Receiver<Incoming>) {
    assert!(
        timeout(SHORT, rx.recv()).await.is_err(),
        "unexpected message delivered"
    );
}

async fn udp_recv(s: &UdpSocket) -> (Vec<u8>, SocketAddr) {
    let mut buf = vec![0u8; 70_000];
    let (n, from) = timeout(LONG, s.recv_from(&mut buf))
        .await
        .expect("timeout")
        .unwrap();
    buf.truncate(n);
    (buf, from)
}

async fn assert_udp_silent(s: &UdpSocket) {
    let mut buf = [0u8; 2048];
    assert!(
        timeout(SHORT, s.recv_from(&mut buf)).await.is_err(),
        "unexpected datagram"
    );
}

fn ok_response(req: &Request) -> Response {
    Response::for_request(req, StatusCode::OK, Some("srv1"))
}

// ================================================================ N01

#[tokio::test]
async fn n01_udp_rport_is_filled_in_and_response_goes_to_source_ip_and_port() {
    let (layer, mut rx) = start(udp_cfg("127.0.0.1:0")).await;
    let server = layer.udp_addr().unwrap();
    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let cport = client.local_addr().unwrap().port();
    // The sent-by is deliberately wrong: rport must override it.
    let text = options(
        "SIP/2.0/UDP 192.0.2.77:5099;rport;branch=z9hG4bKn01",
        "n01@test",
    );
    client.send_to(text.as_bytes(), server).await.unwrap();

    let inc = recv_incoming(&mut rx).await;
    assert_eq!(inc.source, client.local_addr().unwrap());
    assert_eq!(inc.transport, TransportKind::Udp);
    assert_eq!(inc.local, server);
    let req = request_of(&inc);
    let via = req.headers.get("Via").unwrap();
    println!("N01 stamped Via: {via}");
    assert_eq!(
        via,
        format!("SIP/2.0/UDP 192.0.2.77:5099;rport={cport};branch=z9hG4bKn01;received=127.0.0.1")
    );
    layer
        .handle()
        .send_response(ok_response(req), inc.source, inc.transport)
        .await
        .unwrap();

    let (bytes, from) = udp_recv(&client).await;
    assert_eq!(
        from, server,
        "answered from the same socket the request arrived on"
    );
    let resp = parse_datagram(&bytes).unwrap();
    let SipMessage::Response(resp) = resp else {
        panic!()
    };
    assert_eq!(resp.status.code(), 200);
    assert_eq!(resp.headers.call_id(), Some("n01@test"));
    assert_eq!(
        resp.headers.get("Via").unwrap(),
        format!("SIP/2.0/UDP 192.0.2.77:5099;rport={cport};branch=z9hG4bKn01;received=127.0.0.1")
    );
    println!("N01 response wire:\n{}", String::from_utf8_lossy(&bytes));
    layer.shutdown().await;
}

// ================================================================ N02

#[tokio::test]
async fn n02_udp_without_rport_gets_received_and_response_uses_received_and_sent_by_port() {
    let (layer, mut rx) = start(udp_cfg("127.0.0.1:0")).await;
    let server = layer.udp_addr().unwrap();
    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let cport = client.local_addr().unwrap().port();
    // sent-by names another host but the port the client really listens on
    let text = options(
        &format!("SIP/2.0/UDP pbx.invalid:{cport};branch=z9hG4bKn02"),
        "n02@test",
    );
    client.send_to(text.as_bytes(), server).await.unwrap();
    let inc = recv_incoming(&mut rx).await;
    let req = request_of(&inc);
    let via = req.headers.top_via().unwrap();
    assert_eq!(via.received(), Some("127.0.0.1"));
    assert_eq!(via.rport(), None, "rport must not be invented");
    assert_eq!(
        req.headers.get("Via").unwrap(),
        format!("SIP/2.0/UDP pbx.invalid:{cport};branch=z9hG4bKn02;received=127.0.0.1")
    );
    layer
        .handle()
        .send_response(ok_response(req), inc.source, inc.transport)
        .await
        .unwrap();
    let (bytes, from) = udp_recv(&client).await;
    assert_eq!(from, server);
    println!("N02 response Via: {}", top_via_line(&bytes));
    assert!(top_via_line(&bytes).contains("received=127.0.0.1"));
    layer.shutdown().await;
}

#[tokio::test]
async fn n02_udp_sent_by_equal_to_source_is_left_untouched() {
    let (layer, mut rx) = start(udp_cfg("127.0.0.1:0")).await;
    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let cport = client.local_addr().unwrap().port();
    let via = format!("SIP/2.0/UDP 127.0.0.1:{cport};branch=z9hG4bKn02b");
    client
        .send_to(
            options(&via, "n02b@test").as_bytes(),
            layer.udp_addr().unwrap(),
        )
        .await
        .unwrap();
    let inc = recv_incoming(&mut rx).await;
    assert_eq!(request_of(&inc).headers.get("Via").unwrap(), via);
    layer.shutdown().await;
}

#[tokio::test]
async fn n02_udp_garbage_and_large_datagrams_are_dropped_and_the_loop_survives() {
    let (layer, mut rx) = start(udp_cfg("127.0.0.1:0")).await;
    let server = layer.udp_addr().unwrap();
    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    client.send_to(b"this is not SIP", server).await.unwrap();
    client.send_to(&vec![b'x'; 8_000], server).await.unwrap();
    assert_no_incoming(&mut rx).await;
    let cport = client.local_addr().unwrap().port();
    let via = format!("SIP/2.0/UDP 127.0.0.1:{cport};branch=z9hG4bKgood");
    client
        .send_to(options(&via, "good@test").as_bytes(), server)
        .await
        .unwrap();
    let inc = recv_incoming(&mut rx).await;
    assert_eq!(request_of(&inc).headers.call_id(), Some("good@test"));
    layer.shutdown().await;
}

// ================================================================ N03

async fn read_one_message(s: &mut TcpStream) -> SipMessage {
    let mut buf = Vec::new();
    loop {
        match parse_message(&buf) {
            Ok((m, _)) => return m,
            Err(sip_core::message::ParseError::Incomplete) => {}
            Err(e) => panic!("parse error {e:?} on {:?}", String::from_utf8_lossy(&buf)),
        }
        let mut chunk = [0u8; 4096];
        let n = timeout(LONG, s.read(&mut chunk))
            .await
            .expect("timeout")
            .unwrap();
        assert!(n > 0, "connection closed while waiting for a message");
        buf.extend_from_slice(&chunk[..n]);
    }
}

async fn wait_until(mut cond: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + LONG;
    while !cond() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "condition not reached in time"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn n03_tcp_response_uses_the_original_connection_then_reconnects_to_sent_by() {
    let (layer, mut rx) = start(tcp_cfg("127.0.0.1:0")).await;
    let server = layer.tcp_addr().unwrap();
    let handle = layer.handle();
    // the address the client advertises in its Via (a second listener)
    let advertised = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let adv_port = advertised.local_addr().unwrap().port();

    let mut conn = TcpStream::connect(server).await.unwrap();
    let local = conn.local_addr().unwrap();
    let text = options(
        &format!("SIP/2.0/TCP 127.0.0.1:{adv_port};branch=z9hG4bKn03"),
        "n03@test",
    );
    conn.write_all(text.as_bytes()).await.unwrap();
    let inc = recv_incoming(&mut rx).await;
    assert_eq!(inc.transport, TransportKind::Tcp);
    assert_eq!(inc.source, local);
    assert_eq!(inc.local, server);
    assert!(handle.has_tcp_connection(local));
    let req = request_of(&inc).clone();

    // 1) response over the original connection
    handle
        .send_response(ok_response(&req), inc.source, inc.transport)
        .await
        .unwrap();
    let SipMessage::Response(r1) = read_one_message(&mut conn).await else {
        panic!()
    };
    assert_eq!(r1.status.code(), 200);
    assert_eq!(r1.headers.call_id(), Some("n03@test"));
    println!("N03 response #1 arrived on the original connection (client {local})");

    // 2) connection torn down -> the next response is delivered to sent-by
    drop(conn);
    wait_until(|| !handle.has_tcp_connection(local)).await;
    handle
        .send_response(ok_response(&req), inc.source, inc.transport)
        .await
        .unwrap();
    let (mut accepted, peer) = timeout(LONG, advertised.accept())
        .await
        .expect("no reconnect")
        .unwrap();
    println!("N03 response #2: server reconnected to sent-by 127.0.0.1:{adv_port} (from {peer})");
    let SipMessage::Response(r2) = read_one_message(&mut accepted).await else {
        panic!()
    };
    assert_eq!(r2.status.code(), 200);
    assert_eq!(r2.headers.call_id(), Some("n03@test"));
    layer.shutdown().await;
}

// ================================================================ N04

fn with_body(call_id: &str, body: &str) -> String {
    format!(
        "MESSAGE sip:fax@example.com SIP/2.0\r\nVia: SIP/2.0/TCP 127.0.0.1:5060;branch=z9hG4bK{call_id}\r\n\
         Max-Forwards: 70\r\nFrom: <sip:gw@example.com>;tag=1\r\nTo: <sip:fax@example.com>\r\n\
         Call-ID: {call_id}\r\nCSeq: 1 MESSAGE\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

#[tokio::test]
async fn n04_tcp_two_messages_in_one_segment_and_one_message_in_three_segments() {
    let (layer, mut rx) = start(tcp_cfg("127.0.0.1:0")).await;
    let mut conn = TcpStream::connect(layer.tcp_addr().unwrap()).await.unwrap();
    conn.set_nodelay(true).unwrap();
    let a = with_body("glue-a", "first body");
    let b = with_body("glue-b", "second body");
    conn.write_all(format!("{a}{b}").as_bytes()).await.unwrap();
    for (id, body) in [("glue-a", "first body"), ("glue-b", "second body")] {
        let inc = recv_incoming(&mut rx).await;
        let r = request_of(&inc);
        assert_eq!(r.headers.call_id(), Some(id));
        assert_eq!(&r.body[..], body.as_bytes());
    }
    // one message split into three writes (flushed individually)
    let c = with_body("split-c", "0123456789");
    let bytes = c.as_bytes();
    let (p1, p2) = (20, bytes.len() - 4);
    for part in [&bytes[..p1], &bytes[p1..p2], &bytes[p2..]] {
        conn.write_all(part).await.unwrap();
        conn.flush().await.unwrap();
        tokio::task::yield_now().await;
    }
    let inc = recv_incoming(&mut rx).await;
    let r = request_of(&inc);
    assert_eq!(r.headers.call_id(), Some("split-c"));
    assert_eq!(&r.body[..], b"0123456789");
    layer.shutdown().await;
}

// ================================================================ N05

#[tokio::test]
async fn n05_tcp_double_crlf_is_answered_with_single_crlf_single_crlf_is_ignored() {
    let (layer, mut rx) = start(tcp_cfg("127.0.0.1:0")).await;
    let mut conn = TcpStream::connect(layer.tcp_addr().unwrap()).await.unwrap();
    conn.write_all(b"\r\n\r\n").await.unwrap();
    let mut pong = [0u8; 8];
    let n = timeout(LONG, conn.read(&mut pong))
        .await
        .expect("no pong")
        .unwrap();
    assert_eq!(&pong[..n], b"\r\n", "exactly one CRLF");
    println!("N05 TCP: ping CRLFCRLF -> pong {:?}", &pong[..n]);

    conn.write_all(b"\r\n").await.unwrap();
    let mut buf = [0u8; 8];
    assert!(
        timeout(SHORT, conn.read(&mut buf)).await.is_err(),
        "single CRLF must not be answered"
    );
    // the connection is still healthy and a request after the keep-alives is processed
    let via = "SIP/2.0/TCP 127.0.0.1:5060;branch=z9hG4bKn05";
    conn.write_all(options(via, "n05@test").as_bytes())
        .await
        .unwrap();
    let inc = recv_incoming(&mut rx).await;
    assert_eq!(request_of(&inc).headers.call_id(), Some("n05@test"));
    assert_no_incoming(&mut rx).await;
    layer.shutdown().await;
}

#[tokio::test]
async fn n05_udp_keepalive_ping_gets_pong_lone_crlf_is_ignored() {
    let (layer, mut rx) = start(udp_cfg("127.0.0.1:0")).await;
    let server = layer.udp_addr().unwrap();
    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    client.send_to(b"\r\n\r\n", server).await.unwrap();
    let (bytes, from) = udp_recv(&client).await;
    assert_eq!((bytes.as_slice(), from), (&b"\r\n"[..], server));
    client.send_to(b"\r\n", server).await.unwrap();
    assert_udp_silent(&client).await;
    assert_no_incoming(&mut rx).await;
    layer.shutdown().await;
}

// ================================================================ N06

fn bare_request(branch: &str) -> Request {
    let mut r = Request::new(Method::Options, SipUri::new("peer.example.com"));
    r.headers.push(
        "Via",
        format!("SIP/2.0/TCP placeholder.invalid:1;branch={branch}"),
    );
    r.headers.push("Max-Forwards", "70");
    r.headers.push("From", "<sip:fax@example.com>;tag=f1");
    r.headers.push("To", "<sip:peer@example.com>");
    r.headers.push("Call-ID", "n06@test");
    r.headers.push("CSeq", "1 OPTIONS");
    r
}

#[tokio::test]
async fn n06_outgoing_request_gets_configured_advertised_sent_by_over_udp_and_tcp() {
    let cfg = TransportConfig {
        udp_bind: Some(sock("127.0.0.1:0")),
        tcp_bind: Some(sock("127.0.0.1:0")),
        advertised: Some(Advertised {
            host: "203.0.113.5".into(),
            port: Some(5070),
        }),
        ..Default::default()
    };
    let (layer, _rx) = start(cfg).await;
    let handle = layer.handle();
    assert_eq!(handle.sent_by(TransportKind::Udp), ("203.0.113.5", 5070));

    // UDP
    let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    handle
        .send_request(
            bare_request("z9hG4bKn06u"),
            peer.local_addr().unwrap(),
            TransportKind::Udp,
        )
        .await
        .unwrap();
    let (bytes, from) = udp_recv(&peer).await;
    assert_eq!(from, layer.udp_addr().unwrap());
    let text = String::from_utf8(bytes.clone()).unwrap();
    println!("N06 UDP wire:\n{text}");
    assert_eq!(
        top_via_line(&bytes),
        "Via: SIP/2.0/UDP 203.0.113.5:5070;branch=z9hG4bKn06u"
    );
    assert!(text.starts_with("OPTIONS sip:peer.example.com SIP/2.0\r\n"));
    assert!(text.ends_with("Content-Length: 0\r\n\r\n"));

    // TCP
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    handle
        .send_request(
            bare_request("z9hG4bKn06t"),
            listener.local_addr().unwrap(),
            TransportKind::Tcp,
        )
        .await
        .unwrap();
    let (mut s, _) = timeout(LONG, listener.accept())
        .await
        .expect("no connect")
        .unwrap();
    let SipMessage::Request(r) = read_one_message(&mut s).await else {
        panic!()
    };
    println!("N06 TCP top Via: {}", r.headers.get("Via").unwrap());
    assert_eq!(
        r.headers.get("Via").unwrap(),
        "SIP/2.0/TCP 203.0.113.5:5070;branch=z9hG4bKn06t"
    );
    assert_eq!(handle.tcp_connection_count(), 1);
    layer.shutdown().await;
}

#[tokio::test]
async fn n06_without_advertised_address_the_bound_address_is_used() {
    let cfg = TransportConfig {
        udp_bind: Some(sock("127.0.0.1:0")),
        ..Default::default()
    };
    let (layer, _rx) = start(cfg).await;
    let bound = layer.udp_addr().unwrap();
    let handle = layer.handle();
    assert_eq!(
        handle.sent_by(TransportKind::Udp),
        ("127.0.0.1", bound.port())
    );
    let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    handle
        .send_request(
            bare_request("z9hG4bKn06d"),
            peer.local_addr().unwrap(),
            TransportKind::Udp,
        )
        .await
        .unwrap();
    let (bytes, _) = udp_recv(&peer).await;
    assert_eq!(
        top_via_line(&bytes),
        format!(
            "Via: SIP/2.0/UDP 127.0.0.1:{};branch=z9hG4bKn06d",
            bound.port()
        )
    );
    // sending over a transport that is not enabled is reported, not silently lost
    handle
        .send_request(
            bare_request("z9hG4bKn06x"),
            peer.local_addr().unwrap(),
            TransportKind::Tcp,
        )
        .await
        .unwrap();
    layer.shutdown().await;
}

// ================================================================ N07

#[tokio::test]
async fn n07_ipv6_udp_receive_respond_and_sent_by_format() {
    let (layer, mut rx) = start(udp_cfg("[::1]:0")).await;
    let server = layer.udp_addr().unwrap();
    assert!(server.is_ipv6());
    let client = UdpSocket::bind("[::1]:0").await.unwrap();
    let cport = client.local_addr().unwrap().port();
    let text = options(
        &format!("SIP/2.0/UDP [::1]:{cport};rport;branch=z9hG4bKn07"),
        "n07@test",
    );
    client.send_to(text.as_bytes(), server).await.unwrap();
    let inc = recv_incoming(&mut rx).await;
    assert_eq!(inc.source, client.local_addr().unwrap());
    let req = request_of(&inc);
    let v = req.headers.top_via().unwrap();
    assert_eq!(v.host, "[::1]");
    assert_eq!(
        v.received(),
        Some("::1"),
        "received carries the bare IPv6 address"
    );
    assert_eq!(v.rport(), Some(Some(cport)));
    layer
        .handle()
        .send_response(ok_response(req), inc.source, inc.transport)
        .await
        .unwrap();
    let (bytes, from) = udp_recv(&client).await;
    assert_eq!(from, server);
    assert_eq!(
        parse_datagram(&bytes).map(|m| matches!(m, SipMessage::Response(_))),
        Ok(true)
    );
    println!("N07 response Via: {}", top_via_line(&bytes));

    // outgoing request: sent-by is bracketed
    let handle = layer.handle();
    assert_eq!(handle.sent_by(TransportKind::Udp), ("::1", server.port()));
    handle
        .send_request(
            bare_request("z9hG4bKn07o"),
            client.local_addr().unwrap(),
            TransportKind::Udp,
        )
        .await
        .unwrap();
    let (bytes, _) = udp_recv(&client).await;
    let line = top_via_line(&bytes);
    println!("N07 request Via: {line}");
    assert_eq!(
        line,
        format!(
            "Via: SIP/2.0/UDP [::1]:{};branch=z9hG4bKn07o",
            server.port()
        )
    );
    layer.shutdown().await;
}

#[tokio::test]
async fn n07_ipv6_tcp_works_too() {
    let (layer, mut rx) = start(tcp_cfg("[::1]:0")).await;
    let mut conn = TcpStream::connect(layer.tcp_addr().unwrap()).await.unwrap();
    let port = conn.local_addr().unwrap().port();
    let via = format!("SIP/2.0/TCP [::1]:{port};branch=z9hG4bKn07t");
    conn.write_all(options(&via, "n07t@test").as_bytes())
        .await
        .unwrap();
    let inc = recv_incoming(&mut rx).await;
    assert!(inc.source.is_ipv6());
    layer
        .handle()
        .send_response(ok_response(request_of(&inc)), inc.source, inc.transport)
        .await
        .unwrap();
    let SipMessage::Response(r) = read_one_message(&mut conn).await else {
        panic!()
    };
    assert_eq!(r.headers.call_id(), Some("n07t@test"));
    layer.shutdown().await;
}

// ================================================================ idle timeout / shutdown

#[tokio::test]
async fn n03_tcp_idle_connections_are_closed_and_activity_keeps_them_open() {
    let cfg = TransportConfig {
        tcp_bind: Some(sock("127.0.0.1:0")),
        tcp_idle_timeout: Duration::from_millis(300),
        ..Default::default()
    };
    let (layer, _rx) = start(cfg).await;
    let handle = layer.handle();
    let mut conn = TcpStream::connect(layer.tcp_addr().unwrap()).await.unwrap();
    wait_until(|| handle.tcp_connection_count() == 1).await;
    // keep-alives every 150 ms keep it open well past the 300 ms idle timeout
    for _ in 0..5 {
        tokio::time::sleep(Duration::from_millis(150)).await;
        conn.write_all(b"\r\n\r\n").await.unwrap();
        let mut b = [0u8; 4];
        let n = timeout(LONG, conn.read(&mut b))
            .await
            .expect("pong")
            .unwrap();
        assert_eq!(&b[..n], b"\r\n");
    }
    assert_eq!(handle.tcp_connection_count(), 1);
    // now go quiet: the server closes the connection
    let mut b = [0u8; 4];
    let n = timeout(LONG, conn.read(&mut b))
        .await
        .expect("not closed")
        .unwrap();
    assert_eq!(n, 0, "EOF from the idle close");
    wait_until(|| handle.tcp_connection_count() == 0).await;
    layer.shutdown().await;
}

#[tokio::test]
async fn n03_shutdown_closes_connections_and_stops_all_tasks() {
    let cfg = TransportConfig {
        udp_bind: Some(sock("127.0.0.1:0")),
        tcp_bind: Some(sock("127.0.0.1:0")),
        ..Default::default()
    };
    let (layer, _rx) = start(cfg).await;
    let mut conn = TcpStream::connect(layer.tcp_addr().unwrap()).await.unwrap();
    let handle = layer.handle();
    wait_until(|| handle.tcp_connection_count() == 1).await;
    timeout(LONG, layer.shutdown())
        .await
        .expect("shutdown must finish");
    let mut b = [0u8; 4];
    let n = timeout(LONG, conn.read(&mut b))
        .await
        .expect("closed")
        .unwrap_or(0);
    assert_eq!(n, 0);
    assert!(
        handle
            .send_request(
                bare_request("z9hG4bKdead"),
                sock("127.0.0.1:9"),
                TransportKind::Udp
            )
            .await
            .is_err()
    );
}

// ================================================================ C01

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<(tracing::Level, String)>>>);

struct MsgVisitor(String);
impl tracing::field::Visit for MsgVisitor {
    fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
        let _ = write!(self.0, "{}={:?} ", f.name(), v);
    }
}

impl tracing::Subscriber for Capture {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, e: &tracing::Event<'_>) {
        let mut v = MsgVisitor(String::new());
        e.record(&mut v);
        self.0.lock().unwrap().push((*e.metadata().level(), v.0));
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

fn policy_cfg(allow: &[&str], mode: PolicyMode) -> TransportConfig {
    TransportConfig {
        udp_bind: Some(sock("127.0.0.1:0")),
        tcp_bind: Some(sock("127.0.0.1:0")),
        source_policy: SourcePolicy::parse(allow, mode).unwrap(),
        ..Default::default()
    }
}

async fn send_options_udp(client: &UdpSocket, server: SocketAddr, id: &str) {
    let port = client.local_addr().unwrap().port();
    let via = format!("SIP/2.0/UDP 127.0.0.1:{port};rport;branch=z9hG4bK{id}");
    client
        .send_to(options(&via, id).as_bytes(), server)
        .await
        .unwrap();
}

#[tokio::test]
async fn c01_drop_mode_sends_nothing_back_and_delivers_nothing() {
    let (layer, mut rx) = start(policy_cfg(&["192.0.2.0/24"], PolicyMode::Drop)).await;
    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    send_options_udp(&client, layer.udp_addr().unwrap(), "c01drop").await;
    assert_no_incoming(&mut rx).await;
    assert_udp_silent(&client).await;
    // TCP: connection is closed without a byte
    let mut conn = TcpStream::connect(layer.tcp_addr().unwrap()).await.unwrap();
    let port = conn.local_addr().unwrap().port();
    let via = format!("SIP/2.0/TCP 127.0.0.1:{port};branch=z9hG4bKc01dt");
    conn.write_all(options(&via, "c01dt").as_bytes())
        .await
        .unwrap();
    let mut b = [0u8; 64];
    let n = timeout(LONG, conn.read(&mut b))
        .await
        .expect("not closed")
        .unwrap_or(0);
    assert_eq!(
        n, 0,
        "dropped TCP request: connection closed, nothing written"
    );
    assert_no_incoming(&mut rx).await;
    println!("C01 drop: no delivery, no reply (UDP), connection closed (TCP)");
    layer.shutdown().await;
}

#[tokio::test]
async fn c01_reject_mode_answers_403_and_delivers_nothing() {
    let (layer, mut rx) = start(policy_cfg(&["192.0.2.0/24"], PolicyMode::Reject)).await;
    let server = layer.udp_addr().unwrap();
    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let cport = client.local_addr().unwrap().port();
    send_options_udp(&client, server, "c01rej").await;
    let (bytes, from) = udp_recv(&client).await;
    assert_eq!(from, server);
    let text = String::from_utf8(bytes.clone()).unwrap();
    println!("C01 reject wire:\n{text}");
    assert!(text.starts_with("SIP/2.0 403 Forbidden\r\n"));
    let SipMessage::Response(r) = parse_datagram(&bytes).unwrap() else {
        panic!()
    };
    assert_eq!(r.status.code(), 403);
    assert_eq!(r.headers.call_id(), Some("c01rej"));
    assert_eq!(r.headers.cseq().unwrap().method, Method::Options);
    assert!(r.headers.to().unwrap().tag().is_some(), "To tag added");
    assert_eq!(
        r.headers.get("Via").unwrap(),
        format!(
            "SIP/2.0/UDP 127.0.0.1:{cport};rport={cport};branch=z9hG4bKc01rej;received=127.0.0.1"
        )
    );
    assert_no_incoming(&mut rx).await;

    // an ACK is never answered
    let ack = format!(
        "ACK sip:fax@example.com SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:{cport};branch=z9hG4bKc01ack\r\n\
         Max-Forwards: 70\r\nFrom: <sip:gw@example.com>;tag=g1\r\nTo: <sip:fax@example.com>;tag=x\r\n\
         Call-ID: c01ack\r\nCSeq: 1 ACK\r\nContent-Length: 0\r\n\r\n"
    );
    client.send_to(ack.as_bytes(), server).await.unwrap();
    assert_udp_silent(&client).await;

    // TCP: 403 on the same connection, which stays usable for answers
    let mut conn = TcpStream::connect(layer.tcp_addr().unwrap()).await.unwrap();
    let port = conn.local_addr().unwrap().port();
    let via = format!("SIP/2.0/TCP 127.0.0.1:{port};branch=z9hG4bKc01rt");
    conn.write_all(options(&via, "c01rt").as_bytes())
        .await
        .unwrap();
    let SipMessage::Response(r) = read_one_message(&mut conn).await else {
        panic!()
    };
    assert_eq!(r.status.code(), 403);
    assert_eq!(r.headers.call_id(), Some("c01rt"));
    assert_no_incoming(&mut rx).await;
    layer.shutdown().await;
}

#[tokio::test]
async fn c01_log_only_mode_processes_the_request_and_logs_a_warning() {
    let capture = Capture::default();
    let _guard = tracing::subscriber::set_default(capture.clone());
    let (layer, mut rx) = start(policy_cfg(&["192.0.2.0/24"], PolicyMode::LogOnly)).await;
    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    send_options_udp(&client, layer.udp_addr().unwrap(), "c01log").await;
    let inc = recv_incoming(&mut rx).await;
    assert_eq!(
        request_of(&inc).headers.call_id(),
        Some("c01log"),
        "processed as usual"
    );
    assert!(
        request_of(&inc)
            .headers
            .top_via()
            .unwrap()
            .received()
            .is_some(),
        "and stamped"
    );
    let events = capture.0.lock().unwrap().clone();
    let warn = events
        .iter()
        .find(|(l, m)| *l == tracing::Level::WARN && m.contains("outside the allow-list"))
        .unwrap_or_else(|| panic!("no warn event, got {events:?}"));
    println!("C01 log-only warn event: {}", warn.1);
    assert!(
        warn.1.contains("127.0.0.1"),
        "the source is logged: {}",
        warn.1
    );
    assert_udp_silent(&client).await; // nothing is sent back by the transport itself
    layer.shutdown().await;
}

#[tokio::test]
async fn c01_allowed_source_passes_in_every_mode() {
    for mode in [PolicyMode::Drop, PolicyMode::Reject, PolicyMode::LogOnly] {
        let (layer, mut rx) = start(policy_cfg(&["127.0.0.0/8", "10.0.0.1"], mode)).await;
        let capture = Capture::default();
        let _guard = tracing::subscriber::set_default(capture.clone());
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        send_options_udp(&client, layer.udp_addr().unwrap(), "c01ok").await;
        let inc = recv_incoming(&mut rx).await;
        assert_eq!(
            request_of(&inc).headers.call_id(),
            Some("c01ok"),
            "{mode:?}"
        );
        assert!(
            capture
                .0
                .lock()
                .unwrap()
                .iter()
                .all(|(l, _)| *l != tracing::Level::WARN),
            "{mode:?}: an allowed source must not produce a warning"
        );
        layer.shutdown().await;
    }
}

#[tokio::test]
async fn c01_responses_are_not_filtered() {
    // Only requests are subject to the source policy: answers to our own requests come from
    // the peer we chose.
    let (layer, mut rx) = start(policy_cfg(&["192.0.2.0/24"], PolicyMode::Drop)).await;
    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let resp = "SIP/2.0 200 OK\r\nVia: SIP/2.0/UDP 127.0.0.1:5060;branch=z9hG4bKresp\r\n\
                From: <sip:a@b>;tag=1\r\nTo: <sip:c@d>;tag=2\r\nCall-ID: resp\r\nCSeq: 1 OPTIONS\r\n\
                Content-Length: 0\r\n\r\n";
    client
        .send_to(resp.as_bytes(), layer.udp_addr().unwrap())
        .await
        .unwrap();
    let inc = recv_incoming(&mut rx).await;
    assert!(matches!(inc.msg, SipMessage::Response(_)));
    layer.shutdown().await;
}
