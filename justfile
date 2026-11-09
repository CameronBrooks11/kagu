set dotenv-load := false

# Show available recipes
default:
    @just --list

# Install toolchain components (versions from rust-toolchain.toml)
setup:
    rustup component add rustfmt clippy

# Format code
fmt:
    cargo fmt

# Verify formatting
fmt-check:
    cargo fmt --check

# Lint; clippy compiles, so this also type-checks
lint:
    cargo clippy --all-targets --all-features -- -D warnings

# CI-equivalent: format check and lint
check: fmt-check lint

# Unit tests; sandbox integration tests run in CI and outside sessions
test:
    cargo test

# Model-check tla/ with TLC (about 18 minutes, mostly liveness)
tla:
    cd tla && tlc -workers auto -metadir "${TMPDIR:-/tmp}/kagu-tlc" MC
