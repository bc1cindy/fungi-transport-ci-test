//! Arti (in-process Tor) backend for the fungi P2P datagram channel.
//!
//! Same channel contract as `fungi_transport::tor` (the SOCKS5h backend),
//! with Tor running inside the process via `arti-client`: each peer runs an
//! onion service and opens streams to peer `.onion` addresses. Message
//! delimitation is the same [`fungi_transport::framed`] length-prefix layer.

mod error;
