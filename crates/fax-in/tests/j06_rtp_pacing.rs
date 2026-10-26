//! J06 (docs/test-plans/M5.md section 3): the G.711 session sends one RTP packet every 20 ms,
//! the timestamp advances by 160 and the sequence number by 1 per packet, and when the task
//! falls behind the packets it missed are sent back to back (nothing is dropped, the delay
//! does not grow).
//!
//! Tokio's clock is paused: the 20 ms ticks of the session run on virtual time, so the
//! spacing is exact and the test is fast and deterministic. The sockets are real (localhost);
//! the peer is a plain UDP socket that never answers, so the session only listens and sends
//! silence (its engine is not started).

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fax_in::AudioNegotiated;
use fax_in::config::FaxSection;
use fax_in::session::{AudioContext, AudioFaxSession, FaxSessionFactory, SessionOutcome};
use fax_in::t38engine::Pacing;
use fax_in::t38session::{T38SessionEnv, T38SessionFactory};
use sip_core::rtp::{Law, RtpPacket};
use tokio::net::UdpSocket;
use tokio::sync::watch;
use tokio::time::Instant;

struct Rig {
    peer: UdpSocket,
    cancel: watch::Sender<bool>,
    _go: watch::Sender<bool>,
    task: tokio::task::JoinHandle<SessionOutcome>,
}

async fn start(payload_type: u8) -> Rig {
    // a port that is free now
    let probe = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let local_port = probe.local_addr().unwrap().port();
    drop(probe);
    let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let peer_addr: SocketAddr = peer.local_addr().unwrap();
    let env = T38SessionEnv {
        data_dir: std::env::temp_dir().join("fax-in-j06"),
        capture: None,
        bind_ip: "127.0.0.1".parse().unwrap(),
        station_id: "0287654321".into(),
        max_duration: Duration::from_secs(600),
        pacing: Pacing::default(),
    };
    let factory = T38SessionFactory::new(env);
    let (cancel_tx, cancel) = watch::channel(false);
    let (go_tx, go) = watch::channel(false);
    let ctx = AudioContext {
        call_id: "j06".into(),
        trunk: "t".into(),
        audio: AudioNegotiated {
            codec: if payload_type == 8 { "PCMA" } else { "PCMU" }.into(),
            payload_type,
            peer_media: Some(peer_addr.to_string()),
            local_port,
            reason: None,
        },
        fax: FaxSection::default(),
        report: Arc::new(Mutex::new(None)),
        progress: None,
        go,
        cancel,
        station_id: None,
        max_duration: None,
    };
    let session: Box<dyn AudioFaxSession> = factory.create_audio(&ctx).expect("an audio session");
    let task = tokio::spawn(session.run(ctx));
    Rig {
        peer,
        cancel: cancel_tx,
        _go: go_tx,
        task,
    }
}

impl Rig {
    /// The next RTP packet and the (virtual) time it arrived.
    async fn next(&self) -> (RtpPacket, Instant) {
        let mut buf = [0u8; 2048];
        loop {
            // virtual time passes while we wait: the session's next tick is what wakes us
            let (n, _) = self.peer.recv_from(&mut buf).await.unwrap();
            if let Ok(p) = RtpPacket::decode(&buf[..n]) {
                return (p, Instant::now());
            }
        }
    }

    /// Every packet already in the socket.
    fn drain(&self) -> Vec<RtpPacket> {
        let mut out = Vec::new();
        let mut buf = [0u8; 2048];
        while let Ok((n, _)) = self.peer.try_recv_from(&mut buf) {
            out.push(RtpPacket::decode(&buf[..n]).unwrap());
        }
        out
    }

    async fn finish(self) {
        self.cancel.send(true).unwrap();
        let out = self.task.await.unwrap();
        assert_eq!(
            out,
            SessionOutcome::Aborted,
            "listening only: nothing to report"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn j06_one_packet_every_20_ms_with_plus_160_and_plus_one() {
    for (pt, law) in [(0u8, Law::Ulaw), (8u8, Law::Alaw)] {
        let rig = start(pt).await;
        let mut got = Vec::new();
        for _ in 0..50 {
            got.push(rig.next().await);
        }
        for w in got.windows(2) {
            let (a, ta) = (&w[0].0, w[0].1);
            let (b, tb) = (&w[1].0, w[1].1);
            assert_eq!(
                tb - ta,
                Duration::from_millis(20),
                "packets are 20 ms apart"
            );
            assert_eq!(b.sequence, a.sequence.wrapping_add(1), "sequence +1");
            assert_eq!(b.timestamp, a.timestamp.wrapping_add(160), "timestamp +160");
            assert_eq!(b.ssrc, a.ssrc, "one SSRC");
            assert_eq!(b.payload_type, pt);
            assert_eq!(b.payload.len(), 160, "20 ms of G.711");
        }
        // listening: silence of the negotiated law
        assert!(
            got.iter()
                .all(|(p, _)| p.payload == vec![law.silence(); 160])
        );
        rig.finish().await;
    }
}

#[tokio::test(start_paused = true)]
async fn j06_a_late_task_catches_up_without_dropping_packets() {
    let rig = start(0).await;
    let (first, t0) = rig.next().await;
    // the task is "stuck" for 200 ms: the clock moves, the session runs the ticks it missed
    tokio::time::advance(Duration::from_millis(200)).await;
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
    let burst = rig.drain();
    let at = Instant::now();
    assert_eq!(at - t0, Duration::from_millis(200));
    assert_eq!(
        burst.len(),
        10,
        "200 ms late = ten ticks, all sent (not dropped, not merged)"
    );
    let mut prev = first;
    for p in &burst {
        assert_eq!(p.sequence, prev.sequence.wrapping_add(1), "no sequence gap");
        assert_eq!(
            p.timestamp,
            prev.timestamp.wrapping_add(160),
            "no timestamp gap"
        );
        prev = p.clone();
    }
    // and then the pace is 20 ms again, from where the schedule is (no extra delay)
    let (p, t1) = rig.next().await;
    assert_eq!(p.sequence, prev.sequence.wrapping_add(1));
    assert_eq!(p.timestamp, prev.timestamp.wrapping_add(160));
    assert_eq!(t1 - at, Duration::from_millis(20), "the delay did not grow");
    // over the whole stretch the packet count follows the clock: 1 + 10 + 1 packets in 220 ms
    assert_eq!(t1 - t0, Duration::from_millis(220));
    rig.finish().await;
}
