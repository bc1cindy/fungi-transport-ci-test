# arti transport as a self-contained flake-parts module: it exports a crane
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
      packages.fungi-arti-plugin = crane.buildCrate {
        pname = "fungi-arti-plugin";
        crate = "fungi-transport-arti";
        bin = "fungi-arti-plugin";
      };
    };

  # VM glue for the next phase to compose. arti is in-process, so a peer needs
  # only the plugin binary on PATH; it dials against a private-net descriptor
  # file. Wiring that file from the running relays' identities is left as a
  # documented placeholder the VM test fills in (see the private-net compose
  # step in the tor-e2e test).
  flake.nixosModules.artiPeer = { pkgs, ... }: {
    environment.systemPackages = [
      inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.fungi-arti-plugin
    ];
    # Placeholder: the private-net descriptor (/tmp/private-net or a state dir)
    # is produced at test runtime from the live relay fingerprints, not baked in.
  };
}
