# diffy

A live Git diff viewer and staging interface for the terminal, built with Rust,
Ratatui, Tree-sitter, and native filesystem notifications.

- **Syntax-aware unified diffs:** both revisions are parsed in full by Tree-sitter,
  preserving multiline comments and strings. Added/removed lines have distinct
  backgrounds, with old/new line numbers and three lines of context.
- **Live updates:** edits, creates, deletes, renames, staging, commits, and branch
  changes trigger a debounced refresh. Selection and scroll position are preserved
  where possible. Git object writes and file-access events are ignored.
- **File sidebar:** status indicators, file/hunk navigation, horizontal scrolling
  with fixed gutters, and a compact single-pane layout for narrow terminals.
- **Black dark theme:** true-black panes, subtle charcoal panels, and colored diff
  highlights.
- **Unified changes view and branch comparison:** inspect staged and unstaged
  changes together, or a merge-request-style diff against another branch. Supports
  local/remote-tracking refs, detached HEADs, and linked worktrees; the changes view
  also works on unborn branches.
- **Open in your editor:** press `e` in either pane to open the working-tree file
  in `$EDITOR`, then return to diffy when the editor exits. Opens at the first
  visible hunk in the Diff pane, or the file's first hunk in the Files pane.
- **Stage and commit:** press `Space` to stage/unstage a whole file or an individual
  hunk, then `c` to write a commit message in a popup. Only staged changes are
  committed; unstaged edits stay on disk.
- **Pull and push:** `p` fast-forwards the current branch from its upstream; `P`
  pushes only the current branch. Commands run in the background with live output
  and a scrollable details popup. Completed commands show past-tense
  results, including the destination remote and branch for pushes.
- **Explicit writes:** browsing never modifies the index or working tree. Staging,
  unstaging, committing, pulling, pushing, and editing require explicit shortcuts;
  staging/unstaging never changes working-tree files. External diff/textconv tools are
  disabled. Terminal controls in source and paths are escaped, and symlinks are
  previewed without following their targets.

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
diffy --base main           # Committed changes since diverging from main
diffy -b origin/main /repo  # Compare against a local remote-tracking ref
diffy --help
```

### What each view compares

| View / entry | Before | After |
| --- | --- | --- |
| Changes (default), `[ ]` unstaged | Git index | Files on disk, including untracked files |
| Changes, `[x]` staged | HEAD (empty on an unborn branch) | Git index |
| Branch (`--base REF` or `b`) | Merge base of REF and HEAD | HEAD |

**Changes** lists staged and unstaged entries together, sorted by path. A partially
staged file has two adjacent entries, with separate diffs and staging controls:
`[ ]` for its unstaged edits and `[x]` for its staged edits. The diff title also
shows the selected entry's staging state. Renames are intentionally displayed as
deletion/addition pairs. Unmerged files appear once, showing conflict markers
against the index's “ours” stage.

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
**`s`** switches between Changes and Branch; `b` jumps directly to Branch after
confirming a selection. Without a selected base, `s` stays in Changes.

Both revisions are pinned for each refresh so a moving branch cannot mix a file
list from one commit with a preview from another. Watching recomputes the
comparison automatically when HEAD or the base ref changes. If the base ref
disappears, the last snapshot remains visible with an error. Remote
branches such as `origin/main` use existing local refs: selecting a base never
checks out a branch or fetches updates. Use `p` to pull the current branch (which
fetches from its upstream), or fetch other remotes outside diffy when needed.

### Keyboard shortcuts

| Key | Action |
| --- | --- |
| `←` / `→` | Focus files / diff |
| `Tab` / `Shift+Tab` | Switch focus between files and diff |
| `Enter` | Focus the diff |
| `j` / `k`, `↓` / `↑` | Select files in the Files pane; scroll in the Diff pane |
| `Space` | Stage an unstaged entry / unstage a staged entry: whole file in Files, first visible hunk in Diff |
| `c` | Open the commit-message popup for staged changes |
| `p` / `P` | Pull (fast-forward only) / push the current branch |
| `e` | Open in `$EDITOR` at the first visible hunk (Diff pane) or first file hunk (Files pane) |
| `h` / `l` | Scroll code horizontally |
| `PageUp` / `PageDown`, `u` / `d` | Page up / down |
| `Home` / `End`, `g` / `G` | First / last row or file |
| `[` / `]` | Previous / next hunk |
| `s` | Switch changes / branch views (branch requires a selected base) |
| `b` | Select the base branch from a searchable list and open the branch comparison |
| `?` / `F1` | Help (any key closes it) |
| `q` / `Ctrl+C` | Quit |

Below 80 columns, only the focused pane is shown; `←` / `→` or `Tab` switches panes.

### Stage, unstage, and commit

In **Changes**, press **`Space`** on an unstaged (`[ ]`) entry to stage changes,
or on a staged (`[x]`) entry to unstage them. No view switch is needed. The
selection stays on the same file when its staging state changes.

- In the **Files pane**, `Space` acts on the entire selected file, including
  additions, deletions, binary files, and mode changes.
- In the **Diff pane**, `Space` acts only on the hunk containing the top visible
  row, even when its header is offscreen. Use **`[` / `]`** to select a hunk; the
  diff summary shows the active hunk number. Every hunk can be brought to the top,
  even when multiple hunks fit in the viewport.
- Hunk staging for modified files uses exactly the displayed text. If the file or
  index changes before the live refresh, the operation is rejected rather than
  acting on an outdated hunk. Added/deleted files have a single hunk covering all
  their text and use whole-file staging/unstaging.
- Previews without text hunks, conflicts, and submodules must be staged/unstaged
  as whole files from the Files pane. Branch comparisons are read-only.

Press **`c`** to open a commit-message popup when there are staged changes. Type
or paste a message; **`Enter`** inserts a newline, **`Ctrl+S`** commits, and
**`Esc`** cancels. Arrow keys, Home/End, Backspace/Delete, and Unicode text are
supported; **`Ctrl+U`** clears the message. Commit failures (including rejected
hooks) open the output/details popup and preserve the message. Press **`Esc`** to
return to the message, edit, and retry.
Commits use the current index without staging any additional working-tree edits.

### Pull and push the current branch

- **`p`** runs `git pull --ff-only --no-rebase --progress` using the current branch's upstream.
  Diverged histories are rejected without merging or rebasing.
- **`P`** pushes an explicit refspec for the current branch only, without force.
  The configured branch push remote, `remote.pushDefault`, and upstream remote
  are respected, falling back to `origin` or the sole configured remote. When
  pushing to the upstream remote, its target branch is used; otherwise the
  current branch name is used. A first push sets the upstream if none exists.

Detached HEADs cannot be pulled/pushed. Commit, pull, and push run in the
background. Their output popup opens automatically, showing stdout/stderr as it
arrives. Pull/push explicitly enable Git's progress output. After completion the
result changes to **Committed**, **Pulled**, or **Pushed to REMOTE (BRANCH)**,
rather than leaving a running-operation label behind.

In the output popup, **`↑`/`↓`** or **`j`/`k`** scroll, **`PageUp`/`PageDown`**
(or `u`/`d`) page, **`Home`** goes to the beginning, and **`End`** resumes following
new output. Scrolling up pauses following so incoming output does not move the
text you are reading. **`h`/`l`** or `←`/`→` scroll long lines horizontally.
The popup stays open and scrollable until the command finishes. After completion,
**`Esc`** or `q` closes it. Failures and their output remain visible across
filesystem refreshes.

Repository watching continues in the background while Git is running. Further Git
writes, editor launches, and quitting wait until the operation finishes; operation
cancellation is not supported.
Output history is bounded to 256 KiB, 2,000 lines, and 8 KiB per line, with a notice
when output is omitted. Terminal control characters in output are escaped.
Git terminal credential prompts are disabled; configure credentials, SSH agents,
or askpass/signing helpers beforehand. Git hooks and configured commit signing
remain enabled.

### Open a file in your editor

Set `EDITOR` to an editor command, for example `export EDITOR=nvim` or
`export EDITOR='code --wait'`. Arguments and quoted executable paths are supported;
shell expansions and pipelines are not evaluated. An unset or invalid `EDITOR`
shows an error without leaving diffy.

Press **`e`** in the Files pane to open the selected file at its first hunk.
In the Diff pane, **`e`** opens at the first visible hunk, including a partially
visible hunk whose header has scrolled offscreen. The editor opens at the hunk's
first new-file line (including context), not the first visible row. Scrolling
with `j`/`k` or `↑`/`↓` does not add a cursor. Notices without hunks or hunks
without new-file line numbers open at line 1.

Diffy waits for the editor to exit, restores the interface, and refreshes the diff.
Vim/Neovim, nano, Emacs, and other `+LINE` editors are supported, along with Helix,
VS Code/VSCodium, Sublime Text, and Zed. The latter GUI editors receive `--wait`
automatically; configure any other GUI editor to stay in the foreground.

Editing always opens the **file on disk**, including from staged entries and Branch;
line numbers may differ if disk contents have changed since that revision. Missing
files (including deleted files) and directories cannot be opened.

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
  and textconv transformations are not applied to previews or partial hunk
  staging. Whole-file staging uses `git add` and respects Git's filters.
- Watching is recursive, including ignored directories (changes there can cause
  a harmless refresh). Large trees can hit OS watch limits. Native filesystem
  notifications are required and may be unreliable on some network filesystems.
  Changes are coalesced for 150 ms, with a 750 ms maximum delay during continuous
  activity.
- If watching cannot start or fails at runtime, the app exits with an error,
  restoring the terminal if needed.
- Errors and panics restore the terminal before returning. Like other terminal
  apps, it cannot clean up after `SIGKILL`; use `reset` if necessary.

## Development

```sh
cargo run -- /path/to/repository
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```

Tests use isolated temporary Git repositories and cover unified staged/unstaged
entries, staging-aware selection on refresh, conflicts, binary/large files,
unusual paths, submodules, linked worktrees, all
bundled highlighting queries, diff hunks, keyboard navigation, editor commands
and visible-hunk line mapping, small terminal layouts, filesystem/index/ref
notifications, and CLI errors. Branch tests cover
merge-base semantics, exclusion of uncommitted changes, immutable snapshots,
remote-tracking refs, missing/unrelated history, comparison-mode navigation, and
branch selection/filtering. Staging/commit tests cover whole files, individual
hunks, stale previews, unusual paths, CRLF/missing newlines, Unicode message
editing, commit-hook failures/retries, and preserving unstaged edits. Push/pull
tests use isolated local bare remotes, covering first-push tracking, explicit
current-branch refspecs, fast-forwards, divergence, and detached HEAD errors.
Operation tests cover live hook output before completion, concurrent pipe draining
with large commit input, split/invalid UTF-8, carriage-return progress, bounded
history and per-frame polling, scroll/follow behavior, tiny output
popups, preserved commit drafts, and past-tense completion messages.
