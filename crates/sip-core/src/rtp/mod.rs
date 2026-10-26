//! RTP media for audio calls: packets (RFC 3550), G.711 (PCMU / PCMA) and a receive jitter
//! buffer. No sockets here: the caller sends and receives the datagrams.

pub mod g711;
pub mod jitter;
pub mod packet;

pub use g711::Law;
pub use jitter::{JitterBuffer, JitterConfig, JitterStats, Pulled};
pub use packet::{Extension, RTP_HEADER_SIZE, RtpError, RtpPacket};
