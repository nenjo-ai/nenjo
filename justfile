# Nenjo SDK Development Commands

set dotenv-load := true

default:
    @just --list

# Build the entire workspace
build:
    cargo build --workspace

# Build in release mode
build-release:
    cargo build -p nenjo --release

# Run all tests
test:
    cargo test --workspace

# Run tests for a specific crate
test-crate crate:
    cargo test -p {{crate}}

# Run clippy lints
lint:
    cargo clippy --workspace --all-targets -- -D warnings

# Run clippy with auto-fix
lint-fix:
    cargo clippy --workspace --fix --allow-dirty --all-targets -- -D warnings

# Format code
fmt:
    cargo fmt --all

# Check formatting without modifying
fmt-check:
    cargo fmt --all -- --check

# Fix any linting errors and format before making a pull request
pr:
    just lint-fix
    just fmt

# Type-check without building
check:
    cargo check --workspace

# Run the CLI
run *args:
    cargo run --bin nenjo -- {{args}}

# Run the harness in watch mode
dev:
    cargo watch -x 'run --bin nenjo -- run --log-level "info,nenjo=debug"'

# Clean all build artifacts
clean:
    cargo clean
