//! Proves the whole [`fungi_transport::Transport`] factory graph over
//! capnp-rpc, in-process: a [`CapnpTransport`] client wired to a
//! [`serve_plugin`] server wrapping an in-memory [`MemTransport`] backend.
//! `connector`/`listen`/`connect`/`accept`/`send`/`recv` all traverse capnp on
//! every hop, and the factory futures are shown `Send` at compile time.

use fungi_transport::mem::{MemAddr, MemConfig, MemTransport};
use fungi_transport::testkit;
use fungi_transport::{Channel, Connector, ListenParams, Listener, Transport};
use fungi_transport_capnp::{CapnpTransport, serve_plugin};

/// Compile-time proof that a value is `Send`.
fn assert_send<T: Send>(_: &T) {}

/// Wire a `CapnpTransport` client to a plugin server wrapping a fresh
/// `MemTransport`. The server thread handle is detached (it lives until its
/// stream closes) and dropped here.
fn wire() -> CapnpTransport<MemAddr> {
    // Capacity > 1 so queued sends do not block before the peer drains.
    let cfg = MemConfig {
        capacity: Some(8),
        ..MemConfig::default()
    };
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    serve_plugin(server_io, MemTransport::new(cfg));
    CapnpTransport::connect(client_io)
}

/// BASIC: build a connector and a listener through the graph, then connect and
/// accept concurrently and exchange a message in both directions — every hop
/// crossing capnp. Also proves the factory futures are `Send`.
#[tokio::test]
async fn factory_roundtrip_over_capnp() {
    let transport = wire();

    // The factory futures returned across the trait must be `Send`.
    assert_send(&transport.listen(ListenParams::new(1)));

    let connector = transport.connector();
    let (mut listener, addr) = transport.listen(ListenParams::new(1)).await.unwrap();

    assert_send(&connector);
    assert_send(&connector.connect(&addr));
    assert_send(&listener.accept());

    let (client, server) = tokio::join!(connector.connect(&addr), listener.accept());
    let (mut client, mut server) = (client.unwrap(), server.unwrap());

    client.send(b"ping").await.unwrap();
    assert_eq!(server.recv().await.unwrap(), b"ping");
    server.send(b"pong").await.unwrap();
    assert_eq!(client.recv().await.unwrap(), b"pong");
}

/// CONFORMANCE: run the connection-oriented lifecycle suite
/// (connect → accept → roundtrip → drop → detect → reconnect → re-accept →
/// roundtrip) through the whole capnp graph. `MemTransport::listen` is
/// single-use, so it is called ONCE and the helper accepts twice on the one
/// listener. This proves the factory graph is a faithful `Transport` over
/// capnp, not just a one-shot pipe.
#[tokio::test]
async fn connect_use_drop_reconnect_over_capnp() {
    // `_transport` is held for the whole test so the actor thread and the
    // stored `Transport` capability stay alive while its connector/listener are
    // exercised.
    let _transport = wire();
    let connector = _transport.connector();
    let (listener, addr) = _transport.listen(ListenParams::new(1)).await.unwrap();

    testkit::connect_use_drop_reconnect(connector, listener, &addr).await;
}
