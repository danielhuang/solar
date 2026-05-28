use annotate_snippets::renderer::DecorStyle;

use crate::ast::SourceSpan;
use std::collections::HashMap;
use std::fmt;

/// Maps file_id → (filename, source text) for multi-file error reporting.
#[derive(Debug, Clone, Default)]
pub struct SourceMap {
    files: HashMap<u32, (String, String)>,
}

impl SourceMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_file(&mut self, file_id: u32, filename: String, source: String) {
        self.files.insert(file_id, (filename, source));
    }

    pub fn get(&self, file_id: u32) -> Option<(&str, &str)> {
        self.files
            .get(&file_id)
            .map(|(f, s)| (f.as_str(), s.as_str()))
    }
}

#[derive(Debug, Clone)]
pub struct Label {
    pub message: String,
    pub span: SourceSpan,
}

#[derive(Debug, Clone)]
pub struct CompileError {
    pub message: String,
    pub span: SourceSpan,
    pub labels: Vec<Label>,
}

impl CompileError {
    pub fn new(message: String, span: SourceSpan) -> Self {
        Self {
            message,
            span,
            labels: vec![],
        }
    }

    pub fn with_label(mut self, message: impl Into<String>, span: SourceSpan) -> Self {
        self.labels.push(Label {
            message: message.into(),
            span,
        });
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
fn span_to_byte_range(span: &SourceSpan, offsets: &[usize]) -> std::ops::Range<usize> {
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
    // Ensure at least 1-char range so the annotation is visible
    if start >= end {
        start..start + 1
    } else {
        start..end
    }
}

/// Render a CompileError with source context using annotate-snippets.
pub fn render_error(err: &CompileError, source: &str, filename: &str) {
    use annotate_snippets::{AnnotationKind, Group, Level, Renderer, Snippet};

    let offsets = line_offsets(source);
    let range = span_to_byte_range(&err.span, &offsets);

    // Clamp range to source length
    let range = range.start.min(source.len())..range.end.min(source.len());

    let mut snippet = Snippet::source(source)
        .path(filename)
        .fold(false)
        .annotation(AnnotationKind::Primary.span(range));

    for label in &err.labels {
        let label_range = span_to_byte_range(&label.span, &offsets);
        let label_range = label_range.start.min(source.len())..label_range.end.min(source.len());
        // Only add labels with non-default spans (line > 0 or col > 0)
        if label.span.start.line > 0
            || label.span.start.col > 0
            || label.span.end.line > 0
            || label.span.end.col > 0
        {
            snippet = snippet.annotation(
                AnnotationKind::Context
                    .span(label_range)
                    .label(&label.message),
            );
        }
    }

    let report: &[Group] =
        &[Group::with_title(Level::ERROR.primary_title(&err.message)).element(snippet)];

    let renderer = Renderer::styled().decor_style(DecorStyle::Unicode);
    eprintln!("{}", renderer.render(report));
}

/// Render a CompileError using a SourceMap to look up the correct file.
pub fn render_error_with_source_map(err: &CompileError, source_map: &SourceMap) {
    if let Some((filename, source)) = source_map.get(err.span.file_id) {
        render_error(err, source, filename);
    } else {
        // Fallback: just print the message
        eprintln!("error: {}", err.message);
    }
}
