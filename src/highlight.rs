use std::{collections::HashMap, ops::Range, path::Path};

use anyhow::{Context, Result};
use ratatui::{
    style::{Color, Style},
    text::Span,
};
use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};
use unicode_width::UnicodeWidthChar;

use crate::git::safe_text;

pub const TEXT: Color = Color::Rgb(212, 212, 212);
pub const ADD_WORD_BG: Color = Color::Rgb(24, 70, 32);
pub const DEL_WORD_BG: Color = Color::Rgb(84, 24, 30);
const CAPTURES: &[&str] = &[
    "attribute",
    "boolean",
    "comment",
    "constant",
    "constructor",
    "embedded",
    "escape",
    "function",
    "keyword",
    "label",
    "number",
    "operator",
    "property",
    "punctuation",
    "string",
    "tag",
    "type",
    "variable",
    "variable.builtin",
];
const COLORS: &[Color] = &[
    Color::Rgb(229, 192, 123),
    Color::Rgb(255, 158, 100),
    Color::Rgb(115, 130, 155),
    Color::Rgb(255, 158, 100),
    Color::Rgb(125, 207, 255),
    TEXT,
    Color::Rgb(187, 154, 247),
    Color::Rgb(122, 162, 247),
    Color::Rgb(187, 154, 247),
    Color::Rgb(224, 175, 104),
    Color::Rgb(255, 158, 100),
    Color::Rgb(137, 221, 255),
    Color::Rgb(125, 207, 255),
    Color::Rgb(169, 177, 214),
    Color::Rgb(158, 206, 106),
    Color::Rgb(247, 118, 142),
    Color::Rgb(125, 207, 255),
    TEXT,
    Color::Rgb(247, 118, 142),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Language {
    Rust,
    Python,
    JavaScript,
    Jsx,
    TypeScript,
    Tsx,
    Go,
    C,
    Cpp,
    Json,
    Html,
    Css,
    Bash,
}

impl Language {
    pub fn detect(path: &Path) -> Option<Self> {
        let name = path.file_name()?.to_str()?;
        if matches!(name, ".bashrc" | ".bash_profile" | ".profile") {
            return Some(Self::Bash);
        }
        Some(
            match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
                "rs" => Self::Rust,
                "py" | "pyi" | "pyw" => Self::Python,
                "js" | "mjs" | "cjs" => Self::JavaScript,
                "jsx" => Self::Jsx,
                "ts" | "mts" | "cts" => Self::TypeScript,
                "tsx" => Self::Tsx,
                "go" => Self::Go,
                "c" | "h" => Self::C,
                "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" => Self::Cpp,
                "json" | "jsonc" => Self::Json,
                "html" | "htm" => Self::Html,
                "css" => Self::Css,
                "sh" | "bash" => Self::Bash,
                _ => return None,
            },
        )
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Rust => "Rust",
            Self::Python => "Python",
            Self::JavaScript => "JavaScript",
            Self::Jsx => "JSX",
            Self::TypeScript => "TypeScript",
            Self::Tsx => "TSX",
            Self::Go => "Go",
            Self::C => "C",
            Self::Cpp => "C++",
            Self::Json => "JSON",
            Self::Html => "HTML",
            Self::Css => "CSS",
            Self::Bash => "Bash",
        }
    }

    fn configuration(self) -> Result<HighlightConfiguration> {
        let js = || tree_sitter_javascript::HIGHLIGHT_QUERY.to_owned();
        let jsx = || format!("{}\n{}", js(), tree_sitter_javascript::JSX_HIGHLIGHT_QUERY);
        let ts = || format!("{}\n{}", js(), tree_sitter_typescript::HIGHLIGHTS_QUERY);
        let (language, query) = match self {
            Self::Rust => (
                tree_sitter_rust::LANGUAGE,
                tree_sitter_rust::HIGHLIGHTS_QUERY.to_owned(),
            ),
            Self::Python => (
                tree_sitter_python::LANGUAGE,
                tree_sitter_python::HIGHLIGHTS_QUERY.to_owned(),
            ),
            Self::JavaScript => (tree_sitter_javascript::LANGUAGE, js()),
            Self::Jsx => (tree_sitter_javascript::LANGUAGE, jsx()),
            Self::TypeScript => (tree_sitter_typescript::LANGUAGE_TYPESCRIPT, ts()),
            Self::Tsx => (
                tree_sitter_typescript::LANGUAGE_TSX,
                format!("{}\n{}", jsx(), tree_sitter_typescript::HIGHLIGHTS_QUERY),
            ),
            Self::Go => (
                tree_sitter_go::LANGUAGE,
                tree_sitter_go::HIGHLIGHTS_QUERY.to_owned(),
            ),
            Self::C => (
                tree_sitter_c::LANGUAGE,
                tree_sitter_c::HIGHLIGHT_QUERY.to_owned(),
            ),
            Self::Cpp => (
                tree_sitter_cpp::LANGUAGE,
                format!(
                    "{}\n{}",
                    tree_sitter_c::HIGHLIGHT_QUERY,
                    tree_sitter_cpp::HIGHLIGHT_QUERY
                ),
            ),
            Self::Json => (
                tree_sitter_json::LANGUAGE,
                tree_sitter_json::HIGHLIGHTS_QUERY.to_owned(),
            ),
            Self::Html => (
                tree_sitter_html::LANGUAGE,
                tree_sitter_html::HIGHLIGHTS_QUERY.to_owned(),
            ),
            Self::Css => (
                tree_sitter_css::LANGUAGE,
                tree_sitter_css::HIGHLIGHTS_QUERY.to_owned(),
            ),
            Self::Bash => (
                tree_sitter_bash::LANGUAGE,
                tree_sitter_bash::HIGHLIGHT_QUERY.to_owned(),
            ),
        };
        let mut config = HighlightConfiguration::new(language.into(), self.name(), &query, "", "")
            .with_context(|| format!("Could not configure {} highlighting", self.name()))?;
        config.configure(CAPTURES);
        Ok(config)
    }
}

#[derive(Default)]
pub struct SyntaxHighlighter {
    configs: HashMap<Language, HighlightConfiguration>,
    engine: Highlighter,
}

impl SyntaxHighlighter {
    /// Parse complete revisions, not isolated diff lines, to preserve multiline scopes.
    /// Emphasis ranges are sorted, non-overlapping byte ranges in the original source.
    pub fn lines(
        &mut self,
        path: &Path,
        source: &str,
        emphasis: &[Range<usize>],
        emphasis_background: Color,
    ) -> Result<Vec<Vec<Span<'static>>>> {
        let mut builder = LineBuilder::new(source, emphasis, emphasis_background);
        let Some(language) = Language::detect(path) else {
            builder.source(0, source.len(), TEXT);
            return Ok(builder.finish());
        };
        if let std::collections::hash_map::Entry::Vacant(entry) = self.configs.entry(language) {
            entry.insert(language.configuration()?);
        }
        let events =
            self.engine
                .highlight(&self.configs[&language], source.as_bytes(), None, |_| None)?;
        let mut stack = Vec::new();
        for event in events {
            match event? {
                HighlightEvent::HighlightStart(highlight) => stack.push(COLORS[highlight.0]),
                HighlightEvent::HighlightEnd => {
                    stack.pop();
                }
                HighlightEvent::Source { start, end } => {
                    builder.source(start, end, stack.last().copied().unwrap_or(TEXT));
                }
            }
        }
        Ok(builder.finish())
    }
}

struct LineBuilder<'a> {
    source: &'a str,
    emphasis: &'a [Range<usize>],
    emphasis_background: Color,
    emphasis_index: usize,
    lines: Vec<Vec<Span<'static>>>,
    column: usize,
}

impl<'a> LineBuilder<'a> {
    fn new(source: &'a str, emphasis: &'a [Range<usize>], emphasis_background: Color) -> Self {
        Self {
            source,
            emphasis,
            emphasis_background,
            emphasis_index: 0,
            lines: vec![Vec::new()],
            column: 0,
        }
    }

    fn source(&mut self, start: usize, end: usize, color: Color) {
        let mut text = String::new();
        let base = Style::default().fg(color);
        let mut style = base;
        for (offset, ch) in self.source[start..end].char_indices() {
            let position = start + offset;
            while self
                .emphasis
                .get(self.emphasis_index)
                .is_some_and(|range| range.end <= position)
            {
                self.emphasis_index += 1;
            }
            let next_style = if self
                .emphasis
                .get(self.emphasis_index)
                .is_some_and(|range| range.contains(&position))
            {
                base.bg(self.emphasis_background)
            } else {
                base
            };
            if style != next_style {
                self.push(std::mem::take(&mut text), style);
                style = next_style;
            }
            match ch {
                '\n' => {
                    self.push(std::mem::take(&mut text), style);
                    self.lines.push(Vec::new());
                    self.column = 0;
                }
                '\t' => {
                    let spaces = 4 - self.column % 4;
                    text.push_str(&" ".repeat(spaces));
                    self.column += spaces;
                }
                '\r' if self.source.as_bytes().get(start + offset + 1) == Some(&b'\n') => {}
                ch if ch.is_control() => {
                    let escaped = safe_text(&ch.to_string());
                    self.column += escaped.len();
                    text.push_str(&escaped);
                }
                ch => {
                    text.push(ch);
                    self.column += ch.width().unwrap_or(0);
                }
            }
        }
        self.push(text, style);
    }

    fn push(&mut self, text: String, style: Style) {
        if !text.is_empty() {
            self.lines
                .last_mut()
                .unwrap()
                .push(Span::styled(text, style));
        }
    }

    fn finish(mut self) -> Vec<Vec<Span<'static>>> {
        if self.source.is_empty() || self.source.ends_with('\n') {
            self.lines.pop();
        }
        self.lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_grammar_and_query_compiles() {
        for language in [
            Language::Rust,
            Language::Python,
            Language::JavaScript,
            Language::Jsx,
            Language::TypeScript,
            Language::Tsx,
            Language::Go,
            Language::C,
            Language::Cpp,
            Language::Json,
            Language::Html,
            Language::Css,
            Language::Bash,
        ] {
            language.configuration().unwrap();
        }
    }

    #[test]
    fn multiline_comments_keep_their_scope() {
        let source = "/* begin\n  still a comment\n*/\nfn main() {}\n";
        let lines = SyntaxHighlighter::default()
            .lines(Path::new("test.rs"), source, &[], Color::Reset)
            .unwrap();
        assert_eq!(lines.len(), 4);
        assert!(lines[1].iter().all(|span| span.style.fg == Some(COLORS[2])));
        assert!(lines[3].iter().any(|span| span.style.fg == Some(COLORS[8])));
    }

    #[test]
    fn plain_text_is_safe_and_tabs_align() {
        let lines = SyntaxHighlighter::default()
            .lines(
                Path::new("test.txt"),
                "a\tb\r\n界\tx\x1b",
                &[],
                Color::Reset,
            )
            .unwrap();
        let text: Vec<String> = lines
            .iter()
            .map(|line| line.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert_eq!(text, ["a   b", "界  x\\u{1b}"]);
    }

    #[test]
    fn raw_emphasis_ranges_follow_tab_expansion_controls_and_crlf() {
        let lines = SyntaxHighlighter::default()
            .lines(
                Path::new("test.txt"),
                "a\t界\x1b\r\nb\té\n",
                &[1..6, 9..12],
                ADD_WORD_BG,
            )
            .unwrap();
        let text = |line: &[Span<'_>], background: Option<Color>| {
            line.iter()
                .filter(|span| span.style.bg == background)
                .map(|span| span.content.as_ref())
                .collect::<String>()
        };
        assert_eq!(text(&lines[0], None), "a");
        assert_eq!(text(&lines[0], Some(ADD_WORD_BG)), "   界\\u{1b}");
        assert_eq!(text(&lines[1], None), "b");
        assert_eq!(text(&lines[1], Some(ADD_WORD_BG)), "   é");
        assert!(
            lines
                .iter()
                .flatten()
                .all(|span| span.style.fg == Some(TEXT))
        );
    }

    #[test]
    fn emphasis_crosses_syntax_captures_without_changing_foregrounds() {
        let source = "let value = 42;\n";
        let mut highlighter = SyntaxHighlighter::default();
        let plain = highlighter
            .lines(Path::new("test.rs"), source, &[], Color::Reset)
            .unwrap();
        let emphasized = highlighter
            .lines(
                Path::new("test.rs"),
                source,
                std::slice::from_ref(&(0..14)),
                ADD_WORD_BG,
            )
            .unwrap();
        let characters = |lines: Vec<Vec<Span<'_>>>| {
            lines
                .into_iter()
                .flatten()
                .flat_map(|span| {
                    span.content
                        .chars()
                        .map(|ch| (ch, span.style.fg, span.style.bg))
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        };
        let plain = characters(plain);
        let emphasized = characters(emphasized);
        assert_eq!(plain.len(), emphasized.len());
        for (index, ((ch, fg, _), emphasized)) in plain.into_iter().zip(emphasized).enumerate() {
            assert_eq!(
                emphasized,
                (ch, fg, if index < 14 { Some(ADD_WORD_BG) } else { None })
            );
        }
    }

    #[test]
    fn trailing_newlines_do_not_add_phantom_lines() {
        for (source, count) in [("", 0), ("\n", 1), ("a\n\n", 2), ("a", 1)] {
            assert_eq!(
                SyntaxHighlighter::default()
                    .lines(Path::new("x.rs"), source, &[], Color::Reset)
                    .unwrap()
                    .len(),
                count
            );
        }
    }
}
