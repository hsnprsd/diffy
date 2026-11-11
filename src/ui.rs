use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, Clear, List, ListItem, Paragraph, Scrollbar,
        ScrollbarOrientation, ScrollbarState, Wrap,
    },
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::{
    app::{App, CommitPopup, Focus, Status},
    diff::{DiffMode, DiffRow, DiffView, RowKind, SplitRow, WrappedRow, split_widths},
    git::safe_text,
    keymap::{Action, Keymap},
    operation::{OperationDetails, OperationResult},
    theme::Palette,
};

pub fn draw(frame: &mut Frame, app: &mut App) {
    let palette = app.theme().palette();
    let area = frame.area();
    frame.render_widget(
        Block::default().style(Style::default().bg(palette.bg).fg(palette.text)),
        area,
    );
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .split(area);
    let repo_name = app
        .repo
        .root
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    let header = Line::from(vec![
        Span::styled(
            " diffy ",
            Style::default().fg(palette.bg).bg(palette.accent).bold(),
        ),
        Span::styled(
            format!("  {}", safe_text(&repo_name)),
            Style::default().bold(),
        ),
        Span::styled(
            format!("  /  {}  ·  {}", safe_text(&app.branch), app.theme().name()),
            Style::default().fg(palette.muted),
        ),
    ]);
    frame.render_widget(Paragraph::new(header), chunks[0]);

    let body = chunks[1];
    if body.width >= 80 {
        let sidebar = (body.width / 4).clamp(24, 42);
        let panes =
            Layout::horizontal([Constraint::Length(sidebar), Constraint::Min(0)]).split(body);
        files(frame, app, panes[0]);
        diff(frame, app, panes[1]);
    } else if app.focus == Focus::Files {
        files(frame, app, body);
    } else {
        diff(frame, app, body);
    }
    let footer = if let Some(label) = app.operation_label() {
        let details = app.operation_details.as_ref().unwrap();
        Line::styled(
            format!(
                " {label}… · Esc cancel · {}",
                safe_text(details.summary(true))
            ),
            Style::default().fg(palette.accent),
        )
    } else if let Some(error) = &app.error {
        Line::styled(
            format!(" {}", safe_text(error)),
            Style::default().fg(palette.red),
        )
    } else if let Some(status) = &app.status {
        Line::styled(
            format!(" {}", safe_text(status.message())),
            Style::default().fg(match status {
                Status::Success(_) => palette.green,
                Status::Error(_) => palette.red,
                Status::Cancelled(_) => palette.accent,
            }),
        )
    } else {
        Line::styled(
            footer_hints(app, chunks[2].width as usize),
            Style::default().fg(palette.muted),
        )
    };
    frame.render_widget(
        Paragraph::new(footer).style(Style::default().bg(palette.panel)),
        chunks[2],
    );
    if app.worktree_popup.is_some() {
        worktree_popup(frame, area, app);
    }
    if let Some(popup) = &app.commit_popup {
        commit_popup(frame, area, popup, app.operation_label().is_some(), palette);
    }
    if app.show_help {
        help(frame, area, app.keymap, palette);
    }
    if app.show_operation_details
        && let Some(details) = &mut app.operation_details
    {
        operation_details(frame, area, details, app.keymap, palette);
    }
}

fn footer_hints(app: &App, width: usize) -> String {
    let mut hints = Vec::new();
    if let Some(entry) = app.selected()
        && (app.focus == Focus::Files || !app.view.patches.is_empty())
    {
        let action = if entry.staged { "unstage" } else { "stage" };
        let target = if app.focus == Focus::Files {
            "file"
        } else {
            "hunk"
        };
        hints.push(format!("Space {action} {target}"));
    }
    hints.push(
        if app.focus == Focus::Files {
            "Tab diff"
        } else {
            "Tab files"
        }
        .into(),
    );
    hints.push("? help".into());
    let extra = if app.focus == Focus::Files {
        (!app.files.is_empty()).then_some("c commit")
    } else {
        (!app.view.patches.is_empty()).then_some("[/] hunk")
    };
    if let Some(extra) = extra
        && 1 + hints.join(" · ").width() + 3 + extra.width() <= width
    {
        hints.insert(1, extra.into());
    }
    format!(" {}", hints.join(" · "))
}

fn pane(title: String, focused: bool, palette: &Palette) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(if focused {
            BorderType::Thick
        } else {
            BorderType::Plain
        })
        .title(title)
        .title_style(if focused {
            Style::default().bold()
        } else {
            Style::default()
        })
        .border_style(Style::default().fg(if focused {
            palette.accent
        } else {
            palette.border
        }))
        .style(Style::default().fg(palette.text).bg(palette.bg))
}

fn files(frame: &mut Frame, app: &mut App, area: Rect) {
    let palette = app.theme().palette();
    let block = pane(
        format!(" Files {} ", app.files.len()),
        app.focus == Focus::Files,
        palette,
    );
    let inner = block.inner(area);
    let items: Vec<ListItem> = app
        .files
        .iter()
        .map(|entry| {
            let color = match entry.status {
                'A' | '?' => palette.green,
                'D' | 'U' => palette.red,
                _ => palette.accent,
            };
            let mut spans = vec![Span::styled(
                if entry.staged { "[x] " } else { "[ ] " },
                Style::default().fg(if entry.staged {
                    palette.green
                } else {
                    palette.muted
                }),
            )];
            spans.push(Span::styled(
                format!("{} ", entry.status),
                Style::default().fg(color),
            ));
            spans.push(Span::raw(safe_text(&entry.path.to_string_lossy())));
            ListItem::new(Line::from(spans))
        })
        .collect();
    let selection = if app.focus == Focus::Files {
        Style::default().bg(palette.hunk_bg).bold()
    } else {
        Style::default().bg(palette.panel)
    };
    let list = List::new(items)
        .block(block)
        .highlight_symbol("▸ ")
        .highlight_style(selection);
    frame.render_stateful_widget(list, area, &mut app.list_state);
    if app.files.is_empty() {
        frame.render_widget(
            Paragraph::new(" No changes").style(Style::default().fg(palette.muted)),
            inner,
        );
    }
}

fn diff(frame: &mut Frame, app: &mut App, area: Rect) {
    let palette = app.theme().palette();
    let title = app
        .selected()
        .map(|entry| {
            let path = safe_text(&entry.path.to_string_lossy());
            format!(
                "{path} · {}",
                if entry.staged { "Staged" } else { "Unstaged" }
            )
        })
        .unwrap_or_else(|| "Changes".into());
    let block = pane(String::new(), app.focus == Focus::Diff, palette);
    let inner = block.inner(area);
    let mode = app.diff_mode.effective(inner.width as usize);
    let mode_label = if mode != app.diff_mode {
        "Unified (narrow)"
    } else {
        mode.name()
    };
    let block = block.title(format!(" Diff · {mode_label} · {title} "));
    let header_height = u16::from(mode == DiffMode::SideBySide).min(inner.height);
    let content = Rect::new(
        inner.x,
        inner.y + header_height,
        inner.width,
        inner.height.saturating_sub(header_height),
    );
    app.resize_diff(content.width as usize, content.height as usize);
    let language = app.view.language.map_or("Text", |language| language.name());
    let target = if app.focus == Focus::Diff && !app.view.patches.is_empty() {
        if app.selected().is_some_and(|entry| entry.staged) {
            "Space unstage hunk"
        } else {
            "Space stage hunk"
        }
    } else {
        "Hunk"
    };
    let summary = Line::from(vec![
        Span::raw(format!(
            " {target} {}/{} · {language} · ",
            if app.view.hunks.is_empty() {
                0
            } else {
                app.active_hunk() + 1
            },
            app.view.hunks.len()
        )),
        Span::styled(
            format!("+{}", app.view.additions),
            Style::default().fg(palette.green),
        ),
        Span::raw(" / "),
        Span::styled(
            format!("-{} ", app.view.deletions),
            Style::default().fg(palette.red),
        ),
    ]);
    frame.render_widget(block.title_bottom(summary), area);
    if mode == DiffMode::SideBySide {
        let (old_width, new_width) = split_widths(inner.width as usize);
        let (before, after) = if app.selected().is_some_and(|entry| entry.staged) {
            (" Before · HEAD", " After · Index")
        } else {
            (" Before · Index", " After · Working tree")
        };
        frame.render_widget(
            Paragraph::new(format!("{before:<old_width$}│{after:<new_width$}"))
                .style(Style::default().fg(palette.muted).bg(palette.panel)),
            Rect::new(inner.x, inner.y, inner.width, header_height),
        );
    }
    let rows: Vec<Line> = app
        .view
        .wrapped_rows
        .iter()
        .enumerate()
        .skip(app.scroll)
        .take(content.height as usize)
        .map(|(index, wrapped)| {
            if mode == DiffMode::SideBySide {
                render_split_row(
                    &app.view,
                    &app.view.split_rows[index],
                    content.width as usize,
                    palette,
                )
            } else {
                render_row(
                    &app.view.rows[wrapped.row],
                    wrapped,
                    app.view.number_width.max(1),
                    content.width as usize,
                    palette,
                )
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(rows), content);
    if app.focus == Focus::Diff && !app.view.patches.is_empty() && content.width > 0 {
        // Keep the target visible even when its hunk header has scrolled offscreen.
        let row = app.view.wrapped_hunks[app.active_hunk()].max(app.scroll) - app.scroll;
        if row < content.height as usize {
            frame.buffer_mut()[(content.x, content.y + row as u16)]
                .set_symbol("▸")
                .set_style(Style::default().fg(palette.accent).bold());
        }
    }
    if app.view.wrapped_rows.len() > content.height as usize && area.width > 1 {
        let mut state = ScrollbarState::new(app.view.wrapped_rows.len())
            .position(app.scroll)
            .viewport_content_length(content.height as usize);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .thumb_style(Style::default().fg(palette.accent))
                .track_style(Style::default().fg(palette.border)),
            area,
            &mut state,
        );
    }
}

fn row_theme(kind: RowKind, palette: &Palette) -> (char, Color, Color) {
    match kind {
        RowKind::Added => ('+', palette.green, palette.add_bg),
        RowKind::Removed => ('-', palette.red, palette.del_bg),
        RowKind::Hunk => (' ', palette.accent, palette.hunk_bg),
        RowKind::Note | RowKind::Context => (' ', palette.muted, palette.bg),
    }
}

fn render_row(
    row: &DiffRow,
    wrapped: &WrappedRow,
    number_width: usize,
    width: usize,
    palette: &Palette,
) -> Line<'static> {
    let (marker, fg, bg) = row_theme(row.kind, palette);
    let available = width
        .saturating_sub(row.gutter_width(number_width))
        .min(wrapped.width);
    if matches!(row.kind, RowKind::Hunk | RowKind::Note) {
        let mut spans = vec![Span::raw(" ")];
        spans.extend(clip_spans(&row.spans, wrapped.offset, available));
        return Line::from(spans).style(Style::default().fg(fg).bg(bg));
    }
    let continuation = wrapped.offset > 0;
    let number = |value: Option<usize>| {
        value
            .filter(|_| !continuation)
            .map_or_else(String::new, |n| n.to_string())
    };
    let marker = if continuation { ' ' } else { marker };
    let gutter = format!(
        " {:>number_width$} {:>number_width$} │ {marker} ",
        number(row.old_line),
        number(row.new_line)
    );
    let mut spans = vec![Span::styled(gutter, Style::default().fg(fg))];
    spans.extend(clip_spans(&row.spans, wrapped.offset, available));
    Line::from(spans).style(Style::default().fg(palette.text).bg(bg))
}

fn render_split_row(
    view: &DiffView,
    row: &SplitRow,
    width: usize,
    palette: &Palette,
) -> Line<'static> {
    let (old, new) = match row {
        SplitRow::Shared(wrapped) => {
            return render_row(
                &view.rows[wrapped.row],
                wrapped,
                view.number_width,
                width,
                palette,
            );
        }
        SplitRow::Pair { old, new } => (old, new),
    };
    let (old_width, new_width) = split_widths(width);
    let mut spans = render_side(view, *old, true, old_width, palette);
    spans.push(Span::styled(
        "│",
        Style::default().fg(palette.border).bg(palette.bg),
    ));
    spans.extend(render_side(view, *new, false, new_width, palette));
    Line::from(spans)
}

fn render_side(
    view: &DiffView,
    wrapped: Option<WrappedRow>,
    old: bool,
    width: usize,
    palette: &Palette,
) -> Vec<Span<'static>> {
    let Some(wrapped) = wrapped else {
        return vec![Span::styled(
            " ".repeat(width),
            Style::default().bg(palette.panel),
        )];
    };
    let row = &view.rows[wrapped.row];
    let (marker, foreground, background) = row_theme(row.kind, palette);
    let base = Style::default()
        .fg(if row.kind == RowKind::Note {
            palette.muted
        } else {
            palette.text
        })
        .bg(background);
    let number_width = view.number_width.max(1);
    let gutter = if row.kind == RowKind::Note {
        " ".into()
    } else {
        let number = if old { row.old_line } else { row.new_line };
        let number = number
            .filter(|_| wrapped.offset == 0)
            .map_or_else(String::new, |number| number.to_string());
        let marker = if wrapped.offset > 0 { ' ' } else { marker };
        format!(" {number:>number_width$} {marker} ")
    };
    let available = width
        .saturating_sub(row.split_gutter_width(number_width))
        .min(wrapped.width);
    let mut spans = vec![Span::styled(gutter, base.fg(foreground))];
    spans.extend(
        clip_spans(&row.spans, wrapped.offset, available)
            .into_iter()
            .map(|span| Span::styled(span.content, base.patch(span.style))),
    );
    let used: usize = spans.iter().map(Span::width).sum();
    spans.push(Span::styled(" ".repeat(width.saturating_sub(used)), base));
    spans
}

/// Crop in terminal cells, keeping line-number gutters fixed and wide characters intact.
fn clip_spans(spans: &[Span<'_>], offset: usize, width: usize) -> Vec<Span<'static>> {
    let mut result = Vec::new();
    let end = offset.saturating_add(width);
    let mut column: usize = 0;
    let mut has_visible = false;
    for span in spans {
        let mut text = String::new();
        let mut finished = false;
        for ch in span.content.chars() {
            let cells = ch.width().unwrap_or(0);
            if cells == 0 {
                if has_visible && column >= offset && column <= end {
                    text.push(ch);
                }
                continue;
            }
            if column >= end {
                finished = true;
                break;
            }
            let next = column.saturating_add(cells);
            if column >= offset && next <= end {
                text.push(ch);
                has_visible = true;
            } else if next > offset && column < end {
                text.push_str(&" ".repeat(next.min(end) - column.max(offset)));
                has_visible = true;
            }
            column = next;
        }
        if !text.is_empty() {
            result.push(Span::styled(text, span.style));
        }
        if finished {
            break;
        }
    }
    result
}

fn operation_details(
    frame: &mut Frame,
    area: Rect,
    details: &mut OperationDetails,
    keymap: Keymap,
    palette: &Palette,
) {
    let width = area.width.saturating_sub(4).min(110);
    let height = area.height.saturating_sub(2).min(28);
    let rect = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    let (title, color) = match &details.result {
        None if details.cancelling => ("Cancelling…".into(), palette.accent),
        None => (format!("{}…", details.label), palette.accent),
        Some(OperationResult::Success) => (details.success_message.clone(), palette.green),
        Some(OperationResult::Cancelled) => ("Git operation cancelled".into(), palette.accent),
        Some(OperationResult::Failed(_)) => ("Git operation failed".into(), palette.red),
    };
    let block = pane(format!(" {} ", safe_text(&title)), true, palette)
        .border_style(Style::default().fg(color));
    let inner = block.inner(rect);
    frame.render_widget(Clear, rect);
    frame.render_widget(block, rect);
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .split(inner);
    details.viewport_height = chunks[1].height as usize;
    details.viewport_width = chunks[1].width.saturating_sub(1) as usize;
    details.clamp_scroll();
    if details.horizontal > 0 {
        details.horizontal = details.horizontal.min(details.max_horizontal());
    }
    frame.render_widget(
        Paragraph::new(format!(
            "{}{}",
            if details.follow {
                "following output"
            } else {
                "scroll paused · End to follow"
            },
            if details.omitted {
                " · some output omitted (history limit)"
            } else {
                ""
            }
        ))
        .style(Style::default().fg(palette.muted)),
        chunks[0],
    );
    let rows: Vec<Line> = details
        .lines
        .iter()
        .skip(details.scroll)
        .take(chunks[1].height as usize)
        .map(|line| {
            Line::from(clip_spans(
                &[Span::raw(safe_text(line))],
                details.horizontal,
                chunks[1].width.saturating_sub(1) as usize,
            ))
        })
        .collect();
    frame.render_widget(Paragraph::new(rows), chunks[1]);
    if details.lines.len() == 1 && details.lines[0].is_empty() {
        frame.render_widget(
            Paragraph::new(if details.result.is_none() {
                "Waiting for Git output…"
            } else {
                "No output"
            })
            .style(Style::default().fg(palette.muted)),
            chunks[1],
        );
    }
    if details.max_scroll() > 0 && chunks[1].width > 0 && chunks[1].height > 0 {
        let mut state = ScrollbarState::new(details.max_scroll()).position(details.scroll);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .thumb_style(Style::default().fg(palette.accent))
                .track_style(Style::default().fg(palette.border)),
            chunks[1],
            &mut state,
        );
    }
    frame.render_widget(
        Paragraph::new(format!(
            "{}/{} scroll · {}/{} page · {}/{} · {}/{} horizontal{}",
            keymap.label(Action::Up),
            keymap.label(Action::Down),
            keymap.label(Action::PageUp),
            keymap.label(Action::PageDown),
            keymap.label(Action::First),
            keymap.label(Action::Last),
            keymap.label(Action::Left),
            keymap.label(Action::Right),
            if details.result.is_some() {
                " · Esc close"
            } else if details.cancelling {
                " · stopping Git…"
            } else {
                " · Esc cancel"
            }
        ))
        .style(Style::default().fg(palette.accent)),
        chunks[2],
    );
}

fn worktree_popup(frame: &mut Frame, area: Rect, app: &mut App) {
    let palette = app.theme().palette();
    let popup = app.worktree_popup.as_mut().unwrap();
    let width = area.width.saturating_sub(4).min(100);
    let height = area
        .height
        .saturating_sub(2)
        .min(if popup.removal.is_some() {
            12
        } else {
            popup
                .worktrees
                .len()
                .saturating_mul(2)
                .saturating_add(3)
                .min(25) as u16
        });
    let rect = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    let block = if popup.removal.is_some() {
        pane(" Delete worktree ".into(), true, palette)
            .border_style(Style::default().fg(palette.red))
            .title_bottom(" y delete · Esc cancel ")
    } else {
        pane(
            format!(" Worktrees {} ", popup.worktrees.len()),
            true,
            palette,
        )
        .title_bottom(format!(
            " {}/{} select · Enter switch · {} delete · r refresh · Esc close ",
            app.keymap.label(Action::Up),
            app.keymap.label(Action::Down),
            app.keymap.label(Action::DeleteWorktree),
        ))
    };
    let inner = block.inner(rect);
    frame.render_widget(Clear, rect);
    frame.render_widget(block, rect);
    if let Some(worktree) = &popup.removal {
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled(
                    "Permanently delete this worktree?",
                    Style::default().fg(palette.red).bold(),
                ),
                Line::raw(""),
                Line::raw(format!("Branch: {}", safe_text(&worktree.branch))),
                Line::raw(safe_text(&worktree.path.to_string_lossy())),
                Line::raw(""),
                Line::raw("The directory, including ignored files, will be deleted."),
                Line::raw("The branch is kept. Dirty and locked worktrees are refused."),
                Line::raw("Unreferenced detached commits may be garbage-collected."),
            ])
            .wrap(Wrap { trim: false }),
            inner,
        );
        return;
    }
    let chunks = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(inner);
    popup.viewport_height = (chunks[0].height as usize / 2).max(1);
    let items: Vec<ListItem> = popup
        .worktrees
        .iter()
        .map(|worktree| {
            let current = if worktree.path == app.repo.root {
                "* "
            } else {
                "  "
            };
            let locked = if worktree.locked { " [locked]" } else { "" };
            let prunable = if worktree.prunable { " [prunable]" } else { "" };
            ListItem::new(vec![
                Line::from(format!(
                    "{current}{}{locked}{prunable}",
                    safe_text(&worktree.branch)
                )),
                Line::styled(
                    format!("  {}", safe_text(&worktree.path.to_string_lossy())),
                    Style::default().fg(palette.muted),
                ),
            ])
        })
        .collect();
    frame.render_stateful_widget(
        List::new(items).highlight_symbol("▸ ").highlight_style(
            Style::default()
                .bg(palette.hunk_bg)
                .add_modifier(Modifier::BOLD),
        ),
        chunks[0],
        &mut popup.list_state,
    );
    frame.render_widget(
        Paragraph::new(popup.error.as_deref().map(safe_text).unwrap_or_else(|| {
            "* current worktree · Switching does not modify files or the index".into()
        }))
        .style(Style::default().fg(if popup.error.is_some() {
            palette.red
        } else {
            palette.muted
        })),
        chunks[1],
    );
}

fn commit_popup(frame: &mut Frame, area: Rect, popup: &CommitPopup, busy: bool, palette: &Palette) {
    let width = area.width.saturating_sub(4).min(80);
    let height = area.height.saturating_sub(2).min(18);
    let rect = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    let block = pane(" Commit changes ".into(), true, palette);
    let inner = block.inner(rect);
    frame.render_widget(Clear, rect);
    frame.render_widget(block, rect);
    let chunks = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(inner);
    let before = &popup.message[..popup.cursor];
    let row = before.bytes().filter(|&byte| byte == b'\n').count();
    let column = safe_text(before.rsplit('\n').next().unwrap_or("")).width();
    let top = row.saturating_sub(chunks[0].height.saturating_sub(1) as usize);
    let left = column.saturating_sub(chunks[0].width.saturating_sub(1) as usize);
    let lines: Vec<Line> = popup
        .message
        .split('\n')
        .skip(top)
        .take(chunks[0].height as usize)
        .map(|line| {
            Line::from(clip_spans(
                &[Span::raw(safe_text(line))],
                left,
                chunks[0].width as usize,
            ))
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), chunks[0]);
    frame.render_widget(
        Paragraph::new(if busy {
            "Committing… please wait"
        } else {
            "Ctrl+S commit · Enter newline · Esc cancel · Ctrl+U clear"
        })
        .style(Style::default().fg(palette.accent)),
        chunks[1],
    );
    frame.render_widget(
        Paragraph::new(popup.error.as_deref().map(safe_text).unwrap_or_else(|| {
            "Commits staged changes, or all changes if nothing is staged".into()
        }))
        .style(Style::default().fg(if popup.error.is_some() {
            palette.red
        } else {
            palette.muted
        })),
        chunks[2],
    );
    if !busy && chunks[0].width > 0 && chunks[0].height > 0 {
        frame.set_cursor_position((
            chunks[0].x + column.saturating_sub(left) as u16,
            chunks[0].y + row.saturating_sub(top) as u16,
        ));
    }
}

fn help(frame: &mut Frame, area: Rect, keymap: Keymap, palette: &Palette) {
    let width = area.width.saturating_sub(4).min(70);
    let height = area.height.saturating_sub(2).min(30);
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    let text = vec![
        Line::styled(
            "Live Git diffs with Tree-sitter highlighting",
            Style::default().fg(palette.accent).bold(),
        ),
        Line::raw(""),
        Line::raw("Tab                 Switch files / diff focus"),
        Line::raw(format!(
            "{:<20}Select files / scroll diff",
            format!(
                "{}/{}",
                keymap.label(Action::Down),
                keymap.label(Action::Up)
            )
        )),
        Line::raw("v                   Toggle unified / side-by-side diff"),
        Line::raw("t                   Cycle through built-in themes"),
        Line::raw("Space               Stage/unstage file (Files) or hunk (Diff)"),
        Line::raw("c                   Commit changes (message popup)"),
        Line::raw("p / P               Pull (fast-forward only) / push branch"),
        Line::raw("e                   Open editor at first (visible) hunk"),
        Line::raw(format!(
            "w                   Choose a worktree ({} deletes it)",
            keymap.label(Action::DeleteWorktree)
        )),
        Line::raw(format!(
            "{:<20}Page up / down",
            format!(
                "{}/{}",
                keymap.label(Action::PageUp),
                keymap.label(Action::PageDown)
            )
        )),
        Line::raw(format!(
            "{:<20}First / last row or file",
            format!(
                "{}/{}",
                keymap.label(Action::First),
                keymap.label(Action::Last)
            )
        )),
        Line::raw("[ / ]               Previous / next hunk"),
        Line::raw("?                   Show help"),
        Line::raw("q                   Quit"),
        Line::raw(""),
        Line::styled(
            "Changes: [ ] unstaged (index → disk), [x] staged (HEAD → index)",
            Style::default().fg(palette.muted),
        ),
        Line::styled(
            "Partially staged files have both entries. Renames = delete/add.",
            Style::default().fg(palette.muted),
        ),
        Line::styled(
            "On narrow terminals, Tab switches panes.",
            Style::default().fg(palette.muted),
        ),
        Line::raw(""),
        Line::styled("Esc closes help", Style::default().fg(palette.accent)),
    ];
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(text)
            .block(pane(format!(" Help — {keymap:?} keys "), true, palette))
            .style(Style::default().bg(palette.panel)),
        popup,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    const PALETTE: &Palette = crate::theme::Theme::Default.palette();
    use crate::git::tests::{git, repo};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend};

    fn screen(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn every_theme_colors_the_interface_syntax_and_both_diff_modes() {
        use crate::theme::Theme;

        let (dir, repo) = repo();
        std::fs::write(
            dir.path().join("test.rs"),
            "let label = \"start obsolete_value end\";\n",
        )
        .unwrap();
        git(dir.path(), &["add", "."]);
        std::fs::write(
            dir.path().join("test.rs"),
            "let label = \"start replacement_value end\";\n",
        )
        .unwrap();
        let mut expected_patches = None;
        let mut app = App::new(repo).unwrap();
        for (name, theme) in Theme::ALL {
            let palette = theme.palette();
            assert!(!app.selected().unwrap().staged);
            if let Some(patches) = &expected_patches {
                assert_eq!(&app.view.patches, patches, "{name}");
            } else {
                expected_patches = Some(app.view.patches.clone());
            }
            app.focus = Focus::Diff;
            assert_eq!(app.theme(), theme);
            for mode in [DiffMode::Unified, DiffMode::SideBySide] {
                if app.diff_mode != mode {
                    app.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
                }
                let mut terminal = Terminal::new(TestBackend::new(140, 30)).unwrap();
                terminal.draw(|frame| draw(frame, &mut app)).unwrap();
                let buffer = terminal.backend().buffer();
                assert!(screen(&terminal).contains(name));
                assert!(screen(&terminal).contains("? help"));
                assert_eq!(buffer[(1, 0)].fg, palette.bg, "{name}");
                assert_eq!(buffer[(1, 0)].bg, palette.accent, "{name}");
                assert_eq!(buffer[(139, 29)].bg, palette.panel, "{name}");
                for (background, word) in [
                    (palette.del_word_bg, "obsolete_value"),
                    (palette.add_word_bg, "replacement_value"),
                ] {
                    let highlighted: String = buffer
                        .content
                        .iter()
                        .filter(|cell| cell.bg == background)
                        .map(|cell| {
                            assert_eq!(cell.fg, palette.syntax.string, "{name}");
                            cell.symbol()
                        })
                        .collect();
                    assert_eq!(highlighted, word, "{name}: {mode:?}");
                }
                for background in [palette.add_bg, palette.del_bg, palette.hunk_bg] {
                    assert!(
                        buffer.content.iter().any(|cell| cell.bg == background),
                        "{name}"
                    );
                }
            }
            app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE));
        }
        assert_eq!(app.theme(), Theme::Default);
    }

    #[test]
    fn every_theme_colors_popups_and_handles_tiny_terminals() {
        use crate::theme::Theme;

        let (_dir, repo) = repo();
        for (name, theme) in Theme::ALL {
            let mut app = App::with_theme(repo.clone(), theme).unwrap();
            let palette = theme.palette();
            for popup in 0..4 {
                app.show_help = popup == 0;
                app.commit_popup = (popup == 1).then(CommitPopup::default);
                app.show_operation_details = popup == 2;
                if popup == 2 {
                    app.operation_details =
                        Some(OperationDetails::new(&crate::git::Operation::Push));
                }
                if popup == 3 {
                    app.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE));
                }
                for (width, height) in [(120, 40), (10, 4), (1, 1)] {
                    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
                    if width == 120 {
                        let buffer = terminal.backend().buffer();
                        let center = &buffer[(width / 2, height / 2)];
                        let (popup_width, popup_height) = match popup {
                            0 => (70, 30),
                            1 => (80, 18),
                            2 => (110, 28),
                            _ => (100, 5),
                        };
                        let corner =
                            &buffer[((width - popup_width) / 2, (height - popup_height) / 2)];
                        assert_eq!(corner.symbol(), "┏", "{name}: popup {popup}");
                        assert_eq!(corner.fg, palette.accent, "{name}: popup {popup}");
                        assert_eq!(
                            center.bg,
                            if popup == 0 {
                                palette.panel
                            } else {
                                palette.bg
                            },
                            "{name}: popup {popup}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn help_and_popup_hints_follow_the_selected_preset() {
        let (_dir, repo) = repo();
        let mut app = App::new(repo).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        for (keymap, help_keys, delete, output_keys) in [
            (
                Keymap::Standard,
                ["↓/↑", "PgUp/PgDn", "Home/End"],
                "Delete delete",
                "↑/↓ scroll · PgUp/PgDn page · Home/End · ←/→ horizontal",
            ),
            (
                Keymap::Vim,
                ["j/k", "u/d", "g/G"],
                "D delete",
                "k/j scroll · u/d page · g/G · h/l horizontal",
            ),
        ] {
            app.keymap = keymap;
            app.show_help = true;
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            let text = screen(&terminal);
            for keys in help_keys {
                assert!(text.contains(keys), "{keymap:?}: {keys}");
            }
            assert!(text.contains("Tab                 Switch files / diff focus"));
            for action in [
                "Cycle through built-in themes",
                "Choose a worktree",
                "Toggle unified",
                "Pull (fast-forward only)",
                "Open editor",
                "Quit",
            ] {
                assert!(text.contains(action), "Missing help action: {action}");
            }
            assert!(text.contains("Esc closes help"));
            app.show_help = false;

            app.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE));
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            assert!(screen(&terminal).contains(delete));
            app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

            app.operation_details = Some(OperationDetails::new(&crate::git::Operation::Pull));
            app.show_operation_details = true;
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            assert!(screen(&terminal).contains(output_keys));
            app.show_operation_details = false;
        }
    }

    #[test]
    fn wrapped_rows_keep_styles_and_only_number_the_first_segment() {
        for (kind, marker, bg) in [
            (RowKind::Added, '+', PALETTE.add_bg),
            (RowKind::Removed, '-', PALETTE.del_bg),
            (RowKind::Context, ' ', PALETTE.bg),
        ] {
            let mut view = crate::diff::DiffView {
                rows: vec![DiffRow {
                    kind,
                    old_line: Some(1),
                    new_line: Some(1),
                    spans: vec![
                        Span::styled("abcd", Style::default().fg(PALETTE.red)),
                        Span::styled("\u{301}界ef", Style::default().fg(PALETTE.accent)),
                    ],
                }],
                number_width: 1,
                ..crate::diff::DiffView::default()
            };
            view.wrap(13);
            let lines: Vec<_> = view
                .wrapped_rows
                .iter()
                .map(|wrapped| render_row(&view.rows[wrapped.row], wrapped, 1, 13, PALETTE))
                .collect();
            assert_eq!(lines.len(), 2);
            assert_eq!(lines[0].to_string(), format!(" 1 1 │ {marker} abcd\u{301}"));
            assert_eq!(lines[1].to_string(), "     │   界ef");
            assert_eq!(lines[0].spans[1].style.fg, Some(PALETTE.red));
            assert_eq!(lines[0].spans[2].style.fg, Some(PALETTE.accent));
            assert_eq!(lines[1].spans[1].style.fg, Some(PALETTE.accent));
            for line in lines {
                assert_eq!(line.style.bg, Some(bg));
                assert_eq!(line.width(), 13);
            }
        }
    }

    #[test]
    fn word_backgrounds_and_syntax_colors_survive_wrapping_and_rendering() {
        use crate::{diff::DiffView, git::Content, highlight::SyntaxHighlighter};

        let mut view = DiffView::build(
            std::path::Path::new("test.rs"),
            Content::Text("let label = \"start obsolete_value end\";\n".into()),
            Content::Text("let label = \"start replacement_value end\";\n".into()),
            &mut SyntaxHighlighter::default(),
        )
        .unwrap();
        let patches = view.patches.clone();
        for width in [20, 28, 80] {
            view.wrap(width);
            let lines: Vec<_> = view
                .wrapped_rows
                .iter()
                .map(|wrapped| render_row(&view.rows[wrapped.row], wrapped, 1, width, PALETTE))
                .collect();
            let mut terminal =
                Terminal::new(TestBackend::new(width as u16, lines.len() as u16)).unwrap();
            terminal
                .draw(|frame| frame.render_widget(Paragraph::new(lines), frame.area()))
                .unwrap();
            let buffer = terminal.backend().buffer();
            for (background, word) in [
                (PALETTE.del_word_bg, "obsolete_value"),
                (PALETTE.add_word_bg, "replacement_value"),
            ] {
                let highlighted: String = buffer
                    .content
                    .iter()
                    .filter(|cell| cell.bg == background)
                    .map(|cell| {
                        assert_eq!(cell.fg, PALETTE.green); // Tree-sitter's string color.
                        cell.symbol()
                    })
                    .collect();
                assert_eq!(highlighted, word);
            }
            for (y, wrapped) in view.wrapped_rows.iter().enumerate() {
                let row = &view.rows[wrapped.row];
                let background = match row.kind {
                    RowKind::Added => PALETTE.add_bg,
                    RowKind::Removed => PALETTE.del_bg,
                    _ => continue,
                };
                for x in 0..row.gutter_width(1) {
                    assert_eq!(buffer[(x as u16, y as u16)].bg, background);
                }
            }
        }
        assert_eq!(view.patches, patches);
    }

    #[test]
    fn split_rendering_preserves_word_backgrounds_gutters_and_column_alignment() {
        use crate::{git::Content, highlight::SyntaxHighlighter};

        let mut view = DiffView::build(
            std::path::Path::new("test.rs"),
            Content::Text("let label = \"start obsolete_value end\";\n".into()),
            Content::Text("let label = \"start replacement_value end\";\n".into()),
            &mut SyntaxHighlighter::default(),
        )
        .unwrap();
        for width in [60, 61, 80] {
            view.layout(width, DiffMode::SideBySide);
            let lines: Vec<_> = view
                .split_rows
                .iter()
                .map(|row| {
                    let line = render_split_row(&view, row, width, PALETTE);
                    if matches!(row, SplitRow::Pair { .. }) {
                        assert_eq!(line.width(), width);
                    }
                    line
                })
                .collect();
            let mut terminal =
                Terminal::new(TestBackend::new(width as u16, lines.len() as u16)).unwrap();
            terminal
                .draw(|frame| frame.render_widget(Paragraph::new(lines), frame.area()))
                .unwrap();
            let buffer = terminal.backend().buffer();
            for (background, word) in [
                (PALETTE.del_word_bg, "obsolete_value"),
                (PALETTE.add_word_bg, "replacement_value"),
            ] {
                let highlighted: String = buffer
                    .content
                    .iter()
                    .filter(|cell| cell.bg == background)
                    .map(|cell| {
                        assert_eq!(cell.fg, PALETTE.green);
                        cell.symbol()
                    })
                    .collect();
                assert_eq!(highlighted, word);
            }
            let (old_width, _) = split_widths(width);
            for (y, row) in view.split_rows.iter().enumerate() {
                let SplitRow::Pair { old, new } = row else {
                    continue;
                };
                assert_eq!(buffer[(old_width as u16, y as u16)].symbol(), "│");
                for (start, wrapped, background, marker) in [
                    (0, old, PALETTE.del_bg, '-'),
                    (old_width + 1, new, PALETTE.add_bg, '+'),
                ] {
                    assert_eq!(
                        buffer[(start as u16, y as u16)].bg,
                        if wrapped.is_some() {
                            background
                        } else {
                            PALETTE.panel
                        }
                    );
                    if let Some(wrapped) = wrapped {
                        let gutter: String = (start..start + 5)
                            .map(|x| buffer[(x as u16, y as u16)].symbol())
                            .collect();
                        assert_eq!(
                            gutter,
                            if wrapped.offset == 0 {
                                format!(" 1 {marker} ")
                            } else {
                                "     ".into()
                            }
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn split_view_labels_revisions_and_returns_after_narrow_fallback() {
        let (dir, repo) = repo();
        std::fs::write(dir.path().join("test.txt"), "context\nold text\ntail\n").unwrap();
        git(dir.path(), &["add", "."]);
        std::fs::write(dir.path().join("test.txt"), "context\nnew text\ntail\n").unwrap();
        let mut app = App::new(repo).unwrap();
        app.focus = Focus::Diff;
        app.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
        let mut terminal = Terminal::new(TestBackend::new(140, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let text = screen(&terminal);
        assert!(text.contains("Side-by-side"));
        assert!(text.contains("Before · Index"));
        assert!(text.contains("After · Working tree"));
        let buffer = terminal.backend().buffer();
        assert!((0..30).any(|y| {
            let line: String = (0..140).map(|x| buffer[(x, y)].symbol()).collect();
            line.contains("old text") && line.contains("new text")
        }));
        let mut narrow = Terminal::new(TestBackend::new(59, 15)).unwrap();
        narrow.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(screen(&narrow).contains("Unified (narrow)"));
        assert!(app.view.split_rows.is_empty());
        assert_eq!(app.diff_mode, DiffMode::SideBySide);
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(screen(&terminal).contains("Side-by-side"));
        assert!(!app.view.split_rows.is_empty());
        app.focus = Focus::Files;
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert!(app.selected().unwrap().staged);
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let text = screen(&terminal);
        assert!(text.contains("Before · HEAD"));
        assert!(text.contains("After · Index"));
    }

    #[test]
    fn split_rendering_keeps_wide_characters_combining_marks_and_control_escapes() {
        use crate::{git::Content, highlight::SyntaxHighlighter};

        let source = format!("{}\x1b\n", "界e\u{301}".repeat(20));
        let mut view = DiffView::build(
            std::path::Path::new("test.txt"),
            Content::Text(String::new()),
            Content::Text(source),
            &mut SyntaxHighlighter::default(),
        )
        .unwrap();
        view.layout(60, DiffMode::SideBySide);
        let mut text = String::new();
        for row in &view.split_rows {
            if let SplitRow::Pair { new: Some(new), .. } = row {
                let spans = render_side(&view, Some(*new), false, split_widths(60).1, PALETTE);
                text.extend(
                    spans
                        .iter()
                        .skip(1)
                        .take(spans.len() - 2)
                        .map(|span| span.content.as_ref()),
                );
                assert_eq!(
                    spans.iter().map(Span::width).sum::<usize>(),
                    split_widths(60).1
                );
            }
        }
        assert_eq!(text, format!("{}\\u{{1b}}", "界e\u{301}".repeat(20)));
    }

    #[test]
    fn long_lines_scroll_through_continuations_to_the_end() {
        let (dir, repo) = repo();
        std::fs::write(
            dir.path().join("test.txt"),
            format!("{}TAIL\nEND\n", "a".repeat(90)),
        )
        .unwrap();
        let mut app = App::new(repo).unwrap();
        app.focus = Focus::Diff;
        let mut terminal = Terminal::new(TestBackend::new(29, 9)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert_eq!(app.viewport_height, 5);
        assert_eq!(app.view.wrapped_rows.len(), 8);
        let text = screen(&terminal);
        assert!(text.contains("   1 │ + aaaaaaaaaaaaaaaaaa"));
        assert!(text.contains("     │   aaaaaaaaaaaaaaaaaa"));

        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert_eq!(app.scroll, 2);
        assert!(screen(&terminal).contains("     │   aaaaaaaaaaaaaaaaaa"));

        app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert_eq!(app.scroll, 3);
        let text = screen(&terminal);
        assert!(text.contains("     │   TAIL"));
        assert!(text.contains("   2 │ + END"));
    }

    #[test]
    fn crops_wide_unicode_without_splitting_cells() {
        let spans = vec![Span::raw("a界bc")];
        let text = |offset, width| {
            clip_spans(&spans, offset, width)
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        };
        assert_eq!(text(0, 2), "a ");
        assert_eq!(text(2, 3), " bc");
        assert_eq!(text(1, 2), "界");
    }

    #[test]
    fn focused_borders_and_markers_identify_the_active_hunk_in_both_layouts() {
        let (dir, repo) = repo();
        let lines: Vec<_> = (0..80).map(|n| format!("line {n}\n")).collect();
        std::fs::write(dir.path().join("file.txt"), lines.concat()).unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-qm", "Initial"]);
        let mut changed = lines.clone();
        for index in [3, 30, 60] {
            changed[index] = format!("changed {index}\n");
        }
        std::fs::write(dir.path().join("file.txt"), changed.concat()).unwrap();
        let mut app = App::new(repo).unwrap();
        for mode in [DiffMode::Unified, DiffMode::SideBySide] {
            app.set_diff_mode(mode);
            for width in [60, 120, 160] {
                let mut terminal = Terminal::new(TestBackend::new(width, 30)).unwrap();
                app.focus = Focus::Files;
                terminal.draw(|frame| draw(frame, &mut app)).unwrap();
                let buffer = terminal.backend().buffer();
                assert_eq!(buffer[(0, 1)].symbol(), "┏");
                assert_eq!(buffer[(0, 1)].fg, PALETTE.accent);
                let diff_x = if width >= 80 {
                    (width / 4).clamp(24, 42)
                } else {
                    0
                };
                if width >= 80 {
                    assert_eq!(buffer[(diff_x, 1)].symbol(), "┌");
                    assert_eq!(buffer[(diff_x, 1)].fg, PALETTE.border);
                }
                app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
                app.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
                terminal.draw(|frame| draw(frame, &mut app)).unwrap();
                app.handle_key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::NONE));
                for header_visible in [true, false] {
                    if !header_visible {
                        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
                    }
                    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
                    assert_eq!(app.active_hunk(), 1);
                    assert!(screen(&terminal).contains("Space stage hunk 2/3"));
                    let buffer = terminal.backend().buffer();
                    assert_eq!(buffer[(diff_x, 1)].symbol(), "┏");
                    assert_eq!(buffer[(diff_x, 1)].fg, PALETTE.accent);
                    let y =
                        2 + u16::from(mode.effective(app.viewport_width) == DiffMode::SideBySide);
                    assert_eq!(buffer[(diff_x + 1, y)].symbol(), "▸");
                    assert_eq!(buffer[(diff_x + 1, y)].fg, PALETTE.accent);
                    if width >= 80 {
                        assert_eq!(buffer[(0, 1)].symbol(), "┌");
                        assert_eq!(buffer[(0, 1)].fg, PALETTE.border);
                    }
                }
            }
        }
    }

    #[test]
    fn footer_hints_follow_focus_staging_state_and_available_width() {
        let (dir, repo) = repo();
        std::fs::write(dir.path().join("file.txt"), "new\n").unwrap();
        let mut app = App::new(repo).unwrap();
        assert_eq!(
            footer_hints(&app, 80),
            " Space stage file · c commit · Tab diff · ? help"
        );
        assert_eq!(
            footer_hints(&app, 40),
            " Space stage file · Tab diff · ? help"
        );
        app.focus = Focus::Diff;
        assert_eq!(
            footer_hints(&app, 80),
            " Space stage hunk · [/] hunk · Tab files · ? help"
        );
        assert_eq!(
            footer_hints(&app, 40),
            " Space stage hunk · Tab files · ? help"
        );
        git(dir.path(), &["add", "."]);
        app.refresh();
        assert_eq!(
            footer_hints(&app, 80),
            " Space unstage hunk · [/] hunk · Tab files · ? help"
        );
        app.focus = Focus::Files;
        assert_eq!(
            footer_hints(&app, 80),
            " Space unstage file · c commit · Tab diff · ? help"
        );
        std::fs::write(dir.path().join("binary"), b"\0binary").unwrap();
        app.refresh();
        app.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        app.focus = Focus::Diff;
        assert_eq!(footer_hints(&app, 80), " Tab files · ? help");
    }

    #[test]
    fn staging_controls_and_commit_popup_are_visible() {
        let (dir, repo) = repo();
        std::fs::write(dir.path().join("file.txt"), "staged\n").unwrap();
        git(dir.path(), &["add", "."]);
        std::fs::write(dir.path().join("file.txt"), "unstaged\n").unwrap();
        let mut app = App::new(repo).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let text = screen(&terminal);
        assert!(text.contains("Files 2"));
        assert!(text.contains("[ ] M file.txt"));
        assert!(text.contains("[x] A file.txt"));
        assert!(text.contains("file.txt · Unstaged"));
        for hint in ["Space stage file", "Tab diff", "c commit", "? help"] {
            assert!(text.contains(hint), "Missing hint: {hint}");
        }
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let text = screen(&terminal);
        assert!(text.contains("Space unstage file"));
        assert!(text.contains("file.txt · Staged"));
        assert!(text.contains("Hunk 1/1"));
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(screen(&terminal).contains("Space unstage hunk 1/1"));
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
        app.handle_paste("Commit 界\n\nMessage body");
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let text = screen(&terminal);
        assert!(text.contains("Commit changes"));
        assert!(text.contains("Commits staged changes, or all changes if nothing is staged"));
        assert!(text.contains("Commit 界"));
        assert!(text.contains("Message body"));
        assert!(text.contains("Ctrl+S commit"));
        app.commit_popup.as_mut().unwrap().error = Some("Commit hook failed\u{1b}".into());
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(screen(&terminal).contains("Commit hook failed\\u{1b}"));
    }

    #[test]
    fn commit_popup_scrolls_and_renders_at_tiny_sizes() {
        let (_dir, repo) = repo();
        let mut app = App::new(repo).unwrap();
        app.commit_popup = Some(CommitPopup::default());
        app.handle_paste(&format!("{}last row 界", "long 界 line\n".repeat(50)));
        for (width, height) in [(120, 30), (40, 12), (10, 4), (1, 1)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            if height > 4 {
                assert!(screen(&terminal).contains("last row 界"));
                let cursor = terminal.get_cursor_position().unwrap();
                assert!(cursor.x < width && cursor.y < height);
            }
        }
    }

    #[test]
    fn staging_feedback_names_the_target_then_restores_footer_hints() {
        for (focus, target, hints) in [
            (Focus::Files, "file", "c commit · Tab diff · ? help"),
            (Focus::Diff, "hunk", "[/] hunk · Tab files · ? help"),
        ] {
            let (dir, repo) = repo();
            std::fs::write(dir.path().join("file.txt"), "new\n").unwrap();
            let mut app = App::new(repo).unwrap();
            app.focus = focus;
            let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
            for (message, action) in [("Staged", "unstage"), ("Unstaged", "stage")] {
                app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
                app.expire_status(std::time::Instant::now());
                terminal.draw(|frame| draw(frame, &mut app)).unwrap();
                let footer = |terminal: &Terminal<TestBackend>| {
                    let buffer = terminal.backend().buffer();
                    (0..120)
                        .map(|x| buffer[(x, 29)].symbol())
                        .collect::<String>()
                        .trim_end()
                        .to_owned()
                };
                assert_eq!(footer(&terminal), format!(" {message} {target}"));
                assert!(
                    app.expire_status(
                        std::time::Instant::now() + std::time::Duration::from_secs(3)
                    )
                );
                terminal.draw(|frame| draw(frame, &mut app)).unwrap();
                assert_eq!(
                    footer(&terminal),
                    format!(" Space {action} {target} · {hints}")
                );
            }
        }
    }

    #[test]
    fn operation_results_are_visible_in_the_footer_and_details() {
        let (_dir, repo) = repo();
        let mut app = App::new(repo).unwrap();
        let mut details = OperationDetails::new(&crate::git::Operation::Push);
        details.success_message = "Pushed to origin (refs/heads/main)".into();
        app.operation_details = Some(details);
        app.show_operation_details = true;
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(screen(&terminal).contains("Pushing…"));
        assert!(screen(&terminal).contains("Esc cancel"));
        app.operation_details.as_mut().unwrap().cancelling = true;
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(screen(&terminal).contains("Cancelling…"));

        for result in [
            OperationResult::Success,
            OperationResult::Failed("Push rejected".to_owned()),
            OperationResult::Cancelled,
        ] {
            let details = app.operation_details.as_mut().unwrap();
            let status = match &result {
                OperationResult::Success => Status::Success(details.success_message.clone()),
                OperationResult::Failed(error) => Status::Error(error.clone()),
                OperationResult::Cancelled => Status::Cancelled("Git operation cancelled".into()),
            };
            details.finish(result);
            let message = status.message().to_owned();
            let title = match &status {
                Status::Success(_) => message.as_str(),
                Status::Error(_) => "Git operation failed",
                Status::Cancelled(_) => "Git operation cancelled",
            };
            app.status = Some(status);
            for show_details in [false, true] {
                app.show_operation_details = show_details;
                terminal.draw(|frame| draw(frame, &mut app)).unwrap();
                let text = screen(&terminal);
                assert!(text.contains(&message));
                if show_details {
                    assert!(text.contains(title));
                    assert!(text.contains("Esc close"));
                }
            }
        }
    }

    #[test]
    fn operation_details_scroll_escape_controls_and_render_at_tiny_sizes() {
        let (_dir, repo) = repo();
        let mut app = App::new(repo).unwrap();
        let mut details = OperationDetails::new(&crate::git::Operation::Push);
        details.success_message = "Pushed to origin (refs/heads/main)".into();
        for n in 0..100 {
            details.append(&format!("output row {n:03} 界\n"));
        }
        details.append("rejected by hook\u{1b}[31m\n");
        details.finish(OperationResult::Failed(
            "Git exited with exit status: 1".into(),
        ));
        app.operation_details = Some(details);
        app.show_operation_details = true;
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let text = screen(&terminal);
        assert!(text.contains("Git operation failed"));
        assert!(text.contains("following output"));
        assert!(text.contains("rejected by hook\\u{1b}[31m"));
        assert!(text.contains("Git exited with exit status: 1"));
        app.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(screen(&terminal).contains("output row 000 界"));
        assert!(!app.operation_details.as_ref().unwrap().follow);
        app.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(!screen(&terminal).contains("output row 000 界"));
        app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(screen(&terminal).contains("rejected by hook"));
        for (width, height) in [(80, 24), (40, 12), (10, 4), (1, 1)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        }
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.show_operation_details);
        app.show_operation_details = true;
        app.operation_details
            .as_mut()
            .unwrap()
            .finish(OperationResult::Success);
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(screen(&terminal).contains("Pushed to origin (refs/heads/main)"));
    }

    #[test]
    fn worktree_picker_shows_paths_states_controls_and_escaped_errors() {
        let (_dir, _linked, repo, linked_repo) = crate::git::tests::linked_repo();
        let mut app = App::new(repo).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(screen(&terminal).contains("? help"));
        app.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE));
        let popup = app.worktree_popup.as_mut().unwrap();
        popup.worktrees[1].locked = true;
        popup.worktrees[1].prunable = true;
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let text = screen(&terminal);
        for expected in [
            "Worktrees 2",
            "* main",
            "linked [locked] [prunable]",
            "Enter switch",
            "r refresh",
            "Delete delete",
            "Esc close",
            linked_repo.root.to_str().unwrap(),
        ] {
            assert!(text.contains(expected), "Missing: {expected}");
        }
        let popup = app.worktree_popup.as_mut().unwrap();
        popup.worktrees[1].path = "/tmp/worktree\n\u{1b}".into();
        popup.error = Some("Missing\nworktree\u{1b}".into());
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let text = screen(&terminal);
        assert!(text.contains("/tmp/worktree\\n\\u{1b}"));
        assert!(text.contains("Missing\\nworktree\\u{1b}"));
    }

    #[test]
    fn worktree_deletion_confirmation_shows_the_target_and_safety_warning() {
        let (_dir, _linked, repo, linked_repo) = crate::git::tests::linked_repo();
        let mut app = App::new(repo).unwrap();
        for code in [KeyCode::Char('w'), KeyCode::Down, KeyCode::Delete] {
            app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
        }
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let text = screen(&terminal);
        for expected in [
            "Delete worktree",
            "Permanently delete this worktree?",
            "Branch: linked",
            "including ignored files",
            "The branch is kept",
            "Dirty and locked",
            "Unreferenced detached commits",
            "y delete",
            "Esc cancel",
            linked_repo.root.to_str().unwrap(),
        ] {
            assert!(text.contains(expected), "Missing: {expected}");
        }
        app.worktree_popup
            .as_mut()
            .unwrap()
            .removal
            .as_mut()
            .unwrap()
            .path = "/tmp/worktree\n\u{1b}".into();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(screen(&terminal).contains("/tmp/worktree\\n\\u{1b}"));
        for (width, height) in [(80, 24), (40, 12), (10, 4), (1, 1)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        }
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(screen(&terminal).contains("Worktrees 2"));
    }

    #[test]
    fn worktree_picker_scrolls_to_the_selection_and_handles_tiny_terminals() {
        let (_dir, repo) = repo();
        let mut app = App::new(repo).unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE));
        let popup = app.worktree_popup.as_mut().unwrap();
        let template = popup.worktrees[0].clone();
        for n in 0..30 {
            let mut worktree = template.clone();
            worktree.branch = format!("branch-{n}");
            worktree.path = format!("/worktree/{n}").into();
            popup.worktrees.push(worktree);
        }
        app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        for (width, height) in [(120, 30), (80, 24), (40, 12), (10, 4), (1, 1)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            if height > 4 {
                let text = screen(&terminal);
                assert!(text.contains("branch-29"));
                assert!(text.contains("/worktree/29"));
                assert!(app.worktree_popup.as_ref().unwrap().list_state.offset() > 0);
            }
        }
        app.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(screen(&terminal).contains("* main"));
    }
}
