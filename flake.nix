{
  description = "Fungi implementation monorepo";
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
  };
  outputs = inputs@{ flake-parts, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [ "aarch64-darwin" "x86_64-darwin" "x86_64-linux" "aarch64-linux" ];
      # The VM test is its own flake-parts module (x86_64-linux only).
      imports = [ ./nix/checks/tor-e2e.nix ];
      perSystem = { system, ... }:
        let
          pkgs = import inputs.nixpkgs {
            inherit system;
            overlays = [ inputs.rust-overlay.overlays.default ];
          };
          rustToolchain = pkgs.rust-bin.stable.latest.default.override {
            extensions = [ "clippy" "rustfmt" "rust-analyzer" "rust-src" ];
          };
          craneLib = (inputs.crane.mkLib pkgs).overrideToolchain rustToolchain;
          # Shared build inputs. arti pulls a native sqlite; pkg-config finds it.
          # pname/version are set here because the workspace root manifest is
          # virtual (no [package]), so crane can't infer a name for the
          # dependency and check derivations.
          commonArgs = {
            src = craneLib.cleanCargoSource ./.;
            pname = "fungi";
            version = "0.1.0";
            strictDeps = true;
            nativeBuildInputs = [ pkgs.pkg-config ];
            buildInputs = [ pkgs.sqlite ];
          };
          # Build every workspace dependency once; reused by package + checks.
          cargoArtifacts = craneLib.buildDepsOnly commonArgs;
          e2e = craneLib.buildPackage (commonArgs // {
            inherit cargoArtifacts;
            pname = "fungi-transport-e2e";
            cargoExtraArgs = "-p fungi-transport-e2e";
            doCheck = false; # tests run as the `nextest` check, not here
          });
        in {
          packages.fungi-transport-e2e = e2e;
          packages.default = e2e;

          devShells.default = pkgs.mkShell {
            packages = [ rustToolchain pkgs.just pkgs.cargo-nextest ];
          };

          # `nix flake check` runs all of these; on x86_64-linux the VM test
          # (defined in the imported module) joins them.
          checks = {
            nextest = craneLib.cargoNextest (commonArgs // {
              inherit cargoArtifacts;
              partitions = 1;
              partitionType = "count";
            });
            clippy = craneLib.cargoClippy (commonArgs // {
              inherit cargoArtifacts;
              cargoClippyExtraArgs = "--all-targets -- -D warnings";
            });
            fmt = craneLib.cargoFmt { inherit (commonArgs) src; };
            doc = craneLib.cargoDoc (commonArgs // { inherit cargoArtifacts; });
          };
        };
    };
}
