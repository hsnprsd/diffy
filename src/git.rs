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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Working,
    Staged,
    // Pin both revisions so the file list and previews always use the same commits.
    Branch {
        merge_base: String,
        head: String,
    },
}

impl Mode {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Working => "Working tree",
            Self::Staged => "Staged",
            Self::Branch { .. } => "Branch",
        }
    }

    pub fn is_branch(&self) -> bool {
        matches!(self, Self::Branch { .. })
    }
}

#[derive(Clone, Debug)]
pub struct FileEntry {
    pub path: PathBuf,
    pub status: char,
    pub submodule: bool,
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
}

impl Operation {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Commit(_) => "Committing",
            Self::Pull => "Pulling",
            Self::Push => "Pushing",
        }
    }
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

    pub fn branches(&self) -> Result<Vec<String>> {
        let output = self.output(&[
            "for-each-ref",
            "--sort=refname",
            "--format=%(refname:short)%00%(symref)",
            "refs/heads/",
            "refs/remotes/",
        ])?;
        ensure_success(&output)?;
        let refs = std::str::from_utf8(&output.stdout).context("Branch names must be UTF-8")?;
        Ok(refs
            .lines()
            .filter_map(|line| {
                let (name, symbolic) = line.split_once('\0')?;
                symbolic.is_empty().then(|| name.to_owned())
            })
            .collect())
    }

    pub fn compare_branch(&self, target: &str) -> Result<Mode> {
        anyhow::ensure!(
            !target.is_empty(),
            "Enter a base branch, e.g. main or origin/main"
        );
        let resolve = |revision: &str| -> Result<String> {
            let commit = format!("{revision}^{{commit}}");
            let output = self.output(&["rev-parse", "--verify", "--end-of-options", &commit])?;
            ensure_success(&output)
                .with_context(|| format!("Cannot resolve revision {revision:?}"))?;
            Ok(std::str::from_utf8(trim_newline(&output.stdout))?.to_owned())
        };
        let target_commit = resolve(target)?;
        let head =
            resolve("HEAD").context("Branch comparison requires at least one commit on HEAD")?;
        let output = self.output(&["merge-base", &target_commit, &head])?;
        if output.status.code() == Some(1) {
            bail!(
                "No common ancestor between {target:?} and HEAD (history may be unrelated or shallow)"
            );
        }
        ensure_success(&output)?;
        let merge_base = std::str::from_utf8(trim_newline(&output.stdout))?.to_owned();
        Ok(Mode::Branch { merge_base, head })
    }

    pub fn files(&self, mode: Mode) -> Result<Vec<FileEntry>> {
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
        match &mode {
            Mode::Working => {}
            Mode::Staged => args.push("--cached"),
            Mode::Branch { merge_base, head } => args.extend([merge_base.as_str(), head.as_str()]),
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
                });
        }
        if mode == Mode::Working {
            let output =
                self.output(&["ls-files", "--others", "--exclude-standard", "-z", "--"])?;
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
                });
            }
        }
        Ok(files.into_values().collect())
    }

    pub fn contents(&self, entry: &FileEntry, mode: Mode) -> Result<(Content, Content)> {
        if entry.submodule {
            return Ok((
                Content::Notice("Submodule — inspect changes inside its repository"),
                Content::empty(),
            ));
        }
        if entry.status == 'U' && mode == Mode::Staged {
            return Ok((
                Content::Notice("Unmerged index — inspect conflict markers in Working tree"),
                Content::empty(),
            ));
        }
        let old = if matches!(entry.status, 'A' | '?') {
            Content::empty()
        } else {
            let revision = match (&mode, entry.status) {
                (Mode::Working, 'U') => ":2:".to_owned(),
                (Mode::Working, _) => ":".to_owned(),
                (Mode::Staged, _) => "HEAD:".to_owned(),
                (Mode::Branch { merge_base, .. }, _) => format!("{merge_base}:"),
            };
            self.blob(&revision, &entry.path)?
        };
        let new = if entry.status == 'D' {
            Content::empty()
        } else {
            match mode {
                Mode::Working => self.worktree_file(&entry.path)?,
                Mode::Staged => self.blob(":", &entry.path)?,
                Mode::Branch { head, .. } => self.blob(&format!("{head}:"), &entry.path)?,
            }
        };
        Ok((old, new))
    }

    pub fn toggle_file(&self, entry: &FileEntry, mode: &Mode) -> Result<()> {
        let mut cmd = command(&self.root);
        match mode {
            Mode::Working => cmd.args(["add", "-A"]),
            // Reset also works on unborn branches and never changes files on disk.
            Mode::Staged => cmd.args(["reset", "-q"]),
            Mode::Branch { .. } => {
                bail!("Switch to Working tree or Staged to stage/unstage changes")
            }
        };
        let output = cmd.arg("--").arg(&entry.path).output()?;
        ensure_success(&output)
    }

    pub fn toggle_hunk(&self, entry: &FileEntry, mode: &Mode, hunk: &str) -> Result<()> {
        anyhow::ensure!(!mode.is_branch(), "Branch comparisons are read-only");
        anyhow::ensure!(
            entry.status != 'U' && !entry.submodule,
            "Use the Files pane to stage/unstage conflicts or submodules as a whole"
        );
        // Added/deleted files contain exactly one hunk, covering their entire contents.
        if matches!(entry.status, 'A' | '?' | 'D') {
            return self.toggle_file(entry, mode);
        }
        let patch = format!(
            "--- {}\n+++ {}\n{hunk}",
            patch_path("a/", &entry.path),
            patch_path("b/", &entry.path),
        );
        let mut cmd = command(&self.root);
        cmd.args(["apply", "--cached", "--whitespace=nowarn"]);
        if *mode == Mode::Staged {
            cmd.arg("--reverse");
        }
        let output = with_input(cmd, patch.as_bytes())?;
        ensure_success(&output)
    }

    pub fn run_operation(&self, operation: &Operation) -> Result<String> {
        let mut cmd = command(&self.root);
        // Capture command output and disable Git's terminal prompts. Authentication
        // and signing can still use configured agents or askpass programs.
        cmd.env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_EDITOR", "true")
            .env("GIT_SEQUENCE_EDITOR", "true");
        let output = match operation {
            Operation::Commit(message) => {
                anyhow::ensure!(!message.trim().is_empty(), "Enter a commit message");
                cmd.args(["commit", "--cleanup=whitespace", "-F", "-"]);
                with_input(cmd, message.as_bytes())?
            }
            Operation::Pull | Operation::Push => {
                let output = self.output(&["symbolic-ref", "--quiet", "HEAD"])?;
                anyhow::ensure!(output.status.success(), "Cannot pull/push a detached HEAD");
                let branch_ref = std::str::from_utf8(trim_newline(&output.stdout))?;
                let branch = branch_ref
                    .strip_prefix("refs/heads/")
                    .context("HEAD must point to a local branch")?;
                if matches!(operation, Operation::Pull) {
                    cmd.args(["pull", "--ff-only", "--no-rebase"]);
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
                    cmd.arg("push");
                    if upstream_remote.is_none() {
                        cmd.arg("--set-upstream");
                    }
                    // An explicit refspec prevents push.default=matching or remote.push
                    // from accidentally pushing branches other than the current one.
                    cmd.arg("--")
                        .arg(remote)
                        .arg(format!("{branch_ref}:{target}"));
                }
                cmd.output()?
            }
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

    pub fn diverged_repo() -> (TempDir, Repository) {
        let (dir, repo) = repo();
        fs::write(dir.path().join("changed.rs"), "fn base() {}\n").unwrap();
        fs::write(dir.path().join("deleted.rs"), "removed\n").unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-qm", "Base"]);
        git(dir.path(), &["checkout", "-qb", "feature"]);
        fs::write(dir.path().join("changed.rs"), "fn feature() {}\n").unwrap();
        fs::write(dir.path().join("added.rs"), "fn added() {}\n").unwrap();
        fs::remove_file(dir.path().join("deleted.rs")).unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-qm", "Feature"]);
        git(dir.path(), &["checkout", "-q", "main"]);
        fs::write(dir.path().join("changed.rs"), "fn upstream() {}\n").unwrap();
        fs::write(dir.path().join("upstream.rs"), "upstream only\n").unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-qm", "Upstream"]);
        git(dir.path(), &["checkout", "-q", "feature"]);
        (dir, repo)
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
        assert!(repo.files(Mode::Staged).unwrap().is_empty());
        assert_eq!(repo.files(Mode::Working).unwrap().len(), 2);
        git(dir.path(), &["add", "new.rs"]);
        let staged = repo.files(Mode::Staged).unwrap();
        assert_eq!(staged[0].status, 'A');
        let (old, new) = repo.contents(&staged[0], Mode::Staged).unwrap();
        assert_eq!(text(old), "");
        assert_eq!(text(new), "fn main() {}\n");
    }

    #[test]
    fn working_and_staged_are_separate() {
        let (dir, repo) = repo();
        let path = dir.path().join("space and\nnewline.rs");
        fs::write(&path, "original\n").unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-qm", "Initial"]);
        fs::write(&path, "staged\n").unwrap();
        git(dir.path(), &["add", "."]);
        fs::write(&path, "working\n").unwrap();
        for (mode, expected) in [
            (Mode::Working, ("staged\n", "working\n")),
            (Mode::Staged, ("original\n", "staged\n")),
        ] {
            let files = repo.files(mode.clone()).unwrap();
            assert_eq!(files.len(), 1);
            let (old, new) = repo.contents(&files[0], mode).unwrap();
            assert_eq!(
                (text(old), text(new)),
                (expected.0.into(), expected.1.into())
            );
        }
        fs::remove_file(path).unwrap();
        let files = repo.files(Mode::Working).unwrap();
        assert_eq!(files[0].status, 'D');
        assert_eq!(text(repo.contents(&files[0], Mode::Working).unwrap().1), "");
    }

    #[test]
    fn binary_and_large_files_have_notices() {
        let (dir, repo) = repo();
        fs::write(dir.path().join("binary"), [0, 1, 2]).unwrap();
        fs::write(dir.path().join("large"), vec![b'a'; MAX_FILE_BYTES + 1]).unwrap();
        for entry in repo.files(Mode::Working).unwrap() {
            assert!(matches!(
                repo.contents(&entry, Mode::Working).unwrap().1,
                Content::Notice(_)
            ));
        }
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
        assert_eq!(repo.branches().unwrap(), ["linked", "main"]);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_not_followed() {
        let (dir, repo) = repo();
        std::os::unix::fs::symlink("/nonexistent/target", dir.path().join("link")).unwrap();
        let files = repo.files(Mode::Working).unwrap();
        assert_eq!(
            text(repo.contents(&files[0], Mode::Working).unwrap().1),
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
        let files = repo.files(Mode::Working).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].status, 'U');
        let (old, new) = repo.contents(&files[0], Mode::Working).unwrap();
        assert_eq!(text(old), "ours\n");
        assert!(text(new).contains("<<<<<<<"));
        assert!(matches!(
            repo.contents(&files[0], Mode::Staged).unwrap().0,
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
        let files = repo.files(Mode::Staged).unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].submodule);
        assert!(matches!(
            repo.contents(&files[0], Mode::Staged).unwrap().0,
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
        let files = repo.files(Mode::Working).unwrap();
        assert_eq!(files[0].path.as_os_str(), name);
        let (old, new) = repo.contents(&files[0], Mode::Working).unwrap();
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
        let files = repo.files(Mode::Staged).unwrap();
        assert_eq!(files[0].status, 'D');
        let (old, new) = repo.contents(&files[0], Mode::Staged).unwrap();
        assert_eq!((text(old), text(new)), ("goodbye\n".into(), "".into()));
    }

    #[test]
    fn lists_local_and_remote_branches_without_tags_or_symbolic_aliases() {
        let (dir, repo) = repo();
        assert!(repo.branches().unwrap().is_empty());
        git(dir.path(), &["commit", "--allow-empty", "-qm", "Initial"]);
        git(dir.path(), &["branch", "feature/界"]);
        git(
            dir.path(),
            &["update-ref", "refs/remotes/origin/main", "HEAD"],
        );
        git(
            dir.path(),
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        );
        git(dir.path(), &["tag", "v1"]);
        git(dir.path(), &["pack-refs", "--all"]);
        assert_eq!(
            repo.branches().unwrap(),
            ["feature/界", "main", "origin/main"]
        );
        git(dir.path(), &["checkout", "--detach", "-q"]);
        assert_eq!(
            repo.branches().unwrap(),
            ["feature/界", "main", "origin/main"]
        );
    }

    #[test]
    fn listed_branches_disambiguate_colliding_ref_names() {
        let (dir, repo) = repo();
        git(dir.path(), &["commit", "--allow-empty", "-qm", "Initial"]);
        git(dir.path(), &["tag", "main"]);
        git(dir.path(), &["branch", "origin/main", "HEAD"]);
        git(
            dir.path(),
            &["update-ref", "refs/remotes/origin/main", "HEAD"],
        );
        let branches = repo.branches().unwrap();
        assert_eq!(
            branches,
            ["heads/main", "heads/origin/main", "remotes/origin/main"]
        );
        for branch in branches {
            assert!(repo.compare_branch(&branch).is_ok());
        }
    }

    #[test]
    fn branch_diff_uses_merge_base_and_excludes_all_uncommitted_changes() {
        let (dir, repo) = diverged_repo();
        fs::write(dir.path().join("changed.rs"), "staged\n").unwrap();
        git(dir.path(), &["add", "."]);
        fs::write(dir.path().join("changed.rs"), "unstaged\n").unwrap();
        fs::write(dir.path().join("untracked.rs"), "untracked\n").unwrap();
        let mode = repo.compare_branch("main").unwrap();
        let files = repo.files(mode.clone()).unwrap();
        let paths: Vec<_> = files
            .iter()
            .map(|file| file.path.to_str().unwrap())
            .collect();
        assert_eq!(paths, ["added.rs", "changed.rs", "deleted.rs"]);
        let expected = [
            ("", "fn added() {}\n"),
            ("fn base() {}\n", "fn feature() {}\n"),
            ("removed\n", ""),
        ];
        for (file, (before, after)) in files.iter().zip(expected) {
            let (old, new) = repo.contents(file, mode.clone()).unwrap();
            assert_eq!((text(old), text(new)), (before.into(), after.into()));
        }
        let output = repo
            .output(&["diff", "--name-only", "-z", "main...HEAD", "--"])
            .unwrap();
        ensure_success(&output).unwrap();
        let git_paths: Vec<_> = output
            .stdout
            .split(|&b| b == 0)
            .filter(|p| !p.is_empty())
            .map(|p| std::str::from_utf8(p).unwrap())
            .collect();
        assert_eq!(paths, git_paths);
    }

    #[test]
    fn branch_snapshots_remain_consistent_when_head_or_base_moves() {
        let (dir, repo) = diverged_repo();
        let mode = repo.compare_branch("main").unwrap();
        let files = repo.files(mode.clone()).unwrap();
        let changed = files
            .iter()
            .find(|file| file.path == Path::new("changed.rs"))
            .unwrap();
        fs::write(dir.path().join("changed.rs"), "fn later() {}\n").unwrap();
        git(dir.path(), &["commit", "-qam", "Later"]);
        git(dir.path(), &["branch", "-f", "main", "HEAD"]);
        let (old, new) = repo.contents(changed, mode).unwrap();
        assert_eq!(
            (text(old), text(new)),
            ("fn base() {}\n".into(), "fn feature() {}\n".into())
        );
        assert!(
            repo.files(repo.compare_branch("main").unwrap())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn branch_diff_accepts_remote_refs_and_detached_head() {
        let (dir, repo) = diverged_repo();
        git(
            dir.path(),
            &["update-ref", "refs/remotes/origin/main", "main"],
        );
        git(dir.path(), &["checkout", "--detach", "-q"]);
        let mode = repo.compare_branch("origin/main").unwrap();
        assert_eq!(repo.files(mode).unwrap().len(), 3);
    }

    #[test]
    fn invalid_unborn_and_unrelated_branch_comparisons_report_errors() {
        let (dir, repo) = diverged_repo();
        for target in ["", "missing", "--help"] {
            assert!(repo.compare_branch(target).is_err());
        }
        git(dir.path(), &["checkout", "--orphan", "unborn", "-q"]);
        let error = format!("{:#}", repo.compare_branch("main").unwrap_err());
        assert!(error.contains("requires at least one commit"));
        git(dir.path(), &["commit", "-qm", "Unrelated"]);
        let error = format!("{:#}", repo.compare_branch("main").unwrap_err());
        assert!(error.contains("No common ancestor"));
    }

    fn selected_view(repo: &Repository, mode: Mode) -> (FileEntry, crate::diff::DiffView) {
        let entry = repo.files(mode.clone()).unwrap().remove(0);
        let (old, new) = repo.contents(&entry, mode).unwrap();
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
        let entry = repo.files(Mode::Working).unwrap().remove(0);
        assert_eq!(entry.path, Path::new(path));
        repo.toggle_file(&entry, &Mode::Working).unwrap();
        let staged = repo.files(Mode::Staged).unwrap();
        assert_eq!(staged.len(), 1);
        assert_eq!(staged[0].path, entry.path);
        fs::write(dir.path().join(path), "later edits\n").unwrap();
        repo.toggle_file(&staged[0], &Mode::Staged).unwrap();
        assert!(repo.files(Mode::Staged).unwrap().is_empty());
        assert_eq!(
            fs::read_to_string(dir.path().join(path)).unwrap(),
            "later edits\n"
        );
        assert!(
            repo.toggle_file(
                &entry,
                &Mode::Branch {
                    merge_base: String::new(),
                    head: String::new()
                }
            )
            .is_err()
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
        let (entry, view) = selected_view(&repo, Mode::Working);
        assert_eq!(view.hunks.len(), 2);
        repo.toggle_hunk(&entry, &Mode::Working, &view.patches[1])
            .unwrap();
        assert_eq!(
            text(repo.blob(":", &entry.path).unwrap()),
            old.replace("line 39\r\nline 40\r\n", "last")
        );
        let (entry, staged) = selected_view(&repo, Mode::Staged);
        repo.toggle_hunk(&entry, &Mode::Staged, &staged.patches[0])
            .unwrap();
        assert!(repo.files(Mode::Staged).unwrap().is_empty());
        assert_eq!(fs::read_to_string(&path).unwrap(), new);
        // Stage everything, then unstage only the first hunk; the second stays staged.
        repo.toggle_file(&entry, &Mode::Working).unwrap();
        let (entry, staged) = selected_view(&repo, Mode::Staged);
        repo.toggle_hunk(&entry, &Mode::Staged, &staged.patches[0])
            .unwrap();
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
        let (entry, view) = selected_view(&repo, Mode::Working);
        repo.toggle_hunk(&entry, &Mode::Working, &view.patches[0])
            .unwrap();
        assert_eq!(text(repo.blob(":", &entry.path).unwrap()), "new\n");
    }

    #[test]
    fn added_and_deleted_hunks_and_binary_whole_files_round_trip() {
        let (dir, repo) = repo();
        fs::write(dir.path().join("file.txt"), "new text").unwrap();
        let (entry, view) = selected_view(&repo, Mode::Working);
        repo.toggle_hunk(&entry, &Mode::Working, &view.patches[0])
            .unwrap();
        let (entry, view) = selected_view(&repo, Mode::Staged);
        repo.toggle_hunk(&entry, &Mode::Staged, &view.patches[0])
            .unwrap();
        assert!(repo.files(Mode::Staged).unwrap().is_empty());
        repo.toggle_file(&entry, &Mode::Working).unwrap();
        repo.run_operation(&Operation::Commit("Add file".into()))
            .unwrap();
        fs::remove_file(dir.path().join("file.txt")).unwrap();
        let (entry, view) = selected_view(&repo, Mode::Working);
        repo.toggle_hunk(&entry, &Mode::Working, &view.patches[0])
            .unwrap();
        let (entry, view) = selected_view(&repo, Mode::Staged);
        repo.toggle_hunk(&entry, &Mode::Staged, &view.patches[0])
            .unwrap();
        assert!(!dir.path().join("file.txt").exists());
        assert!(repo.files(Mode::Staged).unwrap().is_empty());
        fs::write(dir.path().join("file.txt"), [0, 1, 2]).unwrap();
        let entry = repo.files(Mode::Working).unwrap().remove(0);
        repo.toggle_file(&entry, &Mode::Working).unwrap();
        repo.toggle_file(&entry, &Mode::Staged).unwrap();
        assert_eq!(fs::read(dir.path().join("file.txt")).unwrap(), [0, 1, 2]);
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
        assert!(repo.files(Mode::Staged).unwrap().is_empty());
        assert_eq!(repo.files(Mode::Working).unwrap().len(), 2);
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
