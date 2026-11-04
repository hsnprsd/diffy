use std::{path::Path, time::Duration};

use anyhow::Result;
use ratatui::text::Span;
use similar::{Algorithm, ChangeTag, TextDiff};

use crate::{
    git::Content,
    highlight::{Language, SyntaxHighlighter},
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

#[derive(Default)]
pub struct DiffView {
    pub rows: Vec<DiffRow>,
    pub hunks: Vec<usize>,
    pub patches: Vec<String>,
    pub source: Option<(String, String)>,
    pub additions: usize,
    pub deletions: usize,
    pub language: Option<Language>,
    pub number_width: usize,
}

impl DiffView {
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
        let old_lines = highlighter.lines(path, &old)?;
        let new_lines = highlighter.lines(path, &new)?;
        let diff = TextDiff::configure()
            .algorithm(Algorithm::Myers)
            .timeout(Duration::from_millis(500))
            .diff_lines(&old, &new);
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

impl DiffRow {
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
    }
}
