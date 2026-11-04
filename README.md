# diffy

A read-only, live Git diff viewer for the terminal, built with Rust, Ratatui,
Tree-sitter, and native filesystem notifications.

- **Syntax-aware unified diffs:** both revisions are parsed in full by Tree-sitter,
  preserving multiline comments and strings. Added/removed lines have distinct
  backgrounds, with old/new line numbers and three lines of context.
- **Live updates:** edits, creates, deletes, renames, staging, commits, and branch
  changes trigger a debounced refresh. Selection, filtering, and scroll position
  are preserved where possible. Git object writes and file-access events are ignored.
- **File sidebar:** status indicators, case-insensitive path filtering, file/hunk
  navigation, horizontal scrolling with fixed gutters, and a compact single-pane
  layout for narrow terminals.
- **Working tree, staged, and branch views:** inspect uncommitted changes or a
  merge-request-style diff against another branch. Supports local/remote-tracking
  refs, detached HEADs, and linked worktrees; uncommitted views also work on unborn
  branches.
- **Safe to browse:** no staging, discarding, editing, or index refresh writes.
  External diff/textconv tools are disabled. Terminal controls in source and paths
  are escaped, and symlinks are previewed without following their targets.

## Install

Requires a current stable Rust toolchain, a C compiler (to build the bundled
Tree-sitter grammars), Git 2.31+, and an interactive UTF-8 terminal.

```sh
# From this checkout; installs into ~/.cargo/bin by default
cargo install --path . --locked

# Or install into ~/.local/bin
cargo install --path . --locked --root "$HOME/.local"
```

Ensure the chosen `bin` directory is on your `PATH`. No grammar downloads or
runtime language-server setup are needed. The terminal controls the font;
**IBM Plex Mono Medium** is recommended.

## Run

```sh
diffy                       # Repository containing the current directory
diffy /path/to/repository   # A repository or a directory/file inside it
diffy --no-watch            # Manual refresh only
diffy --base main           # Committed changes since diverging from main
diffy -b origin/main /repo  # Compare against a local remote-tracking ref
diffy --help
```

### What each view compares

| View | Before | After |
| --- | --- | --- |
| Working tree (default) | Git index | Files on disk, including untracked files |
| Staged | HEAD (empty on an unborn branch) | Git index |
| Branch (`--base REF` or `b`) | Merge base of REF and HEAD | HEAD |

A partially staged file can appear in both views, with different changes.
Renames are intentionally displayed as deletion/addition pairs. Unmerged files
show conflict markers against the index's “ours” stage in the working-tree view;
the staged view shows a notice until the conflict is resolved.

### Compare against another branch

```sh
diffy --base main
diffy --base=origin/main
```

This shows the same committed changes as `git diff main...HEAD`: changes on the
current branch **since the branches diverged**, not changes made only on `main`.
Staged, unstaged, and untracked changes are excluded. The base branch and HEAD
must have a common ancestor, and HEAD must have at least one commit.

Press **`b`** to select or change the base branch without restarting. Browse local
and remote-tracking branches with **`↑`/`↓`** (or `Tab`/`Shift+Tab`), then press
**`Enter`** to compare. Type to filter the list (case-insensitive); `Ctrl+U` clears
the filter, and `Esc` cancels. The current base is highlighted when reopening the
picker. You can still enter another ref, such as a tag or commit, when no branches
match. Invalid refs leave the previous view intact. After a base is selected,
**`s`** cycles through Working tree → Staged → Branch → Working tree; `b` jumps
directly to Branch after confirming a selection.

Both revisions are pinned for each refresh so a moving branch cannot mix a file
list from one commit with a preview from another. Watching recomputes the
comparison when HEAD or the base ref changes; `r` does the same manually. If the
base ref disappears, the last snapshot remains visible with an error. Remote
branches such as `origin/main` use existing local refs: diffy never checks out
branches or fetches updates. Fetch them yourself when needed.

### Keyboard shortcuts

| Key | Action |
| --- | --- |
| `←` / `→` | Focus files / diff |
| `Tab` / `Shift+Tab` | Switch focus between files and diff |
| `Enter` | Focus the diff |
| `j` / `k`, `↓` / `↑` | Select files in the Files pane; scroll in the Diff pane |
| `h` / `l` | Scroll code horizontally |
| `PageUp` / `PageDown`, `u` / `d` | Page up / down |
| `Home` / `End`, `g` / `G` | First / last row or file |
| `[` / `]` | Previous / next hunk |
| `s` | Cycle working-tree / staged / branch views (branch requires a selected base) |
| `b` | Select the base branch from a searchable list and open the branch comparison |
| `/` | Filter file paths; type to update live |
| `Enter` / `Esc` while filtering | Apply filter |
| `Ctrl+U` while filtering | Clear filter |
| `Esc` outside filtering | Clear filter |
| `r` | Refresh manually |
| `?` / `F1` | Help (any key closes it) |
| `q` / `Ctrl+C` | Quit |

Below 80 columns, only the focused pane is shown; `←` / `→` or `Tab` switches panes.

## Highlighting

Bundled grammars: **Rust, Python, JavaScript, JSX, TypeScript, TSX, Go, C, C++,
JSON, HTML, CSS, and Bash**. Languages are detected from file extensions (and
common Bash dotfile names). Unknown languages are displayed as plain text;
embedded-language injections are not currently highlighted. Tabs use four-column
stops and horizontal scrolling accounts for wide Unicode characters.

## Limits and troubleshooting

- Binary/non-UTF-8 files, submodules, and special files show a notice instead of a
  text diff. Previews are limited to **2 MiB and 40,000 lines per revision** to
  keep the terminal responsive. Mode-only changes show a metadata notice.
- Bare repositories and arbitrary two-revision ranges are not supported. Branch
  comparisons require a common ancestor; shallow clones may need more history.
- The viewer compares raw Git blobs with disk contents; Git clean/smudge filters
  and textconv transformations are not applied.
- Watching is recursive, including ignored directories (changes there can cause
  a harmless refresh). Large trees can hit OS watch limits. Use `--no-watch` and
  `r` if native watching is unavailable or unreliable, such as on some network
  filesystems. Changes are coalesced for 150 ms, with a 750 ms maximum delay during
  continuous activity.
- If watching fails at runtime, the app switches to manual refresh and displays
  an error. If it cannot start watching, rerun with `--no-watch`.
- Errors and panics restore the terminal before returning. Like other terminal
  apps, it cannot clean up after `SIGKILL`; use `reset` if necessary.

## Development

```sh
cargo run -- /path/to/repository
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```

Tests use isolated temporary Git repositories and cover staged/unstaged changes,
conflicts, binary/large files, unusual paths, submodules, linked worktrees, all
bundled highlighting queries, diff hunks, keyboard navigation, small terminal
layouts, filesystem/index/ref notifications, and CLI errors. Branch tests cover
merge-base semantics, exclusion of uncommitted changes, immutable snapshots,
remote-tracking refs, missing/unrelated history, comparison-mode navigation, and
branch selection/filtering.
