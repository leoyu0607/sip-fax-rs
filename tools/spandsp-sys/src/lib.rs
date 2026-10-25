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

// ---------------------------------------------------------------------------------------------
// G.711 and modem connect tones (M5a tests G01, G02, N03). `alaw_to_linear` and the other
// per-sample converters are `static inline` in g711.h and not exported: the exported
// `g711_encode` / `g711_decode` run exactly the same inline code over a buffer.

opaque!(g711_state_t, modem_connect_tones_rx_state_t);

/// g711.h: `G711_ALAW`
pub const G711_ALAW: c_int = 0;
/// g711.h: `G711_ULAW`
pub const G711_ULAW: c_int = 1;

/// modem_connect_tones.h: 1100 Hz calling tone
pub const MODEM_CONNECT_TONES_FAX_CNG: c_int = 1;
/// modem_connect_tones.h: `MODEM_CONNECT_TONES_ANS` (= `MODEM_CONNECT_TONES_FAX_CED`), 2100 Hz
pub const MODEM_CONNECT_TONES_ANS: c_int = 2;
/// modem_connect_tones.h: ANS with phase reversals
pub const MODEM_CONNECT_TONES_ANS_PR: c_int = 3;
/// modem_connect_tones.h: CED or the V.21 preamble
pub const MODEM_CONNECT_TONES_FAX_CED_OR_PREAMBLE: c_int = 7;

/// `void (*)(void *user_data, int code, int level, int delay)` (`span_tone_report_func_t`)
pub type span_tone_report_func_t =
    Option<unsafe extern "C" fn(user_data: *mut c_void, code: c_int, level: c_int, delay: c_int)>;

unsafe extern "C" {
    pub fn g711_init(s: *mut g711_state_t, mode: c_int) -> *mut g711_state_t;
    pub fn g711_free(s: *mut g711_state_t) -> c_int;
    pub fn g711_decode(
        s: *mut g711_state_t,
        amp: *mut i16,
        g711_data: *const u8,
        g711_bytes: c_int,
    ) -> c_int;
    pub fn g711_encode(
        s: *mut g711_state_t,
        g711_data: *mut u8,
        amp: *const i16,
        len: c_int,
    ) -> c_int;

    pub fn modem_connect_tones_rx_init(
        s: *mut modem_connect_tones_rx_state_t,
        tone_type: c_int,
        tone_callback: span_tone_report_func_t,
        user_data: *mut c_void,
    ) -> *mut modem_connect_tones_rx_state_t;
    pub fn modem_connect_tones_rx_free(s: *mut modem_connect_tones_rx_state_t) -> c_int;
    pub fn modem_connect_tones_rx(
        s: *mut modem_connect_tones_rx_state_t,
        amp: *const i16,
        len: c_int,
    ) -> c_int;
    pub fn modem_connect_tones_rx_get(s: *mut modem_connect_tones_rx_state_t) -> c_int;
}

// ---------------------------------------------------------------------------------------------
// V.27ter transmitter (M5c tests A01-A07, M01 uses `fax_state_t`). The independent source of
// V.27ter signals for our receiver.

opaque!(v27ter_tx_state_t);

/// async.h: `SIG_STATUS_END_OF_DATA`, returned by a `get_bit` callback when the data is over.
pub const SIG_STATUS_END_OF_DATA: c_int = -7;

/// `int (*)(void *user_data)` (`span_get_bit_func_t`): the next bit (0 or 1) or a `SIG_STATUS_*`
/// code (negative).
pub type span_get_bit_func_t = Option<unsafe extern "C" fn(user_data: *mut c_void) -> c_int>;

unsafe extern "C" {
    pub fn v27ter_tx_init(
        s: *mut v27ter_tx_state_t,
        bit_rate: c_int,
        tep: bool,
        get_bit: span_get_bit_func_t,
        user_data: *mut c_void,
    ) -> *mut v27ter_tx_state_t;
    pub fn v27ter_tx_free(s: *mut v27ter_tx_state_t) -> c_int;
    /// Output power in dBm0.
    pub fn v27ter_tx_power(s: *mut v27ter_tx_state_t, power: f32);
    /// Generates up to `len` samples; returns how many (0 once the transmission is over).
    pub fn v27ter_tx(s: *mut v27ter_tx_state_t, amp: *mut i16, len: c_int) -> c_int;
}

// ---------------------------------------------------------------------------------------------
// V.29 and V.17 transmitters (M5d tests B01-B07; B10 sends V.17 at a V.29 receiver). Same shape
// as `v27ter_tx`. Signatures from `v29tx.h` / `v17tx.h`.

opaque!(v29_tx_state_t, v17_tx_state_t);

unsafe extern "C" {
    pub fn v29_tx_init(
        s: *mut v29_tx_state_t,
        bit_rate: c_int,
        tep: bool,
        get_bit: span_get_bit_func_t,
        user_data: *mut c_void,
    ) -> *mut v29_tx_state_t;
    pub fn v29_tx_free(s: *mut v29_tx_state_t) -> c_int;
    /// Output power in dBm0.
    pub fn v29_tx_power(s: *mut v29_tx_state_t, power: f32);
    /// Generates up to `len` samples; returns how many (0 once the transmission is over).
    pub fn v29_tx(s: *mut v29_tx_state_t, amp: *mut i16, len: c_int) -> c_int;

    pub fn v17_tx_init(
        s: *mut v17_tx_state_t,
        bit_rate: c_int,
        tep: bool,
        get_bit: span_get_bit_func_t,
        user_data: *mut c_void,
    ) -> *mut v17_tx_state_t;
    /// Restart with the long (`short_train` false) or the short training sequence.
    pub fn v17_tx_restart(
        s: *mut v17_tx_state_t,
        bit_rate: c_int,
        tep: bool,
        short_train: bool,
    ) -> c_int;
    pub fn v17_tx_free(s: *mut v17_tx_state_t) -> c_int;
    /// Output power in dBm0.
    pub fn v17_tx_power(s: *mut v17_tx_state_t, power: f32);
    pub fn v17_tx(s: *mut v17_tx_state_t, amp: *mut i16, len: c_int) -> c_int;
}
