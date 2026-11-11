# diffy

Review live Git diffs and stage files or hunks in your terminal.

- Syntax and word-level highlighting, with unified or side-by-side diffs.
- Staged and unstaged diffs in one view, updated as you edit.
- Stage files or hunks, commit, pull, push, and open files in your editor.
- Switch between Git worktrees without restarting.
- Choose from 10 built-in dark and light themes.

## Getting started

Requires stable Rust, a C compiler, Git 2.36+, and an interactive UTF-8 terminal.

From this checkout:

```sh
cargo install --path . --locked
```

Make sure `~/.cargo/bin` is on your `PATH`. Alternatively, `just install`
installs into `~/.local/bin`. Syntax grammars are bundled; no extra setup is needed.

Then run from inside a Git working tree (or any of its subdirectories):

```sh
diffy
```

Use `↑` / `↓` to navigate, `Enter` to inspect a diff, and `Esc` to return to Files.
Press `?` for shortcuts or `q` to quit. Browsing never modifies your files or index.

## Usage guide

The [usage guide](docs/usage.md) covers keyboard shortcuts, staging and commits,
worktrees, editor actions, and troubleshooting.

## Configuration guide

See the [configuration guide](docs/configuration.md) for themes, keymaps, and editor setup.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for build and test instructions.
