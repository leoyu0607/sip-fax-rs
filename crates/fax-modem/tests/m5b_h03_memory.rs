//! M5b H03 (test plan `docs/test-plans/M5.md` section 5): the HDLC receiver's memory is bounded
//! whatever the input. Own test binary: it installs a counting allocator, which must not see any
//! other test's allocations (one `#[test]` only).
//!
//! Streams of 20 million bits that never close a frame (the kind that would grow a buffer
//! without a limit): data after one flag with no further flag, runs of ones, an endless
//! "frame" of octets with stuffing, an open frame restarted again and again by aborts.

use fax_modem::hdlc::{FLAG, HdlcEvent, HdlcRx, MAX_FRAME};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

// SAFETY: forwards to the system allocator and only counts.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        // SAFETY: same contract as the caller's.
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            let now = LIVE.fetch_add(l.size(), Ordering::Relaxed) + l.size();
            PEAK.fetch_max(now, Ordering::Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Ordering::Relaxed);
        // SAFETY: same contract as the caller's.
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        // SAFETY: same contract as the caller's.
        let q = unsafe { System.realloc(p, l, new) };
        if !q.is_null() {
            if new >= l.size() {
                let now = LIVE.fetch_add(new - l.size(), Ordering::Relaxed) + (new - l.size());
                PEAK.fetch_max(now, Ordering::Relaxed);
            } else {
                LIVE.fetch_sub(l.size() - new, Ordering::Relaxed);
            }
        }
        q
    }
}

#[global_allocator]
static A: Counting = Counting;

const BITS: usize = 20_000_000;

/// Feeds `bits` (a generator) and returns (the peak live bytes above the start, events, the
/// longest frame reported).
fn run(mut next: impl FnMut(usize) -> bool) -> (usize, usize, usize) {
    let mut rx = HdlcRx::new();
    let base = LIVE.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let mut events = 0;
    let mut longest = 0;
    for i in 0..BITS {
        if let Some(e) = rx.bit(next(i)) {
            events += 1;
            if let HdlcEvent::Frame { data, .. } = e {
                longest = longest.max(data.len());
            }
        }
    }
    (
        PEAK.load(Ordering::Relaxed).saturating_sub(base),
        events,
        longest,
    )
}

fn flag(i: usize) -> bool {
    FLAG >> (i % 8) & 1 != 0
}

#[test]
fn h03_memory_stays_bounded_on_streams_that_never_end_a_frame() {
    // 64 KiB over the starting point is the limit; the frame buffer is MAX_FRAME + 2 octets, and
    // the largest copy at a flag about 8 bytes per bit of it
    const LIMIT: usize = 64 * 1024;
    let mut xs = 0x1234_5678_9ABC_DEF1u64;

    // 1. a flag, then 0xAA octets for ever: no flag, no abort, the frame never ends
    let (peak, events, _) = run(|i| if i < 8 { flag(i) } else { i % 2 == 1 });
    assert!(peak < LIMIT, "endless open frame: peak {peak} bytes");
    assert_eq!(events, 0);

    // 2. all ones (an abort at once, then idle)
    let (peak, events, _) = run(|_| true);
    assert!(peak < LIMIT, "all ones: peak {peak}");
    assert!(events <= 1);

    // 3. all zeros
    let (peak, events, _) = run(|_| false);
    assert!(peak < LIMIT, "all zeros: peak {peak}");
    assert_eq!(events, 0);

    // 4. a flag, then random data in which a zero follows every fifth one in a row (a stuffed
    // stream without flags): an endless frame of random octets
    let mut ones = 0;
    let (peak, events, _) = run(|i| {
        if i < 8 {
            return flag(i);
        }
        if ones == 5 {
            ones = 0;
            return false;
        }
        xs ^= xs << 13;
        xs ^= xs >> 7;
        xs ^= xs << 17;
        let b = xs & 1 != 0;
        ones = if b { ones + 1 } else { 0 };
        b
    });
    assert!(peak < LIMIT, "endless stuffed frame: peak {peak}");
    assert_eq!(events, 0);

    // 5. pure random bits (flags, aborts and frames of every length all the time)
    let (peak, _events, longest) = run(|_| {
        xs ^= xs << 13;
        xs ^= xs >> 7;
        xs ^= xs << 17;
        xs & 1 != 0
    });
    assert!(peak < LIMIT, "random bits: peak {peak}");
    // (the counter works: random bits make frames, and a frame is copied at its end)
    assert!(peak > 0, "the counting allocator saw nothing");
    assert!(longest <= MAX_FRAME);

    // 6. an open frame of 600 octets, then an abort, again and again
    let (peak, _events, longest) = run(|i| {
        let k = i % 5000;
        if k < 8 {
            flag(k)
        } else if k < 4800 {
            // octets 0x55: no flag, no abort
            k % 2 == 0
        } else {
            true // 200 ones: abort
        }
    });
    assert!(peak < LIMIT, "aborts: peak {peak}");
    assert!(longest <= MAX_FRAME);
}
