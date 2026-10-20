//! Safe RAII wrappers over the SpanDSP FFI.

use spandsp_sys as sys;
use std::cell::{Cell, RefCell};
use std::ffi::{CStr, CString, c_void};
use std::os::raw::c_int;
use std::path::Path;

/// Human-readable name of a T.30 completion code.
pub fn completion_str(code: i32) -> String {
    // SAFETY: returns a pointer to a static string (or NULL).
    unsafe {
        let p = sys::t30_completion_code_to_str(code);
        if p.is_null() {
            format!("code {code}")
        } else {
            CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    }
}

/// Per-side T.30 setup.
#[derive(Debug, Clone, Default)]
pub struct T30Config<'a> {
    /// File to transmit (calling side).
    pub tx_file: Option<&'a Path>,
    /// File to store received pages into (answering side).
    pub rx_file: Option<&'a Path>,
    pub ecm: bool,
    pub ident: &'a str,
    /// `T30_SUPPORT_*` bit mask; `None` enables V.27ter, V.29 and V.17.
    pub modems: Option<i32>,
}

fn cpath(p: &Path) -> CString {
    CString::new(p.to_str().expect("utf-8 path")).expect("no NUL in path")
}

/// # Safety
/// `t30` must be a valid T.30 state; `phase_e` must outlive it.
unsafe fn configure_t30(
    t30: *mut sys::t30_state_t,
    cfg: &T30Config<'_>,
    phase_e: &Cell<Option<i32>>,
) {
    unsafe extern "C" fn on_phase_e(user_data: *mut c_void, code: c_int) {
        // SAFETY: user_data is the `Cell` registered below, alive as long as the state.
        let cell = unsafe { &*(user_data as *const Cell<Option<i32>>) };
        cell.set(Some(code));
    }
    let ident = CString::new(cfg.ident).expect("ident");
    unsafe {
        sys::t30_set_tx_ident(t30, ident.as_ptr());
        sys::t30_set_ecm_capability(t30, cfg.ecm);
        sys::t30_set_supported_modems(
            t30,
            cfg.modems
                .unwrap_or(sys::T30_SUPPORT_V27TER | sys::T30_SUPPORT_V29 | sys::T30_SUPPORT_V17),
        );
        sys::t30_set_supported_compressions(
            t30,
            sys::T4_COMPRESSION_T4_1D | sys::T4_COMPRESSION_T4_2D | sys::T4_COMPRESSION_T6,
        );
        if let Some(p) = cfg.tx_file {
            let c = cpath(p);
            sys::t30_set_tx_file(t30, c.as_ptr(), -1, -1);
        }
        if let Some(p) = cfg.rx_file {
            let c = cpath(p);
            sys::t30_set_rx_file(t30, c.as_ptr(), -1);
        }
        sys::t30_set_phase_e_handler(t30, on_phase_e, phase_e as *const _ as *mut c_void);
    }
}

/// One outgoing T.38 packet as handed to the tx handler.
#[derive(Debug, Clone)]
pub struct OutPacket {
    pub data: Vec<u8>,
    /// Number of times SpanDSP wants this packet sent (redundancy).
    pub count: i32,
}

/// RAII wrapper around `t38_terminal_state_t`.
pub struct T38Terminal {
    ptr: *mut sys::t38_terminal_state_t,
    outbox: Box<RefCell<Vec<OutPacket>>>,
    phase_e: Box<Cell<Option<i32>>>,
}

unsafe extern "C" fn tx_packet(
    _s: *mut sys::t38_core_state_t,
    user_data: *mut c_void,
    buf: *const u8,
    len: c_int,
    count: c_int,
) -> c_int {
    // SAFETY: user_data is the boxed outbox; buf/len describe a valid slice.
    let outbox = unsafe { &*(user_data as *const RefCell<Vec<OutPacket>>) };
    let data = unsafe { std::slice::from_raw_parts(buf, len as usize) }.to_vec();
    outbox.borrow_mut().push(OutPacket { data, count });
    0
}

impl T38Terminal {
    /// Create a terminal (SpanDSP allocates the state). Returns `None` if init fails.
    pub fn new(calling_party: bool, cfg: &T30Config<'_>) -> Option<Self> {
        let outbox = Box::new(RefCell::new(Vec::new()));
        let phase_e = Box::new(Cell::new(None));
        // SAFETY: NULL state => SpanDSP allocates; user data outlives the terminal
        // because the boxes are dropped after `Drop::drop` frees the state.
        let ptr = unsafe {
            sys::t38_terminal_init(
                std::ptr::null_mut(),
                calling_party,
                tx_packet,
                &*outbox as *const _ as *mut c_void,
            )
        };
        if ptr.is_null() {
            return None;
        }
        let t = Self {
            ptr,
            outbox,
            phase_e,
        };
        // SAFETY: ptr is valid.
        unsafe {
            configure_t30(sys::t38_terminal_get_t30_state(t.ptr), cfg, &t.phase_e);
            sys::t38_terminal_set_config(t.ptr, 0);
        }
        Some(t)
    }

    /// Advance the terminal clock. Returns true when SpanDSP reports it finished.
    pub fn advance(&mut self, samples: usize) -> bool {
        // SAFETY: ptr is valid.
        unsafe { sys::t38_terminal_send_timeout(self.ptr, samples as c_int) != 0 }
    }

    /// Take packets emitted since the last call.
    pub fn take_packets(&mut self) -> Vec<OutPacket> {
        std::mem::take(&mut *self.outbox.borrow_mut())
    }

    /// Feed a received IFP packet.
    pub fn rx_packet(&mut self, data: &[u8], seq: u16) {
        // SAFETY: ptr valid; data slice valid for the call.
        unsafe {
            let core = sys::t38_terminal_get_t38_core_state(self.ptr);
            sys::t38_core_rx_ifp_packet(core, data.as_ptr(), data.len() as c_int, seq);
        }
    }

    /// Sets the T.38 version SpanDSP uses to encode and decode IFP packets (default 0).
    pub fn set_t38_version(&mut self, version: i32) {
        // SAFETY: ptr valid.
        unsafe {
            let core = sys::t38_terminal_get_t38_core_state(self.ptr);
            sys::t38_set_t38_version(core, version);
        }
    }

    pub fn completion_code(&self) -> Option<i32> {
        self.phase_e.get()
    }
}

impl Drop for T38Terminal {
    fn drop(&mut self) {
        // SAFETY: ptr came from t38_terminal_init and is freed exactly once.
        unsafe { sys::t38_terminal_free(self.ptr) };
    }
}

/// RAII wrapper around `fax_state_t` (audio-domain fax modem + T.30).
pub struct FaxModem {
    ptr: *mut sys::fax_state_t,
    phase_e: Box<Cell<Option<i32>>>,
}

impl FaxModem {
    pub fn new(calling_party: bool, cfg: &T30Config<'_>) -> Option<Self> {
        // SAFETY: NULL => SpanDSP allocates.
        let ptr = unsafe { sys::fax_init(std::ptr::null_mut(), calling_party) };
        if ptr.is_null() {
            return None;
        }
        let f = Self {
            ptr,
            phase_e: Box::new(Cell::new(None)),
        };
        // SAFETY: ptr valid.
        unsafe {
            sys::fax_set_transmit_on_idle(f.ptr, 1);
            configure_t30(sys::fax_get_t30_state(f.ptr), cfg, &f.phase_e);
        }
        Some(f)
    }

    /// Fill `amp` with transmit audio; returns samples produced.
    pub fn tx(&mut self, amp: &mut [i16]) -> usize {
        // SAFETY: ptr valid; amp valid for len samples.
        unsafe { sys::fax_tx(self.ptr, amp.as_mut_ptr(), amp.len() as c_int) as usize }
    }

    /// Feed received audio.
    pub fn rx(&mut self, amp: &mut [i16]) {
        // SAFETY: as above.
        unsafe { sys::fax_rx(self.ptr, amp.as_mut_ptr(), amp.len() as c_int) };
    }

    pub fn completion_code(&self) -> Option<i32> {
        self.phase_e.get()
    }
}

impl Drop for FaxModem {
    fn drop(&mut self) {
        // SAFETY: freed exactly once.
        unsafe { sys::fax_free(self.ptr) };
    }
}
