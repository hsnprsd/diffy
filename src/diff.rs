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
    highlight::{ADD_WORD_BG, DEL_WORD_BG, Language, SyntaxHighlighter},
};

const MAX_LINES: usize = 40_000;

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

#[derive(Default)]
pub struct DiffView {
    pub rows: Vec<DiffRow>,
    pub hunks: Vec<usize>,
    pub wrapped_rows: Vec<WrappedRow>,
    pub wrapped_hunks: Vec<usize>,
    pub patches: Vec<String>,
    pub source: Option<(String, String)>,
    pub additions: usize,
    pub deletions: usize,
    pub language: Option<Language>,
    pub number_width: usize,
}

impl DiffView {
    /// Lay out terminal-cell ranges without splitting wide characters or their combining marks.
    pub fn wrap(&mut self, width: usize) {
        self.wrapped_rows.clear();
        self.wrapped_hunks.clear();
        for (row, source) in self.rows.iter().enumerate() {
            if source.kind == RowKind::Hunk {
                self.wrapped_hunks.push(self.wrapped_rows.len());
            }
            let available = width.saturating_sub(source.gutter_width(self.number_width));
            let mut wrapped = WrappedRow {
                row,
                offset: 0,
                width: 0,
            };
            if available > 0 {
                for ch in source.spans.iter().flat_map(|span| span.content.chars()) {
                    let cells = ch.width().unwrap_or(0);
                    if cells > 0 && wrapped.width > 0 && wrapped.width + cells > available {
                        self.wrapped_rows.push(wrapped);
                        wrapped.offset += wrapped.width;
                        wrapped.width = 0;
                    }
                    wrapped.width += cells;
                }
            }
            // Empty source lines still occupy one screen row.
            self.wrapped_rows.push(wrapped);
        }
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
        let old_lines = highlighter.lines(path, &old, &old_emphasis, DEL_WORD_BG)?;
        let new_lines = highlighter.lines(path, &new, &new_emphasis, ADD_WORD_BG)?;
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
            .filter(|span| matches!(span.style.bg, Some(ADD_WORD_BG | DEL_WORD_BG)))
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn emphasizes_changed_words_without_recoloring_syntax() {
        let old = "let result = compute(10, \"old\");\n";
        let new = "let result = compute(20, \"new\");\n";
        let view = build(old, new);
        for (row, source, words, background) in [
            (&view.rows[1], old, "10old", DEL_WORD_BG),
            (&view.rows[2], new, "20new", ADD_WORD_BG),
        ] {
            assert_eq!(row_text(row), source.trim_end_matches('\n'));
            assert_eq!(emphasized_text(row), words);
            let syntax = SyntaxHighlighter::default()
                .lines(Path::new("test.rs"), source, &[], background)
                .unwrap();
            let colors = |spans: &[Span<'_>]| {
                spans
                    .iter()
                    .flat_map(|span| span.content.chars().map(|ch| (ch, span.style.fg)))
                    .collect::<Vec<_>>()
            };
            assert_eq!(colors(&row.spans), colors(&syntax[0]));
            assert!(
                row.spans
                    .iter()
                    .any(|span| span.style.bg == Some(background))
            );
        }
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
    fn wrapping_maps_hunks_without_changing_source_rows_or_patches() {
        let mut view = build("", "abcdefghijklmnop\n");
        let patches = view.patches.clone();
        view.wrap(13);
        assert_eq!(view.wrapped_hunks, [0]);
        assert_eq!(view.wrapped_rows.len(), 6); // Two header rows and four code rows.
        assert_eq!(view.wrapped_rows[2].row, view.hunks[0] + 1);
        assert_eq!(view.rows.len(), 2);
        assert_eq!(view.patches, patches);
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
    fn distinct_hunks_have_context() {
        let old: String = (0..30).map(|n| format!("line {n}\n")).collect();
        let new = old
            .replace("line 2\n", "changed 2\n")
            .replace("line 25\n", "changed 25\n");
        let view = build(&old, &new);
        assert_eq!(view.hunks.len(), 2);
        assert_eq!((view.additions, view.deletions), (2, 2));
        assert!(view.rows.len() < 30);
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
