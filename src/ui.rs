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
    app::{App, BranchPicker, Focus},
    diff::{DiffRow, RowKind},
    git::{Mode, safe_text},
    highlight::TEXT,
};

const BG: Color = Color::Rgb(17, 21, 31);
const PANEL: Color = Color::Rgb(22, 28, 40);
const MUTED: Color = Color::Rgb(115, 130, 155);
const BORDER: Color = Color::Rgb(54, 65, 86);
const ACCENT: Color = Color::Rgb(122, 162, 247);
const GREEN: Color = Color::Rgb(158, 206, 106);
const RED: Color = Color::Rgb(247, 118, 142);
const ADD_BG: Color = Color::Rgb(25, 48, 39);
const DEL_BG: Color = Color::Rgb(55, 29, 40);
const HUNK_BG: Color = Color::Rgb(28, 37, 57);

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    frame.render_widget(
        Block::default().style(Style::default().bg(BG).fg(TEXT)),
        area,
    );
    let chunks = Layout::vertical([
        Constraint::Length(1),
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

    let tab_style = |mode| {
        if app.mode == mode {
            Style::default().fg(ACCENT).bold()
        } else {
            Style::default().fg(MUTED)
        }
    };
    let mut tabs = vec![
        Span::styled(" [s] ", Style::default().fg(MUTED)),
        Span::styled("Working tree", tab_style(Mode::Working)),
        Span::raw("  ·  "),
        Span::styled("Staged", tab_style(Mode::Staged)),
    ];
    tabs.push(Span::raw("  ·  "));
    tabs.push(Span::styled("[b] ", Style::default().fg(MUTED)));
    tabs.push(Span::styled(
        app.base_branch.as_ref().map_or_else(
            || "Compare branch".into(),
            |base| format!("Branch {}...HEAD", safe_text(base)),
        ),
        if app.mode.is_branch() {
            Style::default().fg(ACCENT).bold()
        } else {
            Style::default().fg(MUTED)
        },
    ));
    if app.editing_filter || !app.filter.is_empty() {
        tabs.push(Span::styled(
            format!("   /{}", safe_text(&app.filter)),
            Style::default().fg(ACCENT),
        ));
    } else {
        tabs.push(Span::styled(
            "   / filter files",
            Style::default().fg(MUTED),
        ));
    }
    let filter_cursor: usize = tabs.iter().map(Span::width).sum();
    frame.render_widget(
        Paragraph::new(Line::from(tabs)).style(Style::default().bg(PANEL)),
        chunks[1],
    );

    let body = chunks[2];
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
    let footer = if let Some(error) = &app.error {
        Line::styled(format!(" {}", safe_text(error)), Style::default().fg(RED))
    } else if app.editing_filter {
        Line::styled(
            " Type to filter · Enter/Esc apply · Ctrl+U clear",
            Style::default().fg(ACCENT),
        )
    } else {
        Line::styled(
            " ←/→/Tab focus · ↑/↓/j/k move · [/] hunks · b branch · r refresh · ? help · q quit",
            Style::default().fg(MUTED),
        )
    };
    frame.render_widget(
        Paragraph::new(footer).style(Style::default().bg(PANEL)),
        chunks[3],
    );
    if app.editing_filter && !app.show_help && chunks[1].width > 0 {
        let x = filter_cursor.min(chunks[1].width.saturating_sub(1) as usize);
        frame.set_cursor_position((chunks[1].x + x as u16, chunks[1].y));
    }
    if let Some(picker) = &mut app.branch_picker {
        branch_picker(frame, area, picker);
    }
    if app.show_help {
        help(frame, area);
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
        format!(" Files {}/{} ", app.visible.len(), app.files.len()),
        app.focus == Focus::Files,
    );
    let inner = block.inner(area);
    let items: Vec<ListItem> = app
        .visible
        .iter()
        .map(|&index| {
            let entry = &app.files[index];
            let color = match entry.status {
                'A' | '?' => GREEN,
                'D' | 'U' => RED,
                _ => ACCENT,
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{} ", entry.status), Style::default().fg(color)),
                Span::raw(safe_text(&entry.path.to_string_lossy())),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(block)
        .highlight_symbol("▸ ")
        .highlight_style(Style::default().bg(HUNK_BG).add_modifier(Modifier::BOLD));
    frame.render_stateful_widget(list, area, &mut app.list_state);
    if app.visible.is_empty() {
        let message = if app.files.is_empty() {
            " No changes"
        } else {
            " No matches"
        };
        frame.render_widget(
            Paragraph::new(message).style(Style::default().fg(MUTED)),
            inner,
        );
    }
}

fn diff(frame: &mut Frame, app: &mut App, area: Rect) {
    let title = app
        .selected()
        .map(|entry| safe_text(&entry.path.to_string_lossy()))
        .unwrap_or_else(|| app.mode.label().into());
    let language = app.view.language.map_or("Text", |language| language.name());
    let summary = Line::from(vec![
        Span::raw(format!(" {language} · {} hunks  ", app.view.hunks.len())),
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
    let block = pane(format!(" {title} "), app.focus == Focus::Diff).title_bottom(summary);
    let inner = block.inner(area);
    app.viewport_height = inner.height as usize;
    app.viewport_width = inner.width as usize;
    app.clamp_scroll();
    frame.render_widget(block, area);
    let rows: Vec<Line> = app
        .view
        .rows
        .iter()
        .skip(app.scroll)
        .take(inner.height as usize)
        .map(|row| {
            render_row(
                row,
                app.view.number_width.max(1),
                app.horizontal,
                inner.width as usize,
            )
        })
        .collect();
    frame.render_widget(Paragraph::new(rows), inner);
    if app.view.rows.len() > inner.height as usize && area.width > 1 {
        let mut state = ScrollbarState::new(app.view.rows.len())
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
    number_width: usize,
    horizontal: usize,
    width: usize,
) -> Line<'static> {
    let (marker, fg, bg) = match row.kind {
        RowKind::Added => ('+', GREEN, ADD_BG),
        RowKind::Removed => ('-', RED, DEL_BG),
        RowKind::Hunk => (' ', ACCENT, HUNK_BG),
        RowKind::Note => (' ', MUTED, BG),
        RowKind::Context => (' ', MUTED, BG),
    };
    if matches!(row.kind, RowKind::Hunk | RowKind::Note) {
        let mut spans = vec![Span::raw(" ")];
        spans.extend(clip_spans(&row.spans, horizontal, width.saturating_sub(1)));
        return Line::from(spans).style(Style::default().fg(fg).bg(bg));
    }
    let number = |value: Option<usize>| value.map_or_else(String::new, |n| n.to_string());
    let gutter = format!(
        " {:>number_width$} {:>number_width$} │ {marker} ",
        number(row.old_line),
        number(row.new_line)
    );
    let available = width.saturating_sub(gutter.width());
    let mut spans = vec![Span::styled(gutter, Style::default().fg(fg))];
    spans.extend(clip_spans(&row.spans, horizontal, available));
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
        for ch in span.content.chars() {
            let cells = ch.width().unwrap_or(0);
            if cells == 0 {
                if has_visible && column >= offset && column <= end {
                    text.push(ch);
                }
                continue;
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
        if column >= end {
            break;
        }
    }
    result
}

fn branch_picker(frame: &mut Frame, area: Rect, picker: &mut BranchPicker) {
    let width = area.width.saturating_sub(4).min(76);
    let height = area.height.saturating_sub(2).min(18);
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    let block = pane(" Base branch ".into(), true).title_bottom(format!(
        " {}/{} branches ",
        picker.visible.len(),
        picker.branches.len()
    ));
    let inner = block.inner(popup);
    frame.render_widget(Clear, popup);
    frame.render_widget(block, popup);
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(inner);
    frame.render_widget(
        Paragraph::new("Merge base → HEAD: committed changes since divergence"),
        chunks[0],
    );
    let input = safe_text(&picker.input);
    let available = chunks[1].width.saturating_sub(3) as usize;
    let offset = input.width().saturating_sub(available);
    let visible = clip_spans(&[Span::raw(input.clone())], offset, available);
    let mut field = vec![Span::styled("> ", Style::default().fg(ACCENT))];
    field.extend(visible);
    frame.render_widget(Paragraph::new(Line::from(field)), chunks[1]);
    let items: Vec<ListItem> = picker
        .visible
        .iter()
        .map(|&index| ListItem::new(safe_text(&picker.branches[index])))
        .collect();
    let list = List::new(items)
        .highlight_symbol("▸ ")
        .highlight_style(Style::default().bg(HUNK_BG).add_modifier(Modifier::BOLD));
    frame.render_stateful_widget(list, chunks[2], &mut picker.list_state);
    if picker.visible.is_empty() {
        frame.render_widget(
            Paragraph::new("No matching branches — type a ref, then Enter")
                .style(Style::default().fg(MUTED)),
            chunks[2],
        );
    }
    frame.render_widget(
        Paragraph::new("↑/↓ select · Type to filter · Enter compare · Esc cancel")
            .style(Style::default().fg(ACCENT)),
        chunks[3],
    );
    frame.render_widget(
        Paragraph::new(
            picker
                .error
                .as_deref()
                .map(safe_text)
                .unwrap_or_else(|| "Ctrl+U clear · Local/remote refs; no checkout or fetch".into()),
        )
        .style(Style::default().fg(if picker.error.is_some() { RED } else { MUTED })),
        chunks[4],
    );
    if chunks[1].width > 0 && chunks[1].height > 0 {
        let x = (2 + input.width().saturating_sub(offset))
            .min(chunks[1].width.saturating_sub(1) as usize);
        frame.set_cursor_position((chunks[1].x + x as u16, chunks[1].y));
    }
}

fn help(frame: &mut Frame, area: Rect) {
    let width = area.width.saturating_sub(4).min(70);
    let height = area.height.saturating_sub(2).min(26);
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    let text = vec![
        Line::styled(
            "Read-only Git diffs with Tree-sitter highlighting",
            Style::default().fg(ACCENT).bold(),
        ),
        Line::raw(""),
        Line::raw("←/→ or Tab          Switch files / diff focus"),
        Line::raw("j/k or ↑/↓          Select files / scroll diff"),
        Line::raw("Enter               Focus the diff"),
        Line::raw("h/l                 Scroll code horizontally"),
        Line::raw("PgUp/PgDn or u/d    Page up / down"),
        Line::raw("Home/End or g/G     First / last row or file"),
        Line::raw("[ / ]               Previous / next hunk"),
        Line::raw("s                   Cycle working tree / staged / branch"),
        Line::raw("b                   Select a base branch (type to filter)"),
        Line::raw("/                   Filter file paths (case-insensitive)"),
        Line::raw("Enter/Esc           Apply filter; Esc again clears it"),
        Line::raw("r                   Refresh manually"),
        Line::raw("? / F1              Show help"),
        Line::raw("q / Ctrl+C          Quit"),
        Line::raw(""),
        Line::styled(
            "Working tree = index → disk, including untracked files",
            Style::default().fg(MUTED),
        ),
        Line::styled(
            "Staged = HEAD → index. Renames appear as delete/add.",
            Style::default().fg(MUTED),
        ),
        Line::styled(
            "Branch = merge base → HEAD (committed changes only).",
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
    fn branch_mode_and_searchable_picker_are_visible() {
        let (dir, repo) = crate::git::tests::diverged_repo();
        git(
            dir.path(),
            &["update-ref", "refs/remotes/origin/main", "main"],
        );
        let mut app = App::new(repo, Some("main".into())).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(screen(&terminal).contains("Branch main...HEAD"));
        app.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE));
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let text = screen(&terminal);
        assert!(text.contains("Base branch"));
        assert!(text.contains("▸ main"));
        assert!(text.contains("origin/main"));
        assert!(text.contains("3/3 branches"));
        assert!(text.contains("Type to filter"));
        assert!(text.contains("committed changes since divergence"));
        for ch in "ori".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let text = screen(&terminal);
        assert!(text.contains("> ori"));
        assert!(text.contains("▸ origin/main"));
        assert!(text.contains("1/3 branches"));
        app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let text = screen(&terminal);
        assert!(text.contains("No matching branches"));
        assert!(text.contains("Cannot resolve"));
    }

    #[test]
    fn branch_picker_scrolls_to_keep_the_selection_visible() {
        let (dir, repo) = crate::git::tests::diverged_repo();
        for index in 0..30 {
            git(dir.path(), &["branch", &format!("topic/{index:02}")]);
        }
        let mut app = App::new(repo, None).unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE));
        for (width, height) in [(80, 24), (40, 12)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            assert!(screen(&terminal).contains("▸ topic/29"));
            app.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            assert!(screen(&terminal).contains("▸ feature"));
        }
    }

    #[test]
    fn renders_diff_and_help_at_different_sizes() {
        let (dir, repo) = repo();
        std::fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        let mut app = App::new(repo, None).unwrap();
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
                app.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE));
                let picker = app.branch_picker.as_mut().unwrap();
                picker.input = "very-long/界/branch/".repeat(20);
                picker.error = Some("Cannot resolve revision".into());
                terminal.draw(|frame| draw(frame, &mut app)).unwrap();
                app.branch_picker = None;
            }
        }
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let text = screen(&terminal);
        assert!(text.contains("main.rs"));
        assert!(text.contains("fn main()"));
        assert!(text.contains("Rust"));
        assert!(!text.contains("● LIVE"));
        assert!(!text.contains("● MANUAL"));
    }
}
