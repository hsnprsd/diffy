use anyhow::{Context, Result};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::ListState;

use crate::{
    diff::DiffView,
    editor::EditRequest,
    git::{FileEntry, Mode, Repository},
    highlight::SyntaxHighlighter,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Files,
    Diff,
}

pub struct BranchPicker {
    pub input: String,
    pub branches: Vec<String>,
    pub visible: Vec<usize>,
    pub list_state: ListState,
    pub error: Option<String>,
}

impl BranchPicker {
    fn new(branches: Vec<String>, selected: Option<&str>) -> Self {
        let position = branches
            .iter()
            .position(|branch| Some(branch.as_str()) == selected);
        let mut picker = Self {
            input: String::new(),
            visible: (0..branches.len()).collect(),
            branches,
            list_state: ListState::default(),
            error: None,
        };
        picker
            .list_state
            .select(position.or_else(|| (!picker.visible.is_empty()).then_some(0)));
        picker
    }

    pub fn selected(&self) -> Option<&str> {
        self.list_state
            .selected()
            .and_then(|selected| self.visible.get(selected))
            .map(|&index| self.branches[index].as_str())
    }

    fn filter(&mut self) {
        let needle = self.input.trim().to_lowercase();
        self.visible = self
            .branches
            .iter()
            .enumerate()
            .filter(|(_, branch)| branch.to_lowercase().contains(&needle))
            .map(|(index, _)| index)
            .collect();
        self.list_state
            .select((!self.visible.is_empty()).then_some(0));
        self.error = None;
    }

    fn move_selection(&mut self, delta: isize) {
        if !self.visible.is_empty() {
            let next = self
                .list_state
                .selected()
                .unwrap_or(0)
                .saturating_add_signed(delta)
                .min(self.visible.len() - 1);
            self.list_state.select(Some(next));
        }
    }
}

pub struct App {
    pub repo: Repository,
    pub mode: Mode,
    pub branch: String,
    pub base_branch: Option<String>,
    pub branch_picker: Option<BranchPicker>,
    pub files: Vec<FileEntry>,
    pub list_state: ListState,
    pub view: DiffView,
    pub focus: Focus,
    pub scroll: usize,
    pub horizontal: usize,
    pub viewport_height: usize,
    pub viewport_width: usize,
    pub show_help: bool,
    pub error: Option<String>,
    pub edit_request: Option<EditRequest>,
    highlighter: SyntaxHighlighter,
}

impl App {
    pub fn new(repo: Repository, base_branch: Option<String>) -> Result<Self> {
        let branch = repo.branch()?;
        let mode = match &base_branch {
            Some(target) => repo.compare_branch(target)?,
            None => Mode::Working,
        };
        let files = repo.files(mode.clone())?;
        let mut app = Self {
            repo,
            branch,
            files,
            mode,
            base_branch,
            branch_picker: None,
            list_state: ListState::default(),
            view: DiffView::default(),
            focus: Focus::Files,
            scroll: 0,
            horizontal: 0,
            viewport_height: 20,
            viewport_width: 80,
            show_help: false,
            error: None,
            edit_request: None,
            highlighter: SyntaxHighlighter::default(),
        };
        app.list_state.select((!app.files.is_empty()).then_some(0));
        app.load_selected();
        Ok(app)
    }

    pub fn selected(&self) -> Option<&FileEntry> {
        self.list_state
            .selected()
            .and_then(|selected| self.files.get(selected))
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
            self.error = Some(format!("Refresh: {error:#}; showing last snapshot"));
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
        let position = selected
            .as_ref()
            .and_then(|path| self.files.iter().position(|entry| &entry.path == path));
        self.list_state
            .select(position.or_else(|| (!self.files.is_empty()).then_some(0)));
        if reset_scroll || self.selected().map(|entry| &entry.path) != selected.as_ref() {
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

    fn load_selected(&mut self) {
        let Some(entry) = self.selected().cloned() else {
            self.view = DiffView::notice(if self.mode.is_branch() {
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
                self.view = DiffView::notice("Could not load this file");
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

    fn request_edit(&mut self) {
        let Some(entry) = self.selected() else {
            return;
        };
        let hunk = if self.focus == Focus::Diff {
            // The top row belongs to this hunk even when its header is scrolled offscreen.
            self.view
                .hunks
                .iter()
                .rposition(|&row| row <= self.scroll)
                .unwrap_or(0)
        } else {
            0
        };
        let line = self
            .view
            .hunks
            .get(hunk)
            .and_then(|&start| {
                let end = self
                    .view
                    .hunks
                    .get(hunk + 1)
                    .copied()
                    .unwrap_or(self.view.rows.len());
                self.view.rows[start..end]
                    .iter()
                    .find_map(|row| row.new_line)
            })
            .unwrap_or(1);
        self.edit_request = Some(EditRequest {
            path: entry.path.clone(),
            line: Some(line),
        });
    }

    fn move_file(&mut self, delta: isize) {
        if self.files.is_empty() {
            return;
        }
        let selected = self.list_state.selected().unwrap_or(0);
        let next = selected
            .saturating_add_signed(delta)
            .min(self.files.len() - 1);
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

    /// Returns true when the user requests exit; editor requests are handled by the event loop.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return true;
        }
        if self.show_help {
            self.show_help = false;
            return false;
        }
        if let Some(picker) = &mut self.branch_picker {
            match key.code {
                KeyCode::Esc => self.branch_picker = None,
                KeyCode::Enter => {
                    let target = picker.selected().unwrap_or(picker.input.trim()).to_owned();
                    match self.compare_with(&target) {
                        Ok(()) => self.branch_picker = None,
                        Err(error) => {
                            self.branch_picker.as_mut().unwrap().error = Some(format!("{error:#}"));
                        }
                    }
                }
                KeyCode::Down | KeyCode::Tab => picker.move_selection(1),
                KeyCode::Up | KeyCode::BackTab => picker.move_selection(-1),
                KeyCode::Home => picker.move_selection(-isize::MAX),
                KeyCode::End => picker.move_selection(isize::MAX),
                KeyCode::Backspace => {
                    picker.input.pop();
                    picker.filter();
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    picker.input.clear();
                    picker.filter();
                }
                KeyCode::Char(ch)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    picker.input.push(ch);
                    picker.filter();
                }
                _ => {}
            }
            return false;
        }
        match key.code {
            KeyCode::Char('q') => return true,
            KeyCode::Char('e') => self.request_edit(),
            KeyCode::Char('?') | KeyCode::F(1) => self.show_help = true,
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = if self.focus == Focus::Files {
                    Focus::Diff
                } else {
                    Focus::Files
                };
            }
            KeyCode::Left => self.focus = Focus::Files,
            KeyCode::Right | KeyCode::Enter => self.focus = Focus::Diff,
            KeyCode::Char('j') | KeyCode::Down => self.vertical(1),
            KeyCode::Char('k') | KeyCode::Up => self.vertical(-1),
            KeyCode::PageDown | KeyCode::Char('d') => {
                self.vertical(self.viewport_height.saturating_sub(2).max(1) as isize)
            }
            KeyCode::PageUp | KeyCode::Char('u') => {
                self.vertical(-(self.viewport_height.saturating_sub(2).max(1) as isize))
            }
            KeyCode::Home | KeyCode::Char('g') => self.vertical(-(isize::MAX)),
            KeyCode::End | KeyCode::Char('G') => self.vertical(isize::MAX),
            KeyCode::Char('h') => self.horizontal = self.horizontal.saturating_sub(4),
            KeyCode::Char('l') => {
                self.horizontal = self.horizontal.saturating_add(4).min(self.max_horizontal())
            }
            KeyCode::Char(']') => self.hunk(true),
            KeyCode::Char('[') => self.hunk(false),
            KeyCode::Char('s') => {
                if let Err(error) = self.cycle_mode() {
                    self.error = Some(format!("Compare: {error:#}"));
                }
            }
            KeyCode::Char('b') => match self.repo.branches() {
                Ok(branches) => {
                    self.branch_picker =
                        Some(BranchPicker::new(branches, self.base_branch.as_deref()));
                }
                Err(error) => self.error = Some(format!("Branches: {error:#}")),
            },
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
    use std::path::PathBuf;

    use super::*;
    use crate::git::tests::{diverged_repo, git, repo};

    fn key(ch: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE)
    }

    #[test]
    fn arrow_keys_focus_panes_without_changing_selection_or_scroll() {
        let (dir, repo) = repo();
        let content = format!("{}\n", "x".repeat(120)).repeat(30);
        std::fs::write(dir.path().join("a.txt"), content).unwrap();
        std::fs::write(dir.path().join("b.txt"), "other\n").unwrap();
        let mut app = App::new(repo, None).unwrap();
        app.handle_key(key('l'));
        assert_eq!(app.horizontal, 4);
        app.scroll = 2;
        let selected = app.selected().unwrap().path.clone();
        for (code, focus) in [
            (KeyCode::Left, Focus::Files),
            (KeyCode::Right, Focus::Diff),
            (KeyCode::Right, Focus::Diff),
            (KeyCode::Left, Focus::Files),
            (KeyCode::Left, Focus::Files),
            (KeyCode::Tab, Focus::Diff),
            (KeyCode::BackTab, Focus::Files),
        ] {
            assert!(!app.handle_key(KeyEvent::new(code, KeyModifiers::NONE)));
            assert!(app.focus == focus);
            assert_eq!(app.selected().unwrap().path, selected);
            assert_eq!(app.horizontal, 4);
            assert_eq!(app.scroll, 2);
        }
        app.handle_key(key('h'));
        assert_eq!(app.horizontal, 0);
    }

    #[test]
    fn pane_arrows_do_not_interrupt_branch_picker() {
        let (_dir, repo) = diverged_repo();
        let mut app = App::new(repo, None).unwrap();
        app.handle_key(key('b'));
        for code in [KeyCode::Right, KeyCode::Left] {
            assert!(!app.handle_key(KeyEvent::new(code, KeyModifiers::NONE)));
            assert!(app.branch_picker.is_some());
            assert!(app.focus == Focus::Files);
        }
    }

    #[test]
    fn vertical_navigation_uses_the_focused_pane() {
        let (dir, repo) = repo();
        let content = "line\n".repeat(30);
        std::fs::write(dir.path().join("a.txt"), &content).unwrap();
        std::fs::write(dir.path().join("b.txt"), &content).unwrap();
        let mut app = App::new(repo, None).unwrap();
        for (code, path) in [
            (KeyCode::Down, "b.txt"),
            (KeyCode::Up, "a.txt"),
            (KeyCode::Char('j'), "b.txt"),
            (KeyCode::Char('k'), "a.txt"),
        ] {
            assert!(!app.handle_key(KeyEvent::new(code, KeyModifiers::NONE)));
            assert_eq!(app.selected().unwrap().path, PathBuf::from(path));
            assert_eq!(app.scroll, 0);
        }
        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        for (code, scroll) in [
            (KeyCode::Down, 1),
            (KeyCode::Char('j'), 2),
            (KeyCode::Up, 1),
            (KeyCode::Char('k'), 0),
        ] {
            assert!(!app.handle_key(KeyEvent::new(code, KeyModifiers::NONE)));
            assert_eq!(app.selected().unwrap().path, PathBuf::from("a.txt"));
            assert_eq!(app.scroll, scroll);
        }
        app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.selected().unwrap().path, PathBuf::from("b.txt"));
    }

    #[test]
    fn removed_shortcuts_do_not_navigate_refresh_or_filter() {
        let (dir, repo) = repo();
        for path in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(dir.path().join(path), "line\n").unwrap();
        }
        let mut app = App::new(repo, None).unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        std::fs::write(dir.path().join("d.txt"), "new file\n").unwrap();
        for code in [KeyCode::Left, KeyCode::Right] {
            app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
            for ch in ['n', 'p', 'r', '/'] {
                assert!(!app.handle_key(key(ch)));
                assert_eq!(app.selected().unwrap().path, PathBuf::from("b.txt"));
                assert_eq!(app.files.len(), 3);
            }
        }
        app.refresh();
        assert_eq!(app.files.len(), 4);
        assert_eq!(app.selected().unwrap().path, PathBuf::from("b.txt"));
        assert!(app.handle_key(key('q')));
    }

    #[test]
    fn refresh_preserves_selection_and_scroll_until_selected_file_disappears() {
        let (dir, repo) = repo();
        std::fs::write(dir.path().join("a.txt"), "first file\n").unwrap();
        let content = format!("{}\n", "x".repeat(120)).repeat(30);
        std::fs::write(dir.path().join("b.txt"), content).unwrap();
        let mut app = App::new(repo, None).unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.scroll = 2;
        app.horizontal = 4;
        std::fs::write(dir.path().join("0.txt"), "new file\n").unwrap();
        app.refresh();
        assert_eq!(app.files.len(), 3);
        assert_eq!(app.selected().unwrap().path, PathBuf::from("b.txt"));
        assert_eq!(app.scroll, 2);
        assert_eq!(app.horizontal, 4);
        std::fs::remove_file(dir.path().join("b.txt")).unwrap();
        app.refresh();
        assert_eq!(app.selected().unwrap().path, PathBuf::from("0.txt"));
        assert_eq!(app.scroll, 0);
        assert_eq!(app.horizontal, 0);
        for path in ["0.txt", "a.txt"] {
            std::fs::remove_file(dir.path().join(path)).unwrap();
        }
        app.refresh();
        assert!(app.files.is_empty());
        assert!(app.selected().is_none());
    }

    fn app_with_hunks() -> (tempfile::TempDir, App) {
        let (dir, repo) = repo();
        let old: String = (1..=90).map(|n| format!("line {n}\n")).collect();
        std::fs::write(dir.path().join("a file.txt"), &old).unwrap();
        git(dir.path(), &["add", "."]);
        let mut new = old;
        for line in [10, 50, 80] {
            new = new.replace(&format!("line {line}\n"), &format!("changed {line}\n"));
        }
        std::fs::write(dir.path().join("a file.txt"), new).unwrap();
        (dir, App::new(repo, None).unwrap())
    }

    #[test]
    fn edit_requests_use_the_first_visible_hunk_including_partial_hunks() {
        let (_dir, mut app) = app_with_hunks();
        app.focus = Focus::Diff;
        app.viewport_height = 4;
        let second = app.view.hunks[1];
        let third = app.view.hunks[2];
        for (scroll, line) in [
            (0, 7),
            (1, 7),
            (second - 1, 7),
            (second, 47),
            (second + 2, 47),
            (third - 1, 47),
            (third, 77),
        ] {
            app.scroll = scroll;
            app.clamp_scroll();
            assert_eq!(app.scroll, scroll);
            app.handle_key(key('e'));
            assert_eq!(
                app.edit_request.take(),
                Some(EditRequest {
                    path: PathBuf::from("a file.txt"),
                    line: Some(line),
                })
            );
            assert_eq!(app.scroll, scroll);
        }
        // Several hunks fit in the pane, but the top one still wins.
        app.viewport_height = app.view.rows.len();
        app.scroll = 0;
        app.handle_key(key('e'));
        assert_eq!(app.edit_request.take().unwrap().line, Some(7));
    }

    #[test]
    fn files_pane_opens_the_first_hunk_regardless_of_diff_scroll() {
        let (_dir, mut app) = app_with_hunks();
        app.focus = Focus::Diff;
        app.viewport_height = 4;
        app.handle_key(key(']'));
        assert_eq!(app.scroll, app.view.hunks[1]);
        app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        app.handle_key(key('e'));
        assert_eq!(
            app.edit_request.take(),
            Some(EditRequest {
                path: PathBuf::from("a file.txt"),
                line: Some(7),
            })
        );
        assert_eq!(app.scroll, app.view.hunks[1]);
    }

    #[test]
    fn diff_scrolls_pages_hunks_and_stays_in_bounds_after_refresh() {
        let (dir, mut app) = app_with_hunks();
        app.focus = Focus::Diff;
        app.viewport_height = 4;
        app.handle_key(key(']'));
        assert_eq!(app.scroll, app.view.hunks[1]);
        app.handle_key(key('j'));
        app.handle_key(key('e'));
        assert_eq!(app.edit_request.take().unwrap().line, Some(47));
        app.handle_key(key('['));
        assert_eq!(app.scroll, app.view.hunks[1]);
        app.handle_key(key('['));
        assert_eq!(app.scroll, 0);
        app.handle_key(key('d'));
        assert_eq!(app.scroll, 2);
        app.handle_key(key('G'));
        assert_eq!(app.scroll, app.view.rows.len() - app.viewport_height);
        app.handle_key(key('g'));
        assert_eq!(app.scroll, 0);
        app.handle_key(key('G'));
        std::fs::write(dir.path().join("a file.txt"), "short\n").unwrap();
        app.refresh();
        assert!(app.scroll <= app.view.rows.len().saturating_sub(app.viewport_height));
    }

    #[test]
    fn missing_hunks_or_new_line_numbers_open_at_line_one() {
        let (dir, repo) = repo();
        std::fs::write(dir.path().join("test.txt"), "gone").unwrap();
        git(dir.path(), &["add", "."]);
        std::fs::write(dir.path().join("test.txt"), "").unwrap();
        let mut app = App::new(repo, None).unwrap();
        assert!(!app.view.hunks.is_empty());
        assert!(app.view.rows.iter().all(|row| row.new_line.is_none()));
        for focus in [Focus::Files, Focus::Diff] {
            app.focus = focus;
            app.handle_key(key('e'));
            assert_eq!(app.edit_request.take().unwrap().line, Some(1));
        }
        app.view = DiffView::notice("Binary file");
        for focus in [Focus::Files, Focus::Diff] {
            app.focus = focus;
            app.handle_key(key('e'));
            assert_eq!(app.edit_request.take().unwrap().line, Some(1));
        }
    }

    #[test]
    fn edit_shortcut_respects_help_branch_input_and_empty_file_lists() {
        let (dir, repo) = repo();
        let mut app = App::new(repo, None).unwrap();
        app.handle_key(key('e'));
        assert!(app.edit_request.is_none());
        std::fs::write(dir.path().join("test.txt"), "test\n").unwrap();
        app.refresh();
        app.handle_key(key('?'));
        app.handle_key(key('e'));
        assert!(!app.show_help);
        assert!(app.edit_request.is_none());
        app.handle_key(key('b'));
        app.handle_key(key('e'));
        assert_eq!(app.branch_picker.as_ref().unwrap().input, "e");
        assert!(app.edit_request.is_none());
    }

    #[test]
    fn branch_picker_validates_cancels_and_applies_without_changing_checkout() {
        let (_dir, repo) = diverged_repo();
        let mut app = App::new(repo, None).unwrap();
        app.handle_key(key('b'));
        for ch in "missing".chars() {
            app.handle_key(key(ch));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            app.branch_picker
                .as_ref()
                .unwrap()
                .error
                .as_ref()
                .unwrap()
                .contains("Cannot resolve")
        );
        assert_eq!(app.mode, Mode::Working);
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.branch_picker.is_none());
        app.handle_key(key('b'));
        for ch in "main".chars() {
            app.handle_key(key(ch));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.branch_picker.is_none());
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
    fn branch_picker_selects_without_typing_and_remembers_the_base() {
        let (_dir, repo) = diverged_repo();
        let mut app = App::new(repo, None).unwrap();
        app.handle_key(key('b'));
        let picker = app.branch_picker.as_ref().unwrap();
        assert_eq!(picker.input, "");
        assert_eq!(picker.selected(), Some("feature"));
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.branch_picker.as_ref().unwrap().selected(), Some("main"));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.branch_picker.is_none());
        assert!(app.mode.is_branch());
        assert_eq!(app.base_branch.as_deref(), Some("main"));
        assert_eq!(app.repo.branch().unwrap(), "feature");
        assert_eq!(app.files.len(), 3);
        app.handle_key(key('b'));
        assert_eq!(app.branch_picker.as_ref().unwrap().selected(), Some("main"));
        assert_eq!(app.branch_picker.as_ref().unwrap().input, "");
        for code in [KeyCode::Up, KeyCode::Up, KeyCode::End, KeyCode::Home] {
            app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
        }
        assert_eq!(
            app.branch_picker.as_ref().unwrap().selected(),
            Some("feature")
        );
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.base_branch.as_deref(), Some("main"));
    }

    #[test]
    fn branch_picker_filters_remote_and_unicode_names_and_reloads_on_open() {
        let (dir, repo) = diverged_repo();
        git(
            dir.path(),
            &["update-ref", "refs/remotes/origin/main", "main"],
        );
        let mut app = App::new(repo, None).unwrap();
        app.handle_key(key('b'));
        for ch in "ORIGIN/MA".chars() {
            app.handle_key(key(ch));
        }
        let picker = app.branch_picker.as_ref().unwrap();
        assert_eq!(picker.visible.len(), 1);
        assert_eq!(picker.selected(), Some("origin/main"));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.base_branch.as_deref(), Some("origin/main"));
        assert_eq!(app.repo.branch().unwrap(), "feature");
        git(dir.path(), &["branch", "fix/界"]);
        app.handle_key(key('b'));
        assert_eq!(
            app.branch_picker.as_ref().unwrap().selected(),
            Some("origin/main")
        );
        app.handle_key(key('界'));
        assert_eq!(
            app.branch_picker.as_ref().unwrap().selected(),
            Some("fix/界")
        );
        app.handle_key(key('x'));
        assert!(app.branch_picker.as_ref().unwrap().visible.is_empty());
        assert!(app.branch_picker.as_ref().unwrap().selected().is_none());
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(
            app.branch_picker.as_ref().unwrap().selected(),
            Some("fix/界")
        );
        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(app.branch_picker.as_ref().unwrap().visible.len(), 4);
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(
            app.branch_picker.as_ref().unwrap().selected(),
            Some("fix/界")
        );
        app.handle_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
        assert_eq!(
            app.branch_picker.as_ref().unwrap().selected(),
            Some("feature")
        );
    }

    #[test]
    fn branch_picker_keeps_manual_refs_and_failed_selections_safe() {
        let (dir, repo) = diverged_repo();
        git(dir.path(), &["tag", "v1", "main"]);
        let mut app = App::new(repo, None).unwrap();
        app.handle_key(key('b'));
        for ch in "v1".chars() {
            app.handle_key(key(ch));
        }
        assert!(app.branch_picker.as_ref().unwrap().visible.is_empty());
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.base_branch.as_deref(), Some("v1"));
        let mode = app.mode.clone();
        app.handle_key(key('b'));
        app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        git(dir.path(), &["branch", "-D", "main"]);
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.branch_picker.as_ref().unwrap().error.is_some());
        assert_eq!(app.mode, mode);
        assert_eq!(app.base_branch.as_deref(), Some("v1"));
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.handle_key(key('b'));
        assert_eq!(app.branch_picker.as_ref().unwrap().branches, ["feature"]);
    }

    #[test]
    fn empty_branch_picker_is_safe_and_reports_an_empty_ref() {
        let (_dir, repo) = repo();
        let mut app = App::new(repo, None).unwrap();
        app.handle_key(key('b'));
        for code in [
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::Enter,
        ] {
            app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
        }
        let picker = app.branch_picker.as_ref().unwrap();
        assert!(picker.visible.is_empty());
        assert!(picker.selected().is_none());
        assert!(
            picker
                .error
                .as_ref()
                .unwrap()
                .contains("Enter a base branch")
        );
        assert_eq!(app.mode, Mode::Working);
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.branch_picker.is_none());
    }

    #[test]
    fn branch_refresh_tracks_commits_and_base_changes_and_retains_failed_snapshot() {
        let (dir, repo) = diverged_repo();
        let mut app = App::new(repo, Some("main".into())).unwrap();
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
        let mut app = App::new(repo, Some("main".into())).unwrap();
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
        let mut app = App::new(repo, None).unwrap();
        app.handle_key(key('b'));
        for ch in "qs?j/k".chars() {
            assert!(!app.handle_key(key(ch)));
        }
        assert_eq!(app.branch_picker.as_ref().unwrap().input, "qs?j/k");
        assert_eq!(app.mode, Mode::Working);
        assert!(!app.show_help);
        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(app.branch_picker.as_ref().unwrap().input, "");
        assert_eq!(app.branch_picker.as_ref().unwrap().visible.len(), 2);
        assert!(app.branch_picker.as_ref().unwrap().error.is_none());
    }

    #[test]
    fn empty_repository_and_help_are_safe() {
        let (_dir, repo) = repo();
        let mut app = App::new(repo, None).unwrap();
        for ch in ['j', 'k', 'G', 'g', '[', ']', 'l', 'h', 's'] {
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
