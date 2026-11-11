# diffy

A live Git diff viewer and staging interface for the terminal, built with Rust,
Ratatui, Tree-sitter, and native filesystem notifications.

- **Syntax-aware diffs:** both revisions are parsed in full by Tree-sitter,
  preserving multiline comments and strings. Added/removed lines have distinct
  backgrounds, with old/new line numbers and three lines of context.
- **Word-level highlighting:** changed words and punctuation within replacement
  blocks get brighter red/green backgrounds while retaining syntax colors.
  Unicode word boundaries, wrapped lines, and expanded tabs are supported.
- **Two diff layouts:** press `v` to toggle unified (inline) and side-by-side views.
  Side-by-side shows the old revision on the left and the new revision on the right,
  with aligned changes, independent line wrapping, and synchronized scrolling.
- **Live updates:** edits, creates, deletes, renames, staging, commits, and branch
  changes trigger a debounced refresh. Selection and scroll position are preserved
  where possible. Git object writes and file-access events are ignored.
- **File sidebar:** status indicators, file/hunk navigation, wrapped diff lines
  with fixed gutters, and a compact single-pane layout for narrow terminals.
- **Black dark theme:** true-black panes, subtle charcoal panels, and colored diff
  highlights.
- **Unified changes view:** inspect staged and unstaged changes together.
  Supports detached HEADs, linked worktrees, and unborn branches.
- **Worktree picker:** press `w` to choose another worktree in the same repository.
  Review its staged and unstaged changes with live updates, without restarting.
  Delete obsolete worktrees with `D` / `Delete` and explicit confirmation.
- **Open in your editor:** press `e` in either pane to open the working-tree file
  in `$EDITOR`, then return to diffy when the editor exits. Opens at the first
  visible hunk in the Diff pane, or the file's first hunk in the Files pane.
- **Stage and commit:** press `Space` to stage/unstage a whole file or an individual
  hunk, then `c` to write a commit message in a popup. Commits include only staged
  changes when anything is staged; otherwise, all changes are staged and committed,
  including untracked files but excluding ignored files.
- **Pull and push:** `p` fast-forwards the current branch from its upstream; `P`
  pushes only the current branch. Commands run in the background with live output
  and a scrollable details popup. Completed commands show past-tense
  results, including the destination remote and branch for pushes.
- **Explicit writes:** browsing never modifies the index or working tree. Staging,
  unstaging, committing, pulling, pushing, editing, and deleting worktrees require
  explicit shortcuts;
  staging/unstaging never changes working-tree files. External diff/textconv tools are
  disabled. Terminal controls in source and paths are escaped, and symlinks are
  previewed without following their targets.

## Install

Requires a current stable Rust toolchain, a C compiler (to build the bundled
Tree-sitter grammars), Git 2.36+, and an interactive UTF-8 terminal.

```sh
# From this checkout; installs into ~/.cargo/bin by default
cargo install --path . --locked

# Or install into ~/.local/bin
cargo install --path . --locked --root "$HOME/.local"
```

With [just](https://github.com/casey/just) installed, `just install` builds and
installs into `~/.local/bin`.

Ensure the chosen `bin` directory is on your `PATH`. No grammar downloads or
runtime language-server setup are needed. The terminal controls the font;
**IBM Plex Mono Medium** is recommended.

## Run

```sh
diffy                       # Repository containing the current directory
diffy /path/to/repository   # A repository or a directory/file inside it
diffy --help
```

### What the changes view compares

| Entry | Before | After |
| --- | --- | --- |
| `[ ]` unstaged | Git index | Files on disk, including untracked files |
| `[x]` staged | HEAD (empty on an unborn branch) | Git index |

**Changes** lists staged and unstaged entries together, sorted by path. A partially
staged file has two adjacent entries, with separate diffs and staging controls:
`[ ]` for its unstaged edits and `[x]` for its staged edits. The diff title also
shows the selected entry's staging state. Renames are intentionally displayed as
deletion/addition pairs. Unmerged files appear once, showing conflict markers
against the index's “ours” stage.

### Keyboard shortcuts

| Key | Action |
| --- | --- |
| `←` / `→` | Focus files / diff |
| `Tab` / `Shift+Tab` | Switch focus between files and diff |
| `Enter` | Focus the diff |
| `v` | Toggle unified (inline) / side-by-side diff |
| `j` / `k`, `↓` / `↑` | Select files in the Files pane; scroll in the Diff pane |
| `Space` | Stage an unstaged entry / unstage a staged entry: whole file in Files, first visible hunk in Diff |
| `w` | Choose a worktree to review |
| `c` | Open the commit-message popup (staged changes, or all changes if nothing is staged) |
| `p` / `P` | Pull (fast-forward only) / push the current branch |
| `Esc` | Cancel a running Git operation; close its output after completion |
| `e` | Open in `$EDITOR` at the first visible hunk (Diff pane) or first file hunk (Files pane) |
| `PageUp` / `PageDown`, `u` / `d` | Page up / down |
| `Home` / `End`, `g` / `G` | First / last row or file |
| `[` / `]` | Previous / next hunk |
| `?` / `F1` | Help (any key closes it) |
| `q` / `Ctrl+C` | Quit |

Long diff lines wrap automatically to the pane width, preserving highlighting and
keeping continuation lines aligned beneath the code. Line numbers and change
markers appear only on the first screen row of each source line. Scrolling and
paging move through screen rows, including wrapped continuations.

Below 80 columns, only the focused pane is shown; `←` / `→` or `Tab` switches panes.

### Toggle diff layouts

The default is **unified (inline)**: removed and added lines appear consecutively.
Press **`v`** from either pane to switch to **side-by-side**, then press it again
to return. The diff title shows the current layout.

In side-by-side mode, **Before** is on the left and **After** on the right:
HEAD → index for staged entries, or index → working tree for unstaged entries.
Context appears on both sides. Removed and added lines are paired in source order;
surplus lines have a blank opposite side. Long lines wrap independently, and each
pair occupies enough screen rows for its longer side. Syntax colors, word-level
highlights, line numbers, and missing-newline notices are preserved.

Both modes use the same scrolling, paging, hunk navigation, staging/unstaging, and
editor shortcuts. Toggling and resizing keep the visible source position where
possible. The chosen layout stays active across file selections, live refreshes,
and worktree switches for the session; restarting defaults to unified view.
Switching layouts never changes files or the index.

If the **diff pane** is narrower than 60 columns (after accounting for the sidebar
and borders), side-by-side temporarily falls back to **Unified (narrow)**. Widening
the pane automatically restores side-by-side without changing your preference.

### Review another worktree

Press **`w`** to open the worktree picker. It lists this repository's main and
linked worktrees with their branch (or detached HEAD), full path, and any locked
or prunable status. **`*`** marks the current worktree. Use **`↑`/`↓`** or **`j`/`k`**
to select one, then **`Enter`** to switch; **`Esc`**, **`q`**, or **`w`** cancels.
PageUp/PageDown (or `u`/`d`) and Home/End (or `g`/`G`) navigate longer lists.
The list is refreshed each time it opens; press **`r`** to reload it while open.
Worktree discovery uses Git 2.36+'s NUL-delimited output to preserve unusual paths.

Switching shows the selected worktree's staged and unstaged changes, not a
comparison between worktrees or their commits. The live watcher, staging,
commits, pull/push, and editor actions all follow the selected worktree.
Switching itself never checks out a branch or modifies files or the index.
Selecting the current worktree leaves the view unchanged. A successful switch
starts at the first changed file and clears the previous worktree's command
status/output. Switching waits until any running Git operation has finished;
close its output or a commit-message popup before opening the picker.

Locked worktrees can still be reviewed. Missing/prunable worktrees remain listed;
if a worktree cannot be opened or watched, the picker shows an error and the
previous worktree and its watcher remain active. You can select another entry
or refresh the list. Bare repositories are not selectable.

### Delete an obsolete worktree

In the worktree picker, select a worktree and press **`D`** or **`Delete`**. The
confirmation shows its branch and full path. Press **`y`** to delete it, or **`n`**
/ **`Esc`** to cancel; Enter does not confirm. Deletion removes the worktree's
directory **including ignored files**, and its Git registration, but **keeps its
branch**. Commits reachable only through a detached HEAD can eventually be
garbage-collected after removal. A missing/prunable worktree can also be removed;
only the selected registration is cleaned up, not every stale worktree.

Deletion runs `git worktree remove -- PATH` in the background, with the same
output and cancellation controls as other Git operations. It never uses force:
staged, unstaged, or untracked changes prevent deletion. Locked worktrees, the
main worktree, and the worktree currently being viewed cannot be deleted.
Worktrees containing other registered worktrees are also protected; remove the
nested worktrees first. Switch to another worktree first to delete a linked
worktree you are viewing. Unlock
locked worktrees explicitly with Git if needed. Git also refuses worktrees with
submodules. Locks and registration are rechecked after confirmation, and Git
checks for dirty files when removal actually runs.

The picker reloads after completion, including on failure or cancellation. Close
the output popup with **`Esc`** to return to it. Cancellation does not undo files
already removed; inspect the worktree before retrying.

### Stage, unstage, and commit

In **Changes**, press **`Space`** on an unstaged (`[ ]`) entry to stage changes,
or on a staged (`[x]`) entry to unstage them. The selection stays on the same file
when its staging state changes.

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
  as whole files from the Files pane.

Press **`c`** to open a commit-message popup when there are changes. Type
or paste a message; **`Enter`** inserts a newline, **`Ctrl+S`** commits, and
**`Esc`** cancels. Arrow keys, Home/End, Backspace/Delete, and Unicode text are
supported; **`Ctrl+U`** clears the message. Commit failures (including rejected
hooks) open the output/details popup and preserve the message. Cancelling a
running commit also preserves its draft. Press **`Esc`** after completion to
return to the message, edit, and retry.
If anything is staged when you submit, only the current index is committed;
additional working-tree edits stay unstaged. If nothing is staged, all changes
throughout the repository are staged and committed automatically, including new
untracked files and deletions, but excluding ignored files. There is no additional
warning or confirmation. Opening or cancelling the message popup does not stage
anything. If staging fails, the commit is not attempted; if a hook rejects the
commit, the staged changes remain available for retry.

### Pull and push the current branch

- **`p`** runs `git pull --ff-only --no-rebase --progress` using the current branch's upstream.
  Diverged histories are rejected without merging or rebasing.
- **`P`** pushes an explicit refspec for the current branch only, without force.
  The configured branch push remote, `remote.pushDefault`, and upstream remote
  are respected, falling back to `origin` or the sole configured remote. When
  pushing to the upstream remote, its target branch is used; otherwise the
  current branch name is used. A first push sets the upstream if none exists.

Detached HEADs cannot be pulled/pushed. Commit, pull, push, and worktree deletion
run in the background. Their output popup opens automatically, showing stdout/stderr as it
arrives. Pull/push explicitly enable Git's progress output. After completion the
result changes to **Committed**, **Pulled**, or **Pushed to REMOTE (BRANCH)**,
rather than leaving a running-operation label behind.

In the output popup, **`↑`/`↓`** or **`j`/`k`** scroll, **`PageUp`/`PageDown`**
(or `u`/`d`) page, **`Home`** goes to the beginning, and **`End`** resumes following
new output. Scrolling up pauses following so incoming output does not move the
text you are reading. **`h`/`l`** or `←`/`→` scroll long lines horizontally.
Press **`Esc`** while a command is running to cancel it. The popup shows
**Cancelling…** while Git stops, then **Git operation cancelled**; output and any
commit draft are preserved. The popup stays open and scrollable during cancellation.
After completion, **`Esc`** or `q` closes it. Failures and their output remain
visible across filesystem refreshes.

On Unix, cancellation interrupts Git and its child processes (including hooks and
transports), allowing Git to clean up; processes that remain after one second are
force-stopped. On Windows, the process tree is force-stopped. Cancellation does
not undo changes already made locally or on a remote; inspect the refreshed
repository state before retrying.

Repository watching continues in the background while Git is running. Further Git
writes, editor launches, and quitting wait until the operation has finished or
stopped after cancellation.
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

Editing always opens the **file on disk**, including from staged entries;
line numbers may differ if disk contents have changed since staging. Missing
files (including deleted files) and directories cannot be opened.

## Highlighting

Bundled grammars: **Rust, Python, JavaScript, JSX, TypeScript, TSX, Go, C, C++,
JSON, HTML, CSS, and Bash**. Languages are detected from file extensions (and
common Bash dotfile names). Unknown languages are displayed as plain text;
embedded-language injections are not currently highlighted. Tabs use four-column
stops, and line wrapping keeps wide Unicode characters intact.

Word-level highlighting is enabled automatically for replacement blocks in both
staged and unstaged diffs, including plain-text files. Brighter red/green backgrounds
emphasize changed words and punctuation without replacing syntax colors; entire
file additions/deletions retain ordinary line shading. Word comparisons share a
100 ms budget per file, so expensive or later replacement blocks may retain only
line-level highlighting. Display highlights never alter staging patches.

## Limits and troubleshooting

- Binary/non-UTF-8 files, submodules, and special files show a notice instead of a
  text diff. Previews are limited to **2 MiB and 40,000 lines per revision** to
  keep the terminal responsive. Mode-only changes show a metadata notice.
- Only working-tree and index changes are shown; bare repositories and committed
  revision comparisons are not supported.
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

With `just` installed:

```sh
just build    # Build target/release/diffy (optimized)
just install  # Install into ~/.local/bin
just check    # Check formatting, run Clippy, and run all tests
```

Cargo can also be used directly:

```sh
cargo run -- /path/to/repository
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```

Tests use isolated temporary Git repositories and cover unified staged/unstaged
entries, staging-aware selection on refresh, conflicts, binary/large files,
unusual paths, submodules, linked worktree discovery and switching (including
watcher and write-operation targeting, picker navigation, and failed switches),
confirmed worktree removal and dirty/locked/current/main worktree protections, all
bundled highlighting queries, word-level changes (including Unicode, punctuation,
syntax-color preservation, wrapped rendering, and unchanged staging patches),
side-by-side alignment and wrapping, layout toggling and narrow-pane fallback,
hunk navigation/staging/editor actions in both layouts, keyboard navigation, editor commands
and visible-hunk line mapping, small terminal layouts, filesystem/index/ref
notifications, and CLI errors. Staging/commit tests cover whole files, individual
hunks, stale previews, unusual paths, CRLF/missing newlines, Unicode message
editing, automatic whole-repository commits when nothing is staged, ignored files,
staging failures, commit-hook failures/retries, and preserving unstaged edits. Push/pull
tests use isolated local bare remotes, covering first-push tracking, explicit
current-branch refspecs, fast-forwards, divergence, and detached HEAD errors.
Operation tests cover live hook output before completion, concurrent pipe draining
with large commit input, split/invalid UTF-8, carriage-return progress, bounded
history and per-frame polling, scroll/follow behavior, tiny output
popups, preserved commit drafts, past-tense completion messages, and Escape
cancellation of running commands and their child processes.
