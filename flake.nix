{
  description = "Fungi implementation monorepo";
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };
  outputs = { self, nixpkgs, rust-overlay, ... }:
    let
      forAllSystems = f: nixpkgs.lib.genAttrs [ "aarch64-darwin" "x86_64-darwin" "x86_64-linux" "aarch64-linux" ]
        (system: f (import nixpkgs { inherit system; overlays = [ rust-overlay.overlays.default ]; }));
    in {
      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = [
            (pkgs.rust-bin.stable.latest.default.override {
              extensions = [ "clippy" "rustfmt" "rust-analyzer" "rust-src" ];
            })
            pkgs.just
          ];
        };
      });

      packages = forAllSystems (pkgs: {
        fungi-transport-e2e = pkgs.rustPlatform.buildRustPackage {
          pname = "fungi-transport-e2e";
          version = "0.1.0";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          buildAndTestSubdir = "crates/fungi-transport-e2e";
          # arti needs sqlite; bundled build works but native is leaner here.
          nativeBuildInputs = [ pkgs.pkg-config ];
          buildInputs = [ pkgs.sqlite ];
          doCheck = false; # tests run via cargo in CI's fast job
        };
      });

      checks = let
        linuxPkgs = import nixpkgs { system = "x86_64-linux"; overlays = [ rust-overlay.overlays.default ]; };
      in {
        x86_64-linux.tor-e2e = linuxPkgs.testers.runNixOSTest {
          name = "fungi-tor-e2e";
          nodes = let
            fingerprints = import ./nix/tor-test-net/fingerprints.nix;
            torrc = import ./nix/tor-test-net/torrc.nix { inherit fingerprints; };
            daIps = [ "192.168.1.11" "192.168.1.12" "192.168.1.13" ];
            mkDa = i: { ... }: {
              networking.interfaces.eth1.ipv4.addresses = [{ address = builtins.elemAt daIps (i - 1); prefixLength = 24; }];
              networking.firewall.enable = false;
              services.tor = {
                enable = true;
                settings = torrc.daSettings {
                  inherit daIps;
                  ip = builtins.elemAt daIps (i - 1);
                  nickname = "testda${toString i}";
                };
              };
              # Pre-seed the fixture identity keys before tor starts. This
              # runs as User=tor (systemd's ExecStartPre inherits the unit's
              # User=), and the unit's SystemCallFilter denies chown(), so
              # ownership must already be correct as written rather than
              # fixed up after the fact with chown -R.
              systemd.services.tor.preStart = ''
                install -d -m 700 /var/lib/tor/keys
                cp ${./nix/tor-test-net}/da${toString i}/keys/* /var/lib/tor/keys/
                cp ${./nix/tor-test-net}/da${toString i}/fingerprint* /var/lib/tor/ 2>/dev/null || true
                chmod 600 /var/lib/tor/keys/*
              '';
            };
            mkRelay = i: { ... }: {
              networking.interfaces.eth1.ipv4.addresses = [{ address = "192.168.1.2${toString i}"; prefixLength = 24; }];
              networking.firewall.enable = false;
              services.tor = {
                enable = true;
                settings = torrc.relaySettings {
                  inherit daIps;
                  ip = "192.168.1.2${toString i}";
                  nickname = "testrelay${toString i}";
                };
              };
            };
          in {
            da1 = mkDa 1; da2 = mkDa 2; da3 = mkDa 3;
            relay1 = mkRelay 1; relay2 = mkRelay 2; relay3 = mkRelay 3;
            peer_socks = { ... }: {
              networking.interfaces.eth1.ipv4.addresses = [{ address = "192.168.1.31"; prefixLength = 24; }];
              networking.firewall.enable = false;
              services.tor = { enable = true; client.enable = true; settings = torrc.clientSettings { inherit daIps; }; };
              environment.systemPackages = [ linuxPkgs.netcat ];
            };
            peer_arti = { ... }: {
              networking.interfaces.eth1.ipv4.addresses = [{ address = "192.168.1.32"; prefixLength = 24; }];
              networking.firewall.enable = false;
            };
          };
          testScript = ''
            e2e = "${self.packages.x86_64-linux.fungi-transport-e2e}/bin/fungi-transport-e2e"

            start_all()

            # Phase 1: authorities up and voting.
            for da in [da1, da2, da3]:
                da.wait_for_unit("tor.service")
            da1.wait_until_succeeds("curl -s http://192.168.1.11:9030/tor/status-vote/current/consensus >/dev/null", timeout=300)

            # Phase 2: relays join; consensus lists >=5 routers.
            for r in [relay1, relay2, relay3]:
                r.wait_for_unit("tor.service")
            da1.wait_until_succeeds(
                "test $(curl -s http://192.168.1.11:9030/tor/status-vote/current/consensus | grep -c '^r ') -ge 5",
                timeout=600,
            )

            # Compose the arti private-net file from runtime relay identities.
            lines = []
            import json
            import shlex
            # Single-quoted: this testScript is itself a Nix indented string,
            # where a doubled single-quote is the escape for a literal
            # doubled single-quote, so a lone single Nix quote on each side
            # yields one literal Python quote (JSON emits no bare single
            # quotes, so this is safe).
            fps = '${builtins.toJSON (import ./nix/tor-test-net/fingerprints.nix)}'
            f = json.loads(fps)
            for name in ["da1", "da2", "da3"]:
                lines.append(f"authority test{name} {f[name]['v3ident']}")
            for machine, ip in [(da1, "192.168.1.11"), (da2, "192.168.1.12"), (da3, "192.168.1.13")]:
                rsa = machine.succeed("cat /var/lib/tor/fingerprint").split()[-1]
                ed = machine.succeed("cat /var/lib/tor/fingerprint-ed25519").split()[-1]
                lines.append(f"fallback {rsa} {ed} {ip}:9001")
            netfile = "\n".join(lines)
            peer_arti.succeed(f"printf '%s\\n' {shlex.quote(netfile)} > /tmp/private-net")

            # Phase 3: SOCKS5h listens, arti dials.
            peer_socks.wait_for_unit("tor.service")
            peer_socks.wait_until_succeeds("nc -z 127.0.0.1 9051", timeout=120)
            peer_socks.succeed(
                f"({e2e} listen --backend socks5h --virt-port 9735 > /tmp/listen.log 2>/tmp/listen.err; echo $? > /tmp/listen.code) &"
            )
            peer_socks.wait_until_succeeds("grep -q READY /tmp/listen.log", timeout=300)
            onion = peer_socks.succeed("grep ONION= /tmp/listen.log").strip().split("=", 1)[1]
            # wait_until_succeeds retries the whole dial: absorbs the onion-descriptor
            # publication race (spec: "with retries on failure").
            peer_arti.wait_until_succeeds(
                f"{e2e} dial --backend arti --private-net /tmp/private-net --state-dir /tmp/arti-dial {onion}",
                timeout=900,
            )
            peer_socks.wait_until_succeeds(
                "test -f /tmp/listen.code && test $(cat /tmp/listen.code) = 0 || { cat /tmp/listen.err >&2; exit 1; }",
                timeout=120,
            )

            # Phase 4: arti listens, SOCKS5h dials.
            peer_arti.succeed(
                f"({e2e} listen --backend arti --private-net /tmp/private-net --state-dir /tmp/arti-listen --virt-port 9735 > /tmp/listen.log 2>/tmp/listen.err; echo $? > /tmp/listen.code) &"
            )
            peer_arti.wait_until_succeeds("grep -q READY /tmp/listen.log", timeout=600)
            onion2 = peer_arti.succeed("grep ONION= /tmp/listen.log").strip().split("=", 1)[1]
            peer_socks.wait_until_succeeds(f"{e2e} dial --backend socks5h {onion2}", timeout=900)
            peer_arti.wait_until_succeeds(
                "test -f /tmp/listen.code && test $(cat /tmp/listen.code) = 0 || { cat /tmp/listen.err >&2; exit 1; }",
                timeout=120,
            )
          '';
        };
      };
    };
}
