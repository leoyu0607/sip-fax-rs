//! S02: FFI lifecycle. Kept in its own test binary (own process) so that RSS
//! measurement is not disturbed by other tests running in parallel threads.

use spandsp_sys as sys;
use std::os::raw::{c_int, c_void};

unsafe extern "C" fn sink(
    _s: *mut sys::t38_core_state_t,
    _u: *mut c_void,
    _b: *const u8,
    _l: c_int,
    _c: c_int,
) -> c_int {
    0
}

fn rss_bytes() -> u64 {
    let s = std::fs::read_to_string("/proc/self/statm").expect("/proc/self/statm");
    let pages: u64 = s.split_whitespace().nth(1).unwrap().parse().unwrap();
    pages * 4096
}

fn cycle() {
    unsafe {
        let t = sys::t38_terminal_init(std::ptr::null_mut(), true, sink, std::ptr::null_mut());
        assert!(!t.is_null(), "t38_terminal_init returned NULL");
        assert!(!sys::t38_terminal_get_t30_state(t).is_null());
        assert_eq!(sys::t38_terminal_free(t), 0);
        let f = sys::fax_init(std::ptr::null_mut(), false);
        assert!(!f.is_null(), "fax_init returned NULL");
        assert!(!sys::fax_get_t30_state(f).is_null());
        assert_eq!(sys::fax_free(f), 0);
    }
}

#[test]
fn s02_ffi_lifecycle_no_leak() {
    for _ in 0..20 {
        cycle(); // warm up allocator arenas
    }
    let before = rss_bytes();
    for _ in 0..100 {
        cycle();
    }
    let after = rss_bytes();
    let growth = after.saturating_sub(before);
    eprintln!("S02 rss before={before} after={after} growth={growth}");
    assert!(
        growth < 1024 * 1024,
        "RSS grew by {growth} bytes over 100 cycles"
    );
}
