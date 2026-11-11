# Contributing

You need stable Rust, a C compiler, and Git 2.36+.

## Build and run

```sh
cargo build --locked
cargo run --locked -- /path/to/repository
```

With [just](https://github.com/casey/just), use `just build` for a release build
or `just install` to install into `~/.local/bin`.

## Checks

Run these before submitting:

```sh
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --all-targets --locked
```

Or run `just check` to perform all three.

Tests use isolated temporary Git repositories and local remotes.
