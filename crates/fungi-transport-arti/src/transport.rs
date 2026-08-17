//! Shared arti bootstrap: one [`TorClient`] per peer, handed to both the
//! connector and listener halves.
//!
//! Bootstrap costs a Tor circuit cold start (~26 round trips, seconds) —
//! never pay it twice. No internal deadlines anywhere in this crate: callers
//! own timeouts (`tokio::time::timeout`); cancelling by drop is safe and
//! discards the attempt.

use std::path::PathBuf;
use std::sync::Arc;

use arti_client::config::TorClientConfigBuilder;
use arti_client::{TorClient, TorClientConfig};
use fungi_transport::ConnectError;
use fungi_transport::framed::DEFAULT_MAX_MSG_LEN;
use tor_rtcompat::PreferredRuntime;

use crate::error::connect_error;

/// Knobs for the in-process arti backend.
#[derive(Debug, Clone)]
pub struct ArtiConfig {
    /// Directory for arti's persistent state (incl. onion-service keys —
    /// identity persists per nickname; point at a throwaway dir for an
    /// ephemeral identity).
    pub state_dir: PathBuf,
    /// Directory for arti's network-directory cache.
    pub cache_dir: PathBuf,
    /// Maximum framed message size, both directions.
    pub max_msg_len: usize,
}

impl ArtiConfig {
    /// A config with the default message-size limit.
    pub fn new(state_dir: impl Into<PathBuf>, cache_dir: impl Into<PathBuf>) -> Self {
        Self {
            state_dir: state_dir.into(),
            cache_dir: cache_dir.into(),
            max_msg_len: DEFAULT_MAX_MSG_LEN,
        }
    }
}

/// Map an [`ArtiConfig`] onto arti's own config type.
pub(crate) fn tor_config(cfg: &ArtiConfig) -> Result<TorClientConfig, ConnectError> {
    TorClientConfigBuilder::from_directories(&cfg.state_dir, &cfg.cache_dir)
        .build()
        .map_err(|e| ConnectError::Transport(e.into()))
}

/// A bootstrapped in-process Tor client, source of both channel halves.
pub struct ArtiTransport {
    pub(crate) client: Arc<TorClient<PreferredRuntime>>,
    pub(crate) max_msg_len: usize,
}

impl std::fmt::Debug for ArtiTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArtiTransport")
            .field("max_msg_len", &self.max_msg_len)
            .finish_non_exhaustive()
    }
}

impl ArtiTransport {
    /// Bootstrap onto the Tor network. Expensive (seconds); do it once per
    /// peer and share the result.
    ///
    /// Requires a rustls `CryptoProvider` to be installed as the process
    /// default. rustls normally auto-installs one the first time it's
    /// needed, but auto-install only works when exactly one crypto-provider
    /// feature (`ring` or `aws-lc-rs`) is enabled across the dependency
    /// graph. If a consumer's other dependencies pull in both, rustls
    /// cannot pick one and this function panics with "CryptoProvider not
    /// installed". The fix is to install one explicitly before calling
    /// `bootstrap`, e.g. `rustls::crypto::ring::default_provider().install_default()`
    /// (or the `aws_lc_rs` equivalent).
    pub async fn bootstrap(cfg: ArtiConfig) -> Result<Self, ConnectError> {
        Self::bootstrap_with(tor_config(&cfg)?, cfg.max_msg_len).await
    }

    /// Bootstrap with a caller-built [`TorClientConfig`] (e.g. a private
    /// test network's authorities); `max_msg_len` as in [`ArtiConfig`].
    pub async fn bootstrap_with(
        tor_cfg: TorClientConfig,
        max_msg_len: usize,
    ) -> Result<Self, ConnectError> {
        let client = TorClient::create_bootstrapped(tor_cfg)
            .await
            .map_err(connect_error)?;
        Ok(Self::from_client(client, max_msg_len))
    }

    /// Wrap an existing client (deterministic tests use an unbootstrapped
    /// Manual client here).
    pub(crate) fn from_client(
        client: Arc<TorClient<PreferredRuntime>>,
        max_msg_len: usize,
    ) -> Self {
        Self {
            client,
            max_msg_len,
        }
    }

    /// The connector half, sharing this transport's client.
    pub fn connector(&self) -> crate::ArtiConnector {
        crate::ArtiConnector {
            client: self.client.clone(),
            max_msg_len: self.max_msg_len,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fungi_transport::framed::DEFAULT_MAX_MSG_LEN;

    fn tmp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("fungi-arti-{}-{}", name, std::process::id()))
    }

    #[test]
    fn config_defaults_max_msg_len() {
        let cfg = ArtiConfig::new(tmp("s1"), tmp("c1"));
        assert_eq!(cfg.max_msg_len, DEFAULT_MAX_MSG_LEN);
    }

    #[test]
    fn tor_config_builds_from_directories() {
        let cfg = ArtiConfig::new(tmp("s2"), tmp("c2"));
        assert!(tor_config(&cfg).is_ok());
    }

    /// No network: an unbootstrapped Manual client can be constructed, which
    /// is the seam the deterministic connector tests use.
    #[tokio::test]
    async fn unbootstrapped_client_constructs_without_network() {
        use arti_client::{BootstrapBehavior, TorClient};
        use rustls::crypto::ring::default_provider;

        let _ = default_provider().install_default();
        let cfg = ArtiConfig::new(tmp("s3"), tmp("c3"));
        let client = TorClient::builder()
            .config(tor_config(&cfg).unwrap())
            .bootstrap_behavior(BootstrapBehavior::Manual)
            .create_unbootstrapped();
        assert!(client.is_ok());
        let transport = ArtiTransport::from_client(client.unwrap(), cfg.max_msg_len);
        let _ = format!("{transport:?}");
    }
}
