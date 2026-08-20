//! Proves the plugin layer across a REAL process boundary: a [`connect_plugin`]
//! harness drives the `mem-plugin` child binary, which serves an in-memory
//! [`MemTransport`] over its own stdin/stdout. `connector`/`listen`/`connect`/
//! `accept`/`send`/`recv` all traverse capnp-rpc over the child's pipes, and the
//! whole mem network lives inside the child.

use std::time::Duration;

use fungi_transport::mem::MemAddr;
use fungi_transport::testkit;
use fungi_transport::{Channel, Connector, ListenParams, Listener, Transport};
use fungi_transport_capnp::{CapnpTransport, connect_plugin};

/// The child plugin binary, built by cargo before this integration test.
const MEM_PLUGIN: &str = env!("CARGO_BIN_EXE_mem-plugin");

/// Spawn the `mem-plugin` child and connect a `CapnpTransport` to it over the
/// child's stdio.
fn wire() -> CapnpTransport<MemAddr> {
    connect_plugin(tokio::process::Command::new(MEM_PLUGIN))
}

/// BASIC: build a connector and a listener through the subprocess plugin, then
/// connect and accept concurrently and exchange a message in both directions —
/// every hop crossing capnp over the child's pipes.
#[tokio::test]
async fn subprocess_roundtrip() {
    let transport = wire();

    let connector = transport.connector();
    let (mut listener, addr) = transport.listen(ListenParams::new(1)).await.unwrap();

    let (client, server) = tokio::join!(connector.connect(&addr), listener.accept());
    let (mut client, mut server) = (client.unwrap(), server.unwrap());

    client.send(b"ping").await.unwrap();
    assert_eq!(server.recv().await.unwrap(), b"ping");
    server.send(b"pong").await.unwrap();
    assert_eq!(client.recv().await.unwrap(), b"pong");
}

/// CONFORMANCE: run the connection-oriented lifecycle suite through the
/// subprocess plugin (connect → accept → roundtrip → drop → detect → reconnect
/// → re-accept → roundtrip). `MemTransport::listen` is single-use, so it is
/// called ONCE and the helper accepts twice on the one listener.
#[tokio::test]
async fn subprocess_conformance() {
    // `_transport` is held for the whole test so the actor thread and the child
    // process stay alive while its connector/listener are exercised.
    let _transport = wire();
    let connector = _transport.connector();
    let (listener, addr) = _transport.listen(ListenParams::new(1)).await.unwrap();

    testkit::connect_use_drop_reconnect(connector, listener, &addr).await;
}

/// LIFECYCLE: a plugin that dies before the capnp handshake must surface as
/// `ConnectError::Unreachable` on the first transport operation — not a hang and
/// not a panic. The `mem-plugin` exits immediately when given any argument.
#[tokio::test]
async fn plugin_crash_is_unreachable() {
    let mut command = tokio::process::Command::new(MEM_PLUGIN);
    command.arg("crash");
    let transport: CapnpTransport<MemAddr> = connect_plugin(command);

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        transport.listen(ListenParams::new(1)),
    )
    .await
    .expect("a crashed plugin must be detected, not hang");

    assert!(
        matches!(result, Err(fungi_transport::ConnectError::Unreachable)),
        "expected Unreachable, got {result:?}"
    );
}

/// LIFECYCLE: a plugin program that cannot even be spawned (non-existent path)
/// must also surface as `ConnectError::Unreachable` on the first transport
/// operation — exercising the `command.spawn()`-`Err` path, distinct from the
/// EOF-after-spawn crash above.
#[tokio::test]
async fn spawn_failure_is_unreachable() {
    let command = tokio::process::Command::new("/nonexistent/fungi-mem-plugin-does-not-exist-xyz");
    let transport: CapnpTransport<MemAddr> = connect_plugin(command);

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        transport.listen(ListenParams::new(1)),
    )
    .await
    .expect("an unspawnable plugin must be detected, not hang");

    assert!(
        matches!(result, Err(fungi_transport::ConnectError::Unreachable)),
        "expected Unreachable, got {result:?}"
    );
}
