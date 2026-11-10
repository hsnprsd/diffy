use std::sync::mpsc::TryRecvError;

use anyhow::{Context, Result};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::ListState;

use crate::{
    diff::DiffView,
    editor::EditRequest,
    git::{Content, FileEntry, Mode, Operation, Repository},
    highlight::SyntaxHighlighter,
    operation::{self, OperationDetails, OperationEvent, OperationHandle, OperationResult},
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

#[derive(Default)]
pub struct CommitPopup {
    pub message: String,
    pub cursor: usize,
    pub error: Option<String>,
}

impl CommitPopup {
    fn insert(&mut self, text: &str) {
        self.message.insert_str(self.cursor, text);
        self.cursor += text.len();
        self.error = None;
    }

    fn edit(&mut self, key: KeyEvent) {
        let previous = self.message[..self.cursor]
            .char_indices()
            .next_back()
            .map_or(0, |(index, _)| index);
        let next = self.message[self.cursor..]
            .chars()
            .next()
            .map_or(self.cursor, |ch| self.cursor + ch.len_utf8());
        match key.code {
            KeyCode::Left => self.cursor = previous,
            KeyCode::Right => self.cursor = next,
            KeyCode::Home => {
                self.cursor = self.message[..self.cursor].rfind('\n').map_or(0, |n| n + 1);
            }
            KeyCode::End => {
                self.cursor += self.message[self.cursor..]
                    .find('\n')
                    .unwrap_or(self.message.len() - self.cursor);
            }
            KeyCode::Up | KeyCode::Down => {
                let start = self.message[..self.cursor].rfind('\n').map_or(0, |n| n + 1);
                let column = self.message[start..self.cursor].chars().count();
                let target = if key.code == KeyCode::Up {
                    start.checked_sub(1).map(|end| {
                        let begin = self.message[..end].rfind('\n').map_or(0, |n| n + 1);
                        (begin, end)
                    })
                } else {
                    self.message[self.cursor..].find('\n').map(|n| {
                        let begin = self.cursor + n + 1;
                        let end = begin
                            + self.message[begin..]
                                .find('\n')
                                .unwrap_or(self.message.len() - begin);
                        (begin, end)
                    })
                };
                if let Some((begin, end)) = target {
                    self.cursor = begin
                        + self.message[begin..end]
                            .char_indices()
                            .nth(column)
                            .map_or(end - begin, |(n, _)| n);
                }
            }
            KeyCode::Backspace => {
                self.message.drain(previous..self.cursor);
                self.cursor = previous;
                self.error = None;
            }
            KeyCode::Delete => {
                self.message.drain(self.cursor..next);
                self.error = None;
            }
            KeyCode::Enter => self.insert("\n"),
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.message.clear();
                self.cursor = 0;
                self.error = None;
            }
            KeyCode::Char(ch)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && !ch.is_control() =>
            {
                self.insert(&ch.to_string());
            }
            _ => {}
        }
    }
}

pub enum Status {
    Success(String),
    Error(String),
    Cancelled(String),
}

impl Status {
    pub fn message(&self) -> &str {
        match self {
            Self::Success(message) | Self::Error(message) | Self::Cancelled(message) => message,
        }
    }
}

pub struct App {
    pub repo: Repository,
    pub mode: Mode,
    pub branch: String,
    pub base_branch: Option<String>,
    pub branch_picker: Option<BranchPicker>,
    pub commit_popup: Option<CommitPopup>,
    pub status: Option<Status>,
    pending: Option<OperationHandle>,
    pub operation_details: Option<OperationDetails>,
    pub show_operation_details: bool,
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
            None => Mode::Changes,
        };
        let files = repo.files(mode.clone())?;
        let mut app = Self {
            repo,
            branch,
            files,
            mode,
            base_branch,
            branch_picker: None,
            commit_popup: None,
            status: None,
            pending: None,
            operation_details: None,
            show_operation_details: false,
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
        let selected = self
            .selected()
            .map(|entry| (entry.path.clone(), entry.staged));
        self.branch = branch;
        self.mode = mode;
        self.files = files;
        self.error = None;
        let position = selected.as_ref().and_then(|(path, staged)| {
            self.files
                .iter()
                .position(|entry| &entry.path == path && entry.staged == *staged)
                // Keep the same file selected when its staging state changes.
                .or_else(|| self.files.iter().position(|entry| &entry.path == path))
        });
        self.list_state
            .select(position.or_else(|| (!self.files.is_empty()).then_some(0)));
        if reset_scroll
            || self.selected().map(|entry| (&entry.path, entry.staged))
                != selected.as_ref().map(|(path, staged)| (path, *staged))
        {
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
            Mode::Changes => match &self.base_branch {
                Some(target) => self.repo.compare_branch(target)?,
                None => return Ok(()),
            },
            Mode::Branch { .. } => Mode::Changes,
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
                .saturating_sub(self.viewport_height.max(1))
                // Let every hunk reach the top, even when several fit in the viewport.
                .max(self.view.hunks.last().copied().unwrap_or(0)),
        );
    }

    pub fn active_hunk(&self) -> usize {
        self.view
            .hunks
            .iter()
            .rposition(|&row| row <= self.scroll)
            .unwrap_or(0)
    }

    pub fn operation_label(&self) -> Option<&'static str> {
        self.pending
            .as_ref()
            .and(self.operation_details.as_ref())
            .map(|details| {
                if details.cancelling {
                    "Cancelling"
                } else {
                    details.label
                }
            })
    }

    fn ensure_idle(&self) -> Result<()> {
        anyhow::ensure!(
            self.pending.is_none(),
            "Wait for the current Git operation to finish"
        );
        Ok(())
    }

    fn toggle_stage(&mut self) -> Result<()> {
        self.ensure_idle()?;
        anyhow::ensure!(
            !self.mode.is_branch(),
            "Branch comparisons are read-only; press s to switch to Changes"
        );
        let Some(entry) = self.selected() else {
            return Ok(());
        };
        let staged = entry.staged;
        if self.focus == Focus::Files {
            self.repo.toggle_file(entry, &self.mode)?;
        } else {
            let patch = self.view.patches.get(self.active_hunk()).context(
                "No text hunk to stage/unstage; use Space in the Files pane for the whole file",
            )?;
            let expected = self.view.source.as_ref().context("No text preview")?;
            let current = self
                .repo
                .files(self.mode.clone())?
                .into_iter()
                .find(|current| current.path == entry.path && current.staged == entry.staged)
                .context("This file has changed; wait for the live refresh and try again")?;
            anyhow::ensure!(
                current.status == entry.status && current.submodule == entry.submodule,
                "This file has changed; wait for the live refresh and try again"
            );
            let (old, new) = self.repo.contents(&current, self.mode.clone())?;
            anyhow::ensure!(
                matches!((&old, &new), (Content::Text(old), Content::Text(new)) if (old, new) == (&expected.0, &expected.1)),
                "This hunk has changed; wait for the live refresh and try again"
            );
            self.repo.toggle_hunk(entry, &self.mode, patch)?;
        }
        self.status = Some(Status::Success(
            if staged {
                "Unstaged changes"
            } else {
                "Staged changes"
            }
            .into(),
        ));
        self.refresh();
        Ok(())
    }

    fn open_commit(&mut self) -> Result<()> {
        self.ensure_idle()?;
        anyhow::ensure!(
            !self.repo.staged_files()?.is_empty(),
            "No staged changes to commit; stage changes with Space first"
        );
        self.commit_popup = Some(CommitPopup::default());
        self.error = None;
        Ok(())
    }

    fn start_operation(&mut self, operation: Operation) -> Result<()> {
        self.ensure_idle()?;
        self.operation_details = Some(OperationDetails::new(&operation));
        self.pending = Some(operation::spawn(self.repo.clone(), operation));
        self.show_operation_details = true;
        self.error = None;
        self.status = None;
        Ok(())
    }

    pub fn poll_operation(&mut self) -> bool {
        if self.pending.is_none() {
            return false;
        }
        let mut dirty = false;
        // Limit work per frame even when a hook continuously produces output.
        for _ in 0..64 {
            let event = match self.pending.as_ref().unwrap().events.try_recv() {
                Ok(event) => event,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => OperationEvent::Finished(
                    OperationResult::Failed("Git worker disconnected".into()),
                ),
            };
            dirty = true;
            match event {
                OperationEvent::Prepared(message) => {
                    self.operation_details.as_mut().unwrap().success_message = message;
                }
                OperationEvent::Output(text) => {
                    self.operation_details.as_mut().unwrap().append(&text);
                }
                OperationEvent::Finished(result) => {
                    self.pending = None;
                    self.refresh();
                    let details = self.operation_details.as_mut().unwrap();
                    let output_summary = details.summary(true).to_owned();
                    details.finish(result);
                    match details.result.as_ref().unwrap() {
                        OperationResult::Success => {
                            if details.commit {
                                self.commit_popup = None;
                            }
                            let message = if details.commit {
                                format!("{}: {}", details.success_message, details.summary(false))
                            } else {
                                details.success_message.clone()
                            };
                            self.status = Some(Status::Success(message));
                        }
                        OperationResult::Cancelled => {
                            let message = "Git operation cancelled".to_owned();
                            if details.commit
                                && let Some(popup) = &mut self.commit_popup
                            {
                                popup.error = Some(message.clone());
                            }
                            self.status = Some(Status::Cancelled(message));
                            self.show_operation_details = true;
                        }
                        OperationResult::Failed(error) => {
                            let message =
                                format!("Git operation failed: {error}; {output_summary}");
                            if details.commit
                                && let Some(popup) = &mut self.commit_popup
                            {
                                popup.error = Some(message.clone());
                            }
                            self.error = Some(message.clone());
                            // Filesystem refreshes must not hide failures or their output.
                            self.status = Some(Status::Error(message));
                            self.show_operation_details = true;
                        }
                    }
                    break;
                }
            }
        }
        dirty
    }

    pub fn handle_paste(&mut self, text: &str) {
        if self.pending.is_some() || self.show_operation_details {
            return;
        }
        if let Some(popup) = &mut self.commit_popup {
            let text: String = text
                .replace("\r\n", "\n")
                .replace('\r', "\n")
                .chars()
                .filter(|ch| !ch.is_control() || *ch == '\n' || *ch == '\t')
                .collect();
            popup.insert(&text);
        } else if let Some(picker) = &mut self.branch_picker {
            picker
                .input
                .extend(text.chars().filter(|ch| !ch.is_control()));
            picker.filter();
        }
    }

    fn request_edit(&mut self) {
        if let Err(error) = self.ensure_idle() {
            self.error = Some(error.to_string());
            return;
        }
        let Some(entry) = self.selected() else {
            return;
        };
        let hunk = if self.focus == Focus::Diff {
            // The top row belongs to this hunk even when its header is scrolled offscreen.
            self.active_hunk()
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
            if let Err(error) = self.ensure_idle() {
                self.error = Some(error.to_string());
                return false;
            }
            return true;
        }
        if key.code == KeyCode::Esc
            && let Some(pending) = &self.pending
        {
            let details = self.operation_details.as_mut().unwrap();
            if !details.cancelling {
                let _ = pending.cancel.send(());
                details.cancelling = true;
            }
            return false;
        }
        if self.show_help {
            self.show_help = false;
            return false;
        }
        if self.show_operation_details {
            let details = self.operation_details.as_mut().unwrap();
            let page = details.viewport_height.saturating_sub(2).max(1) as isize;
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') if self.pending.is_none() => {
                    self.show_operation_details = false;
                }
                KeyCode::Down | KeyCode::Char('j') => details.scroll_by(1),
                KeyCode::Up | KeyCode::Char('k') => details.scroll_by(-1),
                KeyCode::PageDown | KeyCode::Char('d') => details.scroll_by(page),
                KeyCode::PageUp | KeyCode::Char('u') => details.scroll_by(-page),
                KeyCode::Home | KeyCode::Char('g') => {
                    details.follow = false;
                    details.scroll = 0;
                }
                KeyCode::End | KeyCode::Char('G') => {
                    details.follow = true;
                    details.clamp_scroll();
                }
                KeyCode::Char('h') | KeyCode::Left => {
                    details.horizontal = details.horizontal.saturating_sub(4)
                }
                KeyCode::Char('l') | KeyCode::Right => {
                    details.horizontal = details
                        .horizontal
                        .saturating_add(4)
                        .min(details.max_horizontal());
                }
                _ => {}
            }
            return false;
        }
        if self.commit_popup.is_some() {
            if self.pending.is_some() {
                return false;
            }
            match key.code {
                KeyCode::Esc => self.commit_popup = None,
                KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    let message = self.commit_popup.as_ref().unwrap().message.clone();
                    if message.trim().is_empty() {
                        self.commit_popup.as_mut().unwrap().error =
                            Some("Enter a commit message".into());
                    } else if let Err(error) = self.start_operation(Operation::Commit(message)) {
                        self.commit_popup.as_mut().unwrap().error = Some(format!("{error:#}"));
                    }
                }
                _ => self.commit_popup.as_mut().unwrap().edit(key),
            }
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
            KeyCode::Char('q') => {
                if let Err(error) = self.ensure_idle() {
                    self.error = Some(error.to_string());
                } else {
                    return true;
                }
            }
            KeyCode::Char(' ') => {
                if let Err(error) = self.toggle_stage() {
                    self.error = Some(format!("Stage: {error:#}"));
                }
            }
            KeyCode::Char('c') => {
                if let Err(error) = self.open_commit() {
                    self.error = Some(format!("Commit: {error:#}"));
                }
            }
            KeyCode::Char(ch @ ('p' | 'P')) => {
                let operation = if ch == 'p' {
                    Operation::Pull
                } else {
                    Operation::Push
                };
                if let Err(error) = self.start_operation(operation) {
                    self.error = Some(format!("Git: {error:#}"));
                }
            }
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
        assert_eq!(app.mode, Mode::Changes);
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
        assert_eq!(app.mode, Mode::Changes);
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
        assert_eq!(app.mode, Mode::Changes);
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
        assert_eq!(app.mode, Mode::Changes);
        git(dir.path(), &["branch", "-D", "main"]);
        app.handle_key(key('s'));
        assert_eq!(app.mode, Mode::Changes);
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
        assert_eq!(app.mode, Mode::Changes);
        assert!(!app.show_help);
        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(app.branch_picker.as_ref().unwrap().input, "");
        assert_eq!(app.branch_picker.as_ref().unwrap().visible.len(), 2);
        assert!(app.branch_picker.as_ref().unwrap().error.is_none());
    }

    fn wait_for_operation(app: &mut App) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while app.operation_label().is_some() {
            app.poll_operation();
            assert!(
                std::time::Instant::now() < deadline,
                "Git operation timed out"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[cfg(unix)]
    fn wait_for_output(app: &mut App, expected: &[&str]) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            app.poll_operation();
            let details = app.operation_details.as_ref().unwrap();
            if expected
                .iter()
                .all(|text| details.lines.iter().any(|line| line.contains(text)))
            {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "Missing live Git output: {:?}",
                details.lines
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn space_in_files_stages_and_unstages_the_whole_file() {
        let (dir, mut app) = app_with_hunks();
        let path = dir.path().join("a file.txt");
        let working = std::fs::read_to_string(&path).unwrap();
        app.handle_key(key(' '));
        assert!(app.error.is_none(), "{:?}", app.error);
        assert_eq!(app.files.len(), 1);
        assert_eq!(app.mode, Mode::Changes);
        assert!(app.selected().unwrap().staged);
        assert_eq!(app.view.hunks.len(), 1); // New file on an unborn branch.
        app.handle_key(key(' '));
        assert_eq!(app.files.len(), 1);
        assert!(!app.selected().unwrap().staged);
        assert!(app.repo.staged_files().unwrap().is_empty());
        assert_eq!(std::fs::read_to_string(path).unwrap(), working);
        // Without a base branch, s leaves the combined view unchanged.
        app.handle_key(key('s'));
        assert_eq!(app.mode, Mode::Changes);
        assert_eq!(app.files.len(), 1);
    }

    #[test]
    fn space_in_diff_only_toggles_the_active_hunk_even_in_a_tall_viewport() {
        let (dir, mut app) = app_with_hunks();
        git(dir.path(), &["commit", "-qm", "Initial"]);
        app.refresh();
        app.viewport_height = 1000;
        app.handle_key(key(']'));
        assert_eq!(app.active_hunk(), 1);
        assert_eq!(app.scroll, app.view.hunks[1]);
        app.handle_key(key('j')); // The header is offscreen, but the middle hunk is still active.
        assert_eq!(app.active_hunk(), 1);
        app.handle_key(key(' '));
        assert!(app.error.is_none(), "{:?}", app.error);
        assert_eq!(app.view.hunks.len(), 2);
        assert_eq!(app.files.len(), 2);
        assert!(!app.selected().unwrap().staged);
        let staged = app.repo.staged_files().unwrap();
        let staged_text =
            crate::git::tests::text(app.repo.contents(&staged[0], Mode::Changes).unwrap().1);
        assert!(staged_text.contains("changed 50\n"));
        assert!(!staged_text.contains("changed 10\n"));
        assert!(!staged_text.contains("changed 80\n"));
        let working = std::fs::read_to_string(dir.path().join("a file.txt")).unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        app.handle_key(key('j')); // Select the staged entry in the same view.
        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert!(app.selected().unwrap().staged);
        assert_eq!(app.view.hunks.len(), 1);
        app.handle_key(key(' '));
        assert!(app.repo.staged_files().unwrap().is_empty());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a file.txt")).unwrap(),
            working
        );
        // Whole-file stage, then unstage the middle hunk only.
        app.focus = Focus::Files;
        app.handle_key(key(' '));
        assert!(app.selected().unwrap().staged);
        app.handle_key(key(']'));
        app.handle_key(key(' '));
        assert!(app.error.is_none(), "{:?}", app.error);
        assert!(app.selected().unwrap().staged);
        assert_eq!(app.view.hunks.len(), 2);
        let staged = app.repo.staged_files().unwrap();
        let staged_text =
            crate::git::tests::text(app.repo.contents(&staged[0], Mode::Changes).unwrap().1);
        assert!(staged_text.contains("changed 10\n"));
        assert!(!staged_text.contains("changed 50\n"));
        assert!(staged_text.contains("changed 80\n"));
    }

    #[test]
    fn refresh_preserves_the_selected_staging_state_and_resets_scroll_when_it_changes() {
        let (dir, mut app) = app_with_hunks();
        git(dir.path(), &["commit", "-qm", "Initial"]);
        app.refresh();
        app.handle_key(key(' '));
        assert!(app.selected().unwrap().staged);
        app.viewport_height = 4;
        app.handle_key(key(']'));
        app.handle_key(key('j'));
        app.horizontal = 4;
        let scroll = app.scroll;
        let source = app.view.source.clone();

        // A new unstaged entry sorts before the currently selected staged entry.
        let path = dir.path().join("a file.txt");
        let working = std::fs::read_to_string(&path)
            .unwrap()
            .replace("changed 80", "newer 80");
        std::fs::write(&path, &working).unwrap();
        app.refresh();
        assert_eq!(app.files.len(), 2);
        assert_eq!(app.list_state.selected(), Some(1));
        assert!(app.selected().unwrap().staged);
        assert_eq!(app.view.source, source);
        assert_eq!(app.scroll, scroll);
        assert_eq!(app.horizontal, 4);

        // Removing that staged entry keeps the file selected, not its old scroll.
        git(dir.path(), &["reset", "-q", "--", "a file.txt"]);
        app.refresh();
        assert_eq!(app.files.len(), 1);
        assert_eq!(app.selected().unwrap().path, PathBuf::from("a file.txt"));
        assert!(!app.selected().unwrap().staged);
        assert_eq!(app.scroll, 0);
        assert_eq!(app.horizontal, 0);
        assert_eq!(std::fs::read_to_string(path).unwrap(), working);
    }

    #[test]
    fn stale_hunks_are_rejected_until_the_live_view_is_updated() {
        let (dir, mut app) = app_with_hunks();
        git(dir.path(), &["commit", "-qm", "Initial"]);
        app.refresh();
        app.focus = Focus::Diff;
        std::fs::write(dir.path().join("a file.txt"), "newer edits\n").unwrap();
        app.handle_key(key(' '));
        assert!(app.error.as_ref().unwrap().contains("has changed"));
        assert!(app.repo.staged_files().unwrap().is_empty());
        app.refresh();
        app.handle_key(key(' '));
        assert!(app.error.is_none(), "{:?}", app.error);
        assert_eq!(app.files.len(), 1);
        assert!(app.selected().unwrap().staged);
    }

    #[test]
    fn externally_staged_hunks_are_not_toggled_again_before_refresh() {
        let (dir, mut app) = app_with_hunks();
        git(dir.path(), &["commit", "-qm", "Initial"]);
        app.refresh();
        app.focus = Focus::Diff;
        git(dir.path(), &["add", "--", "a file.txt"]);
        app.handle_key(key(' '));
        assert!(app.error.as_ref().unwrap().contains("has changed"));
        assert_eq!(app.repo.staged_files().unwrap().len(), 1);
        app.refresh();
        assert!(app.selected().unwrap().staged);
        app.handle_key(key(' '));
        assert!(app.error.is_none(), "{:?}", app.error);
        assert_eq!(app.view.hunks.len(), 2);
    }

    #[test]
    fn space_respects_notices_branch_comparisons_and_empty_lists() {
        let (dir, repo) = repo();
        let mut app = App::new(repo, None).unwrap();
        app.handle_key(key(' '));
        assert!(app.error.is_none());
        app.handle_key(key('c'));
        assert!(app.error.as_ref().unwrap().contains("No staged changes"));
        assert!(app.commit_popup.is_none());
        std::fs::write(dir.path().join("binary"), [0, 1, 2]).unwrap();
        app.refresh();
        app.focus = Focus::Diff;
        app.handle_key(key(' '));
        assert!(app.error.as_ref().unwrap().contains("No text hunk"));
        assert!(app.repo.staged_files().unwrap().is_empty());
        app.focus = Focus::Files;
        app.handle_key(key(' '));
        assert_eq!(app.repo.staged_files().unwrap().len(), 1);
        let (_dir, repo) = diverged_repo();
        let mut app = App::new(repo, Some("main".into())).unwrap();
        for focus in [Focus::Files, Focus::Diff] {
            app.focus = focus;
            app.handle_key(key(' '));
            assert!(app.error.as_ref().unwrap().contains("read-only"));
            assert!(app.repo.staged_files().unwrap().is_empty());
        }
    }

    #[test]
    fn commit_popup_is_modal_supports_paste_and_keeps_unstaged_changes() {
        let (dir, repo) = repo();
        std::fs::write(dir.path().join("file.txt"), "staged\n").unwrap();
        git(dir.path(), &["add", "."]);
        std::fs::write(dir.path().join("file.txt"), "unstaged\n").unwrap();
        let mut app = App::new(repo, None).unwrap();
        app.handle_key(key('c'));
        app.handle_paste("Cancelled message");
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.commit_popup.is_none());
        assert_eq!(app.repo.staged_files().unwrap().len(), 1);
        app.handle_key(key('c'));
        app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(
            app.commit_popup
                .as_ref()
                .unwrap()
                .error
                .as_ref()
                .unwrap()
                .contains("Enter a commit message")
        );
        for ch in "qb pPe?s".chars() {
            assert!(!app.handle_key(key(ch)));
        }
        assert!(app.branch_picker.is_none());
        assert!(app.edit_request.is_none());
        assert!(!app.show_help);
        assert!(app.operation_label().is_none());
        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        app.handle_paste("Commit 界\r\n\r\nMessage body\u{1b}");
        assert_eq!(
            app.commit_popup.as_ref().unwrap().message,
            "Commit 界\n\nMessage body"
        );
        app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert_eq!(app.operation_label(), Some("Committing"));
        assert!(app.commit_popup.is_some());
        wait_for_operation(&mut app);
        assert!(app.commit_popup.is_none(), "{:?}", app.error);
        assert!(app.error.is_none(), "{:?}", app.error);
        assert!(app.repo.staged_files().unwrap().is_empty());
        assert_eq!(app.files.len(), 1);
        assert_eq!(
            crate::git::tests::text(app.repo.contents(&app.files[0], Mode::Changes).unwrap().0),
            "staged\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("file.txt")).unwrap(),
            "unstaged\n"
        );
    }

    #[test]
    fn commit_message_editor_moves_and_deletes_at_unicode_boundaries() {
        let mut popup = CommitPopup::default();
        popup.insert("a界c\né\nlong line");
        let press =
            |popup: &mut CommitPopup, code| popup.edit(KeyEvent::new(code, KeyModifiers::NONE));
        press(&mut popup, KeyCode::Home);
        press(&mut popup, KeyCode::Up);
        assert_eq!(popup.cursor, "a界c\n".len());
        press(&mut popup, KeyCode::End);
        press(&mut popup, KeyCode::Up);
        assert_eq!(popup.cursor, 1);
        press(&mut popup, KeyCode::Delete);
        assert!(popup.message.starts_with("ac\n"));
        popup.insert("界");
        press(&mut popup, KeyCode::Left);
        assert_eq!(popup.cursor, 1);
        press(&mut popup, KeyCode::Right);
        press(&mut popup, KeyCode::Backspace);
        assert!(popup.message.starts_with("ac\n"));
        press(&mut popup, KeyCode::Down);
        assert_eq!(popup.cursor, "ac\né".len());
        press(&mut popup, KeyCode::Enter);
        assert!(popup.message.starts_with("ac\né\n\n"));
    }

    #[cfg(unix)]
    #[test]
    fn failed_commit_keeps_the_message_and_staged_changes_for_retry() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, repo) = repo();
        std::fs::write(dir.path().join("file.txt"), "staged\n").unwrap();
        git(dir.path(), &["add", "."]);
        let hook = repo.git_dir.join("hooks/pre-commit");
        std::fs::write(
            &hook,
            "#!/bin/sh\necho 'Commit rejected by test hook'\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut app = App::new(repo, None).unwrap();
        app.handle_key(key('c'));
        app.handle_paste("Keep this message");
        app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        wait_for_operation(&mut app);
        let popup = app.commit_popup.as_ref().unwrap();
        assert_eq!(popup.message, "Keep this message");
        assert!(
            popup
                .error
                .as_ref()
                .unwrap()
                .contains("Commit rejected by test hook")
        );
        assert_eq!(app.repo.staged_files().unwrap().len(), 1);
        assert!(app.show_operation_details);
        app.handle_paste("Do not edit the hidden draft");
        assert_eq!(
            app.commit_popup.as_ref().unwrap().message,
            "Keep this message"
        );
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        std::fs::remove_file(hook).unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        wait_for_operation(&mut app);
        assert!(app.commit_popup.is_none(), "{:?}", app.error);
        assert!(app.repo.staged_files().unwrap().is_empty());
    }

    #[test]
    fn push_and_pull_keys_run_in_background_and_block_conflicting_actions() {
        let (dir, _remote, repo) = crate::git::tests::connected_repo();
        git(dir.path(), &["commit", "--allow-empty", "-qm", "Later"]);
        let mut app = App::new(repo, None).unwrap();
        app.handle_key(key('P'));
        assert_eq!(app.operation_label(), Some("Pushing"));
        assert!(!app.handle_key(key('q')));
        assert!(!app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)));
        app.handle_key(key(' '));
        assert!(app.error.as_ref().unwrap().contains("Wait for"));
        app.handle_key(key('e'));
        assert!(app.edit_request.is_none());
        wait_for_operation(&mut app);
        assert!(app.error.is_none(), "{:?}", app.error);
        assert!(
            app.status
                .as_ref()
                .unwrap()
                .message()
                .starts_with("Pushed to origin (refs/heads/main)")
        );
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.handle_key(key('p'));
        assert_eq!(app.operation_label(), Some("Pulling"));
        wait_for_operation(&mut app);
        assert!(app.error.is_none(), "{:?}", app.error);
        assert!(
            app.status
                .as_ref()
                .unwrap()
                .message()
                .starts_with("Pulled main")
        );
    }

    #[test]
    fn command_failures_remain_visible_after_filesystem_refreshes() {
        let (dir, repo) = repo();
        git(dir.path(), &["commit", "--allow-empty", "-qm", "Initial"]);
        let mut app = App::new(repo, None).unwrap();
        app.handle_key(key('p')); // No remote/upstream has been configured.
        wait_for_operation(&mut app);
        let error = app.error.clone().unwrap();
        app.refresh();
        assert!(matches!(app.status, Some(Status::Error(_))));
        assert_eq!(app.status.as_ref().unwrap().message(), error);
    }

    #[cfg(unix)]
    #[test]
    fn slow_commit_streams_output_and_keeps_the_log_open_until_completion() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, repo) = repo();
        std::fs::write(dir.path().join("file.txt"), "staged\n").unwrap();
        git(dir.path(), &["add", "."]);
        let hook = repo.git_dir.join("hooks/pre-commit");
        std::fs::write(&hook, "#!/bin/sh\nprintf 'live stdout 界\\n'\nprintf 'live stderr\\n' >&2\ni=0\nwhile [ $i -lt 250 ]; do\n  [ -f .git/release-hook ] && exit 0\n  sleep 0.02\n  i=$((i + 1))\ndone\nexit 1\n").unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut app = App::new(repo, None).unwrap();
        app.handle_key(key('c'));
        app.handle_paste("Live commit");
        app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        wait_for_output(&mut app, &["live stdout 界", "live stderr"]);
        assert_eq!(app.operation_label(), Some("Committing"));
        assert!(!app.handle_key(key('q')));
        assert!(app.show_operation_details);
        app.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(app.operation_details.as_ref().unwrap().scroll, 0);
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.operation_details.as_ref().unwrap().scroll, 1);
        app.handle_paste("Not part of the message");
        assert_eq!(app.commit_popup.as_ref().unwrap().message, "Live commit");
        std::fs::write(dir.path().join(".git/release-hook"), "").unwrap();
        wait_for_operation(&mut app);
        assert!(
            app.status
                .as_ref()
                .unwrap()
                .message()
                .starts_with("Committed:")
        );
        assert_eq!(
            app.operation_details.as_ref().unwrap().result,
            Some(OperationResult::Success)
        );
        assert!(app.commit_popup.is_none());
        assert!(app.repo.staged_files().unwrap().is_empty());
        assert!(app.show_operation_details);
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.show_operation_details);
    }

    #[cfg(unix)]
    #[test]
    fn escape_cancels_a_running_commit_and_preserves_output_and_draft_for_retry() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, repo) = repo();
        std::fs::write(dir.path().join("file.txt"), "staged\n").unwrap();
        git(dir.path(), &["add", "."]);
        let hook = repo.git_dir.join("hooks/pre-commit");
        std::fs::write(&hook, "#!/bin/sh\nprintf 'hook waiting\\n'\nsleep 30\n").unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut app = App::new(repo, None).unwrap();
        app.handle_key(key('c'));
        app.handle_paste("Keep this draft 界\n\nBody");
        app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        wait_for_output(&mut app, &["hook waiting"]);

        for _ in 0..2 {
            assert!(!app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
            assert_eq!(app.operation_label(), Some("Cancelling"));
            assert!(app.show_operation_details);
            assert!(app.ensure_idle().is_err());
        }
        assert!(!app.handle_key(key('q')));
        app.handle_paste("Do not change the draft");
        wait_for_operation(&mut app);

        let details = app.operation_details.as_ref().unwrap();
        assert_eq!(details.result, Some(OperationResult::Cancelled));
        assert!(!details.cancelling);
        assert!(details.lines.iter().any(|line| line == "hook waiting"));
        assert!(app.show_operation_details);
        assert!(app.error.is_none(), "{:?}", app.error);
        assert_eq!(
            app.commit_popup.as_ref().unwrap().message,
            "Keep this draft 界\n\nBody"
        );
        assert_eq!(app.repo.staged_files().unwrap().len(), 1);
        assert!(!app.repo.git_dir.join("index.lock").exists());
        app.refresh();
        assert!(matches!(app.status, Some(Status::Cancelled(_))));
        assert_eq!(
            app.status.as_ref().unwrap().message(),
            "Git operation cancelled"
        );

        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.show_operation_details);
        assert!(app.commit_popup.is_some());
        std::fs::remove_file(hook).unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        wait_for_operation(&mut app);
        assert!(app.error.is_none(), "{:?}", app.error);
        assert!(app.commit_popup.is_none());
        assert!(app.repo.staged_files().unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn escape_cancels_pull_and_push_while_waiting_on_the_transport() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, _remote, repo) = crate::git::tests::connected_repo();
        let ssh = repo.git_dir.join("test-ssh");
        std::fs::write(
            &ssh,
            "#!/bin/sh\nprintf 'transport waiting\\n' >&2\nsleep 30\n",
        )
        .unwrap();
        std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
        git(
            dir.path(),
            &["config", "core.sshCommand", ssh.to_str().unwrap()],
        );
        git(dir.path(), &["config", "ssh.variant", "ssh"]);
        git(
            dir.path(),
            &[
                "remote",
                "set-url",
                "origin",
                "ssh://diffy-test.invalid/repo.git",
            ],
        );

        for shortcut in ['p', 'P'] {
            let mut app = App::new(repo.clone(), None).unwrap();
            app.handle_key(key(shortcut));
            wait_for_output(&mut app, &["transport waiting"]);
            app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
            assert_eq!(app.operation_label(), Some("Cancelling"));
            wait_for_operation(&mut app);
            assert_eq!(
                app.operation_details.as_ref().unwrap().result,
                Some(OperationResult::Cancelled)
            );
            assert!(matches!(app.status, Some(Status::Cancelled(_))));
            assert!(app.error.is_none(), "{:?}", app.error);
            assert!(app.show_operation_details);
            app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
            assert!(!app.show_operation_details);
            assert!(app.handle_key(key('q')));
        }
    }

    #[test]
    fn operation_polling_is_bounded_and_worker_disconnects_are_reported() {
        let (_dir, repo) = repo();
        let mut app = App::new(repo, None).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        app.pending = Some(OperationHandle {
            events: receiver,
            cancel: std::sync::mpsc::channel().0,
        });
        let mut details = OperationDetails::new(&Operation::Pull);
        details.success_message = "Pulled main".into();
        app.operation_details = Some(details);
        for _ in 0..130 {
            sender
                .send(OperationEvent::Output("line\n".into()))
                .unwrap();
        }
        sender
            .send(OperationEvent::Finished(OperationResult::Success))
            .unwrap();
        assert!(app.poll_operation());
        assert_eq!(app.operation_details.as_ref().unwrap().lines.len(), 65);
        assert!(app.pending.is_some());
        assert!(app.poll_operation());
        assert!(app.pending.is_some());
        assert!(app.poll_operation());
        assert!(app.pending.is_none());
        assert_eq!(app.status.as_ref().unwrap().message(), "Pulled main");

        let (sender, receiver) = std::sync::mpsc::channel();
        app.pending = Some(OperationHandle {
            events: receiver,
            cancel: std::sync::mpsc::channel().0,
        });
        app.operation_details = Some(OperationDetails::new(&Operation::Push));
        drop(sender);
        assert!(app.poll_operation());
        assert!(app.pending.is_none());
        assert!(app.show_operation_details);
        assert!(app.error.as_ref().unwrap().contains("worker disconnected"));
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
