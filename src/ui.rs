use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{
        Block, Borders, Clear, List, ListItem, Paragraph, Scrollbar, ScrollbarOrientation,
        ScrollbarState,
    },
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::{
    app::{App, CommitPopup, Focus, Status},
    diff::{DiffRow, RowKind, WrappedRow},
    git::safe_text,
    highlight::TEXT,
    operation::{OperationDetails, OperationResult},
};

const BG: Color = Color::Rgb(0, 0, 0);
const PANEL: Color = Color::Rgb(10, 10, 10);
const MUTED: Color = Color::Rgb(128, 128, 128);
const BORDER: Color = Color::Rgb(48, 48, 48);
const ACCENT: Color = Color::Rgb(122, 162, 247);
const GREEN: Color = Color::Rgb(158, 206, 106);
const RED: Color = Color::Rgb(247, 118, 142);
const ADD_BG: Color = Color::Rgb(8, 28, 14);
const DEL_BG: Color = Color::Rgb(32, 10, 12);
const HUNK_BG: Color = Color::Rgb(24, 24, 24);

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    frame.render_widget(
        Block::default().style(Style::default().bg(BG).fg(TEXT)),
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
        Span::styled(" diffy ", Style::default().fg(BG).bg(ACCENT).bold()),
        Span::styled(
            format!("  {}", safe_text(&repo_name)),
            Style::default().bold(),
        ),
        Span::styled(
            format!("  /  {}", safe_text(&app.branch)),
            Style::default().fg(MUTED),
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
            Style::default().fg(ACCENT),
        )
    } else if let Some(error) = &app.error {
        Line::styled(format!(" {}", safe_text(error)), Style::default().fg(RED))
    } else if let Some(status) = &app.status {
        Line::styled(
            format!(" {}", safe_text(status.message())),
            Style::default().fg(match status {
                Status::Success(_) => GREEN,
                Status::Error(_) => RED,
                Status::Cancelled(_) => ACCENT,
            }),
        )
    } else {
        Line::styled(
            " Space stage/unstage · c commit · p pull · P push · e edit · ? help · q quit",
            Style::default().fg(MUTED),
        )
    };
    frame.render_widget(
        Paragraph::new(footer).style(Style::default().bg(PANEL)),
        chunks[2],
    );
    if let Some(popup) = &app.commit_popup {
        commit_popup(frame, area, popup, app.operation_label().is_some());
    }
    if app.show_help {
        help(frame, area);
    }
    if app.show_operation_details
        && let Some(details) = &mut app.operation_details
    {
        operation_details(frame, area, details);
    }
}

fn pane(title: String, focused: bool) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(if focused { ACCENT } else { BORDER }))
        .style(Style::default().fg(TEXT).bg(BG))
}

fn files(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = pane(
        format!(" Files {} ", app.files.len()),
        app.focus == Focus::Files,
    )
    .title_bottom(if app.selected().is_some_and(|entry| entry.staged) {
        " Space unstage file "
    } else {
        " Space stage file "
    });
    let inner = block.inner(area);
    let items: Vec<ListItem> = app
        .files
        .iter()
        .map(|entry| {
            let color = match entry.status {
                'A' | '?' => GREEN,
                'D' | 'U' => RED,
                _ => ACCENT,
            };
            let mut spans = vec![Span::styled(
                if entry.staged { "[x] " } else { "[ ] " },
                Style::default().fg(if entry.staged { GREEN } else { MUTED }),
            )];
            spans.push(Span::styled(
                format!("{} ", entry.status),
                Style::default().fg(color),
            ));
            spans.push(Span::raw(safe_text(&entry.path.to_string_lossy())));
            ListItem::new(Line::from(spans))
        })
        .collect();
    let list = List::new(items)
        .block(block)
        .highlight_symbol("▸ ")
        .highlight_style(Style::default().bg(HUNK_BG).add_modifier(Modifier::BOLD));
    frame.render_stateful_widget(list, area, &mut app.list_state);
    if app.files.is_empty() {
        frame.render_widget(
            Paragraph::new(" No changes").style(Style::default().fg(MUTED)),
            inner,
        );
    }
}

fn diff(frame: &mut Frame, app: &mut App, area: Rect) {
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
    let block = pane(format!(" {title} "), app.focus == Focus::Diff);
    let inner = block.inner(area);
    app.resize_diff(inner.width as usize, inner.height as usize);
    let language = app.view.language.map_or("Text", |language| language.name());
    let mut summary = Line::from(vec![
        Span::raw(format!(
            " {language} · hunk {}/{}  ",
            if app.view.hunks.is_empty() {
                0
            } else {
                app.active_hunk() + 1
            },
            app.view.hunks.len()
        )),
        Span::styled(
            format!("+{}", app.view.additions),
            Style::default().fg(GREEN),
        ),
        Span::raw(" / "),
        Span::styled(
            format!("-{} ", app.view.deletions),
            Style::default().fg(RED),
        ),
    ]);
    summary.spans.push(Span::styled(
        if app.selected().is_some_and(|entry| entry.staged) {
            "· Space unstage hunk "
        } else {
            "· Space stage hunk "
        },
        Style::default().fg(MUTED),
    ));
    frame.render_widget(block.title_bottom(summary), area);
    let rows: Vec<Line> = app
        .view
        .wrapped_rows
        .iter()
        .skip(app.scroll)
        .take(inner.height as usize)
        .map(|wrapped| {
            render_row(
                &app.view.rows[wrapped.row],
                wrapped,
                app.view.number_width.max(1),
                inner.width as usize,
            )
        })
        .collect();
    frame.render_widget(Paragraph::new(rows), inner);
    if app.view.wrapped_rows.len() > inner.height as usize && area.width > 1 {
        let mut state = ScrollbarState::new(app.view.wrapped_rows.len())
            .position(app.scroll)
            .viewport_content_length(inner.height as usize);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .thumb_style(Style::default().fg(ACCENT))
                .track_style(Style::default().fg(BORDER)),
            area,
            &mut state,
        );
    }
}

fn render_row(
    row: &DiffRow,
    wrapped: &WrappedRow,
    number_width: usize,
    width: usize,
) -> Line<'static> {
    let (marker, fg, bg) = match row.kind {
        RowKind::Added => ('+', GREEN, ADD_BG),
        RowKind::Removed => ('-', RED, DEL_BG),
        RowKind::Hunk => (' ', ACCENT, HUNK_BG),
        RowKind::Note => (' ', MUTED, BG),
        RowKind::Context => (' ', MUTED, BG),
    };
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
    Line::from(spans).style(Style::default().fg(TEXT).bg(bg))
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

fn operation_details(frame: &mut Frame, area: Rect, details: &mut OperationDetails) {
    let width = area.width.saturating_sub(4).min(110);
    let height = area.height.saturating_sub(2).min(28);
    let rect = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    let (title, color) = match &details.result {
        None if details.cancelling => ("Cancelling…".into(), ACCENT),
        None => (format!("{}…", details.label), ACCENT),
        Some(OperationResult::Success) => (details.success_message.clone(), GREEN),
        Some(OperationResult::Cancelled) => ("Git operation cancelled".into(), ACCENT),
        Some(OperationResult::Failed(_)) => ("Git operation failed".into(), RED),
    };
    let block =
        pane(format!(" {} ", safe_text(&title)), true).border_style(Style::default().fg(color));
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
        .style(Style::default().fg(MUTED)),
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
            .style(Style::default().fg(MUTED)),
            chunks[1],
        );
    }
    if details.max_scroll() > 0 && chunks[1].width > 0 && chunks[1].height > 0 {
        let mut state = ScrollbarState::new(details.max_scroll()).position(details.scroll);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .thumb_style(Style::default().fg(ACCENT))
                .track_style(Style::default().fg(BORDER)),
            chunks[1],
            &mut state,
        );
    }
    frame.render_widget(
        Paragraph::new(format!(
            "↑/↓ scroll · PgUp/PgDn page · Home/End · h/l horizontal{}",
            if details.result.is_some() {
                " · Esc close"
            } else if details.cancelling {
                " · stopping Git…"
            } else {
                " · Esc cancel"
            }
        ))
        .style(Style::default().fg(ACCENT)),
        chunks[2],
    );
}

fn commit_popup(frame: &mut Frame, area: Rect, popup: &CommitPopup, busy: bool) {
    let width = area.width.saturating_sub(4).min(80);
    let height = area.height.saturating_sub(2).min(18);
    let rect = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    let block = pane(" Commit changes ".into(), true);
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
        .style(Style::default().fg(ACCENT)),
        chunks[1],
    );
    frame.render_widget(
        Paragraph::new(popup.error.as_deref().map(safe_text).unwrap_or_else(|| {
            "Commits staged changes, or all changes if nothing is staged".into()
        }))
        .style(Style::default().fg(if popup.error.is_some() { RED } else { MUTED })),
        chunks[2],
    );
    if !busy && chunks[0].width > 0 && chunks[0].height > 0 {
        frame.set_cursor_position((
            chunks[0].x + column.saturating_sub(left) as u16,
            chunks[0].y + row.saturating_sub(top) as u16,
        ));
    }
}

fn help(frame: &mut Frame, area: Rect) {
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
            Style::default().fg(ACCENT).bold(),
        ),
        Line::raw(""),
        Line::raw("←/→ or Tab          Switch files / diff focus"),
        Line::raw("j/k or ↑/↓          Select files / scroll diff"),
        Line::raw("Enter               Focus the diff"),
        Line::raw("Space               Stage/unstage file (Files) or hunk (Diff)"),
        Line::raw("c                   Commit changes (message popup)"),
        Line::raw("p / P               Pull (fast-forward only) / push branch"),
        Line::raw("e                   Edit in $EDITOR at first (visible) hunk"),
        Line::raw("PgUp/PgDn or u/d    Page up / down"),
        Line::raw("Home/End or g/G     First / last row or file"),
        Line::raw("[ / ]               Previous / next hunk"),
        Line::raw("? / F1              Show help"),
        Line::raw("q / Ctrl+C          Quit"),
        Line::raw(""),
        Line::styled(
            "Changes: [ ] unstaged (index → disk), [x] staged (HEAD → index)",
            Style::default().fg(MUTED),
        ),
        Line::styled(
            "Partially staged files have both entries. Renames = delete/add.",
            Style::default().fg(MUTED),
        ),
        Line::styled(
            "On narrow terminals, ←/→ or Tab switches panes.",
            Style::default().fg(MUTED),
        ),
        Line::raw(""),
        Line::styled("Press any key to close", Style::default().fg(ACCENT)),
    ];
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(text)
            .block(pane(" Help ".into(), true))
            .style(Style::default().bg(PANEL)),
        popup,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn renders_black_theme_and_navigation_help() {
        let (_dir, repo) = repo();
        let mut app = App::new(repo).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(119, 0)].bg, Color::Rgb(0, 0, 0));
        assert_eq!(buffer[(1, 5)].bg, Color::Rgb(0, 0, 0));
        assert_eq!(buffer[(119, 1)].bg, BG);
        assert_eq!(buffer[(119, 29)].bg, PANEL);
        let text = screen(&terminal);
        assert!(text.contains("Files 0"));
        app.handle_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let text = screen(&terminal);
        assert!(text.contains("Select files / scroll diff"));
        assert!(text.contains("Edit in $EDITOR at first (visible) hunk"));
    }

    #[test]
    fn wrapped_rows_keep_styles_and_only_number_the_first_segment() {
        for (kind, marker, bg) in [
            (RowKind::Added, '+', ADD_BG),
            (RowKind::Removed, '-', DEL_BG),
            (RowKind::Context, ' ', BG),
        ] {
            let mut view = crate::diff::DiffView {
                rows: vec![DiffRow {
                    kind,
                    old_line: Some(1),
                    new_line: Some(1),
                    spans: vec![
                        Span::styled("abcd", Style::default().fg(RED)),
                        Span::styled("\u{301}界ef", Style::default().fg(ACCENT)),
                    ],
                }],
                number_width: 1,
                ..crate::diff::DiffView::default()
            };
            view.wrap(13);
            let lines: Vec<_> = view
                .wrapped_rows
                .iter()
                .map(|wrapped| render_row(&view.rows[wrapped.row], wrapped, 1, 13))
                .collect();
            assert_eq!(lines.len(), 2);
            assert_eq!(lines[0].to_string(), format!(" 1 1 │ {marker} abcd\u{301}"));
            assert_eq!(lines[1].to_string(), "     │   界ef");
            assert_eq!(lines[0].spans[1].style.fg, Some(RED));
            assert_eq!(lines[0].spans[2].style.fg, Some(ACCENT));
            assert_eq!(lines[1].spans[1].style.fg, Some(ACCENT));
            for line in lines {
                assert_eq!(line.style.bg, Some(bg));
                assert_eq!(line.width(), 13);
            }
        }
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

        app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
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
    fn diff_scrolling_keeps_editor_at_the_hunk_start_after_resize() {
        let (dir, repo) = repo();
        let content: String = (1..=30).map(|n| format!("line {n}\n")).collect();
        std::fs::write(dir.path().join("test.txt"), content).unwrap();
        let mut app = App::new(repo).unwrap();
        app.focus = Focus::Diff;
        for _ in 0..15 {
            app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        }
        for (width, height) in [(120, 30), (40, 12), (80, 24)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let cursor = terminal.get_cursor_position().unwrap();
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            assert_eq!(terminal.get_cursor_position().unwrap(), cursor);
            assert!(app.scroll > 0);
            app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
            assert_eq!(app.edit_request.take().unwrap().line, Some(1));
        }
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
        for hint in [
            "Space stage file",
            "Space stage/unstage",
            "c commit",
            "p pull",
            "P push",
        ] {
            assert!(text.contains(hint), "Missing hint: {hint}");
        }
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let text = screen(&terminal);
        assert!(text.contains("Space unstage file"));
        assert!(text.contains("file.txt · Staged"));
        assert!(text.contains("Space unstage hunk"));
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
    fn renders_diff_and_help_at_different_sizes() {
        let (dir, repo) = repo();
        std::fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        let mut app = App::new(repo).unwrap();
        for (width, height) in [(120, 30), (80, 24), (40, 12), (10, 4), (1, 1)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            for focus in [Focus::Files, Focus::Diff] {
                let code = match focus {
                    Focus::Files => KeyCode::Left,
                    Focus::Diff => KeyCode::Right,
                };
                app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
                assert!(app.focus == focus);
                terminal.draw(|frame| draw(frame, &mut app)).unwrap();
                app.show_help = true;
                terminal.draw(|frame| draw(frame, &mut app)).unwrap();
                app.show_help = false;
            }
        }
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let text = screen(&terminal);
        assert!(text.contains("main.rs"));
        assert!(text.contains("fn main()"));
        assert!(text.contains("Rust"));
    }
}
