//! M5d tests B01-B07, B10: our V.29 receiver (`fax_modem::v29::V29Rx`, and through the audio
//! engine) against SpanDSP's `v29_tx` (and, for B10, `v17_tx`), the independent transmitters.
//!
//! The source is SpanDSP's `v29_tx` (mandatory); our own test transmitter
//! (`fax_modem::v29::testtx::Tx`) is a second source. Both go through the same channel model
//! ([`Chan`], the one of `a_v27ter_spandsp.rs`): level, a frequency offset, a clock deviation,
//! white Gaussian noise at a given SNR, a G.711 round trip (SpanDSP's `g711_encode` /
//! `g711_decode`) and one 20 ms gap. All noise and data come from a seeded PRNG, three seeds per
//! case.

use fax_in::audioengine::{AudioConfig, AudioEngine};
use fax_modem::hdlc::{self, HdlcEvent, HdlcRx};
use fax_modem::level::mean_square;
use fax_modem::v21::{V21Rx, V21Tx};
use fax_modem::v29::testtx::Tx;
use fax_modem::v29::{Rate, RxEvent, V29Rx};
use fax_sim::SplitMix64;
use spandsp_sys as sys;
use std::ffi::c_void;
use std::os::raw::c_int;

const SEEDS: [u64; 3] = [0xB290_0001, 0xB290_0002, 0xB290_0003];
const RATES: [Rate; 2] = [Rate::B7200, Rate::B9600];

fn bps(rate: Rate) -> i32 {
    match rate {
        Rate::B7200 => 7200,
        Rate::B9600 => 9600,
    }
}

// ------------------------------------------------------------------------------------- random

struct Rng(SplitMix64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(SplitMix64::new(seed))
    }
    fn uniform(&mut self) -> f64 {
        ((self.0.next_u64() >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }
    fn gauss(&mut self) -> f64 {
        (-2.0 * self.uniform().ln()).sqrt() * (2.0 * std::f64::consts::PI * self.uniform()).cos()
    }
    fn bits(&mut self, n: usize) -> Vec<bool> {
        (0..n).map(|_| self.0.next_u64() & 1 != 0).collect()
    }
}

// ------------------------------------------------------------------------ the SpanDSP source

struct BitFeed {
    bits: Vec<bool>,
    pos: usize,
}

unsafe extern "C" fn get_bit(user: *mut c_void) -> c_int {
    // SAFETY: `user` is the `BitFeed` registered in `spandsp_v29`, alive for the whole call.
    let feed = unsafe { &mut *(user as *mut BitFeed) };
    match feed.bits.get(feed.pos) {
        Some(&b) => {
            feed.pos += 1;
            c_int::from(b)
        }
        None => sys::SIG_STATUS_END_OF_DATA,
    }
}

/// SpanDSP's V.29 transmission of `bits` (training, then the data, then the shutdown), at
/// -10 dBm0, without leading or trailing silence beyond what SpanDSP produces itself.
fn spandsp_v29(rate: Rate, bits: &[bool]) -> Vec<i16> {
    let mut feed = Box::new(BitFeed {
        bits: bits.to_vec(),
        pos: 0,
    });
    // SAFETY: NULL makes SpanDSP allocate the state; `feed` outlives it (freed below).
    let tx = unsafe {
        sys::v29_tx_init(
            std::ptr::null_mut(),
            bps(rate),
            false,
            Some(get_bit),
            &mut *feed as *mut BitFeed as *mut c_void,
        )
    };
    assert!(!tx.is_null(), "v29_tx_init failed");
    // SAFETY: valid state.
    unsafe { sys::v29_tx_power(tx, -10.0) };
    let mut out = Vec::new();
    let mut buf = [0i16; 160];
    loop {
        // SAFETY: `buf` is valid for 160 samples.
        let n = unsafe { sys::v29_tx(tx, buf.as_mut_ptr(), 160) } as usize;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
        assert!(out.len() < 8_000 * 3_000, "v29_tx does not stop");
    }
    // SAFETY: allocated by the init call, freed once.
    unsafe { sys::v29_tx_free(tx) };
    assert_eq!(feed.pos, bits.len(), "SpanDSP did not take all the bits");
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Src {
    SpanDsp,
    Ours,
}

fn source(src: Src, rate: Rate, bits: &[bool]) -> Vec<i16> {
    match src {
        Src::SpanDsp => spandsp_v29(rate, bits),
        Src::Ours => Tx {
            rate,
            level_dbm0: -10.0,
            carrier_offset_hz: 0.0,
            clock: 1.0,
        }
        .modulate(bits),
    }
}

// ------------------------------------------------------------------------------ channel model

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Law {
    Mu,
    A,
}

/// The channel between the transmitter and the receiver.
#[derive(Clone, Debug)]
struct Chan {
    /// Signal level over the active part (dBm0).
    level: f64,
    /// White noise over the whole band, relative to the signal power.
    snr_db: Option<f64>,
    /// Sender clock deviation (+100 means the sender runs 100 ppm fast).
    ppm: f64,
    /// Frequency shift of the whole signal (Hz).
    offset_hz: f64,
    /// G.711 round trip.
    law: Option<Law>,
    /// One 20 ms gap (zeros) at this fraction of the signal.
    gap_at: Option<f64>,
}

impl Chan {
    fn clean(level: f64) -> Self {
        Self {
            level,
            snr_db: None,
            ppm: 0.0,
            offset_hz: 0.0,
            law: None,
            gap_at: None,
        }
    }
    fn snr(mut self, db: f64) -> Self {
        self.snr_db = Some(db);
        self
    }
    fn ppm(mut self, ppm: f64) -> Self {
        self.ppm = ppm;
        self
    }
    fn offset(mut self, hz: f64) -> Self {
        self.offset_hz = hz;
        self
    }
    fn law(mut self, law: Law) -> Self {
        self.law = Some(law);
        self
    }
    fn gap(mut self, at: f64) -> Self {
        self.gap_at = Some(at);
        self
    }
}

/// Audio after the channel, with where the signal is.
struct Heard {
    audio: Vec<i16>,
    /// First sample of the signal.
    onset: usize,
    /// One past the last sample of the signal.
    end: usize,
}

/// Silence before and after the signal (200 ms): the noise is there too.
const PAD: usize = 1_600;

/// Shifts every frequency of the (real, band-limited) signal by `hz`: the analytic signal from a
/// 255-tap Hilbert transformer (Hann window), times a complex exponential.
fn freq_shift(x: &[f64], hz: f64) -> Vec<f64> {
    const M: usize = 127;
    let w = 2.0 * std::f64::consts::PI * hz / 8000.0;
    let taps: Vec<(usize, f64)> = (1..=M)
        .step_by(2)
        .map(|k| {
            let win = 0.5 * (1.0 + (std::f64::consts::PI * k as f64 / (M + 1) as f64).cos());
            (k, 2.0 / (std::f64::consts::PI * k as f64) * win)
        })
        .collect();
    (0..x.len())
        .map(|i| {
            let mut h = 0.0;
            for &(k, c) in &taps {
                let a = if i >= k { x[i - k] } else { 0.0 };
                let b = x.get(i + k).copied().unwrap_or(0.0);
                h += c * (a - b);
            }
            let ph = w * i as f64;
            x[i] * ph.cos() - h * ph.sin()
        })
        .collect()
}

/// `y[n] = x(n · ratio)`: what a receiver sampling at 8000 Hz gets from a sender whose clock
/// runs `ratio` times too fast. Windowed sinc, 16 taps.
fn resample(x: &[f64], ratio: f64) -> Vec<f64> {
    let n_out = (x.len() as f64 / ratio) as usize;
    (0..n_out)
        .map(|n| {
            let t = n as f64 * ratio;
            let i0 = t.floor() as i64;
            let mut acc = 0.0;
            for k in i0 - 7..=i0 + 8 {
                if k < 0 || k as usize >= x.len() {
                    continue;
                }
                let d = t - k as f64;
                let sinc = if d.abs() < 1e-12 {
                    1.0
                } else {
                    (std::f64::consts::PI * d).sin() / (std::f64::consts::PI * d)
                };
                let win = 0.5 * (1.0 + (std::f64::consts::PI * d / 8.5).cos());
                acc += x[k as usize] * sinc * win;
            }
            acc
        })
        .collect()
}

fn law_round_trip(x: &[i16], law: Law) -> Vec<i16> {
    let mode = match law {
        Law::Mu => sys::G711_ULAW,
        Law::A => sys::G711_ALAW,
    };
    // SAFETY: NULL makes SpanDSP allocate; buffers are valid for `x.len()` entries.
    unsafe {
        let s = sys::g711_init(std::ptr::null_mut(), mode);
        assert!(!s.is_null());
        let mut enc = vec![0u8; x.len()];
        sys::g711_encode(s, enc.as_mut_ptr(), x.as_ptr(), x.len() as c_int);
        let mut out = vec![0i16; x.len()];
        sys::g711_decode(s, out.as_mut_ptr(), enc.as_ptr(), x.len() as c_int);
        sys::g711_free(s);
        out
    }
}

/// Passes `tx` (a transmitter's samples) through the channel.
fn through(tx: &[i16], ch: &Chan, rng: &mut Rng) -> Heard {
    let mut x: Vec<f64> = tx.iter().map(|&s| f64::from(s)).collect();
    if ch.offset_hz != 0.0 {
        x = freq_shift(&x, ch.offset_hz);
    }
    if ch.ppm != 0.0 {
        x = resample(&x, 1.0 + ch.ppm * 1e-6);
    }
    // the active part: from the first to the last sample that is not silence
    let first = x.iter().position(|v| v.abs() > 0.5).expect("signal");
    let last = x.iter().rposition(|v| v.abs() > 0.5).expect("signal") + 1;
    let ms: f64 = x[first..last].iter().map(|v| v * v).sum::<f64>() / (last - first) as f64;
    let target = f64::from(mean_square(ch.level as f32));
    let k = (target / ms).sqrt();
    let mut y = vec![0.0f64; PAD + x.len() + PAD];
    for (i, v) in x.iter().enumerate() {
        y[PAD + i] = k * v;
    }
    if let Some(snr) = ch.snr_db {
        let sd = (target / 10f64.powf(snr / 10.0)).sqrt();
        for v in y.iter_mut() {
            *v += sd * rng.gauss();
        }
    }
    let mut audio: Vec<i16> = y
        .iter()
        .map(|v| v.round().clamp(-32768.0, 32767.0) as i16)
        .collect();
    if let Some(law) = ch.law {
        audio = law_round_trip(&audio, law);
    }
    let onset = PAD + first;
    let end = PAD + last;
    if let Some(at) = ch.gap_at {
        let g = onset + ((end - onset) as f64 * at) as usize;
        audio[g..g + 160].fill(0);
    }
    Heard { audio, onset, end }
}

// ----------------------------------------------------------------------------------- receiver

/// What a [`V29Rx`] reported over some audio.
#[derive(Default, Debug)]
struct Got {
    /// Sample count at the end of the chunk in which each event arrived.
    training: Vec<usize>,
    trained: Vec<usize>,
    lost: Vec<usize>,
    bits: Vec<bool>,
    /// Number of bits received when each `lost` event arrived.
    bits_at_lost: Vec<usize>,
}

fn receive_with(rx: &mut V29Rx, audio: &[i16], base: usize, got: &mut Got) {
    let mut fed = 0;
    for chunk in audio.chunks(160) {
        fed += chunk.len();
        for ev in rx.feed(chunk) {
            match ev {
                RxEvent::Training => got.training.push(base + fed),
                RxEvent::Trained => got.trained.push(base + fed),
                RxEvent::Bits(b) => got.bits.extend(b),
                RxEvent::CarrierLost => {
                    got.lost.push(base + fed);
                    got.bits_at_lost.push(got.bits.len());
                }
            }
        }
    }
}

fn receive(rate: Rate, audio: &[i16]) -> Got {
    let mut rx = V29Rx::new(rate);
    let mut got = Got::default();
    receive_with(&mut rx, audio, 0, &mut got);
    got
}

/// Where `data` starts in `rx` (the receiver's output begins with the ones of Segment 4), and the
/// number of differing bits over `data.len()` (bits the receiver did not deliver count as errors).
fn compare(data: &[bool], rx: &[bool]) -> (usize, usize) {
    let probe = data.len().min(400);
    let off = (0..=320usize)
        .min_by_key(|&o| {
            (0..probe)
                .filter(|&i| rx.get(o + i).is_none_or(|&b| b != data[i]))
                .count()
        })
        .unwrap();
    let errors = (0..data.len())
        .filter(|&i| rx.get(off + i).is_none_or(|&b| b != data[i]))
        .count();
    (off, errors)
}

/// One transmission through a channel into a fresh receiver.
#[derive(Debug)]
struct Run {
    errors: usize,
    nbits: usize,
    offset: usize,
    /// Milliseconds from the first sample of the signal to `Training`.
    training_ms: f64,
    got: Got,
    heard_end: usize,
}

fn run(src: Src, rate: Rate, nbits: usize, seed: u64, ch: &Chan) -> Run {
    let mut rng = Rng::new(seed ^ (bps(rate) as u64) << 32);
    // the data, then 128 more bits so that the receiver's pipeline delay is not measured
    let bits = rng.bits(nbits + 128);
    let tx = source(src, rate, &bits);
    let heard = through(&tx, ch, &mut rng);
    let got = receive(rate, &heard.audio);
    let (offset, errors) = compare(&bits[..nbits], &got.bits);
    let training_ms = got
        .training
        .first()
        .map_or(f64::INFINITY, |&t| (t as f64 - heard.onset as f64) / 8.0);
    Run {
        errors,
        nbits,
        offset,
        training_ms,
        got,
        heard_end: heard.end,
    }
}

/// The events of a run that went well: one `Training`, one `Trained` after it, the data, and one
/// `CarrierLost` after the end of the signal.
fn assert_clean_events(r: &Run, what: &str) {
    let g = &r.got;
    assert_eq!(
        g.training.len(),
        1,
        "{what}: Training events {:?}",
        g.training
    );
    assert_eq!(g.trained.len(), 1, "{what}: Trained events {:?}", g.trained);
    assert!(
        g.training[0] <= g.trained[0],
        "{what}: Trained before Training"
    );
    assert_eq!(g.lost.len(), 1, "{what}: CarrierLost events {:?}", g.lost);
    assert!(
        g.lost[0] >= r.heard_end && g.lost[0] <= r.heard_end + 8 * 40,
        "{what}: carrier lost at {} but the signal ended at {}",
        g.lost[0],
        r.heard_end
    );
}

fn describe(r: &Run) -> String {
    format!(
        "errors {}/{} offset {} training {:.1} ms",
        r.errors, r.nbits, r.offset, r.training_ms
    )
}

// ============================================================================================
// B01: clean signal, -10 dBm0: BER 0 over 10^6 bits, Training, Trained, and the end of the carrier

#[test]
fn b01_spandsp_clean_signal_has_no_bit_errors_over_a_million_bits() {
    for rate in RATES {
        let mut total = 0;
        for seed in SEEDS {
            let r = run(Src::SpanDsp, rate, 340_000, seed, &Chan::clean(-10.0));
            eprintln!("B01 SpanDSP {rate:?} seed {seed:#x}: {}", describe(&r));
            assert_eq!(r.errors, 0, "{rate:?} seed {seed:#x}: {}", describe(&r));
            assert_clean_events(&r, &format!("{rate:?} seed {seed:#x}"));
            // Segment 4 (48 symbols of scrambled ones) comes first in the receiver's bit stream
            assert_eq!(
                r.offset,
                48 * rate.bits_per_symbol(),
                "{rate:?}: data start"
            );
            // (reported with `Trained`, at the end of Segment 3: 60 ms of Segments 1 and 2 plus
            // 160 ms)
            assert!(
                r.training_ms < 300.0,
                "{rate:?}: Training after {} ms",
                r.training_ms
            );
            total += r.nbits;
        }
        assert!(total >= 1_000_000);
    }
}

#[test]
fn b01_our_own_transmitter_is_a_second_source() {
    for rate in RATES {
        for seed in SEEDS {
            let r = run(Src::Ours, rate, 100_000, seed, &Chan::clean(-10.0));
            assert_eq!(r.errors, 0, "{rate:?} seed {seed:#x}: {}", describe(&r));
            assert_clean_events(&r, &format!("ours {rate:?} seed {seed:#x}"));
        }
    }
}

#[test]
fn b01_the_channel_helpers_do_what_they_say() {
    // the level is the level, the noise is the SNR, the gap is 160 zeros, and a clock deviation
    // changes the length by that many ppm
    let mut rng = Rng::new(7);
    let bits = rng.bits(200_000);
    let tx = source(Src::SpanDsp, Rate::B9600, &bits);
    let h = through(&tx, &Chan::clean(-23.0), &mut rng);
    let lvl = fax_modem::level::dbm0(fax_modem::level::block_mean_square(
        &h.audio[h.onset..h.end],
    ));
    assert!((lvl + 23.0).abs() < 0.05, "level {lvl}");
    let h = through(&tx, &Chan::clean(-20.0).snr(15.0), &mut rng);
    let noise = fax_modem::level::dbm0(fax_modem::level::block_mean_square(&h.audio[..PAD - 100]));
    assert!((noise + 35.0).abs() < 0.3, "noise {noise}");
    let h = through(&tx, &Chan::clean(-20.0).gap(0.5), &mut rng);
    let mid = h.onset + (h.end - h.onset) / 2;
    assert!(h.audio[mid..mid + 160].iter().all(|&s| s == 0));
    assert!(h.audio[mid - 160..mid].iter().any(|&s| s != 0));
    let h0 = through(&tx, &Chan::clean(-20.0), &mut rng);
    let h1 = through(&tx, &Chan::clean(-20.0).ppm(1000.0), &mut rng);
    let ratio = (h1.end - h1.onset) as f64 / (h0.end - h0.onset) as f64;
    assert!((ratio - 1.0 / 1.001).abs() < 1e-4, "ratio {ratio}");
}

// ============================================================================================
// B02: level -10 to -43 dBm0

#[test]
fn b02_levels_from_minus_10_to_minus_43_dbm0() {
    for rate in RATES {
        for level in [-10.0, -20.0, -30.0, -40.0, -43.0] {
            for seed in SEEDS {
                let r = run(Src::SpanDsp, rate, 50_000, seed, &Chan::clean(level));
                let what = format!("{rate:?} {level} dBm0 seed {seed:#x}");
                assert_eq!(r.errors, 0, "{what}: {}", describe(&r));
                assert_clean_events(&r, &what);
                assert!(
                    r.training_ms < 300.0,
                    "{what}: Training after {} ms",
                    r.training_ms
                );
            }
        }
    }
}

#[test]
fn b02_our_own_transmitter_at_the_same_levels() {
    for rate in RATES {
        for level in [-10.0, -30.0, -43.0] {
            let r = run(Src::Ours, rate, 30_000, SEEDS[0], &Chan::clean(level));
            let what = format!("ours {rate:?} {level} dBm0");
            assert_eq!(r.errors, 0, "{what}: {}", describe(&r));
            assert_clean_events(&r, &what);
        }
    }
}

#[test]
fn b02_below_43_dbm0_is_recorded_not_required() {
    // -45 and -47 dBm0 lie between the turn-on and the turn-off threshold: no requirement, but
    // the receiver should not misbehave (one Training at most, no panic); the result is printed
    for rate in RATES {
        for level in [-45.0, -47.0] {
            let r = run(Src::SpanDsp, rate, 20_000, SEEDS[0], &Chan::clean(level));
            eprintln!(
                "B02 (recorded) SpanDSP {rate:?} {level} dBm0: {} events T{} t{} L{}",
                describe(&r),
                r.got.training.len(),
                r.got.trained.len(),
                r.got.lost.len()
            );
            assert!(r.got.training.len() <= 1 && r.got.lost.len() <= 1);
        }
    }
}

// ============================================================================================
// B03: white noise

/// The SNR (dB, signal power over white noise power in 0..4 kHz) of a rate's BER case: the plan
/// gives 20 dB for V.29 9600; 7200 needs less, 17 dB is used (as in the unit tests).
fn plan_snr(rate: Rate) -> f64 {
    match rate {
        Rate::B9600 => 20.0,
        Rate::B7200 => 17.0,
    }
}

/// Total bit errors and bits over the three seeds, each seed one transmission; Training and
/// Trained must come exactly once (the noise keeps the signal "present" for ever: no
/// `CarrierLost` to expect).
fn ber(rate: Rate, ch: &Chan, bits_per_seed: usize, what: &str) -> (usize, usize) {
    let mut errors = 0;
    let mut total = 0;
    for seed in SEEDS {
        let r = run(Src::SpanDsp, rate, bits_per_seed, seed, ch);
        eprintln!("{what} {rate:?} seed {seed:#x}: {}", describe(&r));
        assert_eq!(
            r.got.training.len(),
            1,
            "{what} {rate:?} seed {seed:#x}: Training"
        );
        assert_eq!(
            r.got.trained.len(),
            1,
            "{what} {rate:?} seed {seed:#x}: Trained"
        );
        errors += r.errors;
        total += r.nbits;
    }
    (errors, total)
}

/// The BER case of the plan at the plan's SNR (at most `plan_limit`, 1e-5 in the plan) and 2 dB
/// better (at most 1e-5 always).
fn assert_ber(
    rate: Rate,
    make: impl Fn(f64) -> Chan,
    bits_per_seed: usize,
    plan_limit: f64,
    what: &str,
) {
    let plan = plan_snr(rate);
    let (e, n) = ber(
        rate,
        &make(plan),
        bits_per_seed,
        &format!("{what} SNR {plan}"),
    );
    eprintln!(
        "{what} {rate:?} at the plan SNR {plan} dB: {e} errors in {n} bits, BER {:.2e}",
        e as f64 / n as f64
    );
    assert!(
        (e as f64 / n as f64) <= plan_limit,
        "{what} {rate:?} SNR {plan}: {e} errors in {n} bits (limit {plan_limit})"
    );
    let (e, n) = ber(
        rate,
        &make(plan + 2.0),
        bits_per_seed,
        &format!("{what} SNR {}", plan + 2.0),
    );
    assert!(
        e as f64 / n as f64 <= 1e-5,
        "{what} {rate:?} SNR {}: {e} errors in {n} bits",
        plan + 2.0
    );
}

#[test]
fn b03_awgn_ber_at_the_plan_snr_and_two_db_above() {
    // 9600: 1 020 000 bits at 20 dB and at 22 dB. 7200 (17 dB and 19 dB): 300 000 bits, a page of
    // 14 s per seed; a transmission of 45 s loses lock at that SNR, see the tests below.
    for (rate, bits) in [(Rate::B9600, 340_000), (Rate::B7200, 100_000)] {
        assert_ber(rate, |snr| Chan::clean(-20.0).snr(snr), bits, 1e-5, "B03");
    }
}

/// 340 000 bits of 7200 bit/s (47 s) with noise: errors and where the first one is.
fn long_7200(snr: f64, seed: u64) -> (usize, Option<usize>, Run) {
    let rate = Rate::B7200;
    let r = run(
        Src::SpanDsp,
        rate,
        340_000,
        seed,
        &Chan::clean(-20.0).snr(snr),
    );
    let mut rng = Rng::new(seed ^ (bps(rate) as u64) << 32);
    let bits = rng.bits(340_000 + 128);
    let tx = source(Src::SpanDsp, rate, &bits);
    let h = through(&tx, &Chan::clean(-20.0).snr(snr), &mut rng);
    let got = receive(rate, &h.audio);
    let bad: Vec<usize> = (0..340_000)
        .filter(|&i| got.bits.get(r.offset + i).is_none_or(|&b| b != bits[i]))
        .collect();
    (bad.len(), bad.first().copied(), r)
}

#[test]
fn b03_a_long_7200_page_at_22_db_is_error_free() {
    for seed in SEEDS {
        let (e, first, r) = long_7200(22.0, seed);
        assert_eq!((e, first), (0, None), "seed {seed:#x}: {}", describe(&r));
        assert_eq!(r.got.training.len(), 1, "seed {seed:#x}: Training");
        assert_eq!(r.got.trained.len(), 1, "seed {seed:#x}: Trained");
    }
}

/// The plan's requirement at the 7200 SNR of the other cases, over a whole 47 s page, and 2 dB
/// above it. Until the timing moved from the equalizer's tap centroid to a Gardner loop with an
/// integral (ADR-065), the receiver lost the data for good after 30 to 37 s at 17 dB in every
/// seed (docs/test-reports/M5d.md section 4).
#[test]
fn b03_a_long_7200_page_at_the_plan_snr() {
    for snr in [17.0, 19.0] {
        for seed in SEEDS {
            let (e, first, r) = long_7200(snr, seed);
            assert_eq!(
                (e, first),
                (0, None),
                "SNR {snr} seed {seed:#x}: {}",
                describe(&r)
            );
            assert_eq!(
                r.got.training.len(),
                1,
                "SNR {snr} seed {seed:#x}: Training"
            );
        }
    }
}

#[test]
fn b03_ber_curve_is_recorded() {
    for (rate, snrs) in [
        (Rate::B9600, [16.0, 17.0, 18.0, 19.0, 20.0, 22.0]),
        (Rate::B7200, [13.0, 14.0, 15.0, 16.0, 17.0, 19.0]),
    ] {
        for snr in snrs {
            let mut e = 0;
            let mut n = 0;
            for seed in SEEDS {
                let r = run(
                    Src::SpanDsp,
                    rate,
                    100_000,
                    seed,
                    &Chan::clean(-20.0).snr(snr),
                );
                e += r.errors;
                n += r.nbits;
            }
            eprintln!(
                "B03 curve {rate:?} SNR {snr:>4} dB: {e} errors in {n} bits, BER {:.2e}",
                e as f64 / n as f64
            );
        }
    }
}

// ============================================================================================
// B04: clock deviation and frequency offset

#[test]
fn b04_clock_deviation_and_frequency_offset() {
    for rate in RATES {
        for (name, ppm, hz) in [
            ("+100 ppm", 100.0, 0.0),
            ("-100 ppm", -100.0, 0.0),
            ("+7 Hz", 0.0, 7.0),
            ("-7 Hz", 0.0, -7.0),
            ("+100 ppm +7 Hz", 100.0, 7.0),
            ("-100 ppm -7 Hz", -100.0, -7.0),
        ] {
            assert_ber(
                rate,
                |snr| Chan::clean(-20.0).snr(snr).ppm(ppm).offset(hz),
                100_000,
                1e-5,
                &format!("B04 {name}"),
            );
        }
    }
}

#[test]
fn b04_without_noise_every_deviation_is_error_free() {
    for rate in RATES {
        for (ppm, hz) in [
            (100.0, 0.0),
            (-100.0, 0.0),
            (0.0, 7.0),
            (0.0, -7.0),
            (100.0, 7.0),
            (-100.0, -7.0),
        ] {
            for seed in SEEDS {
                let ch = Chan::clean(-20.0).ppm(ppm).offset(hz);
                let r = run(Src::SpanDsp, rate, 50_000, seed, &ch);
                let what = format!("{rate:?} {ppm} ppm {hz} Hz seed {seed:#x}");
                assert_eq!(r.errors, 0, "{what}: {}", describe(&r));
                assert_clean_events(&r, &what);
            }
        }
    }
}

#[test]
fn b04_our_own_transmitter_with_its_own_deviation_inputs() {
    // the second source: the deviation applied by the channel on `testtx::Tx` output
    for rate in RATES {
        for (ppm, hz) in [(100.0, 7.0), (-100.0, -7.0)] {
            let ch = Chan::clean(-20.0).ppm(ppm).offset(hz);
            let r = run(Src::Ours, rate, 50_000, SEEDS[1], &ch);
            assert_eq!(
                r.errors,
                0,
                "ours {rate:?} {ppm} ppm {hz} Hz: {}",
                describe(&r)
            );
        }
    }
}

// ============================================================================================
// B05: G.711

#[test]
fn b05_g711_round_trip_of_both_laws() {
    for law in [Law::Mu, Law::A] {
        for rate in RATES {
            assert_ber(
                rate,
                |snr| Chan::clean(-20.0).snr(snr).law(law),
                100_000,
                1e-5,
                &format!("B05 {law:?}"),
            );
        }
    }
}

#[test]
fn b05_g711_alone_at_low_and_normal_levels_is_error_free() {
    for law in [Law::Mu, Law::A] {
        for rate in RATES {
            for level in [-10.0, -30.0, -40.0] {
                let r = run(
                    Src::SpanDsp,
                    rate,
                    50_000,
                    SEEDS[2],
                    &Chan::clean(level).law(law),
                );
                let what = format!("{law:?} {rate:?} {level} dBm0");
                assert_eq!(r.errors, 0, "{what}: {}", describe(&r));
                assert_clean_events(&r, &what);
            }
        }
    }
}

// ============================================================================================
// B06: one lost RTP packet (20 ms of silence) in the middle of the data

#[test]
fn b06_a_gap_in_the_data_does_not_hang_the_receiver() {
    for rate in RATES {
        for (seed, at) in [(SEEDS[0], 0.3), (SEEDS[1], 0.5), (SEEDS[2], 0.7)] {
            let mut rng = Rng::new(seed ^ 0xB06);
            let bits = rng.bits(30_000);
            let tx = source(Src::SpanDsp, rate, &bits);
            let h1 = through(&tx, &Chan::clean(-15.0).gap(at), &mut rng);
            let gap_start = h1.onset + ((h1.end - h1.onset) as f64 * at) as usize;
            // after it: half a second of silence, then a complete clean transmission, to the same receiver
            let bits2 = rng.bits(10_000);
            let h2 = through(
                &source(Src::SpanDsp, rate, &bits2),
                &Chan::clean(-15.0),
                &mut rng,
            );
            let mut rx = V29Rx::new(rate);
            let mut first = Got::default();
            receive_with(&mut rx, &h1.audio, 0, &mut first);
            let mut second = Got::default();
            receive_with(&mut rx, &vec![0i16; 4000], h1.audio.len(), &mut second);
            let base = h1.audio.len() + 4000;
            receive_with(&mut rx, &h2.audio, base, &mut second);
            let what = format!("{rate:?} gap at {at} seed {seed:#x}");
            eprintln!(
                "B06 {what}: first {{T{:?} t{:?} L{:?} bits {} at lost {:?}}} gap_start {gap_start} end {}",
                first.training,
                first.trained,
                first.lost,
                first.bits.len(),
                first.bits_at_lost,
                h1.end
            );
            // what the receiver does with the gap: reports the end of the carrier soon after it
            // (then the rest of the transmission is not data to it), or keeps going; never hangs
            assert!(!first.training.is_empty(), "{what}");
            match first.lost.first() {
                Some(&l) => {
                    assert!(
                        l >= gap_start && l <= gap_start + 8 * 100,
                        "{what}: carrier lost at {l}, gap at {gap_start}"
                    );
                    // the bits delivered before are right
                    let (_, e) = compare(
                        &bits[..first.bits_at_lost[0].saturating_sub(40).min(2000)],
                        &first.bits,
                    );
                    assert_eq!(e, 0, "{what}: bits before the gap");
                }
                None => {
                    // re-converged: the bits after the gap are the data again
                    let tail = &bits[bits.len() - 2000..];
                    let (_, e) = compare(tail, &first.bits[first.bits.len() - 2000 - 128..]);
                    assert_eq!(e, 0, "{what}: no CarrierLost and the end is wrong");
                }
            }
            // and the next transmission is received completely
            assert_eq!(
                second.training.len(),
                1,
                "{what}: second Training {:?}",
                second.training
            );
            assert_eq!(second.trained.len(), 1, "{what}: second Trained");
            let (_, e2) = compare(&bits2[..10_000], &second.bits);
            assert_eq!(e2, 0, "{what}: second transmission has {e2} errors");
        }
    }
}

// ============================================================================================
// the audio engine: T.30 on top of the receiver (B06, B07)

fn reverse_octets(frame: &[u8]) -> Vec<u8> {
    frame.iter().map(|b| b.reverse_bits()).collect()
}

/// Feeds `input` (then silence) to the engine for `ms`; returns what it sent.
fn run_engine(e: &mut AudioEngine, input: &[i16], ms: u64) -> Vec<i16> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    for _ in 0..ms / 20 {
        let mut rx = [0i16; 160];
        let n = input.len().saturating_sub(pos).min(160);
        rx[..n].copy_from_slice(&input[pos..pos + n]);
        pos += n;
        let mut tx = [0i16; 160];
        e.process(&rx, &mut tx);
        out.extend_from_slice(&tx);
    }
    out
}

/// The frames (T.30 notation) in V.21 audio.
fn frames_in(audio: &[i16]) -> Vec<Vec<u8>> {
    let mut rx = V21Rx::new();
    let mut h = HdlcRx::new();
    rx.feed(audio)
        .into_iter()
        .filter_map(|b| match h.bit(b) {
            Some(HdlcEvent::Frame { data, fcs_ok: true }) => Some(reverse_octets(&data)),
            _ => None,
        })
        .collect()
}

fn fcfs(frames: &[Vec<u8>]) -> Vec<u8> {
    frames.iter().map(|f| f[2]).collect()
}

/// The DCS information field of V.29 at `rate` (T.30 Table 2: bit 11 is 9600, bits 11 and 12
/// are 7200).
fn dcs_fif(rate: Rate) -> Vec<u8> {
    let fif = match rate {
        Rate::B9600 => vec![0x00, 0x20, 0x00],
        Rate::B7200 => vec![0x00, 0x30, 0x00],
    };
    assert_eq!(
        t30::caps::parse_rate(&fif).unwrap(),
        (t30::caps::Modem::V29, bps(rate) as u32)
    );
    fif
}

/// An engine that has answered (CED, CSI, DIS).
fn answered() -> AudioEngine {
    let mut e = AudioEngine::new(AudioConfig::default());
    e.start();
    let _ = run_engine(&mut e, &[], 6_000);
    e
}

/// The peer's TSI and DCS on V.21; the engine listens for 3 s.
fn send_dcs(e: &mut AudioEngine, rate: Rate) {
    let tsi = vec![0xFF, 0xC0, t30::fcf::TSI, b' ', b' ', b'1'];
    let mut dcs = vec![0xFF, 0xC8, t30::fcf::DCS];
    dcs.extend(dcs_fif(rate));
    let wire: Vec<Vec<u8>> = [tsi, dcs].iter().map(|f| reverse_octets(f)).collect();
    let mut v21 = V21Tx::new(-15.0);
    let mut peer = vec![0i16; 800];
    peer.extend(v21.modulate(&hdlc::encode_burst(&wire, 38, 2)));
    let _ = run_engine(e, &peer, 3_000);
}

/// The TCF audio (a V.29 transmission, 200 ms of silence around it) and 4 s more; the frames
/// the engine sent in that time.
fn send_tcf(e: &mut AudioEngine, audio: &[i16]) -> Vec<Vec<u8>> {
    send_tcf_then(e, audio, 4_000)
}

fn send_tcf_then(e: &mut AudioEngine, audio: &[i16], tail_ms: u64) -> Vec<Vec<u8>> {
    let ms = (audio.len() / 160 + 1) as u64 * 20 + tail_ms;
    frames_in(&run_engine(e, audio, ms))
}

fn tcf_audio(src: Src, rate: Rate, bits: &[bool], ch: &Chan, seed: u64) -> Vec<i16> {
    let mut rng = Rng::new(seed ^ 0x7CF);
    through(&source(src, rate, bits), ch, &mut rng).audio
}

/// A TCF of `seconds` of zeros in which each bit is a one with probability `p`.
fn tcf_bits(rate: Rate, p: f64, seed: u64) -> Vec<bool> {
    let mut rng = Rng::new(seed ^ 0x51F);
    (0..(bps(rate) as f64 * 1.5) as usize)
        .map(|_| rng.uniform() < p)
        .collect()
}

#[test]
fn b07_all_zero_tcf_is_confirmed_with_cfr() {
    for src in [Src::SpanDsp, Src::Ours] {
        for rate in RATES {
            for (seed, ch) in [
                (SEEDS[0], Chan::clean(-15.0)),
                (SEEDS[1], Chan::clean(-30.0).snr(plan_snr(rate) + 5.0)),
                (SEEDS[2], Chan::clean(-15.0).law(Law::Mu)),
            ] {
                let mut e = answered();
                send_dcs(&mut e, rate);
                let audio = tcf_audio(src, rate, &tcf_bits(rate, 0.0, seed), &ch, seed);
                let frames = send_tcf(&mut e, &audio);
                let log = e.drain_log();
                let tcf = log.iter().find(|l| l.starts_with("TCF:")).cloned();
                let f = fcfs(&frames);
                assert!(
                    f.contains(&t30::fcf::CFR) && !f.contains(&t30::fcf::FTT),
                    "{src:?} {rate:?} {ch:?}: frames {f:02x?}, {tcf:?}"
                );
            }
        }
    }
}

#[test]
fn b07_a_tcf_with_errors_is_refused_with_ftt() {
    for src in [Src::SpanDsp, Src::Ours] {
        for rate in RATES {
            for (seed, p) in [(SEEDS[0], 0.08), (SEEDS[1], 0.20), (SEEDS[2], 0.5)] {
                let mut e = answered();
                send_dcs(&mut e, rate);
                let audio = tcf_audio(
                    src,
                    rate,
                    &tcf_bits(rate, p, seed),
                    &Chan::clean(-15.0),
                    seed,
                );
                let frames = send_tcf(&mut e, &audio);
                let log = e.drain_log();
                let tcf = log.iter().find(|l| l.starts_with("TCF:")).cloned();
                let f = fcfs(&frames);
                assert!(
                    f.contains(&t30::fcf::FTT) && !f.contains(&t30::fcf::CFR),
                    "{src:?} {rate:?} p {p}: frames {f:02x?}, {tcf:?}"
                );
            }
        }
    }
}

#[test]
fn b07_the_threshold_is_five_percent_ones() {
    // 1 % ones (on top of the 48 symbols of ones of Segment 4 and the ones SpanDSP sends after the
    // data, 3.2 % of a 9600 TCF): still CFR; 8 %: FTT. Same rule as on the T.38 path.
    for rate in RATES {
        for (p, want) in [(0.01, t30::fcf::CFR), (0.08, t30::fcf::FTT)] {
            let mut e = answered();
            send_dcs(&mut e, rate);
            let audio = tcf_audio(
                Src::SpanDsp,
                rate,
                &tcf_bits(rate, p, 3),
                &Chan::clean(-15.0),
                3,
            );
            let f = fcfs(&send_tcf(&mut e, &audio));
            let log = e.drain_log();
            let tcf = log.iter().find(|l| l.starts_with("TCF:")).cloned();
            assert!(f.contains(&want), "{rate:?} p {p}: {f:02x?} {tcf:?}");
        }
    }
}

#[test]
fn b06_after_a_gap_in_the_tcf_t30_answers_and_the_next_dcs_is_heard() {
    for rate in RATES {
        // the TCF is 1.5 s after the training (608 symbols); the gap is in the middle of it
        let train_s = 608.0 / f64::from(rate.baud());
        let total = train_s + 1.5 + 0.1;
        for (seed, extra) in [(SEEDS[0], 0.2), (SEEDS[1], 0.6), (SEEDS[2], 1.0)] {
            let at = (train_s + extra) / total;
            let mut e = answered();
            send_dcs(&mut e, rate);
            let audio = tcf_audio(
                Src::SpanDsp,
                rate,
                &tcf_bits(rate, 0.0, seed),
                &Chan::clean(-15.0).gap(at),
                seed,
            );
            let f = fcfs(&send_tcf_then(&mut e, &audio, 500));
            let what = format!("{rate:?} gap at {at}");
            // the carrier went at the gap: the TCF is too short, so FTT (not a hang, not CFR)
            assert!(
                f.contains(&t30::fcf::FTT) && !f.contains(&t30::fcf::CFR),
                "{what}: {f:02x?}"
            );
            // the sender tries again with the same DCS and now a clean TCF: V.21 and V.29 are heard
            send_dcs(&mut e, rate);
            let audio = tcf_audio(
                Src::SpanDsp,
                rate,
                &tcf_bits(rate, 0.0, seed),
                &Chan::clean(-15.0),
                seed,
            );
            let f = fcfs(&send_tcf(&mut e, &audio));
            let log = e.drain_log();
            assert!(
                f.contains(&t30::fcf::CFR),
                "{what}: second try {f:02x?} done {} {log:#?}",
                e.is_done()
            );
        }
    }
}

#[test]
fn b06_a_gap_after_a_long_enough_tcf_still_gives_cfr() {
    // 1.4 s of the 1.5 s TCF arrive: more than the 1.2 s T.30 asks for, and all zeros
    for rate in RATES {
        let train_s = 608.0 / f64::from(rate.baud());
        let at = (train_s + 1.4) / (train_s + 1.6);
        let mut e = answered();
        send_dcs(&mut e, rate);
        let audio = tcf_audio(
            Src::SpanDsp,
            rate,
            &tcf_bits(rate, 0.0, 9),
            &Chan::clean(-15.0).gap(at),
            9,
        );
        let f = fcfs(&send_tcf(&mut e, &audio));
        assert!(f.contains(&t30::fcf::CFR), "{rate:?}: {f:02x?}");
    }
}

// ============================================================================================
// B10: the peer tries V.17, which our DIS does not offer. SpanDSP's `v17_tx` is the source.

/// SpanDSP's V.17 transmission of `bits` at `bit_rate`, at -10 dBm0 (the training is the long or
/// the short one).
fn spandsp_v17(bit_rate: i32, short_train: bool, bits: &[bool]) -> Vec<i16> {
    let mut feed = Box::new(BitFeed {
        bits: bits.to_vec(),
        pos: 0,
    });
    // SAFETY: NULL makes SpanDSP allocate the state; `feed` outlives it (freed below).
    let tx = unsafe {
        sys::v17_tx_init(
            std::ptr::null_mut(),
            bit_rate,
            false,
            Some(get_bit),
            &mut *feed as *mut BitFeed as *mut c_void,
        )
    };
    assert!(!tx.is_null(), "v17_tx_init failed");
    // SAFETY: valid state.
    unsafe {
        sys::v17_tx_restart(tx, bit_rate, false, short_train);
        sys::v17_tx_power(tx, -10.0);
    }
    let mut out = Vec::new();
    let mut buf = [0i16; 160];
    loop {
        // SAFETY: `buf` is valid for 160 samples.
        let n = unsafe { sys::v17_tx(tx, buf.as_mut_ptr(), 160) } as usize;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
        assert!(out.len() < 8_000 * 3_000, "v17_tx does not stop");
    }
    // SAFETY: allocated by the init call, freed once.
    unsafe { sys::v17_tx_free(tx) };
    assert_eq!(feed.pos, bits.len(), "SpanDSP did not take all the bits");
    out
}

#[test]
fn b10_a_v17_signal_is_not_taken_for_a_v29_training() {
    let mut cases = 0;
    for bit_rate in [7200, 9600, 12000, 14400] {
        for short in [false, true] {
            for (seed, level) in [(SEEDS[0], -10.0), (SEEDS[1], -25.0), (SEEDS[2], -40.0)] {
                let mut rng = Rng::new(seed ^ 0xB10);
                // 1.5 s of TCF zeros, then random data
                let mut bits = vec![false; (bit_rate as f64 * 1.5) as usize];
                bits.extend(rng.bits(20_000));
                let tx = spandsp_v17(bit_rate, short, &bits);
                let h = through(&tx, &Chan::clean(level), &mut rng);
                for rate in RATES {
                    let got = receive(rate, &h.audio);
                    let what = format!("V.17 {bit_rate} short {short} {level} dBm0 at {rate:?}");
                    assert!(
                        got.training.is_empty() && got.trained.is_empty() && got.bits.is_empty(),
                        "{what}: Training {:?} Trained {:?} bits {}",
                        got.training,
                        got.trained,
                        got.bits.len()
                    );
                    cases += 1;
                }
            }
        }
    }
    assert_eq!(cases, 4 * 2 * 3 * 2);
}

#[test]
fn b10_a_v29_receiver_after_a_v17_signal_still_receives_v29() {
    // the same receiver: V.17 first, then half a second of silence, then a V.29 transmission
    for rate in RATES {
        let mut rng = Rng::new(0xB102);
        let v17 = through(
            &spandsp_v17(9600, false, &rng.bits(30_000)),
            &Chan::clean(-15.0),
            &mut rng,
        );
        let bits = rng.bits(20_000);
        let v29 = through(
            &source(Src::SpanDsp, rate, &bits),
            &Chan::clean(-15.0),
            &mut rng,
        );
        let mut rx = V29Rx::new(rate);
        let mut first = Got::default();
        receive_with(&mut rx, &v17.audio, 0, &mut first);
        let mut second = Got::default();
        receive_with(&mut rx, &v29.audio, v17.audio.len(), &mut second);
        assert!(
            first.training.is_empty() && first.bits.is_empty(),
            "{rate:?}: V.17 heard as V.29"
        );
        assert_eq!(second.training.len(), 1, "{rate:?}");
        let (_, e) = compare(&bits, &second.bits);
        assert_eq!(e, 0, "{rate:?}: V.29 after V.17 has {e} errors");
    }
}

/// The DCS information field that selects V.17 at `bit_rate` (T.30 Table 2: bits 11 to 14 are
/// 1001 for 9600, 1101 for 7200, 0101 for 12000 and 0001 for 14400).
fn dcs_fif_v17(bit_rate: u32) -> Vec<u8> {
    let sel: u8 = match bit_rate {
        9600 => 0b1001,
        7200 => 0b1101,
        12000 => 0b0101,
        _ => 0b0001,
    };
    let fif = vec![0x00, sel << 2, 0x00];
    assert_eq!(
        t30::caps::parse_rate(&fif).unwrap(),
        (t30::caps::Modem::V17, bit_rate)
    );
    fif
}

/// What our answering side does when the peer sends a DCS for V.17 and then a V.17 TCF: the
/// engine has no receiver for it. Returns the FCFs it sent in the 12 s after the DCS and its log.
fn v17_dcs_then_tcf(bit_rate: u32) -> (Vec<u8>, Vec<String>, bool) {
    let mut e = answered();
    let tsi = vec![0xFF, 0xC0, t30::fcf::TSI, b' ', b' ', b'1'];
    let mut dcs = vec![0xFF, 0xC8, t30::fcf::DCS];
    dcs.extend(dcs_fif_v17(bit_rate));
    let wire: Vec<Vec<u8>> = [tsi, dcs].iter().map(|f| reverse_octets(f)).collect();
    let mut v21 = V21Tx::new(-15.0);
    let mut peer = vec![0i16; 800];
    peer.extend(v21.modulate(&hdlc::encode_burst(&wire, 38, 2)));
    let _ = run_engine(&mut e, &peer, 3_000);
    let mut rng = Rng::new(0xB103);
    let mut bits = vec![false; (bit_rate as f64 * 1.5) as usize];
    bits.extend(rng.bits(64));
    let audio = through(
        &spandsp_v17(bit_rate as i32, false, &bits),
        &Chan::clean(-15.0),
        &mut rng,
    )
    .audio;
    let frames = frames_in(&run_engine(
        &mut e,
        &audio,
        (audio.len() / 160 + 1) as u64 * 20 + 12_000,
    ));
    let done = e.is_done();
    (fcfs(&frames), e.drain_log(), done)
}

#[test]
fn b10_t30_does_not_confirm_a_v17_training_it_cannot_receive() {
    for bit_rate in [9600, 14400] {
        let (f, log, done) = v17_dcs_then_tcf(bit_rate);
        eprintln!("B10 DCS V.17 {bit_rate}: frames {f:02x?} done {done} log {log:#?}");
        // no CFR for a TCF nobody received: T.30 refuses the training (FTT) and, as the peer does
        // not retry, ends the call (DCN) after T2; it does not renegotiate with a new DIS
        // (docs/test-reports/M5d.md section 4)
        assert_eq!(f, [t30::fcf::FTT, t30::fcf::DCN], "V.17 {bit_rate}");
        // the engine says why there is no receiver
        assert!(
            log.iter()
                .any(|l| l.contains("V17") && l.contains("not received")),
            "V.17 {bit_rate}: log {log:#?}"
        );
    }
}
