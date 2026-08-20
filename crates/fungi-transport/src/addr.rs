//! Transport-native peer addressing.

use std::fmt;
use std::str::FromStr;

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

/// Failure to parse an [`OnionAddr`] from its `host:port` text form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseOnionAddrError;

impl fmt::Display for ParseOnionAddrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("expected an onion address of the form host:port")
    }
}

impl std::error::Error for ParseOnionAddrError {}

impl FromStr for OnionAddr {
    type Err = ParseOnionAddrError;

    /// Parse the `host:port` text form (the inverse of [`Display`]). The host
    /// may itself be empty of colons — the split takes the last colon — so the
    /// port is whatever follows the final `:`. This lets an address survive a
    /// round trip across a text boundary such as capnp `Text`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (host, port) = s.rsplit_once(':').ok_or(ParseOnionAddrError)?;
        if host.is_empty() {
            return Err(ParseOnionAddrError);
        }
        let port: u16 = port.parse().map_err(|_| ParseOnionAddrError)?;
        Ok(Self::new(host, port))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_parse_roundtrip() {
        let addr = OnionAddr::new("abcdefghij234567.onion", 9735);
        let parsed: OnionAddr = addr.to_string().parse().unwrap();
        assert_eq!(parsed, addr);
    }

    #[test]
    fn malformed_input_is_rejected() {
        // No colon at all.
        assert_eq!("noport".parse::<OnionAddr>(), Err(ParseOnionAddrError));
        // Empty host.
        assert_eq!(":9735".parse::<OnionAddr>(), Err(ParseOnionAddrError));
        // Non-numeric port.
        assert_eq!(
            "host:notaport".parse::<OnionAddr>(),
            Err(ParseOnionAddrError)
        );
        // Port out of u16 range.
        assert_eq!("host:70000".parse::<OnionAddr>(), Err(ParseOnionAddrError));
    }
}
