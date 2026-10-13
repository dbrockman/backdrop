# Development tasks. Run `just` to list them.
#
# Recipes run inside the Nix dev shell automatically, so they also work from a
# plain terminal; inside `nix develop` they run directly.

nix := if env("IN_NIX_SHELL", "") == "" { "nix develop --command" } else { "" }

# List the recipes
default:
    @just --list --unsorted

# Build everything (debug)
build:
    {{ nix }} cargo build --workspace --all-targets

# Build the optimized binary (target/release/backdrop)
release:
    {{ nix }} cargo build --release -p backdrop

# Run the app; extra arguments go to it, e.g. `just run paths`
[positional-arguments]
run *args:
    {{ nix }} cargo run -p backdrop -- "$@"

# Run all tests
[positional-arguments]
test *args:
    {{ nix }} cargo test --workspace "$@"

# Format the code
fmt:
    {{ nix }} cargo fmt --all

# Check formatting and lints without changing anything
lint:
    {{ nix }} cargo fmt --all --check
    {{ nix }} cargo clippy --workspace --all-targets -- -D warnings

# Everything CI should run: lint, then test
check: lint test

# Update Cargo dependencies within their semver ranges, and the Nix flake inputs
update:
    {{ nix }} cargo update
    nix flake update

# Remove build artifacts
clean:
    {{ nix }} cargo clean
