use super::*;

const CFG: JitterConfig = JitterConfig {
    start_depth: 320,
    max_depth: 1600,
    resync_after: 16_000,
};

fn frame(ts: u32) -> Vec<i16> {
    (0..160).map(|i| (ts as i64 + i) as i16).collect()
}

fn pull(jb: &mut JitterBuffer) -> (Vec<i16>, Pulled) {
    let mut out = vec![0; 160];
    let p = jb.pull(&mut out);
    (out, p)
}

#[test]
fn plays_in_timestamp_order_after_the_start_depth() {
    let mut jb = JitterBuffer::new(CFG);
    jb.push(1, 1000, frame(1000));
    assert_eq!(
        pull(&mut jb).1,
        Pulled {
            playing: false,
            gap: 0
        }
    );
    // out of order: 1320 before 1160
    jb.push(1, 1320, frame(1320));
    jb.push(1, 1160, frame(1160));
    for ts in [1000, 1160, 1320] {
        let (s, p) = pull(&mut jb);
        assert_eq!(
            p,
            Pulled {
                playing: true,
                gap: 0
            }
        );
        assert_eq!(s, frame(ts));
    }
}

#[test]
fn a_lost_packet_is_a_gap_of_its_length_and_what_follows_stays_in_place() {
    let mut jb = JitterBuffer::new(CFG);
    for ts in [0u32, 160, 480, 640] {
        jb.push(7, ts, frame(ts));
    }
    let _ = pull(&mut jb);
    let _ = pull(&mut jb);
    let (s, p) = pull(&mut jb);
    assert_eq!(p.gap, 160);
    assert!(s.iter().all(|&x| x == 0));
    assert_eq!(pull(&mut jb).0, frame(480));
    assert_eq!(jb.stats().gap_samples, 160);
}

#[test]
fn late_duplicate_wrap_and_new_ssrc() {
    let mut jb = JitterBuffer::new(CFG);
    let base = u32::MAX - 319; // wraps after two frames
    for k in 0..4u32 {
        let ts = base.wrapping_add(160 * k);
        jb.push(1, ts, frame(ts));
    }
    jb.push(1, base, frame(base));
    assert_eq!(jb.stats().duplicate, 1);
    for k in 0..4u32 {
        let ts = base.wrapping_add(160 * k);
        assert_eq!(
            pull(&mut jb),
            (
                frame(ts),
                Pulled {
                    playing: true,
                    gap: 0
                }
            )
        );
    }
    jb.push(1, base, frame(base));
    assert_eq!(jb.stats().late, 1);
    // another source: start over
    jb.push(2, 5000, frame(5000));
    assert_eq!(jb.stats().resyncs, 1);
    assert!(!pull(&mut jb).1.playing);
}

#[test]
fn a_fast_sender_is_trimmed_to_the_start_depth_at_once() {
    let mut jb = JitterBuffer::new(CFG);
    let mut ts = 0u32;
    // two frames in, one out, many times
    for _ in 0..20 {
        jb.push(1, ts, frame(ts));
        ts += 160;
        jb.push(1, ts, frame(ts));
        ts += 160;
        let _ = pull(&mut jb);
        assert!(jb.buffered() <= CFG.max_depth, "{}", jb.buffered());
    }
    assert!(jb.stats().dropped > 0);
    assert_eq!(jb.stats().gap_samples, 0);
}
