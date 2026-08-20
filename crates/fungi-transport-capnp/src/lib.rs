//! A Cap'n Proto plugin layer for the fungi transport crates.
//!
//! capnp-rpc is single-threaded and `!Send` (it is built on `Rc`), yet the
//! [`fungi_transport::Channel`] trait requires `send`/`recv` to return `Send`
//! futures. This crate bridges the two: [`CapnpChannel`] implements `Channel`
//! by owning a dedicated actor OS thread that runs the `!Send` capnp-rpc
//! client, and proxying each `send`/`recv` to it over a
//! [`tokio::sync::mpsc`] command channel plus a
//! [`oneshot`](tokio::sync::oneshot) reply. Because a command and its reply
//! are the only things that cross the trait boundary — both `Send` — the
//! returned futures are `Send`, while the capnp machinery stays confined to
//! its thread. This is the "Send bridge".
//!
//! The server end of the wire is provided by [`serve_loopback`] (a simple
//! FIFO queue) and [`serve_backend`] (wrapping an arbitrary `Channel`
//! backend so conformance can run through capnp-rpc).

use std::future::Future;

use capnp::capability::Promise;
use capnp_rpc::{RpcSystem, pry, rpc_twoparty_capnp::Side, twoparty};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use fungi_transport::{Channel, RecvError, SendError};

/// Generated code for `channel.capnp`.
mod channel_capnp {
    #![allow(clippy::all)]
    #![allow(missing_docs)]
    #![allow(dead_code)]
    #![allow(missing_debug_implementations)]
    include!(concat!(env!("OUT_DIR"), "/channel_capnp.rs"));
}
use channel_capnp::channel;

/// Depth of the command queue feeding the actor thread. Commands are small and
/// consumed one at a time; a little slack avoids needless wakeups.
const COMMAND_QUEUE_DEPTH: usize = 32;

/// A request sent from a [`CapnpChannel`] to its actor thread, carrying a
/// one-shot channel on which the actor returns the capnp call's result.
enum Command {
    Send {
        msg: Vec<u8>,
        reply: oneshot::Sender<Result<(), SendError>>,
    },
    Recv {
        reply: oneshot::Sender<Result<Vec<u8>, RecvError>>,
    },
}

/// A [`Channel`] whose messages are carried over capnp-rpc to a channel
/// server. The capnp-rpc client is `!Send`, so it runs on a private actor OS
/// thread; this handle only holds a `Send` command channel to it, which is
/// what lets `send`/`recv` return `Send` futures.
///
/// Dropping this handle drops both the command sender and the shutdown sender:
/// the actor loop breaks even when parked on an in-flight recv, tears down the
/// RPC client, and lets the thread finish.
#[derive(Debug)]
pub struct CapnpChannel {
    tx: mpsc::Sender<Command>,
    /// Held only to be dropped: when this `CapnpChannel` is dropped, the
    /// sender drops, resolving the actor's shutdown receiver so it can break
    /// out of an in-flight recv (a bare `mpsc` close is invisible while the
    /// actor is parked inside a backend call).
    _shutdown: oneshot::Sender<()>,
}

impl CapnpChannel {
    /// Connect a `CapnpChannel` over `io`, whose peer must run a channel
    /// server (see [`serve_loopback`]/[`serve_backend`]).
    ///
    /// This spawns a dedicated OS thread hosting a `current_thread` runtime and
    /// a [`LocalSet`](tokio::task::LocalSet); the thread drives the capnp-rpc
    /// client and services commands until this handle is dropped.
    pub fn connect<Io>(io: Io) -> Self
    where
        Io: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        let (tx, rx) = mpsc::channel(COMMAND_QUEUE_DEPTH);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        std::thread::spawn(move || run_client_actor(io, rx, shutdown_rx));
        Self {
            tx,
            _shutdown: shutdown_tx,
        }
    }
}

/// Build the `current_thread` runtime + `LocalSet` and run the client actor to
/// completion on this thread.
fn run_client_actor<Io>(io: Io, rx: mpsc::Receiver<Command>, shutdown: oneshot::Receiver<()>)
where
    Io: AsyncRead + AsyncWrite + Unpin + 'static,
{
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("building the capnp client runtime");
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, client_actor(io, rx, shutdown));
}

/// The client actor: bootstrap the remote [`channel::Client`], then translate
/// each [`Command`] into a capnp call and return its result on the one-shot.
///
/// An internal front-buffer keeps `recv` cancel-safe: a message pulled from the
/// backend whose caller has since dropped its reply is pushed back rather than
/// lost, so the next `recv` returns it. The shutdown receiver lets a drop of
/// the owning [`CapnpChannel`] break the loop even while parked in an in-flight
/// backend recv.
async fn client_actor<Io>(
    io: Io,
    mut rx: mpsc::Receiver<Command>,
    mut shutdown: oneshot::Receiver<()>,
) where
    Io: AsyncRead + AsyncWrite + Unpin + 'static,
{
    let (reader, writer) = tokio::io::split(io);
    let network = twoparty::VatNetwork::new(
        reader.compat(),
        writer.compat_write(),
        Side::Client,
        Default::default(),
    );
    let mut rpc_system = RpcSystem::new(Box::new(network), None);
    let channel: channel::Client = rpc_system.bootstrap(Side::Server);
    // Drive the RPC system alongside the command loop; it resolves (ending its
    // task) when the connection drops.
    tokio::task::spawn_local(async move {
        let _ = rpc_system.await;
    });

    // Messages pulled from the backend whose caller dropped the reply, held for
    // the next `recv` so no message is ever lost (cancel safety).
    let mut buffer: std::collections::VecDeque<Vec<u8>> = std::collections::VecDeque::new();

    loop {
        let cmd = tokio::select! {
            biased;
            _ = &mut shutdown => break,
            maybe = rx.recv() => match maybe {
                Some(cmd) => cmd,
                None => break,
            },
        };

        match cmd {
            Command::Send { msg, reply } => {
                let mut req = channel.send_request();
                req.get().set_msg(&msg);
                let send_promise = req.send().promise;
                let result = tokio::select! {
                    biased;
                    _ = &mut shutdown => break,
                    res = send_promise => res.map(|_| ()).map_err(map_send_error),
                };
                let _ = reply.send(result);
            }
            Command::Recv { reply } => {
                // Serve from the front-buffer first: a message already pulled
                // for an abandoned caller must reach the next one.
                if let Some(msg) = buffer.pop_front() {
                    if let Err(Ok(msg)) = reply.send(Ok(msg)) {
                        buffer.push_front(msg);
                    }
                    continue;
                }

                let recv_promise = channel.recv_request().send().promise;
                let result = tokio::select! {
                    biased;
                    _ = &mut shutdown => break,
                    res = recv_promise => match res {
                        Ok(response) => response
                            .get()
                            .and_then(|r| r.get_msg().map(|m| m.to_vec()))
                            .map_err(map_recv_error),
                        Err(e) => Err(map_recv_error(e)),
                    },
                };

                // If the caller dropped its reply, keep a successfully received
                // message for the next `recv` instead of discarding it. A
                // dropped error reply is simply discarded.
                if let Err(Ok(msg)) = reply.send(result) {
                    buffer.push_front(msg);
                }
            }
        }
    }
}

/// A capnp `Disconnected` maps to a closed channel; anything else is an opaque
/// transport error.
fn map_send_error(e: capnp::Error) -> SendError {
    if matches!(e.kind, capnp::ErrorKind::Disconnected) {
        SendError::Closed
    } else {
        SendError::Transport(e.to_string().into())
    }
}

/// See [`map_send_error`]; the same rule for the receive side.
fn map_recv_error(e: capnp::Error) -> RecvError {
    if matches!(e.kind, capnp::ErrorKind::Disconnected) {
        RecvError::Closed
    } else {
        RecvError::Transport(e.to_string().into())
    }
}

/// Server-side: a backend `Closed` becomes a capnp `Disconnected` so the
/// client reconstructs it as [`SendError::Closed`]; other failures stay
/// `Failed` (which the client maps to `Transport`).
fn backend_send_error_to_capnp(e: SendError) -> capnp::Error {
    match e {
        SendError::Closed => capnp::Error::disconnected("channel closed".into()),
        other => capnp::Error::failed(other.to_string()),
    }
}

/// See [`backend_send_error_to_capnp`]; the same rule for the receive side.
fn backend_recv_error_to_capnp(e: RecvError) -> capnp::Error {
    match e {
        RecvError::Closed => capnp::Error::disconnected("channel closed".into()),
        other => capnp::Error::failed(other.to_string()),
    }
}

impl Channel for CapnpChannel {
    fn send(&mut self, msg: &[u8]) -> impl Future<Output = Result<(), SendError>> + Send {
        let tx = self.tx.clone();
        let msg = msg.to_vec();
        async move {
            let (reply, rx) = oneshot::channel();
            tx.send(Command::Send { msg, reply })
                .await
                .map_err(|_| SendError::Closed)?;
            rx.await.map_err(|_| SendError::Closed)?
        }
    }

    fn recv(&mut self) -> impl Future<Output = Result<Vec<u8>, RecvError>> + Send {
        let tx = self.tx.clone();
        async move {
            let (reply, rx) = oneshot::channel();
            tx.send(Command::Recv { reply })
                .await
                .map_err(|_| RecvError::Closed)?;
            rx.await.map_err(|_| RecvError::Closed)?
        }
    }
}

/// A trivial in-memory FIFO queue implementing the capnp [`channel::Server`].
/// `recv` on an empty queue fails; a driving client is expected to `recv` only
/// what it has already `send`-ed (as the minimum conformance test does).
struct Loopback {
    queue: std::collections::VecDeque<Vec<u8>>,
}

impl channel::Server for Loopback {
    fn send(
        &mut self,
        params: channel::SendParams,
        _results: channel::SendResults,
    ) -> Promise<(), capnp::Error> {
        let msg = pry!(pry!(params.get()).get_msg()).to_vec();
        self.queue.push_back(msg);
        Promise::ok(())
    }

    fn recv(
        &mut self,
        _params: channel::RecvParams,
        mut results: channel::RecvResults,
    ) -> Promise<(), capnp::Error> {
        match self.queue.pop_front() {
            Some(msg) => {
                results.get().set_msg(&msg);
                Promise::ok(())
            }
            None => Promise::err(capnp::Error::failed("channel empty".into())),
        }
    }
}

/// A capnp [`channel::Server`] that forwards each call to an async
/// [`Channel`] backend.
///
/// The capnp `Server` methods return a `Promise` and cannot borrow an async
/// `&mut self` across an await point, so the backend lives behind an
/// `Rc<Mutex<..>>` shared into a per-call future. This keeps the whole thing
/// on the actor's single thread (the server is `!Send`) while still allowing
/// the backend's `send`/`recv` futures to suspend.
struct BackendServer<C: Channel> {
    backend: std::rc::Rc<tokio::sync::Mutex<C>>,
}

impl<C: Channel + 'static> channel::Server for BackendServer<C> {
    fn send(
        &mut self,
        params: channel::SendParams,
        _results: channel::SendResults,
    ) -> Promise<(), capnp::Error> {
        let msg = pry!(pry!(params.get()).get_msg()).to_vec();
        let backend = self.backend.clone();
        Promise::from_future(async move {
            backend
                .lock()
                .await
                .send(&msg)
                .await
                .map_err(backend_send_error_to_capnp)
        })
    }

    fn recv(
        &mut self,
        _params: channel::RecvParams,
        mut results: channel::RecvResults,
    ) -> Promise<(), capnp::Error> {
        let backend = self.backend.clone();
        Promise::from_future(async move {
            let msg = backend
                .lock()
                .await
                .recv()
                .await
                .map_err(backend_recv_error_to_capnp)?;
            results.get().set_msg(&msg);
            Ok(())
        })
    }
}

/// Serve a simple FIFO loopback channel over `io`. Everything `send`-ed is
/// queued and returned by later `recv` calls, in order. Pair with a
/// [`CapnpChannel::connect`] on the other end of `io`.
pub fn serve_loopback<Io>(io: Io) -> std::thread::JoinHandle<()>
where
    Io: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    // `new_client` is `!Send`, so build it on the server thread.
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("building the capnp server runtime");
        let local = tokio::task::LocalSet::new();
        local.block_on(&rt, async move {
            let (reader, writer) = tokio::io::split(io);
            let network = twoparty::VatNetwork::new(
                reader.compat(),
                writer.compat_write(),
                Side::Server,
                Default::default(),
            );
            let client: channel::Client = capnp_rpc::new_client(Loopback {
                queue: Default::default(),
            });
            let rpc_system = RpcSystem::new(Box::new(network), Some(client.client));
            let _ = rpc_system.await;
        });
    })
}

/// Serve a channel over `io` backed by an arbitrary [`Channel`], so real
/// conformance can run through capnp-rpc. Each capnp `send`/`recv` is
/// forwarded to `backend`. Pair with a [`CapnpChannel::connect`] on the other
/// end of `io`.
pub fn serve_backend<Io, C>(io: Io, backend: C) -> std::thread::JoinHandle<()>
where
    Io: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    C: Channel + 'static,
{
    // The backend and the capnp client are both `!Send` once wrapped in `Rc`,
    // so construct them on the server thread. `io` and `backend` cross the
    // thread boundary before that wrapping, and both are `Send`.
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("building the capnp server runtime");
        let local = tokio::task::LocalSet::new();
        local.block_on(&rt, async move {
            let (reader, writer) = tokio::io::split(io);
            let network = twoparty::VatNetwork::new(
                reader.compat(),
                writer.compat_write(),
                Side::Server,
                Default::default(),
            );
            let server = BackendServer {
                backend: std::rc::Rc::new(tokio::sync::Mutex::new(backend)),
            };
            let client: channel::Client = capnp_rpc::new_client(server);
            let rpc_system = RpcSystem::new(Box::new(network), Some(client.client));
            let _ = rpc_system.await;
        });
    })
}
