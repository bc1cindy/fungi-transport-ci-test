@0xfd90a0bb844c5a05;

# The plugin protocol, mirroring the fungi-transport Rust traits over capnp-rpc.
# Only the Channel level is implemented in this phase; the remaining interfaces
# fix the wire shape the plugin layer will grow into.

# One connected peer: opaque bytes, one message per call. Mirrors the Rust
# `Channel` trait (method 0 = send, method 1 = recv).
interface Channel {
  send @0 (msg :Data) -> ();
  recv @1 () -> (msg :Data);
}

# Opens new channels to peers, for connection-oriented transports.
interface Connector {
  connect @0 (addr :Text) -> (channel :Channel);
}

# Accepts inbound channels and exposes the published onion address.
interface Listener {
  accept @0 () -> (channel :Channel);
  onionAddr @1 () -> (addr :Text);
}

# Transport factory: opens connectors and creates listeners.
interface Transport {
  connector @0 () -> (connector :Connector);
  listen @1 (virtPort :UInt16, nickname :Text) -> (listener :Listener);
}

# Test-only hooks for driving a private network in conformance/e2e runs.
interface TestFixtures {
  configurePrivateNet @0 (netFile :Data) -> ();
  reset @1 () -> ();
}

# Plugin entry point: the bootstrap capability exposed over the wire.
interface Plugin {
  transport @0 () -> (transport :Transport);
  fixtures @1 () -> (fixtures :TestFixtures);
}
