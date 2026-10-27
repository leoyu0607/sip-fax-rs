use super::*;

fn seqs(b: &Backlog) -> Vec<u64> {
    match b {
        Backlog::Events(v) => v.iter().map(|e| e.seq).collect(),
        Backlog::Resync => panic!("resync"),
    }
}

#[test]
fn since_replays_what_followed_and_resyncs_outside_the_ring() {
    let bus = EventBus::new();
    assert!(matches!(bus.attach(Some(0)).1, Backlog::Events(v) if v.is_empty()));
    for i in 0..(RING as u64 + 10) {
        bus.publish("t", json!(i));
    }
    let last = bus.last_seq();
    assert_eq!(last, RING as u64 + 10);
    assert_eq!(
        seqs(&bus.attach(Some(last - 3)).1),
        [last - 2, last - 1, last]
    );
    assert!(seqs(&bus.attach(Some(last)).1).is_empty());
    assert!(seqs(&bus.attach(None).1).is_empty());
    // the oldest kept is last - RING + 1: since = last - RING still fits, one less does not
    assert_eq!(seqs(&bus.attach(Some(last - RING as u64)).1).len(), RING);
    assert!(matches!(
        bus.attach(Some(last - RING as u64 - 1)).1,
        Backlog::Resync
    ));
    // a client of an earlier process
    assert!(matches!(bus.attach(Some(last + 1)).1, Backlog::Resync));
}

#[test]
fn the_live_receiver_continues_the_backlog_without_gaps() {
    let bus = EventBus::new();
    bus.publish("a", json!(1));
    let (mut rx, b, last) = bus.attach(Some(0));
    assert_eq!(last, 1);
    assert_eq!(seqs(&b), [1]);
    bus.publish("b", json!(2));
    let e = rx.try_recv().unwrap();
    assert_eq!(e.seq, 2);
    let v: Value = serde_json::from_str(&e.json).unwrap();
    assert_eq!(
        (v["seq"].as_u64(), v["type"].as_str()),
        (Some(2), Some("b"))
    );
    assert_eq!(v["data"], 2);
    assert!(v["at"].is_string());
}

#[tokio::test(start_paused = true)]
async fn status_changes_are_published_once() {
    struct S(Mutex<ServiceStatus>);
    impl StatusSource for S {
        fn status(&self) -> ServiceStatus {
            self.0.lock().unwrap().clone()
        }
    }
    let s = Arc::new(S(Mutex::new(ServiceStatus {
        version: "v".into(),
        state: "running".into(),
        started_at: "t".into(),
        uptime_secs: 0,
        listen: vec![],
        active_calls: 0,
        trunks: vec![],
        advertised_missing: false,
    })));
    let bus = Arc::new(EventBus::new());
    let (mut rx, _, _) = bus.attach(None);
    let task = watch_status(bus.clone(), s.clone(), Vec::new, Duration::from_secs(1));
    tokio::time::sleep(Duration::from_millis(2500)).await;
    // the uptime moves, nothing that counts does
    s.0.lock().unwrap().uptime_secs = 99;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(rx.try_recv().is_err());
    s.0.lock().unwrap().active_calls = 1;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let e = rx.try_recv().unwrap();
    assert!(e.json.contains("\"status.changed\""), "{}", e.json);
    assert!(e.json.contains("\"active_calls\":1"), "{}", e.json);
    assert!(rx.try_recv().is_err(), "once");
    task.abort();
}
