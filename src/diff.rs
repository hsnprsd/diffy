use std::{
    ops::Range,
    path::Path,
    time::{Duration, Instant},
};

use anyhow::Result;
use ratatui::text::Span;
use similar::{Algorithm, ChangeTag, DiffTag, TextDiff};
use unicode_width::UnicodeWidthChar;

use crate::{
    git::Content,
    highlight::{Language, SyntaxHighlighter},
};

const MAX_LINES: usize = 40_000;
const MIN_SPLIT_WIDTH: usize = 60;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DiffMode {
    #[default]
    Unified,
    SideBySide,
}

impl DiffMode {
    pub fn toggle(self) -> Self {
        match self {
            Self::Unified => Self::SideBySide,
            Self::SideBySide => Self::Unified,
        }
    }

    pub fn effective(self, width: usize) -> Self {
        if width < MIN_SPLIT_WIDTH {
            Self::Unified
        } else {
            self
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Unified => "Unified",
            Self::SideBySide => "Side-by-side",
        }
    }
}

pub fn split_widths(width: usize) -> (usize, usize) {
    let available = width.saturating_sub(1); // Center divider.
    (available / 2, available - available / 2)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    Context,
    Added,
    Removed,
    Hunk,
    Note,
}

pub struct DiffRow {
    pub kind: RowKind,
    pub old_line: Option<usize>,
    pub new_line: Option<usize>,
    pub spans: Vec<Span<'static>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WrappedRow {
    pub row: usize,
    pub offset: usize,
    pub width: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitRow {
    Shared(WrappedRow),
    Pair {
        old: Option<WrappedRow>,
        new: Option<WrappedRow>,
    },
}

#[derive(Default)]
pub struct DiffView {
    pub rows: Vec<DiffRow>,
    pub hunks: Vec<usize>,
    pub wrapped_rows: Vec<WrappedRow>,
    pub wrapped_hunks: Vec<usize>,
    pub split_rows: Vec<SplitRow>,
    pub patches: Vec<String>,
    pub source: Option<(String, String)>,
    pub additions: usize,
    pub deletions: usize,
    pub language: Option<Language>,
    pub number_width: usize,
}

impl DiffView {
    pub fn layout(&mut self, width: usize, mode: DiffMode) {
        match mode.effective(width) {
            DiffMode::Unified => self.wrap(width),
            DiffMode::SideBySide => self.wrap_split(width),
        }
    }

    /// Lay out terminal-cell ranges without splitting wide characters or their combining marks.
    pub fn wrap(&mut self, width: usize) {
        self.wrapped_rows.clear();
        self.wrapped_hunks.clear();
        self.split_rows.clear();
        for (row, source) in self.rows.iter().enumerate() {
            if source.kind == RowKind::Hunk {
                self.wrapped_hunks.push(self.wrapped_rows.len());
            }
            let available = width.saturating_sub(source.gutter_width(self.number_width));
            self.wrapped_rows
                .extend(wrap_source(row, source, available));
        }
    }

    fn wrap_split(&mut self, width: usize) {
        self.wrapped_rows.clear();
        self.wrapped_hunks.clear();
        self.split_rows.clear();
        let widths = split_widths(width);
        let mut row = 0;
        while row < self.rows.len() {
            match self.rows[row].kind {
                RowKind::Context => {
                    self.push_pair(Some(row), Some(row), widths);
                    row += 1;
                }
                RowKind::Added | RowKind::Removed => {
                    let mut old = Vec::new();
                    let mut new = Vec::new();
                    while row < self.rows.len()
                        && matches!(self.rows[row].kind, RowKind::Added | RowKind::Removed)
                    {
                        let side = if self.rows[row].kind == RowKind::Removed {
                            &mut old
                        } else {
                            &mut new
                        };
                        let source = row;
                        row += 1;
                        let note = (row < self.rows.len() && self.rows[row].kind == RowKind::Note)
                            .then_some(row);
                        if note.is_some() {
                            row += 1;
                        }
                        side.push((source, note));
                    }
                    // Pair replacements in source order; surplus lines get an empty opposite side.
                    for index in 0..old.len().max(new.len()) {
                        self.push_pair(
                            old.get(index).map(|&(source, _)| source),
                            new.get(index).map(|&(source, _)| source),
                            widths,
                        );
                        let old_note = old.get(index).and_then(|&(_, note)| note);
                        let new_note = new.get(index).and_then(|&(_, note)| note);
                        if old_note.is_some() || new_note.is_some() {
                            self.push_pair(old_note, new_note, widths);
                        }
                    }
                }
                RowKind::Hunk | RowKind::Note => {
                    if self.rows[row].kind == RowKind::Hunk {
                        self.wrapped_hunks.push(self.wrapped_rows.len());
                    }
                    for wrapped in wrap_source(row, &self.rows[row], width.saturating_sub(1)) {
                        self.wrapped_rows.push(wrapped);
                        self.split_rows.push(SplitRow::Shared(wrapped));
                    }
                    row += 1;
                }
            }
        }
    }

    fn push_pair(&mut self, old: Option<usize>, new: Option<usize>, widths: (usize, usize)) {
        let wrap = |row: Option<usize>, width: usize| {
            row.map(|row| {
                let source = &self.rows[row];
                let available = width.saturating_sub(source.split_gutter_width(self.number_width));
                wrap_source(row, source, available)
            })
            .unwrap_or_default()
        };
        let old = wrap(old, widths.0);
        let new = wrap(new, widths.1);
        // Wrap each side independently, keeping the next pair below the longer side.
        for index in 0..old.len().max(new.len()) {
            let old = old.get(index).copied();
            let new = new.get(index).copied();
            self.wrapped_rows.push(old.or(new).unwrap());
            self.split_rows.push(SplitRow::Pair { old, new });
        }
    }

    /// Locate the same source position after resizing or switching layouts.
    pub fn position(&self, anchor: WrappedRow) -> usize {
        let matches = |row: &WrappedRow| row.row == anchor.row && row.offset <= anchor.offset;
        let position = if self.split_rows.is_empty() {
            self.wrapped_rows.iter().rposition(matches)
        } else {
            self.split_rows.iter().rposition(|row| match row {
                SplitRow::Shared(row) => matches(row),
                SplitRow::Pair { old, new } => {
                    old.as_ref().is_some_and(matches) || new.as_ref().is_some_and(matches)
                }
            })
        };
        position.unwrap_or_else(|| {
            self.wrapped_rows
                .iter()
                .position(|row| row.row >= anchor.row)
                .unwrap_or_else(|| self.wrapped_rows.len().saturating_sub(1))
        })
    }

    pub fn notice(message: impl Into<String>) -> Self {
        Self {
            rows: vec![DiffRow::note(message)],
            ..Self::default()
        }
    }

    pub fn build(
        path: &Path,
        old: Content,
        new: Content,
        highlighter: &mut SyntaxHighlighter,
    ) -> Result<Self> {
        let (old, new) = match (old, new) {
            (Content::Notice(message), _) | (_, Content::Notice(message)) => {
                return Ok(Self::notice(message));
            }
            (Content::Text(old), Content::Text(new)) => (old, new),
        };
        let line_count = old.lines().count().max(new.lines().count());
        if line_count > MAX_LINES {
            return Ok(Self::notice("File exceeds the 40,000-line preview limit"));
        }
        if old == new {
            return Ok(Self::notice(
                "No textual changes (file mode or metadata may have changed)",
            ));
        }
        let diff = TextDiff::configure()
            .algorithm(Algorithm::Myers)
            .timeout(Duration::from_millis(500))
            .diff_lines(&old, &new);
        let (old_emphasis, new_emphasis) =
            word_emphasis(&diff, Instant::now() + Duration::from_millis(100));
        let palette = highlighter.theme().palette();
        let old_lines = highlighter.lines(path, &old, &old_emphasis, palette.del_word_bg)?;
        let new_lines = highlighter.lines(path, &new, &new_emphasis, palette.add_word_bg)?;
        let mut view = Self {
            language: Language::detect(path),
            number_width: line_count.max(1).to_string().len(),
            ..Self::default()
        };
        for group in diff.grouped_ops(3) {
            let first = group.first().unwrap();
            let last = group.last().unwrap();
            let old_range = first.old_range().start..last.old_range().end;
            let new_range = first.new_range().start..last.new_range().end;
            let start = |range: &std::ops::Range<usize>| {
                if range.is_empty() {
                    range.start
                } else {
                    range.start + 1
                }
            };
            let header = format!(
                "@@ -{},{} +{},{} @@\n",
                start(&old_range),
                old_range.len(),
                start(&new_range),
                new_range.len()
            );
            let mut patch = header.clone();
            view.hunks.push(view.rows.len());
            view.rows.push(DiffRow {
                kind: RowKind::Hunk,
                old_line: None,
                new_line: None,
                spans: vec![Span::raw(header.trim_end().to_owned())],
            });
            for op in group {
                for change in diff.iter_changes(&op) {
                    patch.push(match change.tag() {
                        ChangeTag::Equal => ' ',
                        ChangeTag::Delete => '-',
                        ChangeTag::Insert => '+',
                    });
                    patch.push_str(change.value());
                    if change.missing_newline() {
                        patch.push_str("\n\\ No newline at end of file\n");
                    }
                    let (kind, spans) = match change.tag() {
                        ChangeTag::Equal => {
                            (RowKind::Context, &new_lines[change.new_index().unwrap()])
                        }
                        ChangeTag::Delete => {
                            view.deletions += 1;
                            (RowKind::Removed, &old_lines[change.old_index().unwrap()])
                        }
                        ChangeTag::Insert => {
                            view.additions += 1;
                            (RowKind::Added, &new_lines[change.new_index().unwrap()])
                        }
                    };
                    view.rows.push(DiffRow {
                        kind,
                        old_line: change.old_index().map(|n| n + 1),
                        new_line: change.new_index().map(|n| n + 1),
                        spans: spans.clone(),
                    });
                    if change.missing_newline() {
                        view.rows
                            .push(DiffRow::note("\\ No newline at end of file"));
                    }
                }
            }
            view.patches.push(patch);
        }
        view.source = Some((old, new));
        Ok(view)
    }
}

fn wrap_source(row: usize, source: &DiffRow, available: usize) -> Vec<WrappedRow> {
    let mut rows = Vec::new();
    let mut wrapped = WrappedRow {
        row,
        offset: 0,
        width: 0,
    };
    if available > 0 {
        for ch in source.spans.iter().flat_map(|span| span.content.chars()) {
            let cells = ch.width().unwrap_or(0);
            if cells > 0 && wrapped.width > 0 && wrapped.width + cells > available {
                rows.push(wrapped);
                wrapped.offset += wrapped.width;
                wrapped.width = 0;
            }
            wrapped.width += cells;
        }
    }
    // Empty source lines still occupy one screen row.
    rows.push(wrapped);
    rows
}

/// Find raw byte ranges before display escaping or tab expansion changes their offsets.
fn word_emphasis<'a>(
    diff: &'a TextDiff<'a, 'a, 'a, str>,
    deadline: Instant,
) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    let offsets = |lines: &[&str]| {
        let mut offset = 0;
        lines
            .iter()
            .map(|line| {
                let start = offset;
                offset += line.len();
                start
            })
            .collect::<Vec<_>>()
    };
    let old_offsets = offsets(diff.old_slices());
    let new_offsets = offsets(diff.new_slices());
    let mut old_ranges = Vec::new();
    let mut new_ranges = Vec::new();
    // Share one budget across the file, rather than allowing a timeout per replacement.
    for op in diff.ops() {
        if Instant::now() >= deadline {
            break;
        }
        if op.tag() != DiffTag::Replace {
            continue;
        }
        for change in diff.iter_inline_changes_deadline(op, Some(deadline)) {
            let (mut offset, ranges) = match change.tag() {
                ChangeTag::Delete => (old_offsets[change.old_index().unwrap()], &mut old_ranges),
                ChangeTag::Insert => (new_offsets[change.new_index().unwrap()], &mut new_ranges),
                ChangeTag::Equal => continue,
            };
            for &(emphasized, text) in change.values() {
                let end = offset + text.len();
                if emphasized && offset < end {
                    ranges.push(offset..end);
                }
                offset = end;
            }
        }
    }
    (old_ranges, new_ranges)
}

impl DiffRow {
    pub fn gutter_width(&self, number_width: usize) -> usize {
        if matches!(self.kind, RowKind::Hunk | RowKind::Note) {
            1
        } else {
            2 * number_width.max(1) + 7
        }
    }

    pub fn split_gutter_width(&self, number_width: usize) -> usize {
        if matches!(self.kind, RowKind::Hunk | RowKind::Note) {
            1
        } else {
            number_width.max(1) + 4
        }
    }

    fn note(message: impl Into<String>) -> Self {
        Self {
            kind: RowKind::Note,
            old_line: None,
            new_line: None,
            spans: vec![Span::raw(message.into())],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(old: &str, new: &str) -> DiffView {
        DiffView::build(
            Path::new("test.rs"),
            Content::Text(old.into()),
            Content::Text(new.into()),
            &mut SyntaxHighlighter::default(),
        )
        .unwrap()
    }

    fn row_text(row: &DiffRow) -> String {
        row.spans.iter().map(|span| span.content.as_ref()).collect()
    }

    fn emphasized_text(row: &DiffRow) -> String {
        row.spans
            .iter()
            .filter(|span| {
                let palette = crate::theme::Theme::default().palette();
                span.style.bg == Some(palette.add_word_bg)
                    || span.style.bg == Some(palette.del_word_bg)
            })
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn split_text(view: &DiffView, row: Option<WrappedRow>) -> Option<String> {
        row.map(|row| row_text(&view.rows[row.row]))
    }

    #[test]
    fn split_layout_pairs_context_replacements_and_surplus_lines() {
        let mut view = build(
            "context\nold one\nold two\ntail\n",
            "context\nnew one\nnew two\nnew three\ntail\n",
        );
        let patches = view.patches.clone();
        let new_second = view
            .rows
            .iter()
            .position(|row| row.kind == RowKind::Added && row.new_line == Some(3))
            .unwrap();
        view.layout(80, DiffMode::SideBySide);
        assert_eq!(view.wrapped_hunks, [0]);
        assert_eq!(view.wrapped_rows.len(), 6);
        assert!(matches!(view.split_rows[0], SplitRow::Shared(_)));
        let pairs: Vec<_> = view
            .split_rows
            .iter()
            .filter_map(|row| match *row {
                SplitRow::Pair { old, new } => {
                    Some((split_text(&view, old), split_text(&view, new)))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            pairs,
            [
                (Some("context".into()), Some("context".into())),
                (Some("old one".into()), Some("new one".into())),
                (Some("old two".into()), Some("new two".into())),
                (None, Some("new three".into())),
                (Some("tail".into()), Some("tail".into())),
            ]
        );
        // The right side must map to its own pair, not the next representative source row.
        assert_eq!(
            view.position(WrappedRow {
                row: new_second,
                offset: 0,
                width: 7,
            }),
            3
        );
        assert_eq!(view.patches, patches);
    }

    #[test]
    fn split_layout_handles_one_sided_changes_and_empty_lines() {
        for (old, new, old_present, new_present) in
            [("", "\n", false, true), ("removed\n", "", true, false)]
        {
            let mut view = build(old, new);
            view.layout(80, DiffMode::SideBySide);
            assert_eq!(view.split_rows.len(), 2);
            let SplitRow::Pair { old, new } = view.split_rows[1] else {
                panic!("Expected a one-sided line");
            };
            assert_eq!(old.is_some(), old_present);
            assert_eq!(new.is_some(), new_present);
            assert_eq!(old.or(new).unwrap().offset, 0);
        }
    }

    #[test]
    fn split_layout_keeps_missing_newline_notes_with_their_source_line() {
        let mut view = build("old first\nold last", "new first");
        view.layout(80, DiffMode::SideBySide);
        let pairs: Vec<_> = view
            .split_rows
            .iter()
            .filter_map(|row| match *row {
                SplitRow::Pair { old, new } => {
                    Some((split_text(&view, old), split_text(&view, new)))
                }
                _ => None,
            })
            .collect();
        let note = "\\ No newline at end of file";
        assert_eq!(
            pairs,
            [
                (Some("old first".into()), Some("new first".into())),
                (None, Some(note.into())),
                (Some("old last".into()), None),
                (Some(note.into()), None),
            ]
        );
    }

    #[test]
    fn split_layout_wraps_sides_independently_without_splitting_unicode() {
        let mut view = build(&format!("{}\n", "界e\u{301}".repeat(20)), "short\n");
        let patches = view.patches.clone();
        view.layout(60, DiffMode::SideBySide);
        assert_eq!(view.split_rows.len(), 4);
        for (index, expected_width) in [24, 24, 12].into_iter().enumerate() {
            let SplitRow::Pair { old, new } = view.split_rows[index + 1] else {
                panic!("Expected wrapped code");
            };
            let old = old.unwrap();
            assert_eq!((old.offset, old.width), (index * 24, expected_width));
            assert_eq!(new.map(|row| row.width), (index == 0).then_some(5));
            assert_eq!(view.position(old), index + 1);
        }
        let anchor = view.wrapped_rows[2];
        view.layout(60, DiffMode::Unified);
        let visible = view.wrapped_rows[view.position(anchor)];
        assert_eq!(visible.row, anchor.row);
        assert!(visible.offset <= anchor.offset && visible.offset + visible.width > anchor.offset);
        assert_eq!(view.patches, patches);
    }

    #[test]
    fn split_layout_shows_notices_and_falls_back_when_narrow() {
        let mut view = DiffView::notice("Binary file");
        view.layout(80, DiffMode::SideBySide);
        assert_eq!(view.split_rows, [SplitRow::Shared(view.wrapped_rows[0])]);
        view.layout(59, DiffMode::SideBySide);
        assert!(view.split_rows.is_empty());
        assert_eq!(view.wrapped_rows.len(), 1);
        assert_eq!(row_text(&view.rows[0]), "Binary file");
        assert_eq!(DiffMode::SideBySide.effective(59), DiffMode::Unified);
        assert_eq!(DiffMode::SideBySide.effective(60), DiffMode::SideBySide);
    }

    #[test]
    fn word_boundaries_include_adjacent_code_punctuation() {
        let view = build("if value == 1 { call(); }\n", "if value != 1 { call(); }\n");
        assert_eq!(emphasized_text(&view.rows[1]), "=");
        assert_eq!(emphasized_text(&view.rows[2]), "!");
    }

    #[test]
    fn word_emphasis_tracks_unequal_replacement_blocks_and_context() {
        let view = build(
            "unchanged\nlet value = old;\ntail\n",
            "unchanged\nlet value = new;\nextra\ntail\n",
        );
        let rows: Vec<_> = view
            .rows
            .iter()
            .skip(1)
            .map(|row| (row.kind, row_text(row), emphasized_text(row)))
            .collect();
        assert_eq!(
            rows,
            [
                (RowKind::Context, "unchanged".into(), "".into()),
                (RowKind::Removed, "let value = old;".into(), "old".into()),
                (RowKind::Added, "let value = new;".into(), "new".into()),
                (RowKind::Added, "extra".into(), "extra".into()),
                (RowKind::Context, "tail".into(), "".into()),
            ]
        );
    }

    #[test]
    fn word_emphasis_tracks_separate_hunks_and_multiple_words() {
        let old: String = (0..20)
            .map(|n| format!("let value_{n} = compute({n});\n"))
            .collect();
        let new = old
            .replace("compute(1)", "calculate(101)")
            .replace("compute(18)", "calculate(118)");
        let view = build(&old, &new);
        assert_eq!(view.hunks.len(), 2);
        assert_eq!((view.additions, view.deletions), (2, 2));
        let context: Vec<_> = view
            .rows
            .iter()
            .filter(|row| row.kind == RowKind::Context)
            .map(|row| row.new_line.unwrap())
            .collect();
        assert_eq!(context, [1, 3, 4, 5, 16, 17, 18, 20]);
        let changed: Vec<_> = view
            .rows
            .iter()
            .filter(|row| matches!(row.kind, RowKind::Added | RowKind::Removed))
            .map(emphasized_text)
            .collect();
        assert_eq!(
            changed,
            ["compute1", "calculate101", "compute18", "calculate118"]
        );
    }

    #[test]
    fn unicode_words_and_combining_marks_are_emphasized_intact() {
        for (old_word, new_word) in [("café", "thé"), ("e\u{301}", "a\u{301}"), ("界", "海")] {
            let old = format!("let label = \"{old_word}\";\n");
            let new = format!("let label = \"{new_word}\";\n");
            let view = build(&old, &new);
            assert_eq!(row_text(&view.rows[1]), old.trim_end_matches('\n'));
            assert_eq!(row_text(&view.rows[2]), new.trim_end_matches('\n'));
            assert_eq!(emphasized_text(&view.rows[1]), old_word);
            assert_eq!(emphasized_text(&view.rows[2]), new_word);
        }
    }

    #[test]
    fn pure_additions_and_deletions_keep_line_only_shading() {
        for (old, new, expected) in [
            ("", "added\n", vec![(RowKind::Added, "added")]),
            ("removed\n", "", vec![(RowKind::Removed, "removed")]),
        ] {
            let view = build(old, new);
            let rows: Vec<_> = view
                .rows
                .iter()
                .skip(1)
                .map(|row| {
                    assert!(row.spans.iter().all(|span| span.style.bg.is_none()));
                    (row.kind, row_text(row))
                })
                .collect();
            assert_eq!(
                rows,
                expected
                    .into_iter()
                    .map(|(kind, text)| (kind, text.to_owned()))
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn expired_word_diff_budget_returns_line_only_highlighting() {
        let diff = TextDiff::from_lines("let value = old;\n", "let value = new;\n");
        assert_eq!(
            word_emphasis(&diff, Instant::now() - Duration::from_millis(1)),
            (Vec::new(), Vec::new())
        );
    }

    #[test]
    fn word_highlighting_preserves_raw_patches_and_missing_newline_notes() {
        let old = "let value = 10;\r\n";
        let new = "let value = 20;";
        let view = build(old, new);
        assert_eq!(emphasized_text(&view.rows[1]), "10");
        assert_eq!(emphasized_text(&view.rows[2]), "20");
        assert_eq!(view.rows[3].kind, RowKind::Note);
        assert_eq!(
            view.patches,
            [
                "@@ -1,1 +1,1 @@\n-let value = 10;\r\n+let value = 20;\n\\ No newline at end of file\n"
            ]
        );
        assert_eq!(view.source, Some((old.into(), new.into())));
    }

    #[test]
    fn wraps_terminal_cells_across_spans_and_keeps_empty_lines() {
        let mut view = DiffView {
            rows: vec![DiffRow {
                kind: RowKind::Added,
                old_line: None,
                new_line: Some(1),
                spans: vec![Span::raw("abc"), Span::raw("\u{301}界def")],
            }],
            number_width: 1,
            ..DiffView::default()
        };
        view.wrap(13); // Four code cells after the gutter.
        assert_eq!(
            view.wrapped_rows,
            [
                WrappedRow {
                    row: 0,
                    offset: 0,
                    width: 3
                },
                WrappedRow {
                    row: 0,
                    offset: 3,
                    width: 4
                },
                WrappedRow {
                    row: 0,
                    offset: 7,
                    width: 1
                },
            ]
        );
        view.rows[0].spans.clear();
        view.wrap(13);
        assert_eq!(
            view.wrapped_rows,
            [WrappedRow {
                row: 0,
                offset: 0,
                width: 0
            }]
        );
    }

    #[test]
    fn exact_width_lines_and_tiny_panes_have_one_row() {
        let mut view = DiffView::notice("abcd");
        for width in [5, 0, 1] {
            view.wrap(width);
            assert_eq!(view.wrapped_rows.len(), 1);
            assert_eq!(view.wrapped_rows[0].offset, 0);
        }
        view.wrap(2);
        assert_eq!(view.wrapped_rows.len(), 4);
        view.rows[0].spans = vec![Span::raw("界界")];
        view.wrap(2);
        assert_eq!(view.wrapped_rows.len(), 2);
        assert_eq!(view.wrapped_rows[1].offset, 2);
    }

    #[test]
    fn additions_deletions_and_line_numbers() {
        let view = build("let a = 1;\n", "let a = 2;\nlet b = 3;\n");
        assert_eq!((view.additions, view.deletions), (2, 1));
        assert_eq!(view.hunks, [0]);
        assert_eq!(view.rows[1].kind, RowKind::Removed);
        assert_eq!(view.rows[1].old_line, Some(1));
        assert_eq!(view.rows[2].new_line, Some(1));
    }

    #[test]
    fn empty_ranges_and_missing_newlines() {
        let view = build("", "hello");
        assert_eq!(view.rows[0].spans[0].content, "@@ -0,0 +1,1 @@");
        assert_eq!(view.rows.last().unwrap().kind, RowKind::Note);
        assert_eq!((view.additions, view.deletions), (1, 0));
        let view = build("hello\n", "");
        assert_eq!(view.rows[0].spans[0].content, "@@ -1,1 +0,0 @@");
    }

    #[test]
    fn crlf_unicode_and_newline_only_changes() {
        let view = build("let 界 = \"é\";\r\n", "let 界 = \"é\";");
        assert_eq!((view.additions, view.deletions), (1, 1));
        assert_eq!(row_text(&view.rows[1]), "let 界 = \"é\";");
        assert_eq!(row_text(&view.rows[2]), "let 界 = \"é\";");
        assert_eq!(emphasized_text(&view.rows[1]), "");
        assert_eq!(emphasized_text(&view.rows[2]), "");
    }
}
