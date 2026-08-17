//! Real-network smoke test. NOT run in CI.
//!
//! Run manually with internet access:
//!     cargo test -p fungi-transport-arti --test smoke -- --ignored --nocapture
//!
//! Bootstraps onto the real Tor network, publishes a throwaway onion
//! service, connects to itself, and runs the conformance roundtrip.
//! Takes minutes (circuit cold start + onion-service publication).

use std::time::Duration;

use fungi_transport::{ConnectError, Connector as _, Listener as _, testkit};
use fungi_transport_arti::{ArtiConfig, ArtiTransport};

/// Bounded retries for the initial self-connect: `listen()` returns once the
/// onion service is *created*, but its descriptor upload to the network can
/// take anywhere from seconds to ~a minute. Retry only on `Unreachable`
/// (descriptor not yet published) — any other error is a real failure.
const CONNECT_ATTEMPTS: u32 = 18;
const CONNECT_RETRY_DELAY: Duration = Duration::from_secs(10);

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires internet access and several minutes; run manually"]
async fn bootstrap_publish_self_connect_roundtrip() {
    // Two rustls crypto-provider backends (ring, aws-lc-rs) can both be
    // enabled in the test binary's dependency graph, in which case rustls
    // cannot auto-install a default and `bootstrap` panics. Install one
    // explicitly, matching the unit tests in src/transport.rs.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let base = std::env::temp_dir().join(format!("fungi-arti-smoke-{}", std::process::id()));
    let cfg = ArtiConfig::new(base.join("state"), base.join("cache"));

    let transport = ArtiTransport::bootstrap(cfg).await.expect("bootstrap");
    let mut listener = transport.listen("fungi-smoke", 9735).await.expect("listen");
    let addr = listener.onion_addr().clone();
    eprintln!("onion service published at {addr}; connecting to self...");

    let connector = transport.connector();
    let connect_addr = addr.clone();
    let connect_task = tokio::spawn(async move {
        for attempt in 1..=CONNECT_ATTEMPTS {
            eprintln!("connect attempt {attempt}/{CONNECT_ATTEMPTS}...");
            match connector.connect(&connect_addr).await {
                Ok(channel) => return channel,
                Err(ConnectError::Unreachable) => {
                    eprintln!(
                        "descriptor not yet reachable; retrying in {}s",
                        CONNECT_RETRY_DELAY.as_secs()
                    );
                    tokio::time::sleep(CONNECT_RETRY_DELAY).await;
                }
                Err(e) => panic!("connect failed with non-retryable error: {e}"),
            }
        }
        panic!(
            "self-connect did not succeed after {CONNECT_ATTEMPTS} attempts \
             ({}s total); onion descriptor likely never published",
            CONNECT_ATTEMPTS as u64 * CONNECT_RETRY_DELAY.as_secs()
        );
    });

    let (outbound, inbound) = tokio::join!(connect_task, listener.accept());
    let outbound = outbound.expect("connect task panicked");
    testkit::roundtrip_both_directions(outbound, inbound.expect("accept")).await;
    eprintln!("roundtrip ok");

    drop(listener); // service dies with the listener
    let _ = std::fs::remove_dir_all(base);
}
