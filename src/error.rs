use annotate_snippets::renderer::DecorStyle;

use crate::ast::SourceSpan;
use std::collections::HashMap;
use std::fmt;

/// Source text and paths indexed by compiler file identifier.
#[derive(Debug, Clone, Default)]
pub struct SourceMap {
    files: HashMap<u32, (String, String)>,
    /// The entry file, whose definitions keep their bare names.
    root_file_id: Option<u32>,
}

impl SourceMap {
    /// Creates an empty source map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds or replaces a source file.
    pub fn add_file(&mut self, file_id: u32, filename: String, source: String) {
        self.files.insert(file_id, (filename, source));
    }

    /// Returns a file's path and source text.
    pub fn get(&self, file_id: u32) -> Option<(&str, &str)> {
        self.files
            .get(&file_id)
            .map(|(f, s)| (f.as_str(), s.as_str()))
    }

    /// Marks a file as the compilation root.
    pub fn set_root_file_id(&mut self, file_id: u32) {
        self.root_file_id = Some(file_id);
    }

    /// Returns the compilation root's file identifier.
    pub fn root_file_id(&self) -> Option<u32> {
        self.root_file_id
    }

    /// Finds the source-map identifier for a filesystem path.
    pub fn file_id_for_path(&self, path: &std::path::Path) -> Option<u32> {
        let requested = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.files.iter().find_map(|(&id, (filename, _))| {
            let candidate = std::path::Path::new(filename);
            let candidate = candidate
                .canonicalize()
                .unwrap_or_else(|_| candidate.to_path_buf());
            (candidate == requested).then_some(id)
        })
    }

    /// Returns a unique symbol prefix for a non-root file.
    ///
    /// Colliding basenames are disambiguated with parent path components.
    pub fn module_prefix(&self, file_id: u32) -> String {
        if self.root_file_id == Some(file_id) {
            return String::new();
        }
        let Some((path, _)) = self.files.get(&file_id) else {
            return "__mod0_".to_string();
        };
        let name = self.unique_module_name(file_id, path);
        format!("__mod{}_{}", name.len(), name)
    }

    /// The shortest sanitized path suffix (`stem`, then `dir_stem`, then
    /// `dir_dir_stem`, …) that no other file in this program shares.
    fn unique_module_name(&self, file_id: u32, path: &str) -> String {
        let depth = path_components(path).len();
        for take in 1..=depth {
            let candidate = path_suffix_name(path, take);
            let collides = self.files.iter().any(|(&other_id, (other_path, _))| {
                other_id != file_id
                    && self.root_file_id != Some(other_id)
                    && path_suffix_name(other_path, take) == candidate
            });
            if !collides {
                return candidate;
            }
        }
        format!("{}_{}", path_suffix_name(path, 1), file_id)
    }
}

/// A path split into components, with the final component's extension dropped.
fn path_components(path: &str) -> Vec<String> {
    let p = std::path::Path::new(path);
    let mut comps: Vec<String> = p
        .parent()
        .map(|parent| {
            parent
                .components()
                .filter_map(|c| match c {
                    std::path::Component::Normal(s) => Some(s.to_string_lossy().to_string()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    comps.push(
        p.file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default(),
    );
    comps
}

/// The last `take` path components, sanitized to identifier characters and
/// joined with `_` (e.g. `take = 2` over `a/b/Thing.solar` is `b_Thing`).
fn path_suffix_name(path: &str, take: usize) -> String {
    let comps = path_components(path);
    let start = comps.len().saturating_sub(take);
    let joined = comps[start..].join("_");
    joined
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// A secondary diagnostic annotation.
#[derive(Debug, Clone)]
pub struct Label {
    /// Annotation text.
    pub message: String,
    /// Annotated source span.
    pub span: SourceSpan,
}

/// A compiler diagnostic with a primary span and optional annotations.
#[derive(Debug, Clone)]
pub struct CompileError {
    /// Primary error text.
    pub message: String,
    /// Primary source span.
    pub span: SourceSpan,
    /// Secondary annotations.
    pub labels: Vec<Label>,
    /// The lower-level diagnostic that caused this error, if any.
    pub caused_by: Option<Box<CompileError>>,
}

impl CompileError {
    /// Creates a diagnostic without secondary annotations.
    pub fn new(message: String, span: SourceSpan) -> Self {
        Self {
            message,
            span,
            labels: vec![],
            caused_by: None,
        }
    }

    /// Adds a secondary annotation.
    pub fn with_label(mut self, message: impl Into<String>, span: SourceSpan) -> Self {
        self.labels.push(Label {
            message: message.into(),
            span,
        });
        self
    }

    /// Attaches the lower-level diagnostic that caused this error.
    pub fn with_cause(mut self, caused_by: CompileError) -> Self {
        self.caused_by = Some(Box::new(caused_by));
        self
    }
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}: {}",
            self.span.start.line + 1,
            self.span.start.col + 1,
            self.message
        )
    }
}

/// Build a table of byte offsets for the start of each line in `source`.
fn line_offsets(source: &str) -> Vec<usize> {
    let mut offsets = vec![0];
    for (i, b) in source.bytes().enumerate() {
        if b == b'\n' {
            offsets.push(i + 1);
        }
    }
    offsets
}

/// Convert a SourceSpan (line/col) to a byte range in the source string.
fn span_to_byte_range(
    span: &SourceSpan,
    offsets: &[usize],
    source: &str,
) -> std::ops::Range<usize> {
    let start_line = span.start.line as usize;
    let end_line = span.end.line as usize;
    let start = if start_line < offsets.len() {
        offsets[start_line] + span.start.col as usize
    } else {
        offsets.last().copied().unwrap_or(0)
    };
    let end = if end_line < offsets.len() {
        offsets[end_line] + span.end.col as usize
    } else {
        offsets.last().copied().unwrap_or(0)
    };
    let mut start = start.min(source.len());
    while start > 0 && !source.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = end.min(source.len());
    while end < source.len() && !source.is_char_boundary(end) {
        end += 1;
    }
    if start >= end {
        let mut next = (start + 1).min(source.len());
        while next < source.len() && !source.is_char_boundary(next) {
            next += 1;
        }
        start..next
    } else {
        start..end
    }
}

fn render_single_error(err: &CompileError, source: &str, filename: &str) {
    let renderer = annotate_snippets::Renderer::styled().decor_style(DecorStyle::Unicode);
    eprintln!("{}", format_single_error(err, source, filename, &renderer));
}

fn format_single_error(
    err: &CompileError,
    source: &str,
    filename: &str,
    renderer: &annotate_snippets::Renderer,
) -> String {
    use annotate_snippets::{AnnotationKind, Group, Level, Snippet};

    let offsets = line_offsets(source);
    let range = span_to_byte_range(&err.span, &offsets, source);

    let range = range.start.min(source.len())..range.end.min(source.len());

    // Preserve each annotated span and at most two surrounding lines when folding.
    let context_range = |range: &std::ops::Range<usize>| {
        let first_line = offsets.partition_point(|&offset| offset <= range.start) - 1;
        let last_byte = range.end.saturating_sub(1).max(range.start);
        let last_line = offsets.partition_point(|&offset| offset <= last_byte) - 1;
        let start = offsets[first_line.saturating_sub(2)];
        let end = offsets
            .get(last_line + 3)
            .map_or(source.len(), |offset| offset - 1);
        start..end
    };

    let mut snippet = Snippet::source(source)
        .path(filename)
        .fold(true)
        .annotation(AnnotationKind::Visible.span(context_range(&range)))
        .annotation(AnnotationKind::Primary.span(range));

    for label in &err.labels {
        let label_range = span_to_byte_range(&label.span, &offsets, source);
        let label_range = label_range.start.min(source.len())..label_range.end.min(source.len());
        if label.span.start.line > 0
            || label.span.start.col > 0
            || label.span.end.line > 0
            || label.span.end.col > 0
        {
            snippet = snippet.annotation(AnnotationKind::Visible.span(context_range(&label_range)));
            snippet = snippet.annotation(
                AnnotationKind::Context
                    .span(label_range)
                    .label(&label.message),
            );
        }
    }

    let report: &[Group] =
        &[Group::with_title(Level::ERROR.primary_title(&err.message)).element(snippet)];

    renderer.render(report)
}

/// Render a CompileError and its causes with source context using
/// annotate-snippets. Causes are printed first, leaving the outermost error
/// closest to the command prompt.
pub fn render_error(err: &CompileError, source: &str, filename: &str) {
    if let Some(cause) = &err.caused_by {
        render_error(cause, source, filename);
    }
    render_single_error(err, source, filename);
}

/// Render a CompileError and its causes using a SourceMap to look up each
/// diagnostic's source file.
pub fn render_error_with_source_map(err: &CompileError, source_map: &SourceMap) {
    if let Some(cause) = &err.caused_by {
        render_error_with_source_map(cause, source_map);
    }
    if let Some((filename, source)) = source_map.get(err.span.file_id) {
        render_single_error(err, source, filename);
    } else {
        eprintln!("error: {}", err.message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::SourcePos;

    fn span(start_line: u32, start_col: u32, end_line: u32, end_col: u32) -> SourceSpan {
        SourceSpan {
            start: SourcePos {
                line: start_line,
                col: start_col,
            },
            end: SourcePos {
                line: end_line,
                col: end_col,
            },
            file_id: 0,
        }
    }

    fn render(err: &CompileError, source: &str) -> String {
        format_single_error(
            err,
            source,
            "example.solar",
            &annotate_snippets::Renderer::plain().decor_style(DecorStyle::Unicode),
        )
    }

    fn assert_excerpt(err: &CompileError, expected: &[usize], trailing_newline: bool) -> String {
        let mut source = (1..=20)
            .map(|line| format!("source line {line:02}\n"))
            .collect::<String>();
        if !trailing_newline {
            source.pop();
        }
        let output = render(err, &source);
        let visible = output
            .lines()
            .filter_map(|line| {
                let (prefix, number) = line.split_once("source line ")?;
                let number = number.parse::<usize>().unwrap();
                assert_eq!(
                    prefix
                        .split_whitespace()
                        .next()
                        .unwrap()
                        .parse::<usize>()
                        .unwrap(),
                    number
                );
                Some(number)
            })
            .collect::<Vec<_>>();
        assert_eq!(visible, expected, "{output}");
        output
    }

    #[test]
    fn error_excerpt_has_two_lines_of_context() {
        let err = CompileError::new("bad expression".into(), span(6, 0, 6, 6));
        let output = assert_excerpt(&err, &[5, 6, 7, 8, 9], true);
        assert!(output.contains("error: bad expression"));
        assert!(output.contains("example.solar:7:1"));
    }

    #[test]
    fn multiline_error_excerpt_preserves_all_error_lines() {
        let err = CompileError::new("bad block".into(), span(5, 0, 12, 6));
        assert_excerpt(&err, &(4..=15).collect::<Vec<_>>(), true);
    }

    #[test]
    fn error_excerpt_clamps_context_to_file_boundaries() {
        for trailing_newline in [false, true] {
            let first = CompileError::new("first line".into(), span(0, 0, 0, 6));
            assert_excerpt(&first, &[1, 2, 3], trailing_newline);
            let last = CompileError::new("last line".into(), span(19, 0, 19, 6));
            assert_excerpt(&last, &[18, 19, 20], trailing_newline);
        }
    }

    #[test]
    fn distant_label_gets_its_own_context() {
        let err = CompileError::new("bad use".into(), span(3, 0, 3, 6))
            .with_label("defined here", span(15, 0, 15, 6));
        let output = assert_excerpt(&err, &[2, 3, 4, 5, 6, 14, 15, 16, 17, 18], true);
        assert!(output.contains("defined here"));
    }

    #[test]
    fn overlapping_context_is_shown_once() {
        let err = CompileError::new("bad use".into(), span(6, 0, 6, 6))
            .with_label("nearby definition", span(8, 0, 8, 6));
        assert_excerpt(&err, &[5, 6, 7, 8, 9, 10, 11], true);
    }

    #[test]
    fn empty_source_and_eof_spans_render() {
        let err = CompileError::new("unexpected EOF".into(), span(0, 0, 0, 0));
        assert!(render(&err, "").contains("unexpected EOF"));
        let err = CompileError::new("unexpected EOF".into(), span(20, 0, 20, 0));
        assert_excerpt(&err, &[19, 20], true);
        let err = CompileError::new("unexpected EOF".into(), span(19, 14, 19, 14));
        assert_excerpt(&err, &[18, 19, 20], false);
    }
}
