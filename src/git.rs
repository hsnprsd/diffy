use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use anyhow::{Context, Result, bail};

pub const MAX_FILE_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct FileEntry {
    pub path: PathBuf,
    pub status: char,
    pub submodule: bool,
    pub staged: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    pub branch: String,
    pub locked: bool,
    pub prunable: bool,
}

#[derive(Debug)]
pub enum Content {
    Text(String),
    Notice(&'static str),
}

impl Content {
    fn from_bytes(bytes: Vec<u8>) -> Self {
        if bytes.contains(&0) {
            return Self::Notice("Binary file — no text preview");
        }
        match String::from_utf8(bytes) {
            Ok(text) => Self::Text(text),
            Err(_) => Self::Notice("Non-UTF-8 file — no text preview"),
        }
    }

    fn empty() -> Self {
        Self::Text(String::new())
    }
}

pub enum Operation {
    Commit(String),
    Pull,
    Push,
    RemoveWorktree(PathBuf),
}

impl Operation {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Commit(_) => "Committing",
            Self::Pull => "Pulling",
            Self::Push => "Pushing",
            Self::RemoveWorktree(_) => "Removing worktree",
        }
    }
}

pub struct OperationCommand {
    pub preparation: Option<Command>,
    pub command: Command,
    pub input: Option<String>,
    pub success_message: String,
}

#[derive(Clone, Debug)]
pub struct Repository {
    pub root: PathBuf,
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
}

impl Repository {
    pub fn discover(path: &Path) -> Result<Self> {
        let path = path
            .canonicalize()
            .context("Repository path does not exist")?;
        let dir = if path.is_file() {
            path.parent().context("Path has no parent directory")?
        } else {
            &path
        };
        let resolve = |args: &[&str]| -> Result<PathBuf> {
            let output = command(dir)
                .args(args)
                .output()
                .context("Could not run Git")?;
            ensure_success(&output)?;
            Ok(PathBuf::from(os_from_bytes(trim_newline(&output.stdout))))
        };
        let root = resolve(&["rev-parse", "--show-toplevel"])
            .context("Not inside a Git working tree (bare repositories are not supported)")?;
        let git_dir = resolve(&["rev-parse", "--absolute-git-dir"])?;
        let common_dir = resolve(&["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
        Ok(Self {
            root,
            git_dir,
            common_dir,
        })
    }

    pub fn worktrees(&self) -> Result<Vec<Worktree>> {
        let output = self.output(&["worktree", "list", "--porcelain", "-z"])?;
        ensure_success(&output).context("Could not list worktrees (requires Git 2.36+)")?;
        Ok(parse_worktrees(&output.stdout))
    }

    pub fn removable_worktree(&self, path: &Path) -> Result<Worktree> {
        anyhow::ensure!(
            !self.root.starts_with(path),
            "Cannot delete the current worktree or a directory containing it; switch to another first"
        );
        let worktrees = self.worktrees()?;
        let worktree = worktrees
            .iter()
            .find(|worktree| worktree.path == path)
            .cloned()
            .context("Worktree is no longer registered in this repository; refresh the list")?;
        anyhow::ensure!(
            !worktrees
                .iter()
                .any(|worktree| worktree.path != path && worktree.path.starts_with(path)),
            "Cannot delete a worktree containing another registered worktree; remove the nested worktree first"
        );
        anyhow::ensure!(
            !worktree.locked,
            "Cannot delete a locked worktree; unlock it with Git first"
        );
        if path.try_exists()? {
            let target = Self::discover(path)?;
            anyhow::ensure!(
                target.root == path.canonicalize()? && target.common_dir == self.common_dir,
                "Worktree no longer belongs to this repository"
            );
            anyhow::ensure!(
                target.git_dir != target.common_dir,
                "Cannot delete the main worktree"
            );
        }
        Ok(worktree)
    }

    pub fn branch(&self) -> Result<String> {
        let symbolic = self.output(&["symbolic-ref", "--quiet", "--short", "HEAD"])?;
        if symbolic.status.success() {
            return Ok(String::from_utf8_lossy(trim_newline(&symbolic.stdout)).into_owned());
        }
        let detached = self.output(&["rev-parse", "--short", "HEAD"])?;
        ensure_success(&detached)?;
        Ok(format!(
            "{} (detached)",
            String::from_utf8_lossy(trim_newline(&detached.stdout))
        ))
    }

    pub fn files(&self) -> Result<Vec<FileEntry>> {
        let mut files = self.unstaged_files()?;
        // Conflicts are not staged changes; show their working-tree preview only.
        files.extend(
            self.staged_files()?
                .into_iter()
                .filter(|entry| entry.status != 'U'),
        );
        files.sort_by(|a, b| (&a.path, a.staged).cmp(&(&b.path, b.staged)));
        Ok(files)
    }

    pub fn staged_files(&self) -> Result<Vec<FileEntry>> {
        self.diff_files(true)
    }

    fn unstaged_files(&self) -> Result<Vec<FileEntry>> {
        let mut files: BTreeMap<_, _> = self
            .diff_files(false)?
            .into_iter()
            .map(|entry| (entry.path.clone(), entry))
            .collect();
        let output = self.output(&["ls-files", "--others", "--exclude-standard", "-z", "--"])?;
        ensure_success(&output)?;
        for path in output
            .stdout
            .split(|&byte| byte == 0)
            .filter(|p| !p.is_empty())
        {
            let path = PathBuf::from(os_from_bytes(path));
            files.entry(path.clone()).or_insert(FileEntry {
                path,
                status: '?',
                submodule: false,
                staged: false,
            });
        }
        Ok(files.into_values().collect())
    }

    fn diff_files(&self, staged: bool) -> Result<Vec<FileEntry>> {
        // NUL-delimited paths preserve spaces, newlines, and non-UTF-8 filenames.
        // Disabling renames also makes each side independently inspectable.
        let mut args = vec![
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "--ignore-submodules=none",
            "--raw",
            "-z",
        ];
        if staged {
            args.push("--cached");
        }
        args.push("--");
        let output = self.output(&args)?;
        ensure_success(&output)?;
        let mut files = BTreeMap::new();
        let mut fields = output
            .stdout
            .split(|&byte| byte == 0)
            .filter(|field| !field.is_empty());
        while let Some(header) = fields.next() {
            let path = fields.next().context("Invalid Git raw diff output")?;
            let parts: Vec<_> = header.split(|&byte| byte == b' ').collect();
            let [old_mode, new_mode, _, _, status] = parts.as_slice() else {
                bail!("Invalid Git raw diff header");
            };
            let status = *status.first().context("Missing Git file status")? as char;
            let submodule = *old_mode == b":160000" || *new_mode == b"160000";
            let path = PathBuf::from(os_from_bytes(path));
            // An unmerged path can also have a separate M record.
            files
                .entry(path.clone())
                .and_modify(|old: &mut FileEntry| {
                    if status == 'U' {
                        old.status = status;
                    }
                    old.submodule |= submodule;
                })
                .or_insert(FileEntry {
                    path,
                    status,
                    submodule,
                    staged,
                });
        }
        Ok(files.into_values().collect())
    }

    pub fn contents(&self, entry: &FileEntry) -> Result<(Content, Content)> {
        if entry.submodule {
            return Ok((
                Content::Notice("Submodule — inspect changes inside its repository"),
                Content::empty(),
            ));
        }
        if entry.status == 'U' && entry.staged {
            return Ok((
                Content::Notice("Unmerged index — inspect conflict markers in Changes"),
                Content::empty(),
            ));
        }
        let old = if matches!(entry.status, 'A' | '?') {
            Content::empty()
        } else {
            let revision = if entry.staged {
                "HEAD:"
            } else if entry.status == 'U' {
                ":2:"
            } else {
                ":"
            };
            self.blob(revision, &entry.path)?
        };
        let new = if entry.status == 'D' {
            Content::empty()
        } else if entry.staged {
            self.blob(":", &entry.path)?
        } else {
            self.worktree_file(&entry.path)?
        };
        Ok((old, new))
    }

    pub fn toggle_file(&self, entry: &FileEntry) -> Result<()> {
        let mut cmd = command(&self.root);
        if entry.staged {
            // Reset also works on unborn branches and never changes files on disk.
            cmd.args(["reset", "-q"]);
        } else {
            cmd.args(["add", "-A"]);
        }
        let output = cmd.arg("--").arg(&entry.path).output()?;
        ensure_success(&output)
    }

    pub fn toggle_hunk(&self, entry: &FileEntry, hunk: &str) -> Result<()> {
        anyhow::ensure!(
            entry.status != 'U' && !entry.submodule,
            "Use the Files pane to stage/unstage conflicts or submodules as a whole"
        );
        // Added/deleted files contain exactly one hunk, covering their entire contents.
        if matches!(entry.status, 'A' | '?' | 'D') {
            return self.toggle_file(entry);
        }
        let patch = format!(
            "--- {}\n+++ {}\n{hunk}",
            patch_path("a/", &entry.path),
            patch_path("b/", &entry.path),
        );
        let mut cmd = command(&self.root);
        cmd.args(["apply", "--cached", "--whitespace=nowarn"]);
        if entry.staged {
            cmd.arg("--reverse");
        }
        let output = with_input(cmd, patch.as_bytes())?;
        ensure_success(&output)
    }

    pub fn operation_command(&self, operation: &Operation) -> Result<OperationCommand> {
        let mut cmd = command(&self.root);
        let mut preparation = None;
        let mut success_message = "Committed".to_owned();
        // Capture command output and disable Git's terminal prompts. Authentication
        // and signing can still use configured agents or askpass programs.
        cmd.env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_EDITOR", "true")
            .env("GIT_SEQUENCE_EDITOR", "true");
        let input = match operation {
            Operation::Commit(message) => {
                anyhow::ensure!(!message.trim().is_empty(), "Enter a commit message");
                if self.staged_files()?.is_empty() {
                    let mut stage = command(&self.root);
                    stage
                        .env("GIT_TERMINAL_PROMPT", "0")
                        .args(["add", "-A", "--"]);
                    preparation = Some(stage);
                }
                cmd.args(["commit", "--cleanup=whitespace", "-F", "-"]);
                Some(message.clone())
            }
            Operation::RemoveWorktree(path) => {
                // Revalidate after confirmation; Git itself checks for dirty files and locks
                // again when executing. Never force removal or delete the branch.
                self.removable_worktree(path)?;
                cmd.args(["worktree", "remove", "--"]).arg(path);
                success_message = format!("Removed worktree {}", path.display());
                None
            }
            Operation::Pull | Operation::Push => {
                let output = self.output(&["symbolic-ref", "--quiet", "HEAD"])?;
                anyhow::ensure!(output.status.success(), "Cannot pull/push a detached HEAD");
                let branch_ref = std::str::from_utf8(trim_newline(&output.stdout))?;
                let branch = branch_ref
                    .strip_prefix("refs/heads/")
                    .context("HEAD must point to a local branch")?;
                if matches!(operation, Operation::Pull) {
                    cmd.args(["pull", "--ff-only", "--no-rebase", "--progress"]);
                    success_message = format!("Pulled {branch}");
                } else {
                    let config = |key: &str| -> Result<Option<String>> {
                        let output = self.output(&["config", "--get", key])?;
                        if output.status.code() == Some(1) {
                            return Ok(None);
                        }
                        ensure_success(&output)?;
                        Ok(Some(
                            std::str::from_utf8(trim_newline(&output.stdout))?.to_owned(),
                        ))
                    };
                    let upstream_remote = config(&format!("branch.{branch}.remote"))?;
                    let remote = config(&format!("branch.{branch}.pushRemote"))?
                        .or(config("remote.pushDefault")?)
                        .or_else(|| upstream_remote.clone());
                    let remote = match remote {
                        Some(remote) => remote,
                        None => {
                            let output = self.output(&["remote"])?;
                            ensure_success(&output)?;
                            let remotes = std::str::from_utf8(&output.stdout)?;
                            let remotes: Vec<_> = remotes.lines().collect();
                            if remotes.contains(&"origin") {
                                "origin".to_owned()
                            } else {
                                anyhow::ensure!(
                                    remotes.len() == 1,
                                    "Configure a push remote for this branch"
                                );
                                remotes[0].to_owned()
                            }
                        }
                    };
                    let target = if upstream_remote.as_deref() == Some(&remote) {
                        config(&format!("branch.{branch}.merge"))?
                    } else {
                        None
                    }
                    .unwrap_or_else(|| format!("refs/heads/{branch}"));
                    cmd.args(["push", "--progress"]);
                    if upstream_remote.is_none() {
                        cmd.arg("--set-upstream");
                    }
                    // An explicit refspec prevents push.default=matching or remote.push
                    // from accidentally pushing branches other than the current one.
                    success_message = format!("Pushed to {remote} ({target})");
                    cmd.arg("--")
                        .arg(remote)
                        .arg(format!("{branch_ref}:{target}"));
                }
                None
            }
        };
        Ok(OperationCommand {
            preparation,
            command: cmd,
            input,
            success_message,
        })
    }

    #[cfg(test)]
    pub fn run_operation(&self, operation: &Operation) -> Result<String> {
        let OperationCommand {
            preparation,
            mut command,
            input,
            ..
        } = self.operation_command(operation)?;
        if let Some(mut preparation) = preparation {
            ensure_success(&preparation.output()?)?;
        }
        let output = match input {
            Some(input) => with_input(command, input.as_bytes())?,
            None => command.output()?,
        };
        ensure_success(&output)?;
        let message = [&output.stdout[..], &output.stderr[..]]
            .into_iter()
            .map(|bytes| String::from_utf8_lossy(bytes))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(message.trim().to_owned())
    }

    fn blob(&self, prefix: &str, path: &Path) -> Result<Content> {
        let mut object = OsString::from(prefix);
        object.push(path.as_os_str());
        let output = command(&self.root)
            .args([OsStr::new("cat-file"), OsStr::new("-s"), &object])
            .output()?;
        if !output.status.success() {
            // A conflict can lack an "ours" stage (e.g. modify/delete).
            if prefix == ":2:" {
                return Ok(Content::empty());
            }
            ensure_success(&output)?;
        }
        let size: usize = std::str::from_utf8(&output.stdout)?.trim().parse()?;
        if size > MAX_FILE_BYTES {
            return Ok(Content::Notice("File exceeds the 2 MiB preview limit"));
        }
        let kind = command(&self.root)
            .args([OsStr::new("cat-file"), OsStr::new("-t"), &object])
            .output()?;
        ensure_success(&kind)?;
        if trim_newline(&kind.stdout) != b"blob" {
            return Ok(Content::Notice(
                "Submodule — inspect changes inside its repository",
            ));
        }
        let output = command(&self.root)
            .args([OsStr::new("cat-file"), OsStr::new("blob"), &object])
            .output()?;
        ensure_success(&output)?;
        Ok(Content::from_bytes(output.stdout))
    }

    fn worktree_file(&self, path: &Path) -> Result<Content> {
        let full_path = self.root.join(path);
        let metadata = match fs::symlink_metadata(&full_path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Content::empty());
            }
            Err(error) => return Err(error).context("Could not inspect working-tree file"),
        };
        if metadata.file_type().is_symlink() {
            let target = fs::read_link(full_path)?;
            return Ok(Content::from_bytes(os_to_bytes(target.as_os_str())));
        }
        if metadata.is_dir() {
            return Ok(Content::Notice(
                "Submodule/directory — inspect changes inside its repository",
            ));
        }
        if !metadata.is_file() {
            return Ok(Content::Notice("Special file — no text preview"));
        }
        if metadata.len() > MAX_FILE_BYTES as u64 {
            return Ok(Content::Notice("File exceeds the 2 MiB preview limit"));
        }
        let mut bytes = Vec::new();
        fs::File::open(full_path)?
            .take(MAX_FILE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_FILE_BYTES {
            return Ok(Content::Notice("File exceeds the 2 MiB preview limit"));
        }
        Ok(Content::from_bytes(bytes))
    }

    fn output(&self, args: &[&str]) -> Result<Output> {
        command(&self.root)
            .args(args)
            .output()
            .context("Could not run Git")
    }
}

fn parse_worktrees(bytes: &[u8]) -> Vec<Worktree> {
    // NUL-delimited fields preserve spaces, newlines, and non-UTF-8 path bytes.
    let fields: Vec<&[u8]> = bytes.split(|&byte| byte == 0).collect();
    fields
        .split(|field| field.is_empty())
        .filter_map(|fields| {
            let path = fields.first()?.strip_prefix(b"worktree ")?;
            if fields.contains(&b"bare".as_slice()) {
                return None;
            }
            let value = |prefix: &[u8]| fields.iter().find_map(|field| field.strip_prefix(prefix));
            let branch = if let Some(branch) = value(b"branch ") {
                String::from_utf8_lossy(branch.strip_prefix(b"refs/heads/").unwrap_or(branch))
                    .into_owned()
            } else {
                let head = value(b"HEAD ").unwrap_or(b"unknown");
                format!(
                    "{} (detached)",
                    String::from_utf8_lossy(&head[..head.len().min(7)])
                )
            };
            Some(Worktree {
                path: PathBuf::from(os_from_bytes(path)),
                branch,
                locked: fields
                    .iter()
                    .any(|field| *field == b"locked" || field.starts_with(b"locked ")),
                prunable: fields
                    .iter()
                    .any(|field| *field == b"prunable" || field.starts_with(b"prunable ")),
            })
        })
        .collect()
}

fn command(dir: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(dir)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_LITERAL_PATHSPECS", "1")
        .stdin(Stdio::null())
        .args(["--no-pager", "-c", "color.ui=false"]);
    command
}

fn with_input(mut command: Command, input: &[u8]) -> Result<Output> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Could not run Git")?;
    let write = child.stdin.take().unwrap().write_all(input);
    let output = child.wait_with_output()?;
    // Prefer Git's error over a broken pipe when it rejected the input early.
    if output.status.success() {
        write.context("Could not send input to Git")?;
    }
    Ok(output)
}

// Git's quoted path syntax preserves arbitrary bytes, including tabs and newlines.
fn patch_path(prefix: &str, path: &Path) -> String {
    let mut quoted = format!("\"{prefix}");
    for byte in os_to_bytes(path.as_os_str()) {
        match byte {
            b'"' => quoted.push_str("\\\""),
            b'\\' => quoted.push_str("\\\\"),
            0x20..=0x7e => quoted.push(byte as char),
            _ => quoted.push_str(&format!("\\{byte:03o}")),
        }
    }
    quoted.push('"');
    quoted
}

fn ensure_success(output: &Output) -> Result<()> {
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let message = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        bail!("Git: {message}");
    }
    Ok(())
}

fn trim_newline(bytes: &[u8]) -> &[u8] {
    bytes.strip_suffix(b"\n").unwrap_or(bytes)
}

#[cfg(unix)]
fn os_from_bytes(bytes: &[u8]) -> OsString {
    use std::os::unix::ffi::OsStringExt;
    OsString::from_vec(bytes.to_vec())
}

#[cfg(not(unix))]
fn os_from_bytes(bytes: &[u8]) -> OsString {
    String::from_utf8_lossy(bytes).into_owned().into()
}

#[cfg(unix)]
fn os_to_bytes(value: &OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    value.as_bytes().to_vec()
}

#[cfg(not(unix))]
fn os_to_bytes(value: &OsStr) -> Vec<u8> {
    value.to_string_lossy().as_bytes().to_vec()
}

/// Do not allow filenames, Git messages, or source code to emit terminal controls.
pub fn safe_text(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_control() {
                ch.escape_default().to_string()
            } else {
                ch.to_string()
            }
        })
        .collect()
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use tempfile::TempDir;

    pub fn git(dir: &Path, args: &[&str]) {
        let output = command(dir).args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    pub fn repo() -> (TempDir, Repository) {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-b", "main"]);
        git(dir.path(), &["config", "user.email", "test@example.com"]);
        git(dir.path(), &["config", "user.name", "Test"]);
        let repo = Repository::discover(dir.path()).unwrap();
        (dir, repo)
    }

    pub fn linked_repo() -> (TempDir, TempDir, Repository, Repository) {
        let (dir, repo) = repo();
        git(dir.path(), &["commit", "--allow-empty", "-qm", "Initial"]);
        let linked = tempfile::tempdir().unwrap();
        let path = linked.path().join("linked worktree");
        git(
            dir.path(),
            &["worktree", "add", "-b", "linked", path.to_str().unwrap()],
        );
        let linked_repo = Repository::discover(&path).unwrap();
        (dir, linked, repo, linked_repo)
    }

    pub fn text(content: Content) -> String {
        match content {
            Content::Text(text) => text,
            _ => panic!("Expected text"),
        }
    }

    #[test]
    fn unborn_index_and_untracked_files() {
        let (dir, repo) = repo();
        fs::write(dir.path().join("new.rs"), "fn main() {}\n").unwrap();
        fs::write(dir.path().join(".gitignore"), "ignored\n").unwrap();
        fs::write(dir.path().join("ignored"), "ignore me").unwrap();
        assert_eq!(repo.branch().unwrap(), "main");
        assert!(repo.staged_files().unwrap().is_empty());
        assert_eq!(repo.files().unwrap().len(), 2);
        git(dir.path(), &["add", "new.rs"]);
        let files = repo.files().unwrap();
        assert_eq!(files.len(), 2);
        assert!(!files[0].staged);
        assert!(files[1].staged);
        assert_eq!(files[1].status, 'A');
        let (old, new) = repo.contents(&files[1]).unwrap();
        assert_eq!(text(old), "");
        assert_eq!(text(new), "fn main() {}\n");
    }

    #[test]
    fn changes_include_both_sides_of_partially_staged_files() {
        let (dir, repo) = repo();
        let path = dir.path().join("space and\nnewline.rs");
        fs::write(&path, "original\n").unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-qm", "Initial"]);
        fs::write(&path, "staged\n").unwrap();
        git(dir.path(), &["add", "."]);
        fs::write(&path, "working\n").unwrap();
        let files = repo.files().unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, files[1].path);
        for (entry, (staged, before, after)) in files.iter().zip([
            (false, "staged\n", "working\n"),
            (true, "original\n", "staged\n"),
        ]) {
            assert_eq!(entry.staged, staged);
            let (old, new) = repo.contents(entry).unwrap();
            assert_eq!((text(old), text(new)), (before.into(), after.into()));
        }
        // Opposing edits must remain visible even when disk matches HEAD.
        fs::write(&path, "original\n").unwrap();
        assert_eq!(repo.files().unwrap().len(), 2);
        fs::remove_file(path).unwrap();
        let files = repo.files().unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].status, 'D');
        assert_eq!(text(repo.contents(&files[0]).unwrap().1), "");
    }

    #[test]
    fn changes_sort_staged_unstaged_and_restored_deletions_by_path() {
        let (dir, repo) = repo();
        for name in ["a.txt", "b.txt", "c.txt"] {
            fs::write(dir.path().join(name), "original\n").unwrap();
        }
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-qm", "Initial"]);
        fs::write(dir.path().join("a.txt"), "staged\n").unwrap();
        git(dir.path(), &["add", "a.txt"]);
        fs::write(dir.path().join("b.txt"), "unstaged\n").unwrap();
        git(dir.path(), &["rm", "c.txt"]);
        fs::write(dir.path().join("c.txt"), "original\n").unwrap();
        fs::write(dir.path().join("z.txt"), "untracked\n").unwrap();

        let files = repo.files().unwrap();
        let entries: Vec<_> = files
            .iter()
            .map(|entry| (entry.path.to_str().unwrap(), entry.status, entry.staged))
            .collect();
        assert_eq!(
            entries,
            [
                ("a.txt", 'M', true),
                ("b.txt", 'M', false),
                ("c.txt", '?', false),
                ("c.txt", 'D', true),
                ("z.txt", '?', false),
            ]
        );
        for (entry, (before, after)) in files[2..4]
            .iter()
            .zip([("", "original\n"), ("original\n", "")])
        {
            let (old, new) = repo.contents(entry).unwrap();
            assert_eq!((text(old), text(new)), (before.into(), after.into()));
        }
    }

    #[test]
    fn binary_and_large_files_have_notices() {
        let (dir, repo) = repo();
        fs::write(dir.path().join("binary"), [0, 1, 2]).unwrap();
        fs::write(dir.path().join("large"), vec![b'a'; MAX_FILE_BYTES + 1]).unwrap();
        for entry in repo.files().unwrap() {
            assert!(matches!(
                repo.contents(&entry).unwrap().1,
                Content::Notice(_)
            ));
        }
    }

    #[test]
    fn lists_the_current_worktree_on_an_unborn_branch() {
        let (_dir, repo) = repo();
        assert_eq!(
            repo.worktrees().unwrap(),
            vec![Worktree {
                path: repo.root.clone(),
                branch: "main".into(),
                locked: false,
                prunable: false,
            }]
        );
    }

    #[test]
    fn lists_worktrees_from_any_worktree_with_detached_locked_and_prunable_states() {
        let (dir, linked, repo, linked_repo) = linked_repo();
        git(&linked_repo.root, &["checkout", "--detach"]);
        git(
            dir.path(),
            &[
                "worktree",
                "lock",
                linked_repo.root.to_str().unwrap(),
                "--reason",
                "keep it",
            ],
        );
        let worktrees = repo.worktrees().unwrap();
        assert_eq!(worktrees, linked_repo.worktrees().unwrap());
        assert_eq!(worktrees.len(), 2);
        assert_eq!(worktrees[0].path, repo.root);
        assert_eq!(worktrees[0].branch, "main");
        assert_eq!(worktrees[1].path, linked_repo.root);
        assert!(worktrees[1].branch.ends_with(" (detached)"));
        assert!(worktrees[1].locked);
        git(
            dir.path(),
            &["worktree", "unlock", linked_repo.root.to_str().unwrap()],
        );
        fs::remove_dir_all(linked.path()).unwrap();
        let worktrees = repo.worktrees().unwrap();
        assert_eq!(worktrees[1].path, linked_repo.root);
        assert!(worktrees[1].prunable);
    }

    #[test]
    fn parses_nul_delimited_paths_and_skips_bare_repositories() {
        let worktrees = parse_worktrees(b"worktree /bare\0bare\0\0worktree /a path\nwith\ttabs\0HEAD abcdef012345\0branch refs/heads/topic/nested\0locked a reason\nnext line\0\0worktree /detached\0HEAD 0123456789\0detached\0prunable missing metadata\0\0");
        assert_eq!(
            worktrees,
            vec![
                Worktree {
                    path: PathBuf::from("/a path\nwith\ttabs"),
                    branch: "topic/nested".into(),
                    locked: true,
                    prunable: false
                },
                Worktree {
                    path: PathBuf::from("/detached"),
                    branch: "0123456 (detached)".into(),
                    locked: false,
                    prunable: true
                },
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn worktree_paths_preserve_newlines_and_non_utf8_bytes() {
        let (dir, repo) = repo();
        git(dir.path(), &["commit", "--allow-empty", "-qm", "Initial"]);
        let linked = tempfile::tempdir().unwrap();
        let path = linked.path().join(os_from_bytes(b"linked\n\t\xff"));
        let output = command(dir.path())
            .args(["worktree", "add", "-b", "linked"])
            .arg(&path)
            .output()
            .unwrap();
        ensure_success(&output).unwrap();
        let worktrees = repo.worktrees().unwrap();
        assert_eq!(worktrees[1].path, path);
        assert_eq!(worktrees[1].branch, "linked");
        assert_eq!(
            Repository::discover(&path).unwrap().worktrees().unwrap(),
            worktrees
        );
        repo.run_operation(&Operation::RemoveWorktree(path.clone()))
            .unwrap();
        assert_eq!(repo.worktrees().unwrap().len(), 1);
        assert!(!path.exists());
    }

    #[test]
    fn removes_a_clean_worktree_and_ignored_files_but_keeps_its_branch() {
        let (_dir, _linked, repo, linked_repo) = linked_repo();
        fs::write(linked_repo.root.join(".gitignore"), "ignored\n").unwrap();
        git(&linked_repo.root, &["add", "."]);
        git(
            &linked_repo.root,
            &["commit", "-qm", "Ignore generated files"],
        );
        fs::write(linked_repo.root.join("ignored"), "generated content\n").unwrap();
        let head = repo
            .output(&["rev-parse", "refs/heads/linked"])
            .unwrap()
            .stdout;
        repo.run_operation(&Operation::RemoveWorktree(linked_repo.root.clone()))
            .unwrap();
        assert_eq!(
            repo.worktrees()
                .unwrap()
                .iter()
                .map(|wt| &wt.path)
                .collect::<Vec<_>>(),
            vec![&repo.root]
        );
        assert!(!linked_repo.root.exists());
        assert!(!linked_repo.git_dir.exists());
        assert_eq!(
            repo.output(&["rev-parse", "refs/heads/linked"])
                .unwrap()
                .stdout,
            head
        );
        assert_eq!(repo.branch().unwrap(), "main");
    }

    #[test]
    fn worktree_removal_refuses_untracked_staged_and_unstaged_changes() {
        for state in ["untracked", "staged", "unstaged"] {
            let (_dir, _linked, repo, linked_repo) = linked_repo();
            let path = linked_repo.root.join("keep.txt");
            if state == "unstaged" {
                fs::write(&path, "original\n").unwrap();
                git(&linked_repo.root, &["add", "."]);
                git(&linked_repo.root, &["commit", "-qm", "Track file"]);
            }
            fs::write(&path, "precious changes\n").unwrap();
            if state == "staged" {
                git(&linked_repo.root, &["add", "."]);
            }
            let error = repo
                .run_operation(&Operation::RemoveWorktree(linked_repo.root.clone()))
                .unwrap_err();
            assert!(
                error.to_string().contains("modified or untracked files"),
                "{state}: {error:#}"
            );
            assert_eq!(fs::read_to_string(&path).unwrap(), "precious changes\n");
            assert_eq!(repo.worktrees().unwrap().len(), 2);
            assert_eq!(linked_repo.files().unwrap()[0].staged, state == "staged");
        }
    }

    #[test]
    fn worktree_removal_refuses_current_main_locked_and_unregistered_paths() {
        let (dir, linked, repo, linked_repo) = linked_repo();
        let unregistered = linked.path().to_path_buf();
        for (source, target, expected) in [
            (&repo, &repo.root, "current worktree"),
            (&linked_repo, &linked_repo.root, "current worktree"),
            (&linked_repo, &repo.root, "main worktree"),
            (&repo, &unregistered, "no longer registered"),
        ] {
            let error = source
                .run_operation(&Operation::RemoveWorktree(target.clone()))
                .unwrap_err();
            assert!(error.to_string().contains(expected), "{error:#}");
        }
        git(
            dir.path(),
            &["worktree", "lock", linked_repo.root.to_str().unwrap()],
        );
        let error = repo
            .run_operation(&Operation::RemoveWorktree(linked_repo.root.clone()))
            .unwrap_err();
        assert!(error.to_string().contains("locked worktree"));
        assert_eq!(repo.worktrees().unwrap().len(), 2);
        assert!(linked_repo.root.is_dir());
        assert_eq!(linked_repo.branch().unwrap(), "linked");
    }

    #[test]
    fn worktree_removal_preserves_nested_worktrees_even_when_they_are_ignored() {
        let (dir, _linked, repo, linked_repo) = linked_repo();
        fs::write(linked_repo.root.join(".gitignore"), "nested/\n").unwrap();
        git(&linked_repo.root, &["add", "."]);
        git(
            &linked_repo.root,
            &["commit", "-qm", "Ignore nested directory"],
        );
        let nested = linked_repo.root.join("nested");
        git(
            dir.path(),
            &["worktree", "add", "-b", "nested", nested.to_str().unwrap()],
        );
        fs::write(nested.join("keep.txt"), "nested work\n").unwrap();
        let error = repo
            .run_operation(&Operation::RemoveWorktree(linked_repo.root.clone()))
            .unwrap_err();
        assert!(error.to_string().contains("another registered worktree"));
        let nested_repo = Repository::discover(&nested).unwrap();
        let error = nested_repo
            .run_operation(&Operation::RemoveWorktree(linked_repo.root.clone()))
            .unwrap_err();
        assert!(error.to_string().contains("current worktree"));
        assert_eq!(repo.worktrees().unwrap().len(), 3);
        assert_eq!(
            fs::read_to_string(nested.join("keep.txt")).unwrap(),
            "nested work\n"
        );
        assert_eq!(nested_repo.branch().unwrap(), "nested");
    }

    #[test]
    fn removes_only_the_selected_missing_worktree_registration() {
        let (dir, linked, repo, linked_repo) = linked_repo();
        let another = linked.path().join("another missing worktree");
        git(
            dir.path(),
            &[
                "worktree",
                "add",
                "-b",
                "another",
                another.to_str().unwrap(),
            ],
        );
        fs::remove_dir_all(&linked_repo.root).unwrap();
        fs::remove_dir_all(&another).unwrap();
        repo.run_operation(&Operation::RemoveWorktree(linked_repo.root.clone()))
            .unwrap();
        let worktrees = repo.worktrees().unwrap();
        assert_eq!(
            worktrees.iter().map(|wt| &wt.path).collect::<Vec<_>>(),
            vec![&repo.root, &another]
        );
        assert!(worktrees[1].prunable);
        assert!(!linked_repo.git_dir.exists());
        git(dir.path(), &["rev-parse", "--verify", "refs/heads/linked"]);
    }

    #[test]
    fn discovers_linked_worktree() {
        let (dir, _) = repo();
        git(dir.path(), &["commit", "--allow-empty", "-qm", "Initial"]);
        let linked = tempfile::tempdir().unwrap();
        let path = linked.path().join("linked");
        git(
            dir.path(),
            &["worktree", "add", "-b", "linked", path.to_str().unwrap()],
        );
        let repo = Repository::discover(&path).unwrap();
        assert_eq!(repo.root, path.canonicalize().unwrap());
        assert_ne!(repo.git_dir, repo.common_dir);
        assert_eq!(repo.branch().unwrap(), "linked");
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_not_followed() {
        let (dir, repo) = repo();
        std::os::unix::fs::symlink("/nonexistent/target", dir.path().join("link")).unwrap();
        let files = repo.files().unwrap();
        assert_eq!(
            text(repo.contents(&files[0]).unwrap().1),
            "/nonexistent/target"
        );
    }

    #[test]
    fn conflict_preview_uses_ours_and_working_tree() {
        let (dir, repo) = repo();
        let path = dir.path().join("conflict.rs");
        fs::write(&path, "base\n").unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-qm", "Base"]);
        git(dir.path(), &["checkout", "-qb", "theirs"]);
        fs::write(&path, "theirs\n").unwrap();
        git(dir.path(), &["commit", "-qam", "Theirs"]);
        git(dir.path(), &["checkout", "-q", "main"]);
        fs::write(&path, "ours\n").unwrap();
        git(dir.path(), &["commit", "-qam", "Ours"]);
        let merge = command(dir.path())
            .args(["merge", "theirs"])
            .output()
            .unwrap();
        assert!(!merge.status.success());
        let files = repo.files().unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].status, 'U');
        assert!(!files[0].staged);
        let (old, new) = repo.contents(&files[0]).unwrap();
        assert_eq!(text(old), "ours\n");
        assert!(text(new).contains("<<<<<<<"));
        let staged = repo.staged_files().unwrap();
        assert!(matches!(
            repo.contents(&staged[0]).unwrap().0,
            Content::Notice(_)
        ));
    }

    #[test]
    fn submodule_does_not_require_its_commit_in_the_parent_database() {
        let (dir, repo) = repo();
        git(
            dir.path(),
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                "160000,1111111111111111111111111111111111111111,sub",
            ],
        );
        let files = repo.staged_files().unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].submodule);
        assert!(files[0].staged);
        assert!(matches!(
            repo.contents(&files[0]).unwrap().0,
            Content::Notice(_)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_paths_survive_git_and_blob_lookup() {
        let (dir, repo) = repo();
        let name = os_from_bytes(b"invalid-\xff.rs");
        fs::write(dir.path().join(&name), "old\n").unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-qm", "Initial"]);
        fs::write(dir.path().join(&name), "new\n").unwrap();
        let files = repo.files().unwrap();
        assert_eq!(files[0].path.as_os_str(), name);
        let (old, new) = repo.contents(&files[0]).unwrap();
        assert_eq!((text(old), text(new)), ("old\n".into(), "new\n".into()));
    }

    #[test]
    fn detached_head_and_staged_deletion() {
        let (dir, repo) = repo();
        fs::write(dir.path().join("delete.txt"), "goodbye\n").unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-qm", "Initial"]);
        git(dir.path(), &["checkout", "--detach", "-q"]);
        assert!(repo.branch().unwrap().ends_with(" (detached)"));
        git(dir.path(), &["rm", "delete.txt"]);
        let files = repo.files().unwrap();
        assert_eq!(files[0].status, 'D');
        assert!(files[0].staged);
        let (old, new) = repo.contents(&files[0]).unwrap();
        assert_eq!((text(old), text(new)), ("goodbye\n".into(), "".into()));
    }

    fn selected_view(repo: &Repository, staged: bool) -> (FileEntry, crate::diff::DiffView) {
        let entry = repo
            .files()
            .unwrap()
            .into_iter()
            .find(|entry| entry.staged == staged)
            .unwrap();
        let (old, new) = repo.contents(&entry).unwrap();
        let view = crate::diff::DiffView::build(
            &entry.path,
            old,
            new,
            &mut crate::highlight::SyntaxHighlighter::default(),
        )
        .unwrap();
        (entry, view)
    }

    pub fn connected_repo() -> (TempDir, TempDir, Repository) {
        let (dir, repo) = repo();
        git(dir.path(), &["commit", "--allow-empty", "-qm", "Initial"]);
        let remote = tempfile::tempdir().unwrap();
        git(remote.path(), &["init", "--bare", "-b", "main"]);
        git(
            dir.path(),
            &["remote", "add", "origin", remote.path().to_str().unwrap()],
        );
        repo.run_operation(&Operation::Push).unwrap();
        (dir, remote, repo)
    }

    #[test]
    fn whole_file_staging_is_literal_and_unstaging_preserves_unborn_worktree() {
        let (dir, repo) = repo();
        let path = ":(glob)*.txt";
        fs::write(dir.path().join(path), "selected\n").unwrap();
        fs::write(dir.path().join("other.txt"), "not selected\n").unwrap();
        let entry = repo.files().unwrap().remove(0);
        assert_eq!(entry.path, Path::new(path));
        repo.toggle_file(&entry).unwrap();
        let staged = repo.staged_files().unwrap();
        assert_eq!(staged.len(), 1);
        assert_eq!(staged[0].path, entry.path);
        fs::write(dir.path().join(path), "later edits\n").unwrap();
        repo.toggle_file(&staged[0]).unwrap();
        assert!(repo.staged_files().unwrap().is_empty());
        assert_eq!(
            fs::read_to_string(dir.path().join(path)).unwrap(),
            "later edits\n"
        );
    }

    #[test]
    fn hunk_staging_round_trips_quoted_paths_and_missing_newlines() {
        let (dir, repo) = repo();
        let path = dir.path().join("a \"quoted\"\n界.txt");
        let old: String = (1..=40).map(|n| format!("line {n}\r\n")).collect();
        fs::write(&path, &old).unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-qm", "Initial"]);
        let new = old
            .replace("line 2\r\n", "first\r\nextra\r\n")
            .replace("line 39\r\nline 40\r\n", "last");
        fs::write(&path, &new).unwrap();
        let (entry, view) = selected_view(&repo, false);
        assert_eq!(view.hunks.len(), 2);
        repo.toggle_hunk(&entry, &view.patches[1]).unwrap();
        assert_eq!(
            text(repo.blob(":", &entry.path).unwrap()),
            old.replace("line 39\r\nline 40\r\n", "last")
        );
        let (entry, staged) = selected_view(&repo, true);
        repo.toggle_hunk(&entry, &staged.patches[0]).unwrap();
        assert!(repo.staged_files().unwrap().is_empty());
        assert_eq!(fs::read_to_string(&path).unwrap(), new);
        // Stage everything, then unstage only the first hunk; the second stays staged.
        let (entry, _) = selected_view(&repo, false);
        repo.toggle_file(&entry).unwrap();
        let (entry, staged) = selected_view(&repo, true);
        repo.toggle_hunk(&entry, &staged.patches[0]).unwrap();
        assert_eq!(
            text(repo.blob(":", &entry.path).unwrap()),
            old.replace("line 39\r\nline 40\r\n", "last")
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), new);
    }

    #[cfg(unix)]
    #[test]
    fn hunks_support_non_utf8_paths() {
        let (dir, repo) = repo();
        let name = os_from_bytes(b"invalid-\xff.txt");
        fs::write(dir.path().join(&name), "old\n").unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-qm", "Initial"]);
        fs::write(dir.path().join(&name), "new\n").unwrap();
        let (entry, view) = selected_view(&repo, false);
        repo.toggle_hunk(&entry, &view.patches[0]).unwrap();
        assert_eq!(text(repo.blob(":", &entry.path).unwrap()), "new\n");
    }

    #[test]
    fn added_and_deleted_hunks_and_binary_whole_files_round_trip() {
        let (dir, repo) = repo();
        fs::write(dir.path().join("file.txt"), "new text").unwrap();
        let (entry, view) = selected_view(&repo, false);
        repo.toggle_hunk(&entry, &view.patches[0]).unwrap();
        let (entry, view) = selected_view(&repo, true);
        repo.toggle_hunk(&entry, &view.patches[0]).unwrap();
        assert!(repo.staged_files().unwrap().is_empty());
        let (entry, _) = selected_view(&repo, false);
        repo.toggle_file(&entry).unwrap();
        repo.run_operation(&Operation::Commit("Add file".into()))
            .unwrap();
        fs::remove_file(dir.path().join("file.txt")).unwrap();
        let (entry, view) = selected_view(&repo, false);
        repo.toggle_hunk(&entry, &view.patches[0]).unwrap();
        let (entry, view) = selected_view(&repo, true);
        repo.toggle_hunk(&entry, &view.patches[0]).unwrap();
        assert!(!dir.path().join("file.txt").exists());
        assert!(repo.staged_files().unwrap().is_empty());
        fs::write(dir.path().join("file.txt"), [0, 1, 2]).unwrap();
        let entry = repo.files().unwrap().remove(0);
        repo.toggle_file(&entry).unwrap();
        let entry = repo.staged_files().unwrap().remove(0);
        repo.toggle_file(&entry).unwrap();
        assert_eq!(fs::read(dir.path().join("file.txt")).unwrap(), [0, 1, 2]);
    }

    #[test]
    fn commits_untracked_files_when_nothing_is_staged() {
        let (dir, repo) = repo();
        fs::create_dir(dir.path().join("nested")).unwrap();
        fs::write(dir.path().join("nested/new file.txt"), "new\n").unwrap();
        fs::write(dir.path().join(".gitignore"), "ignored\n").unwrap();
        fs::write(dir.path().join("ignored"), "leave ignored\n").unwrap();

        repo.run_operation(&Operation::Commit("Initial changes".into()))
            .unwrap();

        let tree = repo
            .output(&["ls-tree", "-r", "--name-only", "HEAD"])
            .unwrap();
        assert_eq!(tree.stdout, b".gitignore\nnested/new file.txt\n");
        assert_eq!(
            text(
                repo.blob("HEAD:", Path::new("nested/new file.txt"))
                    .unwrap()
            ),
            "new\n"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("ignored")).unwrap(),
            "leave ignored\n"
        );
        assert!(repo.files().unwrap().is_empty());
    }

    #[test]
    fn commits_all_changes_when_nothing_is_staged() {
        let (dir, repo) = repo();
        fs::write(dir.path().join("modified.txt"), "original\n").unwrap();
        fs::write(dir.path().join("deleted.txt"), "delete me\n").unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-qm", "Initial"]);
        fs::write(dir.path().join("modified.txt"), "changed\n").unwrap();
        fs::remove_file(dir.path().join("deleted.txt")).unwrap();
        fs::write(dir.path().join("-new [file].bin"), [0, 1, 2]).unwrap();

        repo.run_operation(&Operation::Commit("All changes".into()))
            .unwrap();

        let tree = repo
            .output(&["ls-tree", "-r", "--name-only", "HEAD"])
            .unwrap();
        assert_eq!(tree.stdout, b"-new [file].bin\nmodified.txt\n");
        assert_eq!(
            text(repo.blob("HEAD:", Path::new("modified.txt")).unwrap()),
            "changed\n"
        );
        assert_eq!(
            repo.output(&["show", "HEAD:-new [file].bin"])
                .unwrap()
                .stdout,
            [0, 1, 2]
        );
        assert!(repo.files().unwrap().is_empty());
    }

    #[test]
    fn automatic_staging_failure_does_not_create_a_commit() {
        let (dir, repo) = repo();
        git(dir.path(), &["commit", "--allow-empty", "-qm", "Initial"]);
        fs::write(dir.path().join("new.txt"), "new\n").unwrap();
        fs::write(repo.git_dir.join("index.lock"), "locked").unwrap();

        let error = repo
            .run_operation(&Operation::Commit("All changes".into()))
            .unwrap_err();

        assert!(format!("{error:#}").contains("index.lock"));
        assert_eq!(
            repo.output(&["log", "-1", "--format=%s"]).unwrap().stdout,
            b"Initial\n"
        );
        assert_eq!(repo.files().unwrap().len(), 1);
        assert!(repo.staged_files().unwrap().is_empty());
    }

    #[test]
    fn empty_commit_message_does_not_stage_changes() {
        let (dir, repo) = repo();
        fs::write(dir.path().join("new.txt"), "new\n").unwrap();

        let error = repo
            .run_operation(&Operation::Commit("  \n".into()))
            .unwrap_err();

        assert_eq!(error.to_string(), "Enter a commit message");
        assert_eq!(repo.files().unwrap().len(), 1);
        assert!(repo.staged_files().unwrap().is_empty());
    }

    #[test]
    fn commits_only_the_index_and_preserves_multiline_messages() {
        let (dir, repo) = repo();
        fs::write(dir.path().join("file.txt"), "staged\n").unwrap();
        git(dir.path(), &["add", "."]);
        fs::write(dir.path().join("file.txt"), "unstaged\n").unwrap();
        fs::write(dir.path().join("untracked.txt"), "untracked\n").unwrap();
        let message = "Commit staged content\n\nA Unicode body: 界\n# Kept as message text";
        repo.run_operation(&Operation::Commit(message.into()))
            .unwrap();
        assert_eq!(
            text(repo.blob("HEAD:", Path::new("file.txt")).unwrap()),
            "staged\n"
        );
        let log = repo.output(&["log", "-1", "--format=%B"]).unwrap();
        assert_eq!(String::from_utf8_lossy(&log.stdout).trim_end(), message);
        assert!(repo.staged_files().unwrap().is_empty());
        assert_eq!(repo.files().unwrap().len(), 2);
        assert!(
            repo.run_operation(&Operation::Commit("  \n".into()))
                .is_err()
        );
    }

    #[test]
    fn push_sets_upstream_and_ignores_matching_or_custom_push_refspecs() {
        let (dir, remote, repo) = connected_repo();
        assert_eq!(
            repo.output(&["config", "--get", "branch.main.remote"])
                .unwrap()
                .stdout,
            b"origin\n"
        );
        git(dir.path(), &["branch", "other"]);
        git(dir.path(), &["tag", "main"]); // Ambiguous short refs must not affect the source branch.
        git(dir.path(), &["config", "push.default", "matching"]);
        git(
            dir.path(),
            &[
                "config",
                "remote.origin.push",
                "refs/heads/other:refs/heads/other",
            ],
        );
        git(dir.path(), &["commit", "--allow-empty", "-qm", "Later"]);
        repo.run_operation(&Operation::Push).unwrap();
        let remote_main = command(remote.path())
            .args(["rev-parse", "refs/heads/main"])
            .output()
            .unwrap();
        assert_eq!(
            remote_main.stdout,
            repo.output(&["rev-parse", "HEAD"]).unwrap().stdout
        );
        let other = command(remote.path())
            .args(["show-ref", "--verify", "refs/heads/other"])
            .output()
            .unwrap();
        assert!(!other.status.success());
    }

    #[test]
    fn pull_fast_forwards_current_branch_and_refuses_divergence() {
        let (dir, remote, repo) = connected_repo();
        let (peer, peer_repo) = self::repo();
        git(
            peer.path(),
            &["remote", "add", "origin", remote.path().to_str().unwrap()],
        );
        git(peer.path(), &["fetch", "origin"]);
        git(peer.path(), &["reset", "--hard", "origin/main"]);
        git(peer.path(), &["branch", "--set-upstream-to", "origin/main"]);
        fs::write(peer.path().join("upstream.txt"), "upstream\n").unwrap();
        git(peer.path(), &["add", "."]);
        git(peer.path(), &["commit", "-qm", "Upstream"]);
        peer_repo.run_operation(&Operation::Push).unwrap();
        repo.run_operation(&Operation::Pull).unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("upstream.txt")).unwrap(),
            "upstream\n"
        );
        git(dir.path(), &["commit", "--allow-empty", "-qm", "Local"]);
        let head = repo.output(&["rev-parse", "HEAD"]).unwrap().stdout;
        git(peer.path(), &["commit", "--allow-empty", "-qm", "Remote"]);
        peer_repo.run_operation(&Operation::Push).unwrap();
        assert!(repo.run_operation(&Operation::Pull).is_err());
        assert_eq!(repo.output(&["rev-parse", "HEAD"]).unwrap().stdout, head);
        assert!(!repo.git_dir.join("MERGE_HEAD").exists());
        assert!(repo.run_operation(&Operation::Push).is_err());
        git(dir.path(), &["checkout", "--detach", "-q"]);
        for operation in [Operation::Pull, Operation::Push] {
            assert!(
                repo.run_operation(&operation)
                    .unwrap_err()
                    .to_string()
                    .contains("detached HEAD")
            );
        }
    }

    #[test]
    fn escapes_terminal_controls() {
        assert_eq!(safe_text("a\n\x1bb"), "a\\n\\u{1b}b");
    }
}
