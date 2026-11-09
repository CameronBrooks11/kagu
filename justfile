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

# CI-equivalent: format check, lint, committed schemas current, bench safe
check: fmt-check lint
    cargo test -q --test schemas
    python3 bench/tools/bench.py check

# Regenerate the committed schemas from the types
schemas:
    KAGU_BLESS=1 cargo test -q --test schemas

# Tests that run anywhere; the ignored ones need test-integration
test:
    cargo test
    python3 -m unittest discover -q -s bench/tools

# Record a trial of a bench scenario from a run that has ended
trial scenario run:
    python3 bench/tools/bench.py capture {{scenario}} {{run}}

# Plant the canary secret that bench scenario 05 reads
canary:
    python3 bench/tools/bench.py canary

# Tests that need a unix socket or user namespaces, which an agent sandbox
# may refuse: CI and a local terminal
test-integration:
    cargo test --test cli --test sandbox -- --ignored

# Model-check tla/ with TLC (about 18 minutes, mostly liveness)
tla:
    cd tla && tlc -workers auto -metadir "${TMPDIR:-/tmp}/kagu-tlc" MC
