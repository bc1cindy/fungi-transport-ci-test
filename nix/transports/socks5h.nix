# SOCKS5h transport as a self-contained flake-parts module: it exports a crane
# build of the plugin binary and the NixOS-node glue a peer running this
# transport needs. The glue is exported but not consumed here; the VM test
# composes it in the next phase.
{ inputs, ... }:
{
  perSystem = { system, ... }:
    let
      # Same shared crane wiring the main flake uses, so cargoArtifacts is the
      # very same derivation and the workspace deps are not rebuilt per module.
      crane = import ../lib/crane.nix { inherit inputs system; };
    in
    {
      packages.fungi-socks5h-plugin = crane.buildCrate {
        pname = "fungi-socks5h-plugin";
        crate = "fungi-transport-socks5h";
        bin = "fungi-socks5h-plugin";
      };
    };

  # VM glue for the next phase to compose. A peer speaking SOCKS5h needs a local
  # tor daemon (SOCKS + control) and the plugin binary on PATH. Referencing the
  # per-system package lazily keeps this evaluable on any host; it only forces
  # when imported into a nixosSystem (the VM runs on x86_64-linux).
  flake.nixosModules.socks5hPeer = { pkgs, ... }: {
    services.tor = {
      enable = true;
      client.enable = true;
    };
    environment.systemPackages = [
      inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.fungi-socks5h-plugin
    ];
  };
}
