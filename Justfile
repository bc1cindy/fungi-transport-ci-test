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

# Everything CI checks, locally
check:
    cargo fmt --check
    cargo clippy --all-targets -- -D warnings
    cargo test --all-targets
    cargo test --doc
