# Run the test suite
test:
    cargo test --all-targets
    cargo test --doc

# Clippy with warnings denied (what CI runs)
clippy:
    cargo clippy --all-targets -- -D warnings

# Auto-format
fmt:
    cargo fmt

# Fast local checks via cargo (dev loop)
check:
    cargo fmt --check
    cargo clippy --all-targets -- -D warnings
    cargo test --all-targets
    cargo test --doc
    cargo run -p fungi-transport --example echo

# The single CI entry point: crane checks (nextest/clippy/fmt/doc) + VM test,
# reproducibly, pinned by flake.lock.
flake-check:
    nix flake check -L

# Just the cross-backend end-to-end VM test.
e2e:
    nix build .#checks.x86_64-linux.tor-e2e -L
