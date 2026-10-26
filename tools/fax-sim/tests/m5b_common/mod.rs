//! Shared by the M5b tests (H01, H02, V01-V03): the SpanDSP wrappers (HDLC transmitter and
//! receiver, FSK V.21 transmitter and receiver), a seeded random source and the channel model.
//!
//! Not every test file uses every item.
#![allow(dead_code)]

use fax_modem::level::mean_square;
use fax_sim::SplitMix64;
use spandsp_sys as sys;
use std::ffi::c_void;
use std::os::raw::c_int;

// ------------------------------------------------------------------------------------- random

pub struct Rng(pub SplitMix64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(SplitMix64::new(seed))
    }
    pub fn uniform(&mut self) -> f64 {
        ((self.0.next_u64() >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }
    pub fn gauss(&mut self) -> f64 {
        (-2.0 * self.uniform().ln()).sqrt() * (2.0 * std::f64::consts::PI * self.uniform()).cos()
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.0.next_u64() % n as u64) as usize
    }
    pub fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.0.next_u64() as u8).collect()
    }
}

/// A frame like those of T.30: address FF, control 03 or 13, an FCF and 0 to 30 information
/// octets; every fourth is a run of ones (stuffing everywhere).
pub fn t30_like_frame(rng: &mut Rng, i: usize) -> Vec<u8> {
    const FCF: [u8; 8] = [0x80, 0x82, 0x84, 0x86, 0x21, 0x22, 0x23, 0xFA];
    let mut f = vec![
        0xFF,
        if i % 2 == 0 { 0x13 } else { 0x03 },
        FCF[rng.below(8)],
    ];
    let n = rng.below(31);
    if i % 4 == 3 {
        f.extend(std::iter::repeat_n(0xFF, n));
    } else {
        f.extend(rng.bytes(n));
    }
    f
}

// ------------------------------------------------------------------------ SpanDSP HDLC and FSK

struct HdlcFeed {
    tx: *mut sys::hdlc_tx_state_t,
    frames: Vec<Vec<u8>>,
    next: usize,
    closing_flags: i32,
    /// Bits taken so far.
    bits_out: usize,
    /// Bit count at which the closing flags were all taken.
    done_at: Option<usize>,
}

unsafe extern "C" fn underflow(user: *mut c_void) {
    // SAFETY: `user` is the `HdlcFeed` of `spandsp_hdlc_bits`, alive for the whole call; `tx` was
    // set before the first bit was asked for.
    let feed = unsafe { &mut *(user as *mut HdlcFeed) };
    if let Some(f) = feed.frames.get(feed.next) {
        feed.next += 1;
        // SAFETY: valid transmitter and frame.
        let r = unsafe { sys::hdlc_tx_frame(feed.tx, f.as_ptr(), f.len()) };
        assert_eq!(r, 0, "hdlc_tx_frame refused a frame");
    } else if feed.next == feed.frames.len() {
        feed.next += 1;
        // SAFETY: valid transmitter.
        unsafe { sys::hdlc_tx_flags(feed.tx, feed.closing_flags) };
    } else {
        // asked again: the closing flags have all gone (SpanDSP then sends idle flags for ever)
        feed.done_at.get_or_insert(feed.bits_out);
    }
}

/// SpanDSP's `hdlc_tx`: `preamble_flags` flag octets, the frames (one flag between them), then
/// `closing_flags` flags; the bits as sent.
pub fn spandsp_hdlc_bits(frames: &[Vec<u8>], preamble_flags: i32, closing_flags: i32) -> Vec<bool> {
    let mut feed = Box::new(HdlcFeed {
        tx: std::ptr::null_mut(),
        frames: frames.to_vec(),
        next: 0,
        closing_flags,
        bits_out: 0,
        done_at: None,
    });
    // SAFETY: NULL makes SpanDSP allocate; `feed` outlives the transmitter (freed below).
    let tx = unsafe {
        sys::hdlc_tx_init(
            std::ptr::null_mut(),
            false,
            1,
            false,
            Some(underflow),
            &mut *feed as *mut HdlcFeed as *mut c_void,
        )
    };
    assert!(!tx.is_null(), "hdlc_tx_init failed");
    feed.tx = tx;
    // SAFETY: valid transmitter.
    unsafe { sys::hdlc_tx_flags(tx, preamble_flags) };
    let mut bits = Vec::new();
    loop {
        // SAFETY: valid transmitter.
        let b = unsafe { sys::hdlc_tx_get_bit(tx) };
        assert!(b >= 0, "hdlc_tx_get_bit returned {b}");
        bits.push(b != 0);
        feed.bits_out = bits.len();
        if feed.done_at.is_some() {
            break;
        }
        assert!(bits.len() < 50_000_000, "hdlc_tx does not stop");
    }
    // SAFETY: allocated by the init call, freed once.
    unsafe { sys::hdlc_tx_free(tx) };
    let end = feed.done_at.expect("the frames went out");
    bits.truncate(end);
    bits
}

struct BitFeed {
    bits: Vec<bool>,
    pos: usize,
}

unsafe extern "C" fn get_bit(user: *mut c_void) -> c_int {
    // SAFETY: `user` is the `BitFeed` registered by `spandsp_fsk_tx`, alive for the whole call.
    let feed = unsafe { &mut *(user as *mut BitFeed) };
    match feed.bits.get(feed.pos) {
        Some(&b) => {
            feed.pos += 1;
            c_int::from(b)
        }
        None => sys::SIG_STATUS_END_OF_DATA,
    }
}

/// SpanDSP's V.21 channel 2 transmitter on `bits`, at `level_dbm0`; the audio from the first bit
/// to the last (no lead-in or tail).
pub fn spandsp_fsk_tx(bits: &[bool], level_dbm0: f32) -> Vec<i16> {
    let mut feed = Box::new(BitFeed {
        bits: bits.to_vec(),
        pos: 0,
    });
    // SAFETY: NULL makes SpanDSP allocate; `feed` outlives it. The spec table is a static.
    let tx = unsafe {
        sys::fsk_tx_init(
            std::ptr::null_mut(),
            &sys::preset_fsk_specs[sys::FSK_V21CH2],
            Some(get_bit),
            &mut *feed as *mut BitFeed as *mut c_void,
        )
    };
    assert!(!tx.is_null(), "fsk_tx_init failed");
    // SAFETY: valid state.
    unsafe { sys::fsk_tx_power(tx, level_dbm0) };
    let mut out = Vec::with_capacity(bits.len() * 27);
    let mut buf = [0i16; 160];
    while feed.pos < bits.len() {
        // SAFETY: `buf` is valid for 160 samples.
        let n = unsafe { sys::fsk_tx(tx, buf.as_mut_ptr(), 160) } as usize;
        out.extend_from_slice(&buf[..n]);
        assert!(out.len() < 8_000 * 3_000, "fsk_tx does not stop");
    }
    // SAFETY: allocated by the init call, freed once.
    unsafe { sys::fsk_tx_free(tx) };
    out
}

unsafe extern "C" fn put_bit(user: *mut c_void, bit: c_int) {
    // SAFETY: `user` is the `Vec<i32>` registered by `spandsp_fsk_rx`, alive for the whole call.
    let v = unsafe { &mut *(user as *mut Vec<i32>) };
    v.push(bit);
}

/// SpanDSP's V.21 channel 2 receiver: the bits (and, as negative numbers, the status codes) it
/// reports over `samples`.
pub fn spandsp_fsk_rx_raw(samples: &[i16]) -> Vec<i32> {
    let mut got: Box<Vec<i32>> = Box::default();
    // SAFETY: NULL makes SpanDSP allocate; `got` outlives it.
    let rx = unsafe {
        sys::fsk_rx_init(
            std::ptr::null_mut(),
            &sys::preset_fsk_specs[sys::FSK_V21CH2],
            sys::FSK_FRAME_MODE_SYNC,
            Some(put_bit),
            &mut *got as *mut Vec<i32> as *mut c_void,
        )
    };
    assert!(!rx.is_null(), "fsk_rx_init failed");
    // the default cutoff is -42 dBm0; the cases here go down to -43
    // SAFETY: valid state.
    unsafe { sys::fsk_rx_set_signal_cutoff(rx, -50.0) };
    for chunk in samples.chunks(160) {
        // SAFETY: the chunk is valid for its length.
        unsafe { sys::fsk_rx(rx, chunk.as_ptr(), chunk.len() as c_int) };
    }
    // SAFETY: allocated by the init call, freed once.
    unsafe { sys::fsk_rx_free(rx) };
    *got
}

/// The data bits of [`spandsp_fsk_rx_raw`].
pub fn spandsp_fsk_rx(samples: &[i16]) -> Vec<bool> {
    spandsp_fsk_rx_raw(samples)
        .into_iter()
        .filter(|&b| b >= 0)
        .map(|b| b != 0)
        .collect()
}

unsafe extern "C" fn frame_handler(user: *mut c_void, pkt: *const u8, len: c_int, ok: c_int) {
    // SAFETY: `user` is the vector registered by `spandsp_hdlc_rx`; `pkt` is valid for `len`.
    let v = unsafe { &mut *(user as *mut Vec<(Vec<u8>, bool)>) };
    if len < 0 {
        // a status report (framing OK, carrier up, ...), not a frame
        return;
    }
    let data = if pkt.is_null() || len == 0 {
        Vec::new()
    } else {
        // SAFETY: SpanDSP gives `len` valid octets.
        unsafe { std::slice::from_raw_parts(pkt, len as usize) }.to_vec()
    };
    v.push((data, ok != 0));
}

/// SpanDSP's HDLC receiver (CRC-16, bad frames reported) over `bits`: (frame without FCS, ok).
pub fn spandsp_hdlc_rx(bits: &[bool]) -> Vec<(Vec<u8>, bool)> {
    let mut got: Box<Vec<(Vec<u8>, bool)>> = Box::default();
    // SAFETY: NULL makes SpanDSP allocate; `got` outlives it.
    let rx = unsafe {
        sys::hdlc_rx_init(
            std::ptr::null_mut(),
            false,
            true,
            1,
            Some(frame_handler),
            &mut *got as *mut Vec<(Vec<u8>, bool)> as *mut c_void,
        )
    };
    assert!(!rx.is_null(), "hdlc_rx_init failed");
    for &b in bits {
        // SAFETY: valid state.
        unsafe { sys::hdlc_rx_put_bit(rx, c_int::from(b)) };
    }
    // SAFETY: allocated by the init call, freed once.
    unsafe { sys::hdlc_rx_free(rx) };
    *got
}

// ------------------------------------------------------------------------------ channel model

/// The channel between the transmitter and the receiver.
#[derive(Clone, Debug)]
pub struct Chan {
    /// Signal level over the active part (dBm0).
    pub level: f64,
    /// White noise over the whole band, relative to the signal power.
    pub snr_db: Option<f64>,
    /// Sender clock deviation (+100 means the sender runs 100 ppm fast).
    pub ppm: f64,
    /// Frequency shift of the whole signal (Hz).
    pub offset_hz: f64,
}

impl Chan {
    pub fn clean(level: f64) -> Self {
        Self {
            level,
            snr_db: None,
            ppm: 0.0,
            offset_hz: 0.0,
        }
    }
    pub fn snr(mut self, db: f64) -> Self {
        self.snr_db = Some(db);
        self
    }
    pub fn ppm(mut self, ppm: f64) -> Self {
        self.ppm = ppm;
        self
    }
    pub fn offset(mut self, hz: f64) -> Self {
        self.offset_hz = hz;
        self
    }
}

/// Silence before and after the signal (200 ms): the noise is there too.
pub const PAD: usize = 1_600;

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

/// `y[n] = x(n * ratio)`: what a receiver sampling at 8000 Hz gets from a sender whose clock runs
/// `ratio` times too fast. Windowed sinc, 16 taps.
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

/// Audio after the channel, with where the signal is.
pub struct Heard {
    pub audio: Vec<i16>,
    /// First sample of the signal.
    pub onset: usize,
    /// One past the last sample of the signal.
    pub end: usize,
}

impl Heard {
    /// The audio of the signal with 10 ms on each side: what a receiver behind the signal-present
    /// detector of the engine gets (it is not listening to the noise before and after).
    pub fn gated(&self) -> &[i16] {
        &self.audio[self.onset.saturating_sub(80)..(self.end + 80).min(self.audio.len())]
    }
}

/// Passes `tx` (a transmitter's samples) through the channel: the active part (first to last
/// sample that is not silence) is set to the channel level, with `PAD` samples of silence on each
/// side, and white noise over everything at the SNR relative to that level.
pub fn through(tx: &[i16], ch: &Chan, rng: &mut Rng) -> Heard {
    let mut x: Vec<f64> = tx.iter().map(|&s| f64::from(s)).collect();
    if ch.offset_hz != 0.0 {
        x = freq_shift(&x, ch.offset_hz);
    }
    if ch.ppm != 0.0 {
        x = resample(&x, 1.0 + ch.ppm * 1e-6);
    }
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
    let audio = y
        .iter()
        .map(|v| v.round().clamp(-32768.0, 32767.0) as i16)
        .collect();
    Heard {
        audio,
        onset: PAD + first,
        end: PAD + last,
    }
}
