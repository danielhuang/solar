use annotate_snippets::renderer::DecorStyle;

use crate::ast::SourceSpan;
use std::collections::HashMap;
use std::fmt;

/// Maps file_id → (filename, source text) for multi-file error reporting.
#[derive(Debug, Clone, Default)]
pub struct SourceMap {
    files: HashMap<u32, (String, String)>,
    /// The entry (root) file. Its definitions are NOT module-mangled — they keep
    /// their bare source names (so `main` stays `main`). Set by `resolve`.
    root_file_id: Option<u32>,
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

    pub fn set_root_file_id(&mut self, file_id: u32) {
        self.root_file_id = Some(file_id);
    }

    pub fn root_file_id(&self) -> Option<u32> {
        self.root_file_id
    }

    /// The module-mangling prefix for a file's definitions — the piece formerly
    /// produced by `resolve::module_prefix`, now applied here at the
    /// `mangled_ast` stage. Empty for the root file (bare names). Otherwise
    /// `__mod{len}_{name}`, where `name` is normally the file's sanitized
    /// basename.
    ///
    /// The basename alone is NOT unique: a program can import two files with
    /// the same name from different directories (`a/Thing.solar` and
    /// `b/Thing.solar`, or the 315 `package-info.java` units of a Java port).
    /// Those used to mangle to the same prefix, silently merging both files'
    /// definitions into one module — a wrong-symbol miscompile, not an error.
    /// So when a basename is shared, enough leading directory components are
    /// folded into the name to disambiguate it (`a_Thing` vs `b_Thing`), and
    /// the file id is the last-resort tiebreaker. Files with a unique basename
    /// keep exactly the symbols they had before.
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
        // Two files with identical full paths cannot both be in the map, but a
        // relative and an absolute spelling of one file could still tie here.
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
