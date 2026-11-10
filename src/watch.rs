use std::{
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, TryRecvError},
    time::{Duration, Instant},
};

use anyhow::{Result, bail};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher as NotifyWatcher};

use crate::git::Repository;

const DEBOUNCE: Duration = Duration::from_millis(150);
const MAX_DELAY: Duration = Duration::from_millis(750);

pub struct Watcher {
    _watcher: RecommendedWatcher,
    receiver: Receiver<Result<(), String>>,
    first_event: Option<Instant>,
    last_event: Option<Instant>,
}

impl Watcher {
    pub fn new(repo: &Repository) -> Result<Self> {
        // A single invalidation is sufficient, even during a large checkout/build.
        let (sender, receiver) = mpsc::sync_channel(1);
        let git_dirs = vec![repo.git_dir.clone(), repo.common_dir.clone()];
        let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
            let invalidation = match event {
                Ok(event) if relevant(&event, &git_dirs) => Ok(()),
                Ok(_) => return,
                Err(error) => Err(error.to_string()),
            };
            let _ = sender.try_send(invalidation);
        })?;
        watcher.watch(&repo.root, RecursiveMode::Recursive)?;
        if !repo.git_dir.starts_with(&repo.root) {
            watcher.watch(&repo.git_dir, RecursiveMode::Recursive)?;
        }
        if !repo.common_dir.starts_with(&repo.root) && !repo.common_dir.starts_with(&repo.git_dir) {
            watcher.watch(&repo.common_dir, RecursiveMode::Recursive)?;
        }
        Ok(Self {
            _watcher: watcher,
            receiver,
            first_event: None,
            last_event: None,
        })
    }

    pub fn poll(&mut self) -> Result<bool> {
        let now = Instant::now();
        loop {
            match self.receiver.try_recv() {
                Ok(Ok(())) => {
                    self.first_event.get_or_insert(now);
                    self.last_event = Some(now);
                }
                Ok(Err(error)) => bail!("{error}"),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => bail!("Filesystem watcher disconnected"),
            }
        }
        let ready = self
            .last_event
            .is_some_and(|last| now.duration_since(last) >= DEBOUNCE)
            || self
                .first_event
                .is_some_and(|first| now.duration_since(first) >= MAX_DELAY);
        if ready {
            self.first_event = None;
            self.last_event = None;
        }
        Ok(ready)
    }
}

fn relevant(event: &Event, git_dirs: &[PathBuf]) -> bool {
    // Reading files during refresh must never cause another refresh.
    if matches!(event.kind, EventKind::Access(_)) {
        return false;
    }
    event.paths.is_empty()
        || event.paths.iter().any(|path| {
            for dir in git_dirs {
                if let Ok(relative) = path.strip_prefix(dir) {
                    return git_metadata(relative);
                }
            }
            true
        })
}

fn git_metadata(path: &Path) -> bool {
    path == Path::new("index")
        || path == Path::new("HEAD")
        || path == Path::new("packed-refs")
        || path == Path::new("config")
        || path.starts_with("refs")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::tests::{git, repo};
    use notify::event::{AccessKind, ModifyKind};

    fn wait_for_change(watcher: &mut Watcher) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if watcher.poll().unwrap() {
                return;
            }
            assert!(Instant::now() < deadline, "Watcher did not detect a change");
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    #[test]
    fn ignores_access_and_git_object_churn() {
        let dir = PathBuf::from("/repo/.git");
        let event = Event::new(EventKind::Access(AccessKind::Any)).add_path("/repo/main.rs".into());
        assert!(!relevant(&event, std::slice::from_ref(&dir)));
        let event = Event::new(EventKind::Modify(ModifyKind::Any))
            .add_path("/repo/.git/objects/aa/xyz".into());
        assert!(!relevant(&event, std::slice::from_ref(&dir)));
        let event =
            Event::new(EventKind::Modify(ModifyKind::Any)).add_path("/repo/.git/index".into());
        assert!(relevant(&event, &[dir]));
    }

    #[test]
    fn watches_linked_worktree_metadata_outside_its_root() {
        let (dir, _) = repo();
        git(dir.path(), &["commit", "--allow-empty", "-qm", "Initial"]);
        let linked = tempfile::tempdir().unwrap();
        let path = linked.path().join("linked");
        git(
            dir.path(),
            &["worktree", "add", "-b", "linked", path.to_str().unwrap()],
        );
        let repo = Repository::discover(&path).unwrap();
        let mut watcher = Watcher::new(&repo).unwrap();
        std::fs::write(path.join("test.rs"), "fn main() {}\n").unwrap();
        wait_for_change(&mut watcher);
        git(&path, &["add", "."]);
        wait_for_change(&mut watcher);
        git(&path, &["checkout", "-qb", "another"]);
        wait_for_change(&mut watcher);
    }

    #[test]
    fn detects_commits_and_packed_refs() {
        let (dir, repo) = repo();
        let mut watcher = Watcher::new(&repo).unwrap();
        git(dir.path(), &["commit", "--allow-empty", "-qm", "Initial"]);
        wait_for_change(&mut watcher);
        assert_eq!(repo.branch().unwrap(), "main");
        assert!(repo.files().unwrap().is_empty());
        git(dir.path(), &["pack-refs", "--all"]);
        wait_for_change(&mut watcher);
    }

    #[test]
    fn detects_file_changes_and_index_updates() {
        let (dir, repo) = repo();
        let mut watcher = Watcher::new(&repo).unwrap();
        std::fs::write(dir.path().join("new.rs"), "fn main() {}\n").unwrap();
        wait_for_change(&mut watcher);
        git(dir.path(), &["add", "."]);
        wait_for_change(&mut watcher);
        std::fs::rename(dir.path().join("new.rs"), dir.path().join("renamed.rs")).unwrap();
        wait_for_change(&mut watcher);
        std::fs::remove_file(dir.path().join("renamed.rs")).unwrap();
        wait_for_change(&mut watcher);
    }
}
