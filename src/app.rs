use std::path::PathBuf;

use anyhow::{Context, Result};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::ListState;

use crate::{
    diff::DiffView,
    git::{FileEntry, Mode, Repository},
    highlight::SyntaxHighlighter,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Files,
    Diff,
}

pub struct App {
    pub repo: Repository,
    pub mode: Mode,
    pub branch: String,
    pub base_branch: Option<String>,
    pub base_input: Option<String>,
    pub base_error: Option<String>,
    pub watching: bool,
    pub files: Vec<FileEntry>,
    pub visible: Vec<usize>,
    pub list_state: ListState,
    pub view: DiffView,
    pub focus: Focus,
    pub scroll: usize,
    pub horizontal: usize,
    pub viewport_height: usize,
    pub viewport_width: usize,
    pub filter: String,
    pub editing_filter: bool,
    pub show_help: bool,
    pub error: Option<String>,
    highlighter: SyntaxHighlighter,
}

impl App {
    pub fn new(repo: Repository, watching: bool, base_branch: Option<String>) -> Result<Self> {
        let branch = repo.branch()?;
        let mode = match &base_branch {
            Some(target) => repo.compare_branch(target)?,
            None => Mode::Working,
        };
        let files = repo.files(mode.clone())?;
        let visible = (0..files.len()).collect();
        let mut app = Self {
            repo,
            branch,
            files,
            visible,
            watching,
            mode,
            base_branch,
            base_input: None,
            base_error: None,
            list_state: ListState::default(),
            view: DiffView::default(),
            focus: Focus::Files,
            scroll: 0,
            horizontal: 0,
            viewport_height: 20,
            viewport_width: 80,
            filter: String::new(),
            editing_filter: false,
            show_help: false,
            error: None,
            highlighter: SyntaxHighlighter::default(),
        };
        app.list_state.select(if app.visible.is_empty() {
            None
        } else {
            Some(0)
        });
        app.load_selected();
        Ok(app)
    }

    pub fn selected(&self) -> Option<&FileEntry> {
        self.list_state
            .selected()
            .and_then(|selected| self.visible.get(selected))
            .and_then(|&index| self.files.get(index))
    }

    pub fn refresh(&mut self) {
        let result = (|| {
            let mode = if self.mode.is_branch() {
                self.repo.compare_branch(
                    self.base_branch
                        .as_deref()
                        .context("No base branch selected")?,
                )?
            } else {
                self.mode.clone()
            };
            self.update(mode, false)
        })();
        if let Err(error) = result {
            self.error = Some(format!(
                "Refresh: {error:#}; showing last snapshot; r retries"
            ));
        }
    }

    fn update(&mut self, mode: Mode, reset_scroll: bool) -> Result<()> {
        // Read everything before replacing state, so failed comparisons keep the old view intact.
        let branch = self.repo.branch()?;
        let files = self.repo.files(mode.clone())?;
        let selected = self.selected().map(|entry| entry.path.clone());
        self.branch = branch;
        self.mode = mode;
        self.files = files;
        self.error = None;
        self.filter_files(selected);
        if reset_scroll {
            self.scroll = 0;
            self.horizontal = 0;
        }
        self.load_selected();
        Ok(())
    }

    pub fn compare_with(&mut self, target: &str) -> Result<()> {
        let mode = self.repo.compare_branch(target)?;
        self.update(mode, true)?;
        self.base_branch = Some(target.to_owned());
        Ok(())
    }

    fn cycle_mode(&mut self) -> Result<()> {
        let mode = match &self.mode {
            Mode::Working => Mode::Staged,
            Mode::Staged => match &self.base_branch {
                Some(target) => self.repo.compare_branch(target)?,
                None => Mode::Working,
            },
            Mode::Branch { .. } => Mode::Working,
        };
        self.update(mode, true)
    }

    fn filter_files(&mut self, selected: Option<PathBuf>) {
        let needle = self.filter.to_lowercase();
        self.visible = self
            .files
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                entry
                    .path
                    .to_string_lossy()
                    .to_lowercase()
                    .contains(&needle)
            })
            .map(|(index, _)| index)
            .collect();
        let position = selected.as_ref().and_then(|path| {
            self.visible
                .iter()
                .position(|&index| &self.files[index].path == path)
        });
        self.list_state
            .select(position.or_else(|| (!self.visible.is_empty()).then_some(0)));
        if self.selected().map(|entry| &entry.path) != selected.as_ref() {
            self.scroll = 0;
            self.horizontal = 0;
        }
    }

    fn load_selected(&mut self) {
        let Some(entry) = self.selected().cloned() else {
            self.view = DiffView::notice(if !self.files.is_empty() {
                "No files match the filter"
            } else if self.mode.is_branch() {
                "No committed changes since the merge base"
            } else {
                "No changes — working tree/index is clean"
            });
            self.scroll = 0;
            return;
        };
        match self
            .repo
            .contents(&entry, self.mode.clone())
            .and_then(|(old, new)| DiffView::build(&entry.path, old, new, &mut self.highlighter))
        {
            Ok(view) => self.view = view,
            Err(error) => {
                self.view = DiffView::notice("Could not load this file — press r to retry");
                self.error = Some(format!("{}: {error:#}", entry.path.display()));
            }
        }
        self.clamp_scroll();
    }

    pub fn clamp_scroll(&mut self) {
        self.scroll = self.scroll.min(
            self.view
                .rows
                .len()
                .saturating_sub(self.viewport_height.max(1)),
        );
    }

    fn move_file(&mut self, delta: isize) {
        if self.visible.is_empty() {
            return;
        }
        let selected = self.list_state.selected().unwrap_or(0);
        let next = selected
            .saturating_add_signed(delta)
            .min(self.visible.len() - 1);
        if next != selected {
            self.list_state.select(Some(next));
            self.scroll = 0;
            self.horizontal = 0;
            self.error = None;
            self.load_selected();
        }
    }

    fn vertical(&mut self, delta: isize) {
        if self.focus == Focus::Files {
            self.move_file(delta);
        } else {
            self.scroll = self.scroll.saturating_add_signed(delta);
            self.clamp_scroll();
        }
    }

    fn hunk(&mut self, next: bool) {
        self.focus = Focus::Diff;
        let target = if next {
            self.view
                .hunks
                .iter()
                .copied()
                .find(|&row| row > self.scroll)
        } else {
            self.view
                .hunks
                .iter()
                .copied()
                .rev()
                .find(|&row| row < self.scroll)
        };
        if let Some(target) = target {
            self.scroll = target;
            self.clamp_scroll();
        }
    }

    /// Returns true when the user requests exit. Never writes to the repository.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return true;
        }
        if self.show_help {
            self.show_help = false;
            return false;
        }
        if let Some(input) = &mut self.base_input {
            match key.code {
                KeyCode::Esc => {
                    self.base_input = None;
                    self.base_error = None;
                }
                KeyCode::Enter => {
                    let target = input.trim().to_owned();
                    match self.compare_with(&target) {
                        Ok(()) => {
                            self.base_input = None;
                            self.base_error = None;
                        }
                        Err(error) => self.base_error = Some(format!("{error:#}")),
                    }
                }
                KeyCode::Backspace => {
                    input.pop();
                    self.base_error = None;
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    input.clear();
                    self.base_error = None;
                }
                KeyCode::Char(ch)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    input.push(ch);
                    self.base_error = None;
                }
                _ => {}
            }
            return false;
        }
        if self.editing_filter {
            let selected = self.selected().map(|entry| entry.path.clone());
            match key.code {
                KeyCode::Enter | KeyCode::Esc => self.editing_filter = false,
                KeyCode::Backspace => {
                    self.filter.pop();
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.filter.clear()
                }
                KeyCode::Char(ch)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    self.filter.push(ch)
                }
                _ => return false,
            }
            self.filter_files(selected);
            self.load_selected();
            return false;
        }
        match key.code {
            KeyCode::Char('q') => return true,
            KeyCode::Char('?') | KeyCode::F(1) => self.show_help = true,
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = if self.focus == Focus::Files {
                    Focus::Diff
                } else {
                    Focus::Files
                };
            }
            KeyCode::Enter => self.focus = Focus::Diff,
            KeyCode::Char('j') | KeyCode::Down => self.vertical(1),
            KeyCode::Char('k') | KeyCode::Up => self.vertical(-1),
            KeyCode::Char('n') => self.move_file(1),
            KeyCode::Char('p') => self.move_file(-1),
            KeyCode::PageDown | KeyCode::Char('d') => {
                self.vertical(self.viewport_height.saturating_sub(2).max(1) as isize)
            }
            KeyCode::PageUp | KeyCode::Char('u') => {
                self.vertical(-(self.viewport_height.saturating_sub(2).max(1) as isize))
            }
            KeyCode::Home | KeyCode::Char('g') => self.vertical(-(isize::MAX)),
            KeyCode::End | KeyCode::Char('G') => self.vertical(isize::MAX),
            KeyCode::Char('h') | KeyCode::Left => {
                self.horizontal = self.horizontal.saturating_sub(4)
            }
            KeyCode::Char('l') | KeyCode::Right => {
                self.horizontal = self.horizontal.saturating_add(4).min(self.max_horizontal())
            }
            KeyCode::Char(']') => self.hunk(true),
            KeyCode::Char('[') => self.hunk(false),
            KeyCode::Char('r') => self.refresh(),
            KeyCode::Char('s') => {
                if let Err(error) = self.cycle_mode() {
                    self.error = Some(format!("Compare: {error:#}"));
                }
            }
            KeyCode::Char('b') => {
                self.base_input = Some(self.base_branch.clone().unwrap_or_default());
                self.base_error = None;
            }
            KeyCode::Char('/') => self.editing_filter = true,
            KeyCode::Esc => {
                let selected = self.selected().map(|entry| entry.path.clone());
                self.filter.clear();
                self.filter_files(selected);
                self.load_selected();
            }
            _ => {}
        }
        false
    }

    fn max_horizontal(&self) -> usize {
        self.view
            .rows
            .iter()
            .map(|row| row.spans.iter().map(|span| span.width()).sum::<usize>())
            .max()
            .unwrap_or(0)
            .saturating_sub(
                self.viewport_width
                    .saturating_sub(2 * self.view.number_width.max(1) + 7),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::tests::{diverged_repo, git, repo};

    fn key(ch: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE)
    }

    #[test]
    fn filtering_navigation_and_refresh_preserve_selection() {
        let (dir, repo) = repo();
        std::fs::write(dir.path().join("a.rs"), "fn a() {}\n").unwrap();
        std::fs::write(dir.path().join("b.py"), "print(1)\n").unwrap();
        let mut app = App::new(repo, false, None).unwrap();
        app.handle_key(key('n'));
        assert_eq!(app.selected().unwrap().path, PathBuf::from("b.py"));
        std::fs::write(dir.path().join("0.txt"), "new file").unwrap();
        app.refresh();
        assert_eq!(app.selected().unwrap().path, PathBuf::from("b.py"));
        app.handle_key(key('/'));
        for ch in ".rs".chars() {
            app.handle_key(key(ch));
        }
        assert_eq!(app.visible.len(), 1);
        assert_eq!(app.selected().unwrap().path, PathBuf::from("a.rs"));
        assert!(!app.handle_key(key('q'))); // q is part of the filter, not quit.
        assert!(app.visible.is_empty());
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.visible.len(), 3);
    }

    #[test]
    fn base_prompt_validates_cancels_and_applies_without_changing_checkout() {
        let (_dir, repo) = diverged_repo();
        let mut app = App::new(repo, false, None).unwrap();
        app.handle_key(key('b'));
        for ch in "missing".chars() {
            app.handle_key(key(ch));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.base_error.as_ref().unwrap().contains("Cannot resolve"));
        assert_eq!(app.mode, Mode::Working);
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.base_input.is_none());
        assert!(app.base_error.is_none());
        app.handle_key(key('b'));
        for ch in "main".chars() {
            app.handle_key(key(ch));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.base_input.is_none());
        assert!(app.mode.is_branch());
        assert_eq!(app.base_branch.as_deref(), Some("main"));
        assert_eq!(app.repo.branch().unwrap(), "feature");
        assert_eq!(app.files.len(), 3);
        app.handle_key(key('s'));
        assert_eq!(app.mode, Mode::Working);
        app.handle_key(key('s'));
        assert_eq!(app.mode, Mode::Staged);
        app.handle_key(key('s'));
        assert!(app.mode.is_branch());
        let mode = app.mode.clone();
        assert!(app.compare_with("missing").is_err());
        assert_eq!(app.mode, mode);
        assert_eq!(app.base_branch.as_deref(), Some("main"));
    }

    #[test]
    fn branch_refresh_tracks_commits_and_base_changes_and_retains_failed_snapshot() {
        let (dir, repo) = diverged_repo();
        let mut app = App::new(repo, true, Some("main".into())).unwrap();
        std::fs::write(dir.path().join("later.rs"), "fn later() {}\n").unwrap();
        app.refresh();
        assert_eq!(app.files.len(), 3); // Untracked files must not leak into the branch view.
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-qm", "Later"]);
        app.refresh();
        assert_eq!(app.files.len(), 4);
        let mode = app.mode.clone();
        git(dir.path(), &["branch", "-D", "main"]);
        app.refresh();
        assert!(
            app.error
                .as_ref()
                .unwrap()
                .contains("showing last snapshot")
        );
        assert_eq!(app.mode, mode);
        assert_eq!(app.files.len(), 4);
        git(dir.path(), &["branch", "main", "HEAD"]);
        app.refresh();
        assert!(app.error.is_none());
        assert!(app.files.is_empty());
        assert!(
            app.view.rows[0].spans[0]
                .content
                .contains("No committed changes")
        );
    }

    #[test]
    fn failed_mode_switch_keeps_the_previous_mode() {
        let (dir, repo) = diverged_repo();
        let mut app = App::new(repo, false, Some("main".into())).unwrap();
        app.handle_key(key('s'));
        app.handle_key(key('s'));
        assert_eq!(app.mode, Mode::Staged);
        git(dir.path(), &["branch", "-D", "main"]);
        app.handle_key(key('s'));
        assert_eq!(app.mode, Mode::Staged);
        assert!(app.error.is_some());
    }

    #[test]
    fn branch_input_does_not_interpret_shortcuts() {
        let (_dir, repo) = diverged_repo();
        let mut app = App::new(repo, false, None).unwrap();
        app.handle_key(key('b'));
        for ch in "qs?j/k".chars() {
            assert!(!app.handle_key(key(ch)));
        }
        assert_eq!(app.base_input.as_deref(), Some("qs?j/k"));
        assert_eq!(app.mode, Mode::Working);
        assert!(!app.show_help);
        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(app.base_input.as_deref(), Some(""));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            app.base_error
                .as_ref()
                .unwrap()
                .contains("Enter a base branch")
        );
    }

    #[test]
    fn empty_repository_and_help_are_safe() {
        let (_dir, repo) = repo();
        let mut app = App::new(repo, false, None).unwrap();
        for ch in ['n', 'p', 'G', 'g', '[', ']', 'l', 'h', 's', 'r'] {
            assert!(!app.handle_key(key(ch)));
        }
        assert!(app.selected().is_none());
        app.handle_key(key('?'));
        assert!(app.show_help);
        assert!(!app.handle_key(key('q')));
        assert!(!app.show_help);
        assert!(app.handle_key(key('q')));
    }
}
