//! The P2P datagram channel abstraction for the Fungi protocol.
//!
//! A channel is a connection to ONE peer moving opaque byte messages, one
//! message per call. No delivery ordering across channels, no deduplication,
//! no framing, no anonymity semantics — those belong to other layers.
//!
//! The API is [`Channel`] (send/recv one connected peer), [`Connector`]
//! (open new channels) and [`Listener`] (accept inbound ones), plus
//! [`into_stream`] to adapt a `Channel` into a `Stream` where that's more
//! convenient. [`mem`] is an in-memory implementation for tests and for
//! exercising the contract before a real transport (SOCKS5h, arti, an OHTTP
//! mailbox) lands; [`testkit`] holds the transport-agnostic conformance
//! suite every `Channel` implementation is expected to pass.

pub mod channel;
pub mod error;
pub mod framed;
pub mod mem;
pub mod testkit;

pub use channel::{Channel, Connector, Listener, into_stream};
pub use error::{BoxError, ConnectError, RecvError, SendError};
