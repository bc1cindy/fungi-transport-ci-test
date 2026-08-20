//! A capnp plugin process backed by the in-process arti Tor client.
//!
//! It bootstraps onto the Tor network and then speaks the server side of the
//! plugin protocol over its own stdin/stdout, so a harness can drive it through
//! [`connect_plugin`](fungi_transport_capnp::connect_plugin). Its persistent
//! directories are taken from the environment, defaulting under the system
//! temp dir:
//!
//! - `FUNGI_ARTI_STATE_DIR` — arti's persistent state, including onion-service
//!   keys (identity persists per nickname; a throwaway dir gives an ephemeral
//!   identity).
//! - `FUNGI_ARTI_CACHE_DIR` — arti's network-directory cache.
//!
//! Bootstrap needs a live Tor network, so this binary has no deterministic
//! test; it is exercised end to end by the NixOS VM suite. What is asserted
//! here is that it compiles and is a valid plugin server.

use std::path::PathBuf;

use fungi_transport_arti::{ArtiConfig, ArtiTransport};
use fungi_transport_capnp::serve_plugin;

/// Read a directory path from `var`, falling back to `<temp>/default_leaf`.
fn env_dir(var: &str, default_leaf: &str) -> PathBuf {
    match std::env::var_os(var) {
        Some(value) => PathBuf::from(value),
        None => std::env::temp_dir().join(default_leaf),
    }
}

fn main() {
    let cfg = ArtiConfig::new(
        env_dir("FUNGI_ARTI_STATE_DIR", "fungi-arti-state"),
        env_dir("FUNGI_ARTI_CACHE_DIR", "fungi-arti-cache"),
    );

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

        let transport = ArtiTransport::bootstrap(cfg)
            .await
            .expect("bootstrapping arti onto the Tor network");
        serve_plugin(transport, tokio::io::stdin(), tokio::io::stdout()).await;
    });
}
