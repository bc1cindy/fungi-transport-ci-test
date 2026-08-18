//! Transport-native peer addressing.

use std::fmt;

/// Transport-native address of a tor peer: a `.onion` hostname and port,
/// obtained out of band, opaque to consumers. Shared by every Tor backend
/// (the SOCKS5h daemon backend and the in-process arti backend).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnionAddr {
    host: String,
    port: u16,
}

impl OnionAddr {
    /// An onion address from its hostname (`<56 chars>.onion`) and port.
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
        }
    }

    /// The `.onion` hostname.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The virtual port on the onion service.
    pub fn port(&self) -> u16 {
        self.port
    }
}

impl fmt::Display for OnionAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.host, self.port)
    }
}
