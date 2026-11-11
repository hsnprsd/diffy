use std::{path::PathBuf, sync::mpsc::TryRecvError};

use anyhow::{Context, Result};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::ListState;

use crate::{
    diff::{DiffMode, DiffView},
    editor::EditRequest,
    git::{Content, FileEntry, Operation, Repository, Worktree},
    highlight::SyntaxHighlighter,
    keymap::{self, Action, Keymap},
    operation::{self, OperationDetails, OperationEvent, OperationHandle, OperationResult},
    theme::Theme,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Files,
    Diff,
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
        if key.code == KeyCode::Char('u') && key.modifiers == KeyModifiers::CONTROL {
            self.message.clear();
            self.cursor = 0;
            self.error = None;
            return;
        }
        let Some(code) = keymap::plain_code(key) else {
            return;
        };
        let previous = self.message[..self.cursor]
            .char_indices()
            .next_back()
            .map_or(0, |(index, _)| index);
        let next = self.message[self.cursor..]
            .chars()
            .next()
            .map_or(self.cursor, |ch| self.cursor + ch.len_utf8());
        match code {
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
            KeyCode::Char(ch) if !ch.is_control() => {
                self.insert(&ch.to_string());
            }
            _ => {}
        }
    }
}

pub struct WorktreePopup {
    pub worktrees: Vec<Worktree>,
    pub list_state: ListState,
    pub viewport_height: usize,
    pub error: Option<String>,
    pub removal: Option<Worktree>,
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
    pub branch: String,
    pub commit_popup: Option<CommitPopup>,
    pub worktree_popup: Option<WorktreePopup>,
    pub worktree_request: Option<PathBuf>,
    pub status: Option<Status>,
    pending: Option<OperationHandle>,
    pub operation_details: Option<OperationDetails>,
    pub show_operation_details: bool,
    pub files: Vec<FileEntry>,
    pub list_state: ListState,
    pub view: DiffView,
    pub diff_mode: DiffMode,
    pub focus: Focus,
    pub keymap: Keymap,
    pub scroll: usize,
    pub viewport_height: usize,
    pub viewport_width: usize,
    pub show_help: bool,
    pub error: Option<String>,
    pub edit_request: Option<EditRequest>,
    highlighter: SyntaxHighlighter,
}

impl App {
    #[cfg(test)]
    pub fn new(repo: Repository) -> Result<Self> {
        Self::with_theme(repo, Theme::default())
    }

    pub fn theme(&self) -> Theme {
        self.highlighter.theme()
    }

    fn cycle_theme(&mut self) -> Result<()> {
        let mut highlighter = SyntaxHighlighter::new(self.theme().next());
        // Recolor the displayed snapshot without rereading or modifying the repository.
        if let Some(entry) = self.selected()
            && let Some((old, new)) = &self.view.source
        {
            let mut view = DiffView::build(
                &entry.path,
                Content::Text(old.clone()),
                Content::Text(new.clone()),
                &mut highlighter,
            )?;
            view.layout(self.viewport_width, self.diff_mode);
            self.view = view;
            self.clamp_scroll();
        }
        self.highlighter = highlighter;
        Ok(())
    }

    pub fn with_theme(repo: Repository, theme: Theme) -> Result<Self> {
        let branch = repo.branch()?;
        let files = repo.files()?;
        let mut app = Self {
            repo,
            branch,
            files,
            commit_popup: None,
            worktree_popup: None,
            worktree_request: None,
            status: None,
            pending: None,
            operation_details: None,
            show_operation_details: false,
            list_state: ListState::default(),
            view: DiffView::default(),
            diff_mode: DiffMode::default(),
            focus: Focus::Files,
            keymap: Keymap::default(),
            scroll: 0,
            viewport_height: 20,
            viewport_width: 80,
            show_help: false,
            error: None,
            edit_request: None,
            highlighter: SyntaxHighlighter::new(theme),
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
        let result = self.update();
        if let Err(error) = result {
            self.error = Some(format!("Refresh: {error:#}; showing last snapshot"));
        }
    }

    fn update(&mut self) -> Result<()> {
        // Read everything before replacing state, so a failed refresh keeps the old view intact.
        let branch = self.repo.branch()?;
        let files = self.repo.files()?;
        let selected = self
            .selected()
            .map(|entry| (entry.path.clone(), entry.staged));
        self.branch = branch;
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
        if self.selected().map(|entry| (&entry.path, entry.staged))
            != selected.as_ref().map(|(path, staged)| (path, *staged))
        {
            self.scroll = 0;
        }
        self.load_selected();
        Ok(())
    }

    fn load_selected(&mut self) {
        let Some(entry) = self.selected().cloned() else {
            self.view = DiffView::notice("No changes — working tree/index is clean");
            self.scroll = 0;
            self.view.layout(self.viewport_width, self.diff_mode);
            return;
        };
        match self
            .repo
            .contents(&entry)
            .and_then(|(old, new)| DiffView::build(&entry.path, old, new, &mut self.highlighter))
        {
            Ok(view) => self.view = view,
            Err(error) => {
                self.view = DiffView::notice("Could not load this file");
                self.error = Some(format!("{}: {error:#}", entry.path.display()));
            }
        }
        self.view.layout(self.viewport_width, self.diff_mode);
        self.clamp_scroll();
    }

    pub fn resize_diff(&mut self, width: usize, height: usize) {
        self.viewport_height = height;
        if self.viewport_width != width {
            self.relayout_diff(width);
        }
        self.clamp_scroll();
    }

    pub fn set_diff_mode(&mut self, mode: DiffMode) {
        if self.diff_mode != mode {
            self.diff_mode = mode;
            self.relayout_diff(self.viewport_width);
            self.clamp_scroll();
        }
    }

    fn relayout_diff(&mut self, width: usize) {
        let anchor = self.view.wrapped_rows.get(self.scroll).copied();
        self.viewport_width = width;
        self.view.layout(width, self.diff_mode);
        if let Some(anchor) = anchor {
            self.scroll = self.view.position(anchor);
        }
    }

    pub fn clamp_scroll(&mut self) {
        self.scroll = self.scroll.min(
            self.view
                .wrapped_rows
                .len()
                .saturating_sub(self.viewport_height.max(1))
                // Let every hunk reach the top, even when several fit in the viewport.
                .max(self.view.wrapped_hunks.last().copied().unwrap_or(0)),
        );
    }

    pub fn active_hunk(&self) -> usize {
        self.view
            .wrapped_hunks
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
        let Some(entry) = self.selected() else {
            return Ok(());
        };
        let staged = entry.staged;
        if self.focus == Focus::Files {
            self.repo.toggle_file(entry)?;
        } else {
            let patch = self.view.patches.get(self.active_hunk()).context(
                "No text hunk to stage/unstage; use Space in the Files pane for the whole file",
            )?;
            let expected = self.view.source.as_ref().context("No text preview")?;
            let current = self
                .repo
                .files()?
                .into_iter()
                .find(|current| current.path == entry.path && current.staged == entry.staged)
                .context("This file has changed; wait for the live refresh and try again")?;
            anyhow::ensure!(
                current.status == entry.status && current.submodule == entry.submodule,
                "This file has changed; wait for the live refresh and try again"
            );
            let (old, new) = self.repo.contents(&current)?;
            anyhow::ensure!(
                matches!((&old, &new), (Content::Text(old), Content::Text(new)) if (old, new) == (&expected.0, &expected.1)),
                "This hunk has changed; wait for the live refresh and try again"
            );
            self.repo.toggle_hunk(entry, patch)?;
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

    fn open_worktrees(&mut self) -> Result<()> {
        self.ensure_idle()?;
        let worktrees = self.repo.worktrees()?;
        let selected = self
            .worktree_popup
            .as_ref()
            .and_then(|popup| {
                popup
                    .list_state
                    .selected()
                    .and_then(|i| popup.worktrees.get(i))
            })
            .map_or(&self.repo.root, |worktree| &worktree.path);
        let position = worktrees
            .iter()
            .position(|worktree| &worktree.path == selected);
        let mut list_state = ListState::default();
        list_state.select(position.or_else(|| (!worktrees.is_empty()).then_some(0)));
        self.worktree_popup = Some(WorktreePopup {
            worktrees,
            list_state,
            viewport_height: 10,
            error: None,
            removal: None,
        });
        self.error = None;
        Ok(())
    }

    fn confirm_worktree_removal(&mut self) -> Result<()> {
        self.ensure_idle()?;
        let popup = self.worktree_popup.as_ref().unwrap();
        let selected = popup
            .list_state
            .selected()
            .and_then(|i| popup.worktrees.get(i))
            .context("No worktree selected")?;
        let worktree = self.repo.removable_worktree(&selected.path)?;
        let popup = self.worktree_popup.as_mut().unwrap();
        popup.removal = Some(worktree);
        popup.error = None;
        Ok(())
    }

    fn handle_worktree_key(&mut self, action: Option<Action>) {
        let popup = self.worktree_popup.as_mut().unwrap();
        if popup.removal.is_some() {
            match action {
                Some(Action::ConfirmRemoval) => {
                    let worktree = popup.removal.take().unwrap();
                    if let Err(error) =
                        self.start_operation(Operation::RemoveWorktree(worktree.path))
                    {
                        self.worktree_popup.as_mut().unwrap().error = Some(format!("{error:#}"));
                    }
                }
                Some(Action::Close) => popup.removal = None,
                _ => {}
            }
            return;
        }
        let page = popup.viewport_height.max(1) as isize;
        let delta = match action {
            Some(Action::Close) => {
                self.worktree_popup = None;
                return;
            }
            Some(Action::Select) => {
                if let Some(worktree) = popup
                    .list_state
                    .selected()
                    .and_then(|i| popup.worktrees.get(i))
                {
                    if worktree.path == self.repo.root {
                        self.worktree_popup = None;
                    } else {
                        self.worktree_request = Some(worktree.path.clone());
                    }
                }
                return;
            }
            Some(Action::DeleteWorktree) => {
                if let Err(error) = self.confirm_worktree_removal() {
                    self.worktree_popup.as_mut().unwrap().error =
                        Some(format!("Delete worktree: {error:#}"));
                }
                return;
            }
            Some(Action::RefreshWorktrees) => {
                if let Err(error) = self.open_worktrees() {
                    self.worktree_popup.as_mut().unwrap().error = Some(format!("{error:#}"));
                }
                return;
            }
            Some(Action::Down) => 1,
            Some(Action::Up) => -1,
            Some(Action::PageDown) => page,
            Some(Action::PageUp) => -page,
            Some(Action::First) => -isize::MAX,
            Some(Action::Last) => isize::MAX,
            _ => return,
        };
        if !popup.worktrees.is_empty() {
            let selected = popup.list_state.selected().unwrap_or(0);
            popup.list_state.select(Some(
                selected
                    .saturating_add_signed(delta)
                    .min(popup.worktrees.len() - 1),
            ));
            popup.error = None;
        }
    }

    fn open_commit(&mut self) -> Result<()> {
        self.ensure_idle()?;
        anyhow::ensure!(!self.repo.files()?.is_empty(), "No changes to commit");
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
                    if self.operation_details.as_ref().unwrap().worktree_removal
                        && let Err(error) = self.open_worktrees()
                    {
                        self.worktree_popup.as_mut().unwrap().error =
                            Some(format!("Refresh: {error:#}"));
                    }
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
                .wrapped_hunks
                .iter()
                .copied()
                .find(|&row| row > self.scroll)
        } else {
            self.view
                .wrapped_hunks
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
        let action = self.keymap.action(key);
        if action == Some(Action::Close)
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
            if action == Some(Action::Close) {
                self.show_help = false;
            }
            return false;
        }
        if self.show_operation_details {
            let details = self.operation_details.as_mut().unwrap();
            let page = details.viewport_height.saturating_sub(2).max(1) as isize;
            match action {
                Some(Action::Close) if self.pending.is_none() => {
                    self.show_operation_details = false;
                }
                Some(Action::Down) => details.scroll_by(1),
                Some(Action::Up) => details.scroll_by(-1),
                Some(Action::PageDown) => details.scroll_by(page),
                Some(Action::PageUp) => details.scroll_by(-page),
                Some(Action::First) => {
                    details.follow = false;
                    details.scroll = 0;
                }
                Some(Action::Last) => {
                    details.follow = true;
                    details.clamp_scroll();
                }
                Some(Action::Left) => details.horizontal = details.horizontal.saturating_sub(4),
                Some(Action::Right) => {
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
            match key {
                _ if action == Some(Action::Close) => self.commit_popup = None,
                KeyEvent {
                    code: KeyCode::Char('s'),
                    modifiers: KeyModifiers::CONTROL,
                    ..
                } => {
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
        if self.worktree_popup.is_some() {
            self.handle_worktree_key(action);
            return false;
        }
        match action {
            Some(Action::Worktrees) => {
                if let Err(error) = self.open_worktrees() {
                    self.error = Some(format!("Worktrees: {error:#}"));
                }
            }
            Some(Action::Quit) => {
                if let Err(error) = self.ensure_idle() {
                    self.error = Some(error.to_string());
                } else {
                    return true;
                }
            }
            Some(Action::Stage) => {
                if let Err(error) = self.toggle_stage() {
                    self.error = Some(format!("Stage: {error:#}"));
                }
            }
            Some(Action::Commit) => {
                if let Err(error) = self.open_commit() {
                    self.error = Some(format!("Commit: {error:#}"));
                }
            }
            Some(action @ (Action::Pull | Action::Push)) => {
                let operation = if action == Action::Pull {
                    Operation::Pull
                } else {
                    Operation::Push
                };
                if let Err(error) = self.start_operation(operation) {
                    self.error = Some(format!("Git: {error:#}"));
                }
            }
            Some(Action::Edit) => self.request_edit(),
            Some(Action::ToggleView) => self.set_diff_mode(self.diff_mode.toggle()),
            Some(Action::NextTheme) => {
                if let Err(error) = self.cycle_theme() {
                    self.error = Some(format!("Theme: {error:#}"));
                }
            }
            Some(Action::Help) => self.show_help = true,
            Some(Action::Focus) => {
                self.focus = if self.focus == Focus::Files {
                    Focus::Diff
                } else {
                    Focus::Files
                };
            }
            Some(Action::Down) => self.vertical(1),
            Some(Action::Up) => self.vertical(-1),
            Some(Action::PageDown) => {
                self.vertical(self.viewport_height.saturating_sub(2).max(1) as isize)
            }
            Some(Action::PageUp) => {
                self.vertical(-(self.viewport_height.saturating_sub(2).max(1) as isize))
            }
            Some(Action::First) => self.vertical(-(isize::MAX)),
            Some(Action::Last) => self.vertical(isize::MAX),
            Some(Action::NextHunk) => self.hunk(true),
            Some(Action::PreviousHunk) => self.hunk(false),
            _ => {}
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::git::tests::{git, repo};

    fn key(ch: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE)
    }

    #[test]
    fn tab_is_the_only_pane_focus_shortcut_in_both_presets() {
        for keymap in [Keymap::Standard, Keymap::Vim] {
            let (dir, repo) = repo();
            std::fs::write(dir.path().join("a.txt"), "line\n".repeat(30)).unwrap();
            let mut app = App::new(repo).unwrap();
            app.keymap = keymap;
            app.scroll = 2;
            for focus in [Focus::Diff, Focus::Files] {
                assert!(!app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)));
                assert!(app.focus == focus);
                for code in [
                    KeyCode::Left,
                    KeyCode::Right,
                    KeyCode::Enter,
                    KeyCode::BackTab,
                    KeyCode::Char('h'),
                    KeyCode::Char('l'),
                ] {
                    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
                    assert!(app.focus == focus);
                }
                assert_eq!(app.selected().unwrap().path, PathBuf::from("a.txt"));
                assert_eq!(app.scroll, 2);
            }
        }
    }

    #[test]
    fn output_navigation_uses_only_the_active_preset() {
        for (keymap, keys, inactive) in [
            (
                Keymap::Standard,
                [
                    KeyCode::Home,
                    KeyCode::Down,
                    KeyCode::PageDown,
                    KeyCode::Up,
                    KeyCode::PageUp,
                    KeyCode::Right,
                    KeyCode::Left,
                    KeyCode::End,
                ],
                KeyCode::Char('j'),
            ),
            (
                Keymap::Vim,
                [
                    KeyCode::Char('g'),
                    KeyCode::Char('j'),
                    KeyCode::Char('d'),
                    KeyCode::Char('k'),
                    KeyCode::Char('u'),
                    KeyCode::Char('l'),
                    KeyCode::Char('h'),
                    KeyCode::Char('G'),
                ],
                KeyCode::Down,
            ),
        ] {
            let (_dir, repo) = repo();
            let mut app = App::new(repo).unwrap();
            app.keymap = keymap;
            let mut details = OperationDetails::new(&Operation::Pull);
            details.viewport_height = 4;
            details.viewport_width = 10;
            details.append(&("long output line\n".repeat(20)));
            details.finish(OperationResult::Success);
            let bottom = details.max_scroll();
            app.operation_details = Some(details);
            app.show_operation_details = true;
            for (code, scroll, horizontal, follow) in [
                (keys[0], 0, 0, false),
                (keys[1], 1, 0, false),
                (keys[2], 3, 0, false),
                (keys[3], 2, 0, false),
                (keys[4], 0, 0, false),
                (keys[5], 0, 4, false),
                (keys[6], 0, 0, false),
                (keys[7], bottom, 0, true),
            ] {
                app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
                let details = app.operation_details.as_ref().unwrap();
                assert_eq!(
                    (details.scroll, details.horizontal, details.follow),
                    (scroll, horizontal, follow),
                    "{keymap:?}: {code:?}"
                );
            }
            for code in [inactive, KeyCode::Char('q')] {
                app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
                assert!(app.show_operation_details);
                assert_eq!(app.operation_details.as_ref().unwrap().scroll, bottom);
            }
            app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
            assert!(!app.show_operation_details);
        }
    }

    #[test]
    fn vim_commit_editor_inserts_navigation_letters_as_text() {
        let (dir, repo) = repo();
        std::fs::write(dir.path().join("file.txt"), "changes\n").unwrap();
        let mut app = App::new(repo).unwrap();
        app.keymap = Keymap::Vim;
        app.handle_key(key('c'));
        for ch in "hjkludgG".chars() {
            app.handle_key(key(ch));
        }
        assert_eq!(app.commit_popup.as_ref().unwrap().message, "hjkludgG");
        app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(app.commit_popup.as_ref().unwrap().message, "hjkludG");
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(app.commit_popup.as_ref().unwrap().message, "hjkludG");
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.commit_popup.is_none());
        assert!(app.handle_key(key('q')));
    }

    #[test]
    fn vertical_navigation_uses_the_focused_pane_in_both_presets() {
        for (keymap, down, up) in [
            (Keymap::Standard, KeyCode::Down, KeyCode::Up),
            (Keymap::Vim, KeyCode::Char('j'), KeyCode::Char('k')),
        ] {
            let (dir, repo) = repo();
            for path in ["a.txt", "b.txt"] {
                std::fs::write(dir.path().join(path), "line\n".repeat(30)).unwrap();
            }
            let mut app = App::new(repo).unwrap();
            app.keymap = keymap;
            for (code, path) in [(down, "b.txt"), (up, "a.txt")] {
                app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
                assert_eq!(app.selected().unwrap().path, PathBuf::from(path));
                assert_eq!(app.scroll, 0);
            }
            app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
            for (code, scroll) in [(down, 1), (down, 2), (up, 1), (up, 0)] {
                app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
                assert_eq!(app.selected().unwrap().path, PathBuf::from("a.txt"));
                assert_eq!(app.scroll, scroll);
            }
        }
    }

    #[test]
    fn refresh_preserves_selection_and_scroll_until_selected_file_disappears() {
        let (dir, repo) = repo();
        std::fs::write(dir.path().join("a.txt"), "first file\n").unwrap();
        let content = format!("{}\n", "x".repeat(120)).repeat(30);
        std::fs::write(dir.path().join("b.txt"), content).unwrap();
        let mut app = App::new(repo).unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.scroll = 2;
        std::fs::write(dir.path().join("0.txt"), "new file\n").unwrap();
        app.refresh();
        assert_eq!(app.files.len(), 3);
        assert_eq!(app.selected().unwrap().path, PathBuf::from("b.txt"));
        assert_eq!(app.scroll, 2);
        std::fs::remove_file(dir.path().join("b.txt")).unwrap();
        app.refresh();
        assert_eq!(app.selected().unwrap().path, PathBuf::from("0.txt"));
        assert_eq!(app.scroll, 0);
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
        (dir, App::new(repo).unwrap())
    }

    #[test]
    fn theme_shortcut_cycles_and_preserves_the_review_and_repository() {
        for keymap in [Keymap::Standard, Keymap::Vim] {
            for mode in [DiffMode::Unified, DiffMode::SideBySide] {
                let (dir, mut app) = app_with_hunks();
                app.keymap = keymap;
                app.set_diff_mode(mode);
                app.focus = Focus::Diff;
                app.resize_diff(80, 4);
                app.handle_key(key(']'));
                let scroll = app.scroll;
                let selected = app.list_state.selected();
                let source = app.view.source.clone();
                let patches = app.view.patches.clone();
                let path = dir.path().join("a file.txt");
                let contents = std::fs::read(&path).unwrap();
                let index = std::fs::read(app.repo.git_dir.join("index")).unwrap();
                app.status = Some(Status::Success("Previous operation result".into()));
                for (_, theme) in Theme::ALL.iter().cycle().skip(1).take(Theme::ALL.len()) {
                    assert!(!app.handle_key(key('t')));
                    assert_eq!(app.theme(), *theme);
                    assert_eq!(app.scroll, scroll);
                    assert_eq!(app.list_state.selected(), selected);
                    assert!(app.focus == Focus::Diff);
                    assert_eq!(app.diff_mode, mode);
                    assert_eq!(app.view.source, source);
                    assert_eq!(app.view.patches, patches);
                    assert_eq!(
                        app.status.as_ref().unwrap().message(),
                        "Previous operation result"
                    );
                }
                assert_eq!(app.theme(), Theme::Default);
                assert_eq!(std::fs::read(&path).unwrap(), contents);
                assert_eq!(
                    std::fs::read(app.repo.git_dir.join("index")).unwrap(),
                    index
                );
            }
        }
    }

    #[test]
    fn theme_changes_keep_the_displayed_snapshot_until_refresh() {
        let (dir, mut app) = app_with_hunks();
        let source = app.view.source.clone();
        std::fs::write(dir.path().join("a file.txt"), "newer edits\n").unwrap();
        app.handle_key(key('t'));
        assert_eq!(app.theme(), Theme::TokyoNight);
        assert_eq!(app.view.source, source);
        app.refresh();
        assert_eq!(app.theme(), Theme::TokyoNight);
        assert_eq!(app.view.source.as_ref().unwrap().1, "newer edits\n");
        assert!(
            app.view
                .rows
                .iter()
                .filter(|row| row.kind == crate::diff::RowKind::Added)
                .flat_map(|row| &row.spans)
                .all(|span| span.style.fg == Some(Theme::TokyoNight.palette().text))
        );
    }

    #[test]
    fn theme_shortcut_respects_modal_input_and_works_without_changes() {
        let (_dir, repo) = repo();
        let mut app = App::new(repo).unwrap();
        app.handle_key(key('t'));
        assert_eq!(app.theme(), Theme::TokyoNight);
        app.handle_key(key('?'));
        app.handle_key(key('t'));
        assert_eq!(app.theme(), Theme::TokyoNight);
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.commit_popup = Some(CommitPopup::default());
        app.handle_key(key('t'));
        assert_eq!(app.commit_popup.as_ref().unwrap().message, "t");
        assert_eq!(app.theme(), Theme::TokyoNight);
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.handle_key(key('w'));
        app.handle_key(key('t'));
        assert_eq!(app.theme(), Theme::TokyoNight);
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.handle_key(key('t'));
        assert_eq!(app.theme(), Theme::Dracula);
    }

    #[test]
    fn view_toggle_preserves_file_focus_source_patches_and_repository_contents() {
        for keymap in [Keymap::Standard, Keymap::Vim] {
            assert_view_toggle(keymap);
        }
    }

    fn assert_view_toggle(keymap: Keymap) {
        let (dir, mut app) = app_with_hunks();
        app.keymap = keymap;
        let path = dir.path().join("a file.txt");
        let contents = std::fs::read(&path).unwrap();
        let index = std::fs::read(app.repo.git_dir.join("index")).unwrap();
        let source = app.view.source.clone();
        let patches = app.view.patches.clone();
        let selected = app.selected().unwrap().path.clone();
        assert_eq!(app.diff_mode, DiffMode::Unified);
        app.handle_key(key('v'));
        assert_eq!(app.diff_mode, DiffMode::SideBySide);
        assert!(!app.view.split_rows.is_empty());
        assert!(app.focus == Focus::Files);
        assert_eq!(app.selected().unwrap().path, selected);
        assert_eq!(app.view.source, source);
        assert_eq!(app.view.patches, patches);
        let down = if keymap == Keymap::Standard {
            KeyCode::Down
        } else {
            KeyCode::Char('j')
        };
        app.handle_key(KeyEvent::new(down, KeyModifiers::NONE));
        assert!(app.selected().unwrap().staged);
        assert!(!app.view.split_rows.is_empty());
        app.refresh();
        assert_eq!(app.diff_mode, DiffMode::SideBySide);
        assert!(!app.view.split_rows.is_empty());
        app.handle_key(key('v'));
        assert_eq!(app.diff_mode, DiffMode::Unified);
        assert!(app.view.split_rows.is_empty());
        assert_eq!(std::fs::read(&path).unwrap(), contents);
        assert_eq!(
            std::fs::read(app.repo.git_dir.join("index")).unwrap(),
            index
        );
    }

    #[test]
    fn toggling_and_narrow_fallback_preserve_the_active_hunk_and_mode_preference() {
        let (_dir, mut app) = app_with_hunks();
        app.resize_diff(80, 4);
        app.handle_key(key(']'));
        assert_eq!(app.active_hunk(), 1);
        for _ in 0..2 {
            app.handle_key(key('v'));
            assert_eq!(app.active_hunk(), 1);
            assert_eq!(app.view.wrapped_rows[app.scroll].row, app.view.hunks[1]);
        }
        app.handle_key(key('v'));
        app.resize_diff(59, 4);
        assert_eq!(app.diff_mode, DiffMode::SideBySide);
        assert!(app.view.split_rows.is_empty());
        assert_eq!(app.active_hunk(), 1);
        app.resize_diff(100, 4);
        assert!(!app.view.split_rows.is_empty());
        assert_eq!(app.active_hunk(), 1);
    }

    #[test]
    fn wrapped_hunks_keep_navigation_editing_and_staging_on_the_source_hunk() {
        for mode in [DiffMode::Unified, DiffMode::SideBySide] {
            assert_wrapped_hunk_actions(mode);
        }
    }

    fn assert_wrapped_hunk_actions(mode: DiffMode) {
        let (dir, mut app) = app_with_hunks();
        let path = dir.path().join("a file.txt");
        let content = std::fs::read_to_string(&path)
            .unwrap()
            .replace("changed 10", &"x".repeat(150))
            .replace("changed 50", &"y".repeat(150));
        std::fs::write(&path, content).unwrap();
        app.refresh();
        app.focus = Focus::Diff;
        app.set_diff_mode(mode);
        app.resize_diff(if mode == DiffMode::Unified { 24 } else { 60 }, 4);
        let second = app.view.wrapped_hunks[1];
        assert!(second > app.view.hunks[1]);
        app.handle_key(key(']'));
        assert_eq!(app.scroll, second);
        assert_eq!(app.active_hunk(), 1);

        let continuation = app
            .view
            .wrapped_rows
            .iter()
            .position(|wrapped| {
                app.view.rows[wrapped.row].new_line == Some(50) && wrapped.offset > 0
            })
            .unwrap();
        app.scroll = continuation;
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.scroll, continuation + 1);
        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.scroll, continuation);
        app.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(app.scroll, continuation + 2);
        app.handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
        assert_eq!(app.scroll, continuation);
        app.handle_key(key('e'));
        assert_eq!(app.edit_request.take().unwrap().line, Some(47));
        app.handle_key(key('['));
        assert_eq!(app.scroll, second);
        app.handle_key(key(']'));
        assert_eq!(app.scroll, app.view.wrapped_hunks[2]);

        app.scroll = continuation;
        let expected = app
            .view
            .source
            .as_ref()
            .unwrap()
            .0
            .replace("line 50\n", &format!("{}\n", "y".repeat(150)));
        app.handle_key(key(' '));
        let staged = app.repo.staged_files().unwrap();
        assert_eq!(staged.len(), 1);
        let contents = crate::git::tests::text(app.repo.contents(&staged[0]).unwrap().1);
        assert_eq!(contents, expected);
    }

    #[test]
    fn resizing_wrapped_lines_keeps_the_source_position_visible() {
        let (dir, repo) = repo();
        std::fs::write(
            dir.path().join("long.txt"),
            format!("{}\n", "x".repeat(150)),
        )
        .unwrap();
        let mut app = App::new(repo).unwrap();
        app.resize_diff(24, 3);
        app.scroll = 5;
        let anchor = app.view.wrapped_rows[app.scroll];
        assert_eq!((anchor.row, anchor.offset), (1, 60));
        app.resize_diff(40, 3);
        let visible = app.view.wrapped_rows[app.scroll];
        assert_eq!(visible.row, anchor.row);
        assert!(visible.offset <= anchor.offset && visible.offset + visible.width > anchor.offset);
        assert_eq!(app.scroll, 2);
        app.resize_diff(24, 3);
        assert_eq!(app.view.wrapped_rows[app.scroll].row, anchor.row);
        assert_eq!(app.view.wrapped_rows[app.scroll].offset, 30);
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
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
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
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.handle_key(key('e'));
        assert_eq!(app.edit_request.take().unwrap().line, Some(47));
        app.handle_key(key('['));
        assert_eq!(app.scroll, app.view.hunks[1]);
        app.handle_key(key('['));
        assert_eq!(app.scroll, 0);
        app.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(app.scroll, 2);
        app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(app.scroll, app.view.rows.len() - app.viewport_height);
        app.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(app.scroll, 0);
        app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
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
        let mut app = App::new(repo).unwrap();
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
    fn edit_shortcut_respects_help_and_empty_file_lists() {
        let (dir, repo) = repo();
        let mut app = App::new(repo).unwrap();
        app.handle_key(key('e'));
        assert!(app.edit_request.is_none());
        std::fs::write(dir.path().join("test.txt"), "test\n").unwrap();
        app.refresh();
        app.handle_key(key('?'));
        for ch in ['e', 'v', 'q'] {
            assert!(!app.handle_key(key(ch)));
        }
        assert!(app.show_help);
        assert_eq!(app.diff_mode, DiffMode::Unified);
        assert!(app.edit_request.is_none());
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.handle_key(key('e'));
        assert_eq!(
            app.edit_request.take(),
            Some(EditRequest {
                path: PathBuf::from("test.txt"),
                line: Some(1),
            })
        );
    }

    #[test]
    fn refresh_tracks_commits_and_branch_changes() {
        let (dir, repo) = repo();
        std::fs::write(dir.path().join("file.txt"), "initial\n").unwrap();
        let mut app = App::new(repo).unwrap();
        assert_eq!(app.files.len(), 1);
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-qm", "Initial"]);
        git(dir.path(), &["checkout", "-qb", "feature"]);
        app.refresh();
        assert_eq!(app.branch, "feature");
        assert!(app.files.is_empty());
        assert_eq!(
            app.view.rows[0].spans[0].content,
            "No changes — working tree/index is clean"
        );
        std::fs::write(dir.path().join("file.txt"), "changed\n").unwrap();
        app.refresh();
        assert_eq!(app.files.len(), 1);
        assert_eq!(
            app.view.source,
            Some(("initial\n".into(), "changed\n".into()))
        );
    }

    #[test]
    fn failed_refresh_preserves_the_last_snapshot_and_recovers() {
        let (dir, repo) = repo();
        std::fs::write(dir.path().join("file.txt"), "snapshot\n").unwrap();
        let mut app = App::new(repo).unwrap();
        let source = app.view.source.clone();
        let git_dir = dir.path().join(".git");
        let backup = dir.path().join("git-backup");
        std::fs::rename(&git_dir, &backup).unwrap();
        app.refresh();
        assert!(
            app.error
                .as_ref()
                .unwrap()
                .contains("showing last snapshot")
        );
        assert_eq!(app.files.len(), 1);
        assert_eq!(app.selected().unwrap().path, PathBuf::from("file.txt"));
        assert_eq!(app.view.source, source);
        std::fs::rename(backup, git_dir).unwrap();
        std::fs::write(dir.path().join("file.txt"), "recovered\n").unwrap();
        app.refresh();
        assert!(app.error.is_none());
        assert_eq!(app.view.source, Some(("".into(), "recovered\n".into())));
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
        assert!(app.selected().unwrap().staged);
        assert_eq!(app.view.hunks.len(), 1); // New file on an unborn branch.
        app.handle_key(key(' '));
        assert_eq!(app.files.len(), 1);
        assert!(!app.selected().unwrap().staged);
        assert!(app.repo.staged_files().unwrap().is_empty());
        assert_eq!(std::fs::read_to_string(path).unwrap(), working);
    }

    #[test]
    fn space_in_diff_only_toggles_the_active_hunk_even_in_a_tall_viewport() {
        for mode in [DiffMode::Unified, DiffMode::SideBySide] {
            assert_hunk_staging(mode);
        }
    }

    fn assert_hunk_staging(mode: DiffMode) {
        let (dir, mut app) = app_with_hunks();
        app.set_diff_mode(mode);
        git(dir.path(), &["commit", "-qm", "Initial"]);
        app.refresh();
        app.viewport_height = 1000;
        app.handle_key(key(']'));
        assert_eq!(app.active_hunk(), 1);
        assert_eq!(app.scroll, app.view.wrapped_hunks[1]);
        // The header is offscreen, but the middle hunk is still active.
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.active_hunk(), 1);
        app.handle_key(key(' '));
        assert!(app.error.is_none(), "{:?}", app.error);
        assert_eq!(app.view.hunks.len(), 2);
        assert_eq!(app.files.len(), 2);
        assert!(!app.selected().unwrap().staged);
        let staged = app.repo.staged_files().unwrap();
        let staged_text = crate::git::tests::text(app.repo.contents(&staged[0]).unwrap().1);
        assert!(staged_text.contains("changed 50\n"));
        assert!(!staged_text.contains("changed 10\n"));
        assert!(!staged_text.contains("changed 80\n"));
        let working = std::fs::read_to_string(dir.path().join("a file.txt")).unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        // Select the staged entry in the same view.
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
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
        let staged_text = crate::git::tests::text(app.repo.contents(&staged[0]).unwrap().1);
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
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
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

        // Removing that staged entry keeps the file selected, not its old scroll.
        git(dir.path(), &["reset", "-q", "--", "a file.txt"]);
        app.refresh();
        assert_eq!(app.files.len(), 1);
        assert_eq!(app.selected().unwrap().path, PathBuf::from("a file.txt"));
        assert!(!app.selected().unwrap().staged);
        assert_eq!(app.scroll, 0);
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
    fn space_respects_notices_and_empty_lists() {
        let (dir, repo) = repo();
        let mut app = App::new(repo).unwrap();
        app.handle_key(key(' '));
        assert!(app.error.is_none());
        app.handle_key(key('c'));
        assert!(app.error.as_ref().unwrap().contains("No changes to commit"));
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
    }

    #[test]
    fn unstaged_changes_are_committed_only_after_submitting_the_message() {
        let (dir, repo) = repo();
        std::fs::write(dir.path().join("first.txt"), "first\n").unwrap();
        std::fs::write(dir.path().join("second.txt"), "second\n").unwrap();
        let mut app = App::new(repo).unwrap();

        app.handle_key(key('c'));
        assert!(app.commit_popup.is_some(), "{:?}", app.error);
        app.handle_paste("Cancelled message");
        assert!(app.repo.staged_files().unwrap().is_empty());
        assert!(app.operation_label().is_none());
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.commit_popup.is_none());
        assert!(app.repo.staged_files().unwrap().is_empty());
        assert_eq!(app.repo.files().unwrap().len(), 2);

        app.handle_key(key('c'));
        app.handle_paste("Commit everything");
        app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert_eq!(app.operation_label(), Some("Committing"));
        wait_for_operation(&mut app);

        assert!(app.commit_popup.is_none(), "{:?}", app.error);
        assert!(matches!(app.status, Some(Status::Success(_))));
        assert!(app.repo.files().unwrap().is_empty());
        assert!(app.files.is_empty());
    }

    #[test]
    fn commit_scope_uses_the_index_at_submission_instead_of_the_popup_snapshot() {
        let (dir, repo) = repo();
        std::fs::write(dir.path().join("file.txt"), "staged later\n").unwrap();
        let mut app = App::new(repo).unwrap();
        app.handle_key(key('c'));
        git(dir.path(), &["add", "."]);
        std::fs::write(dir.path().join("file.txt"), "unstaged\n").unwrap();
        std::fs::write(dir.path().join("untracked.txt"), "untracked\n").unwrap();

        app.handle_paste("Commit the index");
        app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        wait_for_operation(&mut app);

        assert!(matches!(app.status, Some(Status::Success(_))));
        assert_eq!(app.files.len(), 2);
        assert_eq!(
            crate::git::tests::text(app.repo.contents(&app.files[0]).unwrap().0),
            "staged later\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("file.txt")).unwrap(),
            "unstaged\n"
        );
        assert_eq!(app.files[1].path, PathBuf::from("untracked.txt"));
        assert_eq!(app.files[1].status, '?');
    }

    #[test]
    fn commit_popup_is_modal_supports_paste_and_keeps_unstaged_changes() {
        let (dir, repo) = repo();
        std::fs::write(dir.path().join("file.txt"), "staged\n").unwrap();
        git(dir.path(), &["add", "."]);
        std::fs::write(dir.path().join("file.txt"), "unstaged\n").unwrap();
        let mut app = App::new(repo).unwrap();
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
        for ch in "qb pPe?sv".chars() {
            assert!(!app.handle_key(key(ch)));
        }
        assert_eq!(app.commit_popup.as_ref().unwrap().message, "qb pPe?sv");
        assert_eq!(app.diff_mode, DiffMode::Unified);
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
            crate::git::tests::text(app.repo.contents(&app.files[0]).unwrap().0),
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
    fn automatic_staging_streams_output_and_can_be_cancelled_before_committing() {
        let (dir, repo) = repo();
        std::fs::write(dir.path().join(".gitattributes"), "*.txt filter=wait\n").unwrap();
        std::fs::write(dir.path().join("file.txt"), "new\n").unwrap();
        git(
            dir.path(),
            &[
                "config",
                "filter.wait.clean",
                "echo 'filter waiting' >&2; sleep 60; cat",
            ],
        );
        git(dir.path(), &["config", "filter.wait.required", "true"]);
        let mut app = App::new(repo).unwrap();
        app.handle_key(key('c'));
        app.handle_paste("Commit after staging");
        app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        wait_for_output(&mut app, &["filter waiting"]);

        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        wait_for_operation(&mut app);

        assert_eq!(
            app.operation_details.as_ref().unwrap().result,
            Some(OperationResult::Cancelled)
        );
        assert_eq!(
            app.commit_popup.as_ref().unwrap().message,
            "Commit after staging"
        );
        assert_eq!(app.repo.files().unwrap().len(), 2);
        assert!(app.repo.staged_files().unwrap().is_empty());

        git(dir.path(), &["config", "filter.wait.clean", "cat"]);
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        wait_for_operation(&mut app);
        assert!(matches!(app.status, Some(Status::Success(_))));
        assert!(app.files.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn failed_automatic_commit_keeps_the_message_and_staged_changes_for_retry() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, repo) = repo();
        std::fs::write(dir.path().join("file.txt"), "staged\n").unwrap();
        let hook = repo.git_dir.join("hooks/pre-commit");
        std::fs::write(
            &hook,
            "#!/bin/sh\necho 'Commit rejected by test hook'\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut app = App::new(repo).unwrap();
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
        let mut app = App::new(repo).unwrap();
        app.handle_key(key('P'));
        assert_eq!(app.operation_label(), Some("Pushing"));
        assert!(!app.handle_key(key('q')));
        assert!(!app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)));
        app.handle_key(key(' '));
        assert_eq!(app.operation_label(), Some("Pushing"));
        assert!(app.show_operation_details);
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
        let mut app = App::new(repo).unwrap();
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
        let mut app = App::new(repo).unwrap();
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
        let mut app = App::new(repo).unwrap();
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
            let mut app = App::new(repo.clone()).unwrap();
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
        let mut app = App::new(repo).unwrap();
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
    fn worktree_picker_is_modal_and_requests_a_switch_without_changing_the_repository() {
        let (_dir, _linked, repo, linked_repo) = crate::git::tests::linked_repo();
        std::fs::write(repo.root.join("main.txt"), "main changes\n").unwrap();
        let mut app = App::new(repo).unwrap();
        app.handle_key(key('w'));
        assert_eq!(
            app.worktree_popup.as_ref().unwrap().list_state.selected(),
            Some(0)
        );
        for ch in [' ', 'c', 'e', 'p', 'P', '?', 'q', 'w', 'v'] {
            assert!(!app.handle_key(key(ch)));
        }
        app.handle_paste("ignored while choosing a worktree");
        assert_eq!(app.diff_mode, DiffMode::Unified);
        assert_eq!(app.branch, "main");
        assert_eq!(app.selected().unwrap().path, PathBuf::from("main.txt"));
        assert_eq!(app.repo.files().unwrap().len(), 1);
        assert!(!app.repo.files().unwrap()[0].staged);
        assert!(app.commit_popup.is_none());
        assert!(app.edit_request.is_none());
        assert!(app.pending.is_none());
        assert!(!app.show_help);

        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(
            app.worktree_popup.as_ref().unwrap().list_state.selected(),
            Some(1)
        );
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.worktree_request.take(), Some(linked_repo.root));
        assert_eq!(app.branch, "main"); // The event loop installs the watcher before switching.
        assert!(app.worktree_popup.is_some());
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.worktree_popup.is_none());
        assert_eq!(app.selected().unwrap().path, PathBuf::from("main.txt"));
        assert!(!app.handle_key(key('w')));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.worktree_popup.is_none()); // Choosing the active worktree is a no-op.
        assert!(app.worktree_request.is_none());
    }

    #[test]
    fn worktree_picker_refreshes_and_navigates_from_a_linked_worktree() {
        let (dir, linked, repo, linked_repo) = crate::git::tests::linked_repo();
        let mut app = App::new(linked_repo).unwrap();
        app.handle_key(key('w'));
        assert_eq!(
            app.worktree_popup.as_ref().unwrap().list_state.selected(),
            Some(1)
        );
        let added = linked.path().join("another");
        git(
            dir.path(),
            &["worktree", "add", "-b", "another", added.to_str().unwrap()],
        );
        app.handle_key(key('r'));
        let popup = app.worktree_popup.as_ref().unwrap();
        assert_eq!(popup.worktrees.len(), 3);
        assert_eq!(
            popup.worktrees[popup.list_state.selected().unwrap()].path,
            app.repo.root
        );
        for (code, expected) in [
            (KeyCode::Home, 0),
            (KeyCode::Up, 0),
            (KeyCode::End, 2),
            (KeyCode::Down, 2),
            (KeyCode::PageUp, 0),
            (KeyCode::PageDown, 2),
        ] {
            app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
            assert_eq!(
                app.worktree_popup.as_ref().unwrap().list_state.selected(),
                Some(expected)
            );
        }
        app.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.worktree_request.take(), Some(repo.root));
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.worktree_popup.is_none());
    }

    #[test]
    fn worktree_deletion_requires_confirmation_and_refreshes_the_picker() {
        let (dir, _linked, repo, linked_repo) = crate::git::tests::linked_repo();
        std::fs::write(repo.root.join("main.txt"), "main changes\n").unwrap();
        let mut app = App::new(repo).unwrap();
        app.handle_key(key('w'));
        app.handle_key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
        assert!(
            app.worktree_popup
                .as_ref()
                .unwrap()
                .error
                .as_ref()
                .unwrap()
                .contains("current worktree")
        );
        assert!(app.worktree_popup.as_ref().unwrap().removal.is_none());
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        for _ in 0..2 {
            app.handle_key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
            assert_eq!(
                app.worktree_popup
                    .as_ref()
                    .unwrap()
                    .removal
                    .as_ref()
                    .unwrap()
                    .path,
                linked_repo.root
            );
            for code in [
                KeyCode::Enter,
                KeyCode::Down,
                KeyCode::Char('r'),
                KeyCode::Char(' '),
            ] {
                app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
            }
            assert!(linked_repo.root.is_dir());
            assert_eq!(app.worktree_popup.as_ref().unwrap().worktrees.len(), 2);
            assert_eq!(
                app.worktree_popup.as_ref().unwrap().list_state.selected(),
                Some(1)
            );
            assert!(app.pending.is_none());
            assert!(app.worktree_request.is_none());
            app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
            assert!(app.worktree_popup.as_ref().unwrap().removal.is_none());
            assert!(linked_repo.root.is_dir());
        }
        app.handle_key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
        app.handle_key(key('y'));
        assert_eq!(app.operation_label(), Some("Removing worktree"));
        assert!(app.show_operation_details);
        assert!(!app.handle_key(key('q')));
        wait_for_operation(&mut app);
        assert_eq!(
            app.operation_details.as_ref().unwrap().result,
            Some(OperationResult::Success)
        );
        assert!(
            app.status
                .as_ref()
                .unwrap()
                .message()
                .starts_with("Removed worktree ")
        );
        assert_eq!(app.worktree_popup.as_ref().unwrap().worktrees.len(), 1);
        assert_eq!(
            app.worktree_popup.as_ref().unwrap().list_state.selected(),
            Some(0)
        );
        assert_eq!(
            app.view.source,
            Some((String::new(), "main changes\n".into()))
        );
        assert_eq!(app.branch, "main");
        assert!(!linked_repo.root.exists());
        git(dir.path(), &["show-ref", "--verify", "refs/heads/linked"]);
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.show_operation_details);
        assert!(app.worktree_popup.is_some());
    }

    #[test]
    fn worktree_deletion_rechecks_dirty_files_and_locks_after_confirmation() {
        for locked in [false, true] {
            let (dir, _linked, repo, linked_repo) = crate::git::tests::linked_repo();
            let mut app = App::new(repo).unwrap();
            app.handle_key(key('w'));
            app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
            app.handle_key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
            assert!(app.worktree_popup.as_ref().unwrap().removal.is_some());
            if locked {
                git(
                    dir.path(),
                    &["worktree", "lock", linked_repo.root.to_str().unwrap()],
                );
            } else {
                std::fs::write(
                    linked_repo.root.join("keep.txt"),
                    "new work after confirmation\n",
                )
                .unwrap();
            }
            app.handle_key(key('y'));
            wait_for_operation(&mut app);
            assert!(matches!(
                app.operation_details.as_ref().unwrap().result,
                Some(OperationResult::Failed(_))
            ));
            let details = app.operation_details.as_ref().unwrap();
            let output = details.lines.iter().cloned().collect::<Vec<_>>().join("\n");
            assert!(
                output.contains(if locked {
                    "locked worktree"
                } else {
                    "modified or untracked files"
                }),
                "{output}"
            );
            assert_eq!(app.worktree_popup.as_ref().unwrap().worktrees.len(), 2);
            assert_eq!(
                app.worktree_popup.as_ref().unwrap().list_state.selected(),
                Some(1)
            );
            assert_eq!(linked_repo.branch().unwrap(), "linked");
            assert_eq!(app.branch, "main");
            if locked {
                assert!(app.worktree_popup.as_ref().unwrap().worktrees[1].locked);
            } else {
                assert_eq!(
                    std::fs::read_to_string(linked_repo.root.join("keep.txt")).unwrap(),
                    "new work after confirmation\n"
                );
            }
        }
    }

    #[test]
    fn worktree_picker_waits_for_running_git_operations() {
        let (_dir, repo) = repo();
        let mut app = App::new(repo).unwrap();
        let (_sender, events) = std::sync::mpsc::channel();
        app.pending = Some(OperationHandle {
            events,
            cancel: std::sync::mpsc::channel().0,
        });
        app.handle_key(key('w'));
        assert!(
            app.error
                .as_ref()
                .unwrap()
                .contains("Wait for the current Git operation")
        );
        assert!(app.worktree_popup.is_none());
        assert!(app.worktree_request.is_none());
    }
}
