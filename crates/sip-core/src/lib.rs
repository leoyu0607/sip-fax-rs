//! Standalone SIP engine: message model, transport, transactions, dialogs and SDP (incl. T.38).
//! Must not depend on any fax crate.
//!
//! Phase M1a provides the pure (I/O-free) building blocks, M1b the I/O layers on top:
//!
//! * [`message`]: SIP message model, parser and serializer (RFC 3261, tested against RFC 4475).
//! * [`sdp`]: SDP model, T.38 attribute handling and offer/answer negotiation (RFC 4566/3264).
//! * [`auth`]: Digest authentication for UAC use (RFC 2617 / 3261 section 22).
//! * [`net`]: IPv4/IPv6 address string helpers.
//! * [`transport::framing`]: stream (TCP) message framing.
//! * [`transport`]: UDP / TCP transport, RFC 3581 stamping, response routing, source policy.
//! * [`transaction`]: the four RFC 3261 transaction state machines (RFC 6026) and the layer
//!   that runs them.
//! * [`dialog`] and [`endpoint`]: dialogs, UAS / UAC core, session timers and the high-level API.
//! * [`registration`]: REGISTER client with digest authentication, refresh and backoff.

#![deny(missing_docs)]
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable
    )
)]

pub mod auth;
pub mod dialog;
pub mod endpoint;
pub mod message;
pub mod net;
pub mod registration;
pub mod sdp;
pub mod transaction;
pub mod transport;
