//! Minimal hand-written FFI to SpanDSP 3.1.x.
//!
//! All SpanDSP state structures are treated as opaque; pass NULL to the `*_init`
//! functions so SpanDSP allocates them itself. No bindgen / libclang needed.
#![allow(non_camel_case_types)]

use std::os::raw::{c_char, c_int, c_void};

/// SpanDSP version found by pkg-config at build time (e.g. "3.1.1").
pub const SPANDSP_VERSION: &str = env!("SPANDSP_VERSION");

macro_rules! opaque {
    ($($name:ident),*) => {$(
        #[repr(C)]
        pub struct $name { _private: [u8; 0] }
    )*};
}
opaque!(
    t38_terminal_state_t,
    t38_core_state_t,
    t30_state_t,
    fax_state_t
);

/// `int (*)(t38_core_state_t *s, void *user_data, const uint8_t *buf, int len, int count)`
pub type t38_tx_packet_handler_t = unsafe extern "C" fn(
    s: *mut t38_core_state_t,
    user_data: *mut c_void,
    buf: *const u8,
    len: c_int,
    count: c_int,
) -> c_int;

pub type t30_phase_e_handler_t =
    unsafe extern "C" fn(user_data: *mut c_void, completion_code: c_int);

// t30.h: t30_err_e
pub const T30_ERR_OK: c_int = 0;

// t30.h: modem support bit masks
pub const T30_SUPPORT_V27TER: c_int = 0x01;
pub const T30_SUPPORT_V29: c_int = 0x02;
pub const T30_SUPPORT_V17: c_int = 0x04;

// t4_rx.h: compression bit masks
pub const T4_COMPRESSION_T4_1D: c_int = 0x02;
pub const T4_COMPRESSION_T4_2D: c_int = 0x04;
pub const T4_COMPRESSION_T6: c_int = 0x08;

// t30.h: internet aware fax modes (t30_set_iaf_mode)
pub const T30_IAF_MODE_T38: c_int = 0x02;
pub const T30_IAF_MODE_NO_TCF: c_int = 0x10;

// t30.h: result codes used by the tests
pub const T30_ERR_INCOMPATIBLE: c_int = 8;

// t38_terminal.h: options
pub const T38_TERMINAL_OPTION_NO_PACING: c_int = 0x01;

unsafe extern "C" {
    pub fn t38_terminal_init(
        s: *mut t38_terminal_state_t,
        calling_party: bool,
        tx_packet_handler: t38_tx_packet_handler_t,
        tx_packet_user_data: *mut c_void,
    ) -> *mut t38_terminal_state_t;
    pub fn t38_terminal_free(s: *mut t38_terminal_state_t) -> c_int;
    pub fn t38_terminal_send_timeout(s: *mut t38_terminal_state_t, samples: c_int) -> c_int;
    pub fn t38_terminal_get_t30_state(s: *mut t38_terminal_state_t) -> *mut t30_state_t;
    pub fn t38_terminal_get_t38_core_state(s: *mut t38_terminal_state_t) -> *mut t38_core_state_t;
    pub fn t38_terminal_set_config(s: *mut t38_terminal_state_t, config: c_int);
    pub fn t38_terminal_set_tep_mode(s: *mut t38_terminal_state_t, use_tep: bool);

    pub fn t38_core_rx_ifp_packet(
        s: *mut t38_core_state_t,
        buf: *const u8,
        len: c_int,
        seq_no: u16,
    ) -> c_int;
    pub fn t38_set_t38_version(s: *mut t38_core_state_t, t38_version: c_int);

    pub fn t30_set_tx_file(
        s: *mut t30_state_t,
        file: *const c_char,
        start_page: c_int,
        stop_page: c_int,
    );
    pub fn t30_set_rx_file(s: *mut t30_state_t, file: *const c_char, stop_page: c_int);
    pub fn t30_set_tx_ident(s: *mut t30_state_t, id: *const c_char) -> c_int;
    pub fn t30_set_ecm_capability(s: *mut t30_state_t, enabled: bool) -> c_int;
    pub fn t30_set_supported_modems(s: *mut t30_state_t, supported_modems: c_int) -> c_int;
    pub fn t30_set_supported_compressions(
        s: *mut t30_state_t,
        supported_compressions: c_int,
    ) -> c_int;
    pub fn t30_set_phase_e_handler(
        s: *mut t30_state_t,
        handler: t30_phase_e_handler_t,
        user_data: *mut c_void,
    );
    pub fn t30_set_iaf_mode(s: *mut t30_state_t, iaf: c_int);
    pub fn t30_terminate(s: *mut t30_state_t);
    pub fn t30_completion_code_to_str(result: c_int) -> *const c_char;

    pub fn fax_init(s: *mut fax_state_t, calling_party: bool) -> *mut fax_state_t;
    pub fn fax_free(s: *mut fax_state_t) -> c_int;
    pub fn fax_rx(s: *mut fax_state_t, amp: *mut i16, len: c_int) -> c_int;
    pub fn fax_tx(s: *mut fax_state_t, amp: *mut i16, max_len: c_int) -> c_int;
    pub fn fax_set_transmit_on_idle(s: *mut fax_state_t, transmit_on_idle: c_int);
    pub fn fax_get_t30_state(s: *mut fax_state_t) -> *mut t30_state_t;
}

// ---------------------------------------------------------------------------------------------
// Reference UDPTL (SpanDSP tests/udptl.c, compiled by build.rs). libspandsp does not export it.

opaque!(udptl_state_t);

/// `int (*)(void *user_data, const uint8_t msg[], int len, int seq_no)`
pub type udptl_rx_packet_handler_t = unsafe extern "C" fn(
    user_data: *mut c_void,
    msg: *const u8,
    len: c_int,
    seq_no: c_int,
) -> c_int;

/// udptl.h: `UDPTL_ERROR_CORRECTION_NONE`
pub const UDPTL_ERROR_CORRECTION_NONE: c_int = 0;
/// udptl.h: `UDPTL_ERROR_CORRECTION_FEC`
pub const UDPTL_ERROR_CORRECTION_FEC: c_int = 1;
/// udptl.h: `UDPTL_ERROR_CORRECTION_REDUNDANCY`
pub const UDPTL_ERROR_CORRECTION_REDUNDANCY: c_int = 2;
/// udptl.h: `LOCAL_FAX_MAX_DATAGRAM`
pub const UDPTL_LOCAL_FAX_MAX_DATAGRAM: usize = 400;

unsafe extern "C" {
    pub fn udptl_init(
        s: *mut udptl_state_t,
        ec_scheme: c_int,
        span: c_int,
        entries: c_int,
        rx_packet_handler: udptl_rx_packet_handler_t,
        user_data: *mut c_void,
    ) -> *mut udptl_state_t;
    pub fn udptl_free(s: *mut udptl_state_t) -> c_int;
    pub fn udptl_build_packet(
        s: *mut udptl_state_t,
        buf: *mut u8,
        msg: *const u8,
        msg_len: c_int,
    ) -> c_int;
    pub fn udptl_rx_packet(s: *mut udptl_state_t, buf: *const u8, len: c_int) -> c_int;
}
