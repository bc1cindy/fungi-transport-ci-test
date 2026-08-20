//! listen/dial driver: constructs a backend and drives it over the
//! `Transport` trait. The generic channel-driving primitives live in
//! `fungi_transport::harness`; this binary adds the CLI and the
//! stdout ONION/READY/OK protocol the NixOS VM test greps for.

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
    pub(crate) backend: BackendKind,
    pub(crate) private_net: Option<std::path::PathBuf>,
    pub(crate) state_dir: std::path::PathBuf,
}

pub(crate) enum Cmd {
    Listen { virt_port: u16 },
    Dial { target: OnionAddr },
}

pub(crate) enum BackendKind {
    Socks5h,
    Arti,
}

pub(crate) fn parse_args(args: Vec<String>) -> Result<Cli, String> {
    let mut it = args.into_iter().skip(1);
    let cmd_word = it.next().ok_or("usage: fungi-e2e listen|dial ...")?;
    let mut backend = None;
    let mut private_net = None;
    let mut virt_port = None;
    let mut target = None;
    let mut state_dir = std::env::temp_dir().join("fungi-e2e");
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--backend" => {
                backend = Some(match it.next().as_deref() {
                    Some("socks5h") => BackendKind::Socks5h,
                    Some("arti") => BackendKind::Arti,
                    other => return Err(format!("--backend socks5h|arti, got {other:?}")),
                })
            }
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
            "--state-dir" => state_dir = it.next().ok_or("--state-dir needs a path")?.into(),
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
    let backend = backend.ok_or("--backend is required")?;
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
        backend,
        private_net,
        state_dir,
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

/// Run the parsed command over the chosen backend. Establishment awaits
/// (bind/listen/connect/bootstrap) are bounded by [`STEP_TIMEOUT`];
/// `accept()` is bounded by the wider [`ACCEPT_TIMEOUT`] so the listener
/// outlives the dialer's retry budget in the VM test; the data phase
/// (`echo_one_peer`/`dial_sequence`) is bounded by [`STEP_TIMEOUT`] at
/// each call site below.
pub(crate) async fn run(cli: Cli) -> Result<(), String> {
    match cli.backend {
        BackendKind::Socks5h => {
            use fungi_transport_socks5h::{TorConfig, TorTransport};
            let transport = TorTransport::new(TorConfig::default());
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
        BackendKind::Arti => {
            let transport = arti_transport(&cli).await?;
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
    }
}

async fn arti_transport(cli: &Cli) -> Result<fungi_transport_arti::ArtiTransport, String> {
    use fungi_transport_arti::{ArtiConfig, ArtiTransport};
    // Both rustls provider crates are in this binary's graph; install one.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut cfg_builder = arti_client::config::TorClientConfigBuilder::from_directories(
        cli.state_dir.join("state"),
        cli.state_dir.join("cache"),
    );
    if let Some(path) = &cli.private_net {
        let text = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| e.to_string())?;
        crate::net::PrivateNet::parse(&text)?.apply(&mut cfg_builder)?;
        let tor_cfg = cfg_builder.build().map_err(|e| e.to_string())?;
        tokio::time::timeout(
            STEP_TIMEOUT,
            ArtiTransport::bootstrap_with(tor_cfg, fungi_transport::framed::DEFAULT_MAX_MSG_LEN),
        )
        .await
        .map_err(|_| "bootstrap timed out".to_owned())?
        .map_err(|e| e.to_string())
    } else {
        tokio::time::timeout(
            STEP_TIMEOUT,
            ArtiTransport::bootstrap(ArtiConfig::new(
                cli.state_dir.join("state"),
                cli.state_dir.join("cache"),
            )),
        )
        .await
        .map_err(|_| "bootstrap timed out".to_owned())?
        .map_err(|e| e.to_string())
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

    #[test]
    fn cli_parsing_rejects_missing_backend() {
        assert!(
            parse_args(
                ["fungi-e2e", "listen", "--virt-port", "1"]
                    .map(String::from)
                    .to_vec()
            )
            .is_err()
        );
    }
}
