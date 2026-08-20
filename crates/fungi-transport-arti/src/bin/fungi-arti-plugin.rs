//! A capnp plugin process backed by the in-process arti Tor client.
//!
//! It bootstraps onto a Tor network and then speaks the server side of the
//! plugin protocol over its own stdin/stdout, so a harness can drive it through
//! [`connect_plugin`](fungi_transport_capnp::connect_plugin). Its configuration
//! is taken entirely from the environment at startup, defaulting under the
//! system temp dir:
//!
//! - `FUNGI_STATE_DIR` — arti's persistent state, including onion-service keys
//!   (identity persists per nickname; a throwaway dir gives an ephemeral
//!   identity).
//! - `FUNGI_CACHE_DIR` — arti's network-directory cache.
//! - `FUNGI_PRIVATE_NET` — optional path to a private-network descriptor
//!   ([`PrivateNet`](fungi_transport_arti::PrivateNet)). When set, its
//!   authorities + fallback caches are applied to the config **before**
//!   bootstrap, so the plugin joins a private test network instead of the public
//!   Tor directory. Delivering the private net through the environment at
//!   startup is what replaces the capnp `TestFixtures.configurePrivateNet`
//!   lifecycle for the plugin topology (the exposed fixtures stay `NoopFixtures`):
//!   arti's directory authorities must be fixed before the one bootstrap, not
//!   reconfigured on a live client.
//!
//! Bootstrap needs a live Tor network, so this binary has no deterministic
//! test; it is exercised end to end by the NixOS VM suite. What is asserted
//! here is that it compiles and is a valid plugin server.

use std::path::PathBuf;

use fungi_transport::framed::DEFAULT_MAX_MSG_LEN;
use fungi_transport_arti::{ArtiConfig, ArtiTransport, PrivateNet};
use fungi_transport_capnp::serve_plugin;

/// Read a directory path from `var`, falling back to `<temp>/default_leaf`.
fn env_dir(var: &str, default_leaf: &str) -> PathBuf {
    match std::env::var_os(var) {
        Some(value) => PathBuf::from(value),
        None => std::env::temp_dir().join(default_leaf),
    }
}

fn main() {
    let state_dir = env_dir("FUNGI_STATE_DIR", "fungi-arti-state");
    let cache_dir = env_dir("FUNGI_CACHE_DIR", "fungi-arti-cache");
    let private_net = std::env::var_os("FUNGI_PRIVATE_NET").map(PathBuf::from);

    // capnp-rpc is `!Send`; drive it (and the bootstrap) on a current-thread
    // runtime under a `LocalSet`, as every plugin server must.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("building the plugin runtime");
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async {
        // Install a crypto provider explicitly: auto-install only fires when a
        // single provider is in the graph, so make the choice unambiguous.
        let _ = rustls::crypto::ring::default_provider().install_default();

        let transport = match private_net {
            // Private test network: apply the descriptor's authorities +
            // fallback caches before the single bootstrap.
            Some(path) => {
                let text = std::fs::read_to_string(&path)
                    .expect("reading the FUNGI_PRIVATE_NET descriptor");
                let tor_cfg = PrivateNet::parse(&text)
                    .expect("parsing the private-net descriptor")
                    .build_config(&state_dir, &cache_dir)
                    .expect("building the private-net arti config");
                ArtiTransport::bootstrap_with(tor_cfg, DEFAULT_MAX_MSG_LEN)
                    .await
                    .expect("bootstrapping arti onto the private Tor network")
            }
            // Stock config: bootstrap onto the public Tor network.
            None => ArtiTransport::bootstrap(ArtiConfig::new(state_dir, cache_dir))
                .await
                .expect("bootstrapping arti onto the Tor network"),
        };
        serve_plugin(transport, tokio::io::stdin(), tokio::io::stdout()).await;
    });
}
