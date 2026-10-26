//! M5b test V05 (test plan `docs/test-plans/M5.md` section 5), half duplex: while we send V.21
//! the input is our own signal's echo (-10 dB, 0 to 60 ms late); no frame may come out of it, and
//! 75 ms after we stop sending the engine listens again.
//!
//! The half-duplex rule lives in `AudioEngine` (`deaf_until`: deaf while transmitting and for
//! `echo_guard_ms` = 75 ms after), so the test drives the engine in 20 ms steps with an echo
//! line of its own output as the input, and a peer that starts to send V.21 at a chosen moment.
//! Noise in the input at -50 dBm0 (seeded PRNG, three seeds); the seeds also shift the step phase
//! (the first step is 0-159 samples shorter).

use fax_in::audioengine::{AudioConfig, AudioEngine};
use fax_modem::hdlc;
use fax_modem::level::mean_square;
use fax_modem::v21::V21Tx;

const SEEDS: [u64; 3] = [0x5B05_0001, 0x5B05_0002, 0x5B05_0003];
const STEP: usize = 160;

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed ^ 0x9E37_79B9_7F4A_7C15)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn uniform(&mut self) -> f64 {
        ((self.next() >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }
    fn gauss(&mut self) -> f64 {
        (-2.0 * self.uniform().ln()).sqrt() * (2.0 * std::f64::consts::PI * self.uniform()).cos()
    }
}

/// The peer's TSI and DCS (V.29 9600, standard resolution), T.30 notation to wire order.
fn peer_frames() -> Vec<Vec<u8>> {
    let tsi = vec![0xFF, 0xC0, t30::fcf::TSI, b' ', b' ', b'1'];
    let dcs = vec![0xFF, 0xC8, t30::fcf::DCS, 0x00, 0x46, 0x00];
    [tsi, dcs]
        .iter()
        .map(|f| f.iter().map(|b| b.reverse_bits()).collect())
        .collect()
}

/// The peer's burst: `flags` preamble flags, TSI, DCS, closing flags.
fn peer_burst(flags: usize) -> Vec<i16> {
    V21Tx::new(-15.0).modulate(&hdlc::encode_burst(&peer_frames(), flags, 2))
}

/// The sample after the last one of the `nth` transmission (0: CED and the first DIS, 1: the DIS
/// repeated after T4, ...) of an engine that hears nothing: its output does not depend on the
/// input until a frame arrives. Transmissions are told apart by 1 s of silence.
fn transmission_end(cfg: &AudioConfig, nth: usize) -> usize {
    let mut engine = AudioEngine::new(cfg.clone());
    engine.start();
    let silence = [0i16; STEP];
    let mut ends: Vec<usize> = Vec::new();
    let mut last = 0usize;
    for step in 0..(16 * 50) {
        let mut tx = [0i16; STEP];
        engine.process(&silence, &mut tx);
        for (i, &s) in tx.iter().enumerate() {
            if s != 0 {
                let t = step * STEP + i + 1;
                if last != 0 && t - last > 8000 {
                    ends.push(last);
                }
                last = t;
            }
        }
    }
    ends.push(last);
    assert!(
        ends.len() > nth,
        "only {} transmissions in 16 s",
        ends.len()
    );
    ends[nth]
}

/// One call. The engine's own output comes back after `delay` samples at `gain` (0.316 is
/// -10 dB), starting from sample `echo_from` of its output; the peer starts `gap` samples after
/// the end of the engine's `nth` transmission (and mixes with the echo, which is silent by then
/// unless the delay is long); `burst` is the peer's audio. The run ends 2 s after it.
struct Call {
    cfg: AudioConfig,
    delay: usize,
    gain: f64,
    echo_from: usize,
    gap: usize,
    burst: Vec<i16>,
    nth: usize,
    seed: u64,
}

impl Call {
    fn new(seed: u64) -> Self {
        Self {
            cfg: AudioConfig::default(),
            delay: 0,
            gain: 0.316,
            echo_from: 0,
            gap: 75 * 8,
            burst: Vec::new(),
            nth: 0,
            seed,
        }
    }

    fn run(self) -> Outcome {
        let mut rng = Rng::new(self.seed);
        let noise_sd = f64::from(mean_square(-50.0)).sqrt();
        let tx_end = transmission_end(&self.cfg, self.nth);
        let peer_start = tx_end + self.gap;
        let end_at = peer_start + self.burst.len() + 2 * 8000;
        let mut engine = AudioEngine::new(self.cfg.clone());
        engine.start();
        let mut sent: Vec<i16> = Vec::new(); // everything the engine has sent
        let mut before = Vec::new();
        let mut after = Vec::new();
        let first = (self.seed % 160) as usize;
        let mut pos = 0usize;
        while pos < end_at {
            let n = if pos == 0 && first > 0 { first } else { STEP };
            let rx: Vec<i16> = (0..n)
                .map(|i| {
                    let t = pos + i;
                    let echo = if t >= self.delay
                        && t - self.delay >= self.echo_from
                        && t - self.delay < sent.len()
                    {
                        f64::from(sent[t - self.delay]) * self.gain
                    } else {
                        0.0
                    };
                    let peer = if t >= peer_start && t - peer_start < self.burst.len() {
                        f64::from(self.burst[t - peer_start])
                    } else {
                        0.0
                    };
                    (echo + peer + noise_sd * rng.gauss())
                        .round()
                        .clamp(-32768.0, 32767.0) as i16
                })
                .collect();
            let mut tx = vec![0i16; n];
            engine.process(&rx, &mut tx);
            sent.extend_from_slice(&tx);
            pos += n;
            let log = engine.drain_log();
            // lines of the step in which the peer's first sample went in belong to "after"
            if pos > peer_start {
                after.extend(log);
            } else {
                before.extend(log);
            }
        }
        Outcome {
            before,
            after,
            tx_end,
            peer_start,
            state: engine.state(),
            done: engine.is_done(),
        }
    }
}

struct Outcome {
    /// Log lines before the peer began.
    before: Vec<String>,
    /// Log lines from the peer's start on.
    after: Vec<String>,
    tx_end: usize,
    peer_start: usize,
    state: t30::State,
    done: bool,
}

impl std::fmt::Debug for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "before {:#?}, after {:#?}", self.before, self.after)
    }
}

impl Outcome {
    /// Lines that say something was heard (a frame, CNG) before the peer began.
    fn heard_before(&self) -> Vec<&String> {
        self.before
            .iter()
            .filter(|l| l.starts_with("V.21 rx") || l.contains("CNG heard"))
            .collect()
    }
    fn heard_dcs(&self) -> bool {
        self.after
            .iter()
            .any(|l| l.starts_with("V.21 rx DCS FF C8 "))
            && self.after.iter().any(|l| l.starts_with("V.21 rx TSI "))
    }
}

// ------------------------------------------------------------------------------------- tests

#[test]
fn v05_our_own_echo_is_never_a_frame() {
    // no peer at all: the engine sends CED, the DIS, and the DIS again on T4 (3 s); its own
    // signal comes back at -10 dB, 0 to 60 ms late, with noise. Nothing is heard, nothing is
    // answered, the call is where it was.
    for seed in SEEDS {
        for delay_ms in [0usize, 10, 20, 40, 60] {
            let mut c = Call::new(seed);
            c.delay = delay_ms * 8;
            c.nth = 1; // the run lasts through the second DIS
            let o = c.run();
            assert!(
                o.before
                    .iter()
                    .filter(|l| l.starts_with("V.21 tx DIS"))
                    .count()
                    >= 2,
                "seed {seed:#x}: {o:?}"
            );
            assert!(
                o.heard_before().is_empty() && o.after.iter().all(|l| !l.starts_with("V.21 rx")),
                "seed {seed:#x}, echo {delay_ms} ms: {o:?}"
            );
            assert!(!o.done);
            assert_eq!(o.state, t30::State::WaitCommand);
        }
    }
}

#[test]
fn v05_listening_resumes_75_ms_after_our_signal_ends() {
    // the peer starts exactly 75 ms after the end of our repeated DIS, with a short preamble (4
    // flags: nothing of it to spare), with our echo up to 60 ms behind us
    let burst = peer_burst(4);
    for seed in SEEDS {
        for delay_ms in [0usize, 10, 20, 40, 60] {
            let mut c = Call::new(seed);
            c.delay = delay_ms * 8;
            c.nth = 1;
            c.burst = burst.clone();
            let o = c.run();
            assert!(o.heard_before().is_empty(), "seed {seed:#x}: {o:?}");
            assert!(
                o.heard_dcs(),
                "seed {seed:#x}, echo {delay_ms} ms: the peer's DCS, 75 ms after our end \
                 (peer at sample {}, our end {}), was not heard: {o:?}",
                o.peer_start,
                o.tx_end
            );
        }
    }
}

#[test]
fn v05_a_peer_that_starts_at_once_is_heard_too_with_a_normal_preamble() {
    // a peer with the usual 1 s of flags that starts 0, 30, 60 ms after our end: the first part
    // of its preamble falls into the guard, the rest is heard (repeated DIS)
    let burst = peer_burst(38);
    for seed in SEEDS {
        for gap_ms in [0usize, 30, 60] {
            let mut c = Call::new(seed);
            c.delay = 40 * 8;
            c.nth = 1;
            c.gap = gap_ms * 8;
            c.burst = burst.clone();
            let o = c.run();
            assert!(o.heard_before().is_empty(), "{o:?}");
            assert!(
                o.heard_dcs(),
                "seed {seed:#x}, peer {gap_ms} ms after our end: {o:?}"
            );
        }
    }
}

#[test]
fn v05_after_the_first_transmission_a_peer_from_120_ms_on_is_heard() {
    for seed in SEEDS {
        for gap_ms in [120usize, 150, 300] {
            let mut c = Call::new(seed);
            c.delay = 40 * 8;
            c.gap = gap_ms * 8;
            c.burst = peer_burst(4);
            let o = c.run();
            assert!(
                o.heard_dcs(),
                "seed {seed:#x}, peer {gap_ms} ms after our end: {o:?}"
            );
        }
    }
}

#[test]
fn v05_after_the_first_transmission_a_peer_from_55_ms_on_is_heard() {
    // The engine listens only once its CED and first DIS are out; the peer's DCS may already be
    // on by then. Every gap T.30 allows (75 ms +- 20 ms) and beyond, every seed. (Before the M5b
    // review the detector took the first block it heard as the line's noise and missed these.)
    let mut missed = Vec::new();
    for seed in SEEDS {
        for gap_ms in (0..=140).step_by(5) {
            let mut c = Call::new(seed);
            c.delay = 40 * 8;
            c.gap = gap_ms * 8;
            c.burst = peer_burst(4);
            if !c.run().heard_dcs() {
                missed.push((seed, gap_ms));
            }
        }
    }
    eprintln!("V05 first DIS, (seed, peer gap ms) whose DCS was not heard: {missed:?}");
    assert!(missed.iter().all(|&(_, g)| g < 55), "{missed:?}");
}

#[test]
fn v05_plan_a_peer_75_ms_after_the_first_transmission_is_heard() {
    for seed in SEEDS {
        for delay_ms in [0usize, 20, 60] {
            let mut c = Call::new(seed);
            c.delay = delay_ms * 8;
            c.burst = peer_burst(4);
            let o = c.run();
            assert!(o.heard_before().is_empty(), "{o:?}");
            assert!(o.heard_dcs(), "seed {seed:#x}, echo {delay_ms} ms: {o:?}");
        }
    }
}

#[test]
fn v05_control_an_echo_that_comes_back_after_the_guard_is_heard() {
    // so that the tests above could have failed: our DIS (not the CED) comes back 1.5 s late,
    // after the guard, and is taken for a frame of the peer
    let mut c = Call::new(SEEDS[0]);
    c.delay = 12_000;
    c.echo_from = 3 * 8000 + 1_000; // from the DIS on (CED ends at 3.2 s)
    c.nth = 1;
    let o = c.run();
    let heard: Vec<&String> = o
        .before
        .iter()
        .chain(&o.after)
        .filter(|l| l.starts_with("V.21 rx DIS"))
        .collect();
    assert!(
        !heard.is_empty(),
        "an echo 1.5 s late should be taken for a DIS: {o:?}"
    );
}

#[test]
fn v05_the_guard_setting_is_not_the_only_protection() {
    // the guard at 0 ms: the deaf period during our own signal still covers the echo while we
    // send, and the echo's tail beyond the end is flags without a frame: no frame either way
    for guard in [0u32, 75] {
        let mut c = Call::new(SEEDS[1]);
        c.cfg.echo_guard_ms = guard;
        c.delay = 60 * 8;
        c.nth = 1;
        let o = c.run();
        eprintln!(
            "V05 guard {guard} ms: {} lines heard",
            o.heard_before().len()
        );
        assert!(o.heard_before().is_empty(), "guard {guard}: {o:?}");
    }
}
