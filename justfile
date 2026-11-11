# Build the optimized binary in target/release/diffy.
build:
    cargo build --release

# Install the optimized binary into ~/.local/bin.
install:
    cargo install --path . --locked --root "$HOME/.local"

# Check formatting, lint all targets, and run the tests.
check:
    cargo fmt --all --check
    cargo clippy --all-targets -- -D warnings
    cargo test --all-targets
