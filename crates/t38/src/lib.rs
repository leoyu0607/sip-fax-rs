//! T.38 IFP packet codec (ASN.1 PER, v0-v3) and UDPTL transport with redundancy / parity FEC.
//!
//! The crate is pure: it never performs I/O, owns no clock and never panics on any input.
//!
//! * [`ifp`]: the IFP packet model and its aligned-PER codec (ITU-T T.38 ASN.1, versions 0-3).
//! * [`udptl`]: the UDPTL packet codec and the sender ([`udptl::UdptlSender`]) producing
//!   redundancy or parity-FEC error recovery.
//! * [`receiver`]: [`receiver::UdptlReceiver`], which orders, de-duplicates and repairs a
//!   received UDPTL stream and reports unrecoverable losses as gaps.
//! * [`per`]: the small subset of ALIGNED PER (X.691) primitives shared by both codecs.

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

pub mod ifp;
pub mod per;
pub mod receiver;
pub mod udptl;

pub use ifp::{DataType, Field, FieldType, Ifp, IfpError, Indicator, T38Version};
pub use receiver::{ReceiverConfig, ReceiverStats, RxEvent, Source, UdptlReceiver};
pub use udptl::{ErrorRecovery, UdptlError, UdptlPacket, UdptlRecovery, UdptlSender};
