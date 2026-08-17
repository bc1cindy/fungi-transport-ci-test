//! listen/dial driver logic. The message-sequence and echo cores are
//! generic over the Channel trait so the mem mock tests them in CI; the
//! backend wiring is exercised only inside the NixOS VMs.

use std::time::Duration;

use fungi_transport::{Channel, Connector, Listener, OnionAddr};

/// Bounded wait for every network step: the VM test must fail, not hang.
pub(crate) const STEP_TIMEOUT: Duration = Duration::from_secs(300);

/// Bounded wait for `accept()`: must cover the dialer's full retry budget
/// in the VM test (the dial side's `wait_until_succeeds` retries for up to
/// 900s while the listener sits in `accept()`), so this is wider than
/// [`STEP_TIMEOUT`].
pub(crate) const ACCEPT_TIMEOUT: Duration = Duration::from_secs(900);

/// The dial side's message sequence: distinct sizes incl. empty and multi-KiB.
fn sequence() -> Vec<Vec<u8>> {
    vec![
        b"hello over tor".to_vec(),
        Vec::new(),
        vec![0xAB; 64 * 1024],
        b"last".to_vec(),
    ]
}

/// Echo every message from one accepted channel until the peer closes.
/// `RecvError::Closed` ends the loop cleanly (the peer hung up); any other
/// `recv` error (e.g. `RecvError::Transport`) is a genuine mid-session
/// failure and is propagated as `Err`, not swallowed as success.
pub(crate) async fn echo_one_peer<C: Channel>(mut ch: C) -> Result<(), String> {
    loop {
        match ch.recv().await {
            Ok(msg) => ch.send(&msg).await.map_err(|e| e.to_string())?,
            Err(fungi_transport::RecvError::Closed) => return Ok(()), // peer closed: clean end
            Err(e) => return Err(format!("recv failed mid-session: {e}")),
        }
    }
}

/// Send the sequence, assert each echo matches.
pub(crate) async fn dial_sequence<C: Channel>(mut ch: C) -> Result<(), String> {
    for (i, msg) in sequence().into_iter().enumerate() {
        ch.send(&msg).await.map_err(|e| format!("send {i}: {e}"))?;
        let back = ch.recv().await.map_err(|e| format!("recv {i}: {e}"))?;
        if back != msg {
            return Err(format!(
                "echo {i} mismatch: {} vs {} bytes",
                back.len(),
                msg.len()
            ));
        }
    }
    Ok(())
}

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

/// Run the parsed command over the chosen backend. Establishment awaits
/// (bind/listen/connect/bootstrap) are bounded by [`STEP_TIMEOUT`];
/// `accept()` is bounded by the wider [`ACCEPT_TIMEOUT`] so the listener
/// outlives the dialer's retry budget in the VM test; the data phase
/// (`echo_one_peer`/`dial_sequence`) is bounded by [`STEP_TIMEOUT`] at
/// each call site below.
pub(crate) async fn run(cli: Cli) -> Result<(), String> {
    match (&cli.backend, &cli.cmd) {
        (BackendKind::Socks5h, Cmd::Listen { virt_port }) => {
            use fungi_transport_socks5h::{TorConfig, TorListener};
            let cfg = TorConfig::default();
            let mut l = tokio::time::timeout(STEP_TIMEOUT, TorListener::bind(&cfg, *virt_port))
                .await
                .map_err(|_| "bind timed out".to_string())?
                .map_err(|e| e.to_string())?;
            println!("ONION={}", l.onion_addr());
            println!("READY");
            let ch = tokio::time::timeout(ACCEPT_TIMEOUT, l.accept())
                .await
                .map_err(|_| "accept timed out".to_string())?
                .map_err(|e| e.to_string())?;
            tokio::time::timeout(STEP_TIMEOUT, echo_one_peer(ch))
                .await
                .map_err(|_| "echo/dial phase timed out".to_string())?
        }
        (BackendKind::Socks5h, Cmd::Dial { target }) => {
            use fungi_transport_socks5h::{TorConfig, TorConnector};
            let c = TorConnector::new(TorConfig::default());
            let ch = tokio::time::timeout(STEP_TIMEOUT, c.connect(target))
                .await
                .map_err(|_| "connect timed out".to_string())?
                .map_err(|e| e.to_string())?;
            tokio::time::timeout(STEP_TIMEOUT, dial_sequence(ch))
                .await
                .map_err(|_| "echo/dial phase timed out".to_string())??;
            println!("OK");
            Ok(())
        }
        (BackendKind::Arti, cmd) => {
            let transport = arti_transport(&cli).await?;
            match cmd {
                Cmd::Listen { virt_port } => {
                    // Alphanumeric nickname: arti's HsNickname parse is strict.
                    let mut l = tokio::time::timeout(
                        STEP_TIMEOUT,
                        transport.listen("fungie2e", *virt_port),
                    )
                    .await
                    .map_err(|_| "listen timed out".to_string())?
                    .map_err(|e| e.to_string())?;
                    println!("ONION={}", l.onion_addr());
                    println!("READY");
                    let ch = tokio::time::timeout(ACCEPT_TIMEOUT, l.accept())
                        .await
                        .map_err(|_| "accept timed out".to_string())?
                        .map_err(|e| e.to_string())?;
                    tokio::time::timeout(STEP_TIMEOUT, echo_one_peer(ch))
                        .await
                        .map_err(|_| "echo/dial phase timed out".to_string())?
                }
                Cmd::Dial { target } => {
                    let c = transport.connector();
                    let ch = tokio::time::timeout(STEP_TIMEOUT, c.connect(target))
                        .await
                        .map_err(|_| "connect timed out".to_string())?
                        .map_err(|e| e.to_string())?;
                    tokio::time::timeout(STEP_TIMEOUT, dial_sequence(ch))
                        .await
                        .map_err(|_| "echo/dial phase timed out".to_string())??;
                    println!("OK");
                    Ok(())
                }
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
    use fungi_transport::mem::{MemConfig, duplex};

    #[tokio::test]
    async fn echo_then_dial_sequence_roundtrips_over_mem() {
        let (a, b) = duplex(MemConfig {
            capacity: Some(16),
            ..MemConfig::default()
        });
        let echo = tokio::spawn(echo_one_peer(a));
        dial_sequence(b)
            .await
            .expect("sequence should pass against echo");
        echo.await.unwrap().expect("echo side clean");
    }

    #[tokio::test]
    async fn dial_sequence_fails_on_wrong_echo() {
        use fungi_transport::Channel as _;
        let (a, b) = duplex(MemConfig {
            capacity: Some(16),
            ..MemConfig::default()
        });
        // A "broken" peer: receives but answers garbage once, then echoes.
        let broken = tokio::spawn(async move {
            let mut b = b;
            let _ = b.recv().await.unwrap();
            b.send(b"wrong").await.unwrap();
            while let Ok(m) = b.recv().await {
                if b.send(&m).await.is_err() {
                    break;
                }
            }
        });
        assert!(dial_sequence(a).await.is_err());
        broken.abort();
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

    /// A one-shot test double: `recv` yields one message, then a
    /// `RecvError::Transport` (never `Closed`) — proves `echo_one_peer`
    /// does not conflate a mid-session transport failure with a clean
    /// peer-closed end.
    struct OneThenTransportError {
        first: Option<Vec<u8>>,
    }

    impl fungi_transport::Channel for OneThenTransportError {
        async fn send(&mut self, _msg: &[u8]) -> Result<(), fungi_transport::SendError> {
            Ok(())
        }

        async fn recv(&mut self) -> Result<Vec<u8>, fungi_transport::RecvError> {
            match self.first.take() {
                Some(msg) => Ok(msg),
                None => Err(fungi_transport::RecvError::Transport("boom".into())),
            }
        }
    }

    #[tokio::test]
    async fn echo_one_peer_fails_on_mid_session_transport_error() {
        let ch = OneThenTransportError {
            first: Some(b"hi".to_vec()),
        };
        let err = echo_one_peer(ch).await.unwrap_err();
        assert!(
            err.contains("recv failed mid-session"),
            "unexpected error: {err}"
        );
    }
}
