# Task runner for hacker-news-tui. Install `just`: https://github.com/casey/just
# Run `just` with no args to run the full check suite (what CI runs).

set quiet

# Default: the full local gate (format check, lint, test).
default: check

# Everything CI enforces, in one shot.
check: fmt-check lint test

# Apply formatting.
fmt:
    cargo fmt --all

# Verify formatting without changing files (CI mode).
fmt-check:
    cargo fmt --all --check

# Lint with warnings treated as errors, across all targets.
lint:
    cargo clippy --all-targets -- -D warnings

# Run the test suite.
test:
    cargo test --all

# Audit the locked dependency tree for security advisories.
# Install once with: cargo install cargo-audit
audit:
    cargo audit

# Run the app (release build for snappy startup).
run:
    cargo run --release

# Build an optimized binary.
build:
    cargo build --release
