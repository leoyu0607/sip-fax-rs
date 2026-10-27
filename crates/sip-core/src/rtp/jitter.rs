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
//! * When the buffer runs dry (nothing buffered at or after the playout position: a sender clock
//!   slower than ours, a stall) playout does not carry on by the clock, which would leave it
//!   ahead of every packet still to come and every one of them late for good: it waits for
//!   `start_depth` samples again, at the position where it ran dry, and plays the silence of the
//!   wait as a gap ([`Pulled::gap`]).
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
    /// Playout ran dry and waits for `start_depth` samples again (`next` is kept).
    priming: bool,
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
            priming: false,
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
        self.priming = false;
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
        if self.next.is_some() && self.frames.is_empty() {
            self.priming = true;
        }
        if self.next.is_none() || self.priming {
            let first = self.frames.keys().next().copied();
            match (first, self.end()) {
                (Some(f), Some(e)) if e - f >= self.cfg.start_depth as i64 => {
                    // after a dry spell carry on at the position where it ran dry (or at the
                    // first packet, when the ones in between were lost)
                    self.next = Some(self.next.map_or(f, |n| n.max(f)));
                    self.priming = false;
                }
                _ if self.next.is_some() => {
                    let gap = out.len() as u32;
                    self.stats.gap_samples += gap as u64;
                    return Pulled { playing: true, gap };
                }
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
mod tests;
