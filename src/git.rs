use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Output},
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

#[derive(Debug)]
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
        .args(["--no-pager", "-c", "color.ui=false"]);
    command
}

fn ensure_success(output: &Output) -> Result<()> {
    if !output.status.success() {
        bail!("Git: {}", String::from_utf8_lossy(&output.stderr).trim());
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

    #[test]
    fn escapes_terminal_controls() {
        assert_eq!(safe_text("a\n\x1bb"), "a\\n\\u{1b}b");
    }
}
