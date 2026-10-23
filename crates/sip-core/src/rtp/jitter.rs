//! A receive jitter buffer for constant-rate audio (G.711): packets go in by RTP timestamp, the
//! player takes samples out at the clock rate. No I/O and no clock: the caller pulls a block
//! every time its own clock says so (fax-in: every 20 ms).
//!
//! * Samples are placed by RTP timestamp, never by arrival time: reordering inside the buffer
//!   depth costs nothing, and a lost packet leaves a hole of exactly its length (filled with
//!   silence and reported as [`Pulled::gap`]) so that what follows stays in place — a modem
//!   receiver keeps its symbol timing across it.
//! * Playout starts once `start_depth` samples are buffered.
//! * A packet older than the playout position is dropped ([`JitterStats::late`]); one whose
//!   timestamp was seen already is dropped ([`JitterStats::duplicate`]).
//! * A new SSRC, or a timestamp more than [`JitterConfig::resync_after`] away from the playout
//!   position, starts over ([`JitterStats::resyncs`]).
//! * Sender clock drift fills the buffer slowly; past `max_depth` the oldest samples are dropped
//!   down to `start_depth` at once ([`JitterStats::dropped`]), never one by one in the middle of
//!   the signal.

use std::collections::BTreeMap;

/// Depths and limits, in samples of the clock rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JitterConfig {
    /// Samples buffered before playout starts (and kept after an overflow).
    pub start_depth: u32,
    /// Buffered samples beyond which the oldest are dropped.
    pub max_depth: u32,
    /// A timestamp this far from the playout position (either way) restarts the buffer.
    pub resync_after: u32,
}

impl JitterConfig {
    /// 8 kHz: start at 60 ms, drop beyond 300 ms, restart beyond 2 s.
    pub const G711: JitterConfig = JitterConfig {
        start_depth: 480,
        max_depth: 2400,
        resync_after: 16_000,
    };
}

/// Counters since the buffer was made.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JitterStats {
    /// Packets accepted.
    pub packets: u64,
    /// Packets older than the playout position.
    pub late: u64,
    /// Packets with a timestamp already buffered.
    pub duplicate: u64,
    /// Restarts (new SSRC, timestamp jump).
    pub resyncs: u64,
    /// Samples played as silence because nothing was there (lost packets).
    pub gap_samples: u64,
    /// Samples dropped because the buffer grew past `max_depth`.
    pub dropped: u64,
}

/// What one [`JitterBuffer::pull`] produced besides the samples.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Pulled {
    /// Playout has started (before that the block is silence and not a gap).
    pub playing: bool,
    /// Samples of the block that were filled with silence for want of a packet.
    pub gap: u32,
}

/// The buffer.
#[derive(Debug, Clone)]
pub struct JitterBuffer {
    cfg: JitterConfig,
    ssrc: Option<u32>,
    /// Last timestamp seen and its extended (unwrapped) value.
    last: Option<(u32, i64)>,
    /// Buffered packets by extended timestamp.
    frames: BTreeMap<i64, Vec<i16>>,
    /// Playout position (extended timestamp of the next sample), once started.
    next: Option<i64>,
    stats: JitterStats,
}

impl JitterBuffer {
    /// An empty buffer.
    pub fn new(cfg: JitterConfig) -> Self {
        Self {
            cfg,
            ssrc: None,
            last: None,
            frames: BTreeMap::new(),
            next: None,
            stats: JitterStats::default(),
        }
    }

    /// The counters.
    pub fn stats(&self) -> JitterStats {
        self.stats
    }

    /// Samples buffered ahead of the playout position (or in total before playout starts).
    pub fn buffered(&self) -> u32 {
        let Some(end) = self.end() else { return 0 };
        let from = self
            .next
            .or_else(|| self.frames.keys().next().copied())
            .unwrap_or(end);
        (end - from).max(0) as u32
    }

    fn end(&self) -> Option<i64> {
        self.frames
            .iter()
            .next_back()
            .map(|(k, v)| k + v.len() as i64)
    }

    fn restart(&mut self) {
        self.frames.clear();
        self.next = None;
        self.last = None;
        self.stats.resyncs += 1;
    }

    /// Unwraps a 32-bit timestamp against the last one seen.
    fn extend(&mut self, ts: u32) -> i64 {
        let ext = match self.last {
            None => (1i64 << 32) + ts as i64,
            Some((lts, lext)) => lext + ts.wrapping_sub(lts) as i32 as i64,
        };
        match self.last {
            Some((_, lext)) if ext < lext => {}
            _ => self.last = Some((ts, ext)),
        }
        ext
    }

    /// Adds a packet's decoded samples.
    pub fn push(&mut self, ssrc: u32, timestamp: u32, samples: Vec<i16>) {
        if samples.is_empty() {
            return;
        }
        if self.ssrc != Some(ssrc) {
            if self.ssrc.is_some() {
                self.restart();
            }
            self.ssrc = Some(ssrc);
        }
        let mut ext = self.extend(timestamp);
        if let Some(next) = self.next
            && (ext - next).abs() > self.cfg.resync_after as i64
        {
            self.restart();
            ext = self.extend(timestamp);
        }
        let end = ext + samples.len() as i64;
        if let Some(next) = self.next
            && end <= next
        {
            self.stats.late += 1;
            return;
        }
        if self.frames.contains_key(&ext) {
            self.stats.duplicate += 1;
            return;
        }
        self.stats.packets += 1;
        self.frames.insert(ext, samples);
        self.limit();
    }

    /// Keeps at most `max_depth` samples ahead of the playout position.
    fn limit(&mut self) {
        let (Some(next), Some(end)) = (self.next, self.end()) else {
            return;
        };
        if end - next <= self.cfg.max_depth as i64 {
            return;
        }
        let to = end - self.cfg.start_depth as i64;
        self.stats.dropped += (to - next) as u64;
        self.next = Some(to);
        self.forget_before(to);
    }

    fn forget_before(&mut self, pos: i64) {
        while let Some((&k, v)) = self.frames.iter().next() {
            if k + v.len() as i64 <= pos {
                self.frames.remove(&k);
            } else {
                break;
            }
        }
    }

    /// Fills `out` with the next samples (silence before playout starts and where packets are
    /// missing).
    pub fn pull(&mut self, out: &mut [i16]) -> Pulled {
        out.fill(0);
        if self.next.is_none() {
            let first = self.frames.keys().next().copied();
            match (first, self.end()) {
                (Some(f), Some(e)) if e - f >= self.cfg.start_depth as i64 => self.next = Some(f),
                _ => return Pulled::default(),
            }
        }
        let Some(mut pos) = self.next else {
            return Pulled::default();
        };
        let mut gap = 0u32;
        let mut i = 0usize;
        while i < out.len() {
            // the frame that covers `pos`, else the first one after it
            let covering = self
                .frames
                .range(..=pos)
                .next_back()
                .filter(|(k, v)| pos < **k + v.len() as i64)
                .map(|(k, _)| *k);
            if let Some(k) = covering {
                let v = &self.frames[&k];
                let from = (pos - k) as usize;
                let n = (v.len() - from).min(out.len() - i);
                out[i..i + n].copy_from_slice(&v[from..from + n]);
                i += n;
                pos += n as i64;
            } else {
                let until = self.frames.range(pos..).next().map(|(k, _)| *k);
                let n = until.map_or(out.len() - i, |u| ((u - pos) as usize).min(out.len() - i));
                gap += n as u32;
                i += n;
                pos += n as i64;
            }
        }
        self.next = Some(pos);
        self.forget_before(pos);
        self.stats.gap_samples += gap as u64;
        Pulled { playing: true, gap }
    }
}

#[cfg(test)]
mod tests {
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
}
