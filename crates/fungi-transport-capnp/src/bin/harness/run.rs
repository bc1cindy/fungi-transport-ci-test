//! listen/dial driver: spawns a backend plugin subprocess and drives it over
//! the `Transport` trait via capnp-rpc. The generic channel-driving primitives
//! live in `fungi_transport::harness`; this binary adds the CLI and the stdout
//! ONION/READY/OK protocol the NixOS VM test greps for. It is backend-agnostic:
//! every backend is reached the same way, through its plugin binary.

use std::time::Duration;

use fungi_transport::harness::{dial_sequence, echo_one_peer};
use fungi_transport::{Connector, ListenParams, Listener, OnionAddr, Transport};

/// Bounded wait for every network step: the VM test must fail, not hang.
pub(crate) const STEP_TIMEOUT: Duration = Duration::from_secs(300);

/// Bounded wait for `accept()`: must cover the dialer's full retry budget
/// in the VM test (the dial side's `wait_until_succeeds` retries for up to
/// 900s while the listener sits in `accept()`), so this is wider than
/// [`STEP_TIMEOUT`].
pub(crate) const ACCEPT_TIMEOUT: Duration = Duration::from_secs(900);

/// Parsed CLI.
pub(crate) struct Cli {
    pub(crate) cmd: Cmd,
    pub(crate) private_net: Option<std::path::PathBuf>,
    pub(crate) state_dir: Option<std::path::PathBuf>,
    /// The plugin binary to drive: the harness spawns it and speaks capnp-rpc
    /// to it over its stdio. Every backend is reached this way, so this is
    /// required.
    pub(crate) plugin: std::path::PathBuf,
}

pub(crate) enum Cmd {
    Listen { virt_port: u16 },
    Dial { target: OnionAddr },
}

pub(crate) fn parse_args(args: Vec<String>) -> Result<Cli, String> {
    let mut it = args.into_iter().skip(1);
    let cmd_word = it.next().ok_or("usage: fungi-harness listen|dial ...")?;
    let mut private_net = None;
    let mut virt_port = None;
    let mut target = None;
    let mut plugin = None;
    let mut state_dir = None;
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--private-net" => {
                private_net = Some(it.next().ok_or("--private-net needs a file")?.into())
            }
            "--virt-port" => {
                virt_port = Some(
                    it.next()
                        .ok_or("--virt-port needs a number")?
                        .parse::<u16>()
                        .map_err(|e| e.to_string())?,
                )
            }
            "--state-dir" => state_dir = Some(it.next().ok_or("--state-dir needs a path")?.into()),
            "--plugin" => plugin = Some(it.next().ok_or("--plugin needs a path")?.into()),
            other if !other.starts_with("--") && target.is_none() => {
                let (host, port) = other.rsplit_once(':').ok_or("target must be host:port")?;
                target = Some(OnionAddr::new(
                    host,
                    port.parse()
                        .map_err(|e: std::num::ParseIntError| e.to_string())?,
                ));
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let plugin = plugin.ok_or("--plugin is required")?;
    let cmd = match cmd_word.as_str() {
        "listen" => Cmd::Listen {
            virt_port: virt_port.ok_or("listen needs --virt-port")?,
        },
        "dial" => Cmd::Dial {
            target: target.ok_or("dial needs a host:port target")?,
        },
        other => return Err(format!("unknown subcommand {other}")),
    };
    Ok(Cli {
        cmd,
        private_net,
        state_dir,
        plugin,
    })
}

/// Listen over any backend: publish, accept one peer, echo until it closes.
/// Establishment awaits are bounded by `STEP_TIMEOUT`; `accept()` by the wider
/// `ACCEPT_TIMEOUT`.
pub(crate) async fn run_listen<T>(transport: T, params: ListenParams) -> Result<(), String>
where
    T: Transport,
    T::Addr: std::fmt::Display,
{
    let (mut listener, addr) = tokio::time::timeout(STEP_TIMEOUT, transport.listen(params))
        .await
        .map_err(|_| "listen timed out".to_string())?
        .map_err(|e| e.to_string())?;
    println!("ONION={addr}");
    println!("READY");
    let ch = tokio::time::timeout(ACCEPT_TIMEOUT, listener.accept())
        .await
        .map_err(|_| "accept timed out".to_string())?
        .map_err(|e| e.to_string())?;
    tokio::time::timeout(STEP_TIMEOUT, echo_one_peer(ch))
        .await
        .map_err(|_| "echo/dial phase timed out".to_string())?
}

/// Dial one peer over any connector: connect, run the message sequence.
pub(crate) async fn run_dial<Co>(connector: Co, target: &Co::Addr) -> Result<(), String>
where
    Co: Connector,
{
    let ch = tokio::time::timeout(STEP_TIMEOUT, connector.connect(target))
        .await
        .map_err(|_| "connect timed out".to_string())?
        .map_err(|e| e.to_string())?;
    tokio::time::timeout(STEP_TIMEOUT, dial_sequence(ch))
        .await
        .map_err(|_| "echo/dial phase timed out".to_string())??;
    println!("OK");
    Ok(())
}

/// Drive the parsed command over a plugin subprocess: spawn `cli.plugin` and
/// speak capnp-rpc to it (via
/// [`connect_plugin`](fungi_transport_capnp::connect_plugin)), then run the
/// generic [`run_listen`]/[`run_dial`]. Establishment awaits
/// (listen/connect/bootstrap) are bounded by [`STEP_TIMEOUT`]; `accept()` is
/// bounded by the wider [`ACCEPT_TIMEOUT`] so the listener outlives the dialer's
/// retry budget in the VM test; the data phase is bounded by [`STEP_TIMEOUT`].
///
/// Backend configuration reaches the plugin through its environment, set here
/// from the harness's generic flags. Each backend reads only the env it needs
/// and ignores the rest:
///
/// - `FUNGI_STATE_DIR`/`FUNGI_CACHE_DIR` (from `--state-dir`, when given): arti
///   reads these for its persistent state and directory cache; socks5h ignores
///   them.
/// - `FUNGI_PRIVATE_NET` (from `--private-net`, when given): arti applies the
///   private-net descriptor before it bootstraps, so the private test network is
///   installed at plugin startup — this is what replaces the capnp
///   `TestFixtures` private-net lifecycle for the plugin topology. socks5h
///   ignores it.
pub(crate) async fn run(cli: Cli) -> Result<(), String> {
    use fungi_transport::OnionAddr;
    use fungi_transport_capnp::{CapnpTransport, connect_plugin};

    let mut command = tokio::process::Command::new(&cli.plugin);
    if let Some(dir) = &cli.state_dir {
        command.env("FUNGI_STATE_DIR", dir.join("state"));
        command.env("FUNGI_CACHE_DIR", dir.join("cache"));
    }
    if let Some(path) = &cli.private_net {
        command.env("FUNGI_PRIVATE_NET", path);
    }

    let transport: CapnpTransport<OnionAddr> = connect_plugin(command);
    match &cli.cmd {
        Cmd::Listen { virt_port } => {
            run_listen(
                transport,
                ListenParams::new(*virt_port).with_nickname("fungie2e"),
            )
            .await
        }
        Cmd::Dial { target } => run_dial(transport.connector(), target).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn run_listen_and_run_dial_roundtrip_over_mem() {
        use fungi_transport::mem::{MemAddr, MemConfig, MemTransport};
        use fungi_transport::{ListenParams, Transport};
        let transport = MemTransport::new(MemConfig {
            capacity: Some(16),
            ..MemConfig::default()
        });
        let connector = transport.connector();
        let listen = tokio::spawn(run_listen(transport, ListenParams::new(1)));
        // The dialer connects, runs the sequence, then drops — the echo side sees
        // the close and run_listen returns Ok.
        run_dial(connector, &MemAddr).await.unwrap();
        listen.await.unwrap().unwrap();
    }

    /// `--plugin <path>` is required: without it the parse fails.
    #[test]
    fn cli_parsing_rejects_missing_plugin() {
        assert!(
            parse_args(
                ["fungi-harness", "listen", "--virt-port", "1"]
                    .map(String::from)
                    .to_vec()
            )
            .is_err()
        );
    }

    /// `--plugin <path>` populates the plugin field with the binary to drive.
    #[test]
    fn cli_parsing_accepts_plugin_path() {
        let cli = parse_args(
            [
                "fungi-harness",
                "listen",
                "--plugin",
                "/nix/store/xxx/bin/fungi-arti-plugin",
                "--virt-port",
                "9735",
            ]
            .map(String::from)
            .to_vec(),
        )
        .unwrap();
        assert_eq!(
            cli.plugin,
            std::path::Path::new("/nix/store/xxx/bin/fungi-arti-plugin")
        );
    }

    /// The generic flags parse without a backend selector: `--state-dir` and
    /// `--private-net` are backend-agnostic and reach the plugin via env.
    #[test]
    fn cli_parsing_accepts_generic_flags() {
        let cli = parse_args(
            [
                "fungi-harness",
                "dial",
                "--plugin",
                "/nix/store/xxx/bin/fungi-socks5h-plugin",
                "--private-net",
                "/tmp/private-net",
                "--state-dir",
                "/tmp/arti-dial",
                "host.onion:9735",
            ]
            .map(String::from)
            .to_vec(),
        )
        .unwrap();
        assert_eq!(
            cli.private_net.as_deref(),
            Some(std::path::Path::new("/tmp/private-net"))
        );
        assert_eq!(
            cli.state_dir.as_deref(),
            Some(std::path::Path::new("/tmp/arti-dial"))
        );
    }

    /// The harness's generic `run_listen`/`run_dial` drive the SAME
    /// [`CapnpTransport`](fungi_transport_capnp::CapnpTransport) handle that the
    /// real plugin path returns from `connect_plugin`. Here the plugin server is
    /// an in-process `serve_plugin(MemTransport)` reached over a duplex rather
    /// than a subprocess's stdio; the subprocess topology itself is covered by
    /// the capnp crate's `connect_plugin` subprocess tests. This proves the
    /// harness's drive path works over the capnp transport handle end to end,
    /// locally.
    #[tokio::test]
    async fn run_listen_and_run_dial_over_capnp_plugin() {
        use fungi_transport::ListenParams;
        use fungi_transport::mem::{MemAddr, MemConfig, MemTransport};
        use fungi_transport_capnp::{CapnpTransport, serve_plugin};

        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        // `serve_plugin` is `!Send`; drive it on its own current-thread runtime.
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("building the server runtime");
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, async move {
                let (reader, writer) = tokio::io::split(server_io);
                let cfg = MemConfig {
                    capacity: Some(16),
                    ..MemConfig::default()
                };
                serve_plugin(MemTransport::new(cfg), reader, writer).await;
            });
        });

        let transport: CapnpTransport<MemAddr> = CapnpTransport::connect(client_io);
        let connector = transport.connector();
        let listen = tokio::spawn(run_listen(transport, ListenParams::new(1)));
        // The dialer connects, runs the sequence, then drops — the echo side sees
        // the close and run_listen returns Ok.
        run_dial(connector, &MemAddr).await.unwrap();
        listen.await.unwrap().unwrap();
    }
}
