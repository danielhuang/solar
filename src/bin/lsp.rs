//! Language Server Protocol support for Solar.

use serde_json::{Value, json};
use solar::{
    ast::{self, SourceSpan},
    error::{CompileError, SourceMap},
    resolve, typed_ast,
};
use std::{
    collections::{HashMap, HashSet},
    io::{self, BufRead, Write},
    path::PathBuf,
};
use tree_sitter::{Node, Parser};

const TOKEN_TYPES: &[&str] = &[
    "comment",
    "string",
    "number",
    "keyword",
    "operator",
    "function",
    "method",
    "type",
    "typeParameter",
    "enumMember",
    "property",
    "parameter",
    "variable",
    "namespace",
    "decorator",
];

fn main() {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    let mut documents = HashMap::<String, String>::new();
    // Per-document analysis, replaced whenever the buffer changes. Resolving
    // reparses the stdlib, so diagnostics, navigation, hover, and semantic
    // tokens share one resolve and type-check per document revision.
    let mut cache = HashMap::<String, Document>::new();
    // Root document URI → every URI to which its last check published
    // diagnostics. Used to clear errors that disappear after an edit.
    let mut diagnostic_uris = HashMap::<String, HashMap<String, Vec<Value>>>::new();

    while let Some(message) = read_message(&mut input) {
        let method = message.get("method").and_then(Value::as_str);
        let id = message.get("id").cloned();
        let params = message.get("params").cloned().unwrap_or(Value::Null);

        match method {
            Some("initialize") => respond(
                &mut output,
                id,
                json!({
                    "capabilities": {
                        "textDocumentSync": 1,
                        "hoverProvider": true,
                        "definitionProvider": true,
                        "semanticTokensProvider": {
                            "legend": { "tokenTypes": TOKEN_TYPES, "tokenModifiers": [] },
                            "full": true,
                            "range": false
                        }
                    },
                    "serverInfo": { "name": "solar-lsp", "version": env!("CARGO_PKG_VERSION") }
                }),
            ),
            Some("shutdown") => respond(&mut output, id, Value::Null),
            Some("exit") => break,
            Some("textDocument/didOpen") => {
                if let Some((uri, text)) = document_and_text(&params) {
                    let (document, diagnostics) = compute_with_diagnostics(&uri, &text);
                    cache.insert(uri.clone(), document);
                    documents.insert(uri.clone(), text.clone());
                    publish_check_diagnostics(&mut output, &uri, diagnostics, &mut diagnostic_uris);
                }
            }
            Some("textDocument/didChange") => {
                let uri = params.pointer("/textDocument/uri").and_then(Value::as_str);
                let text = params
                    .pointer("/contentChanges/0/text")
                    .and_then(Value::as_str);
                if let (Some(uri), Some(text)) = (uri, text) {
                    let (document, diagnostics) = compute_with_diagnostics(uri, text);
                    cache.insert(uri.to_owned(), document);
                    documents.insert(uri.to_owned(), text.to_owned());
                    publish_check_diagnostics(&mut output, uri, diagnostics, &mut diagnostic_uris);
                }
            }
            Some("textDocument/didClose") => {
                if let Some(uri) = params.pointer("/textDocument/uri").and_then(Value::as_str) {
                    documents.remove(uri);
                    cache.remove(uri);
                    clear_check_diagnostics(&mut output, uri, &mut diagnostic_uris);
                }
            }
            Some("textDocument/semanticTokens/full") => {
                let uri = params.pointer("/textDocument/uri").and_then(Value::as_str);
                let data = match uri.and_then(|uri| documents.get(uri).map(|text| (uri, text))) {
                    Some((uri, text)) => {
                        let document = cached(&mut cache, uri, text);
                        semantic_tokens(text, document.analysis.as_ref())
                    }
                    None => Vec::new(),
                };
                respond(&mut output, id, json!({ "data": data }));
            }
            Some("textDocument/hover") => {
                let uri = params.pointer("/textDocument/uri").and_then(Value::as_str);
                let line = params.pointer("/position/line").and_then(Value::as_u64);
                let character = params
                    .pointer("/position/character")
                    .and_then(Value::as_u64);
                let target = uri.and_then(|uri| documents.get(uri).map(|text| (uri, text)));
                let result = match (target, line, character) {
                    (Some((uri, text)), Some(line), Some(character)) => {
                        let document = cached(&mut cache, uri, text);
                        hover(text, line as u32, character as u32, document)
                    }
                    _ => None,
                };
                respond(&mut output, id, result.unwrap_or(Value::Null));
            }
            Some("textDocument/definition") => {
                let uri = params.pointer("/textDocument/uri").and_then(Value::as_str);
                let line = params.pointer("/position/line").and_then(Value::as_u64);
                let character = params
                    .pointer("/position/character")
                    .and_then(Value::as_u64);
                let target = uri.and_then(|uri| documents.get(uri).map(|text| (uri, text)));
                let result = match (target, line, character) {
                    (Some((uri, text)), Some(line), Some(character)) => {
                        let document = cached(&mut cache, uri, text);
                        definition(text, line as u32, character as u32, document)
                    }
                    _ => None,
                };
                respond(&mut output, id, result.unwrap_or(Value::Null));
            }
            _ => {
                // Notifications and methods outside this server's deliberately
                // small surface are harmless. Return MethodNotFound only for a
                // request, as required by JSON-RPC.
                if let Some(id) = id {
                    write_message(
                        &mut output,
                        &json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": "method not found" } }),
                    );
                }
            }
        }
    }
}

fn document_and_text(params: &Value) -> Option<(String, String)> {
    Some((
        params.pointer("/textDocument/uri")?.as_str()?.to_owned(),
        params.pointer("/textDocument/text")?.as_str()?.to_owned(),
    ))
}

fn publish_check_diagnostics(
    output: &mut impl Write,
    root_uri: &str,
    diagnostics: HashMap<String, Vec<Value>>,
    published: &mut HashMap<String, HashMap<String, Vec<Value>>>,
) {
    let mut affected: HashSet<String> = published
        .get(root_uri)
        .into_iter()
        .flat_map(|old| old.keys().cloned())
        .chain(diagnostics.keys().cloned())
        .collect();
    affected.insert(root_uri.to_owned());
    published.insert(root_uri.to_owned(), diagnostics);

    for uri in affected {
        publish_aggregate_diagnostics(output, &uri, published);
    }
}

fn clear_check_diagnostics(
    output: &mut impl Write,
    root_uri: &str,
    published: &mut HashMap<String, HashMap<String, Vec<Value>>>,
) {
    let mut affected: HashSet<String> = published
        .remove(root_uri)
        .into_iter()
        .flat_map(|old| old.into_keys())
        .collect();
    affected.insert(root_uri.to_owned());
    for uri in affected {
        publish_aggregate_diagnostics(output, &uri, published);
    }
}

fn publish_aggregate_diagnostics(
    output: &mut impl Write,
    uri: &str,
    published: &HashMap<String, HashMap<String, Vec<Value>>>,
) {
    let diagnostics: Vec<Value> = published
        .values()
        .filter_map(|by_uri| by_uri.get(uri))
        .flatten()
        .cloned()
        .collect();
    publish_diagnostics(output, uri, &diagnostics);
}

fn publish_diagnostics(output: &mut impl Write, uri: &str, diagnostics: &[Value]) {
    write_message(
        output,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": { "uri": uri, "diagnostics": diagnostics },
        }),
    );
}

/// Compile the in-memory document as the program root and return LSP diagnostics
/// grouped by the source file they point into.
#[cfg(test)]
fn check_document(root_uri: &str, source: &str) -> HashMap<String, Vec<Value>> {
    compute_with_diagnostics(root_uri, source).1
}

fn diagnostics_from_errors(
    root_uri: &str,
    root_source: &str,
    errors: &[CompileError],
    source_map: &SourceMap,
) -> HashMap<String, Vec<Value>> {
    let mut diagnostics = HashMap::<String, Vec<Value>>::new();
    let root_path = file_uri_to_path(root_uri).map(|path| path.canonicalize().unwrap_or(path));
    for error in errors {
        // Parsing can fail before resolve_source has recorded root_file_id, so
        // also recognize the root by its canonical filename in the SourceMap.
        let mapped_source = source_map.get(error.span.file_id);
        let is_root = source_map.root_file_id() == Some(error.span.file_id)
            || mapped_source.is_some_and(|(filename, _)| {
                let path = PathBuf::from(filename);
                let path = path.canonicalize().unwrap_or(path);
                root_path.as_ref() == Some(&path)
            });
        let (uri, file_source) = if is_root {
            (root_uri.to_owned(), root_source)
        } else if let Some((filename, source)) = mapped_source {
            (path_to_file_uri(filename), source)
        } else {
            (root_uri.to_owned(), root_source)
        };
        diagnostics
            .entry(uri)
            .or_default()
            .push(compile_error_diagnostic(error, file_source));
    }
    diagnostics
}

fn compile_error_diagnostic(error: &CompileError, source: &str) -> Value {
    let position = |pos: ast::SourcePos| {
        let line = source.lines().nth(pos.line as usize).unwrap_or("");
        json!({
            "line": pos.line,
            "character": utf16_column(line, pos.col as usize),
        })
    };
    json!({
        "range": {
            "start": position(error.span.start),
            "end": position(error.span.end),
        },
        "severity": 1,
        "source": "solar",
        "message": error.message,
    })
}

fn respond(output: &mut impl Write, id: Option<Value>, result: Value) {
    if let Some(id) = id {
        write_message(
            output,
            &json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        );
    }
}

fn read_message(input: &mut impl BufRead) -> Option<Value> {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        if input.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            content_length = value.trim().parse::<usize>().ok();
        }
    }
    let mut bytes = vec![0; content_length?];
    input.read_exact(&mut bytes).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn write_message(output: &mut impl Write, value: &Value) {
    let body = serde_json::to_vec(value).expect("serializing LSP response");
    write!(output, "Content-Length: {}\r\n\r\n", body.len()).expect("writing LSP header");
    output.write_all(&body).expect("writing LSP response");
    output.flush().expect("flushing LSP response");
}

#[derive(Clone, Copy)]
struct Token {
    line: u32,
    start: u32,
    length: u32,
    kind: u32,
}

fn semantic_tokens(source: &str, analysis: Option<&Analysis>) -> Vec<u32> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_solar::LANGUAGE.into())
        .expect("Solar grammar must load");
    let Some(tree) = parser.parse(source, None) else {
        return vec![];
    };

    // The cached resolve feeds the name tables (so every reference to a global
    // type/variant is coloured the same, wherever it appears) and the
    // per-expression overlays (so a call is a function, a field read a property,
    // etc.). Any parse/resolve/type error simply leaves the CST's
    // error-tolerant classification in place.

    // Type-parameter names are gathered straight from the syntax tree (they
    // survive no further than monomorphization, so type checking can't report
    // them) and colour a `T` the same at its `#[T]` declaration and every use.
    let mut type_params = HashSet::new();
    collect_type_params(tree.root_node(), source, &mut type_params);

    let context = Context {
        names: analysis.map(|analysis| &analysis.names),
        type_params: &type_params,
    };

    let mut tokens = Vec::new();
    collect_tokens(tree.root_node(), source, &context, &mut tokens);
    if let Some(analysis) = analysis {
        apply_typed_overlays(&analysis.typed, analysis.file_id, source, &mut tokens);
    }
    tokens.sort_by_key(|token| (token.line, token.start));

    let mut data = Vec::with_capacity(tokens.len() * 5);
    let (mut previous_line, mut previous_start) = (0, 0);
    for token in tokens {
        let line_delta = token.line - previous_line;
        let start_delta = if line_delta == 0 {
            token.start - previous_start
        } else {
            token.start
        };
        data.extend([line_delta, start_delta, token.length, token.kind, 0]);
        previous_line = token.line;
        previous_start = token.start;
    }
    data
}

/// Returns hover information for the symbol under the cursor.
fn hover(source: &str, line: u32, character: u32, document: &Document) -> Option<Value> {
    let mut entries = Vec::new();
    let mut seen = HashSet::new();
    for target in symbol_targets(source, line, character, document) {
        if !seen.insert(span_key(target)) {
            continue;
        }
        let signature = document
            .signatures
            .get(&span_key(target))
            .cloned()
            .or_else(|| span_source_text(target, &document.source_map));
        let doc = document.docs.get(&span_key(target));
        let Some(signature) = signature else {
            continue;
        };
        let mut entry = format!("```solar\n{signature}\n```");
        if let Some(doc) = doc {
            entry.push_str("\n\n");
            entry.push_str(doc);
        }
        entries.push(entry);
    }
    if entries.is_empty() {
        return None;
    }
    let value = entries.join("\n\n---\n\n");
    Some(json!({
        "contents": {
            "kind": "markdown",
            "value": value,
        }
    }))
}

/// Indexes documentation by declaration span.
type SpanKey = (u32, u32, u32, u32, u32);

fn span_key(span: SourceSpan) -> SpanKey {
    (
        span.file_id,
        span.start.line,
        span.start.col,
        span.end.line,
        span.end.col,
    )
}

fn collect_docs(ast: &ast::SourceFile) -> HashMap<SpanKey, String> {
    use solar::ast::TopLevelItem;
    let mut docs = HashMap::new();
    for item in &ast.items {
        let (span, doc) = match item {
            TopLevelItem::Function(function) | TopLevelItem::Method(function) => {
                (function.span, &function.doc)
            }
            TopLevelItem::Struct(def) => (def.span, &def.doc),
            TopLevelItem::Enum(def) => (def.span, &def.doc),
            TopLevelItem::Const(def) => (def.span, &def.doc),
            TopLevelItem::Static(def) => (def.span, &def.doc),
            TopLevelItem::TypeAlias(def) => (def.span, &def.doc),
            TopLevelItem::Import(_) => continue,
        };
        if let Some(doc) = doc {
            docs.insert(span_key(span), doc.clone());
        }
    }
    docs
}

fn collect_signatures(ast: &ast::SourceFile, source_map: &SourceMap) -> HashMap<SpanKey, String> {
    use solar::ast::TopLevelItem;
    let mut signatures = HashMap::new();
    for item in &ast.items {
        match item {
            TopLevelItem::Function(def) | TopLevelItem::Method(def) => {
                insert_signature(
                    &mut signatures,
                    def.span,
                    source_map,
                    SignatureShape::Header,
                );
            }
            TopLevelItem::Struct(def) => {
                insert_signature(
                    &mut signatures,
                    def.span,
                    source_map,
                    SignatureShape::Header,
                );
                for field in &def.fields {
                    insert_signature(
                        &mut signatures,
                        field.span,
                        source_map,
                        SignatureShape::Full,
                    );
                }
            }
            TopLevelItem::Enum(def) => {
                insert_signature(
                    &mut signatures,
                    def.span,
                    source_map,
                    SignatureShape::Header,
                );
                for variant in &def.variants {
                    insert_signature(
                        &mut signatures,
                        variant.span,
                        source_map,
                        SignatureShape::Full,
                    );
                }
            }
            TopLevelItem::Const(def) => {
                insert_signature(&mut signatures, def.span, source_map, SignatureShape::Full)
            }
            TopLevelItem::Static(def) => {
                insert_signature(&mut signatures, def.span, source_map, SignatureShape::Full)
            }
            TopLevelItem::TypeAlias(def) => {
                insert_signature(&mut signatures, def.span, source_map, SignatureShape::Full)
            }
            TopLevelItem::Import(_) => {}
        }
    }
    signatures
}

#[derive(Clone, Copy)]
enum SignatureShape {
    Header,
    Full,
}

fn insert_signature(
    signatures: &mut HashMap<SpanKey, String>,
    span: SourceSpan,
    source_map: &SourceMap,
    shape: SignatureShape,
) {
    let Some(mut text) = span_source_text(span, source_map) else {
        return;
    };
    if matches!(shape, SignatureShape::Header)
        && let Some(body) = text.find('{')
    {
        text.truncate(body);
    }
    let text = text.trim().trim_end_matches(',').trim().to_owned();
    if !text.is_empty() {
        signatures.insert(span_key(span), text);
    }
}

fn span_source_text(span: SourceSpan, source_map: &SourceMap) -> Option<String> {
    let (_, source) = source_map.get(span.file_id)?;
    let offsets = source_line_offsets(source);
    let start = offsets.get(span.start.line as usize)? + span.start.col as usize;
    let end = offsets.get(span.end.line as usize)? + span.end.col as usize;
    source.get(start..end).map(str::to_owned)
}

fn source_line_offsets(source: &str) -> Vec<usize> {
    let mut offsets = vec![0];
    for (byte, ch) in source.char_indices() {
        if ch == '\n' {
            offsets.push(byte + 1);
        }
    }
    offsets
}

/// Converts an LSP UTF-16 position to a byte offset.
fn position_to_byte(source: &str, line: u32, character: u32) -> Option<usize> {
    let mut offset = 0;
    for (index, text) in source.split_inclusive('\n').enumerate() {
        if index as u32 == line {
            let mut utf16 = 0;
            for (byte, ch) in text.char_indices() {
                if utf16 >= character {
                    return Some(offset + byte);
                }
                utf16 += ch.len_utf16() as u32;
            }
            return Some(offset + text.len());
        }
        offset += text.len();
    }
    None
}

/// Returns definitions for the symbol under the cursor.
fn definition(source: &str, line: u32, character: u32, document: &Document) -> Option<Value> {
    let targets = symbol_targets(source, line, character, document);
    let mut locations = Vec::new();
    for span in targets {
        if let Some(location) = span_to_location(span, &document.source_map) {
            locations.push(location);
        }
    }
    match locations.len() {
        0 => None,
        1 => locations.pop(),
        _ => Some(Value::Array(locations)),
    }
}

/// Resolves a cursor position to declaration spans.
fn symbol_targets(source: &str, line: u32, character: u32, document: &Document) -> Vec<SourceSpan> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_solar::LANGUAGE.into())
        .expect("Solar grammar must load");
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };
    let Some(byte) = position_to_byte(source, line, character) else {
        return Vec::new();
    };
    let Some(node) = tree.root_node().descendant_for_byte_range(byte, byte) else {
        return Vec::new();
    };
    if node.kind() != "identifier" {
        return Vec::new();
    }
    let name = &source[node.byte_range()];
    let start = node.start_position();
    let cursor = (start.row as u32, start.column as u32);

    if let Some(parent) = declaration_parent(node)
        && let Some(file_id) = document.source_map.root_file_id()
    {
        return vec![node_span(parent, file_id)];
    }
    if let Some(binding) = local_definition(node, name, source)
        && let Some(file_id) = document.source_map.root_file_id()
    {
        let target = node_span(binding, file_id);
        return vec![target];
    }
    if let Some(target) = type_definition(node, name, source, document) {
        return vec![target];
    }
    if let Some(target) = path_definition(node, name, source, document) {
        return vec![target];
    }

    let anchor = definition_anchor(node);
    let generic_site = document
        .generic_bodies
        .iter()
        .any(|span| span_contains(*span, document.source_map.root_file_id(), cursor));

    // Precise pass: resolve the specific overload(s) via the typed AST. Only
    // calls in functions defined in this file can sit at the cursor, so the walk
    // is restricted to them (which also prunes the entire stdlib).
    let mut targets: Vec<SourceSpan> = Vec::new();
    if let Some(analysis) = &document.analysis {
        let mut finder = DefFinder {
            typed: &analysis.typed,
            root_file: analysis.file_id,
            cursor,
            name,
            anchor,
            generic_site,
            function_defs: &document.function_defs,
            method_defs: &document.method_defs,
            field_defs: &document.field_defs,
            variant_defs: &document.variant_defs,
            type_defs: &document.type_defs,
            global_defs: &document.global_defs,
            field_init: node.parent().is_some_and(|parent| {
                parent.kind() == "field_init" && parent.child_by_field_name("name") == Some(node)
            }),
            out: &mut targets,
        };
        for function in analysis.typed.functions.values() {
            if function.def_span.file_id == analysis.file_id {
                for statement in &function.body {
                    finder.walk_statement(statement);
                }
            }
        }
        for static_item in &analysis.typed.statics {
            finder.walk_expr(&static_item.init);
        }
    }

    let mut seen = HashSet::new();
    targets.retain(|span| seen.insert(span_key(*span)));
    targets
}

fn declaration_parent(node: Node<'_>) -> Option<Node<'_>> {
    let parent = node.parent()?;
    matches!(
        parent.kind(),
        "function_def"
            | "method_def"
            | "struct_def"
            | "enum_def"
            | "type_alias_def"
            | "const_def"
            | "static_def"
    )
    .then(|| parent.child_by_field_name("name"))
    .flatten()
    .filter(|name| *name == node)
    .map(|_| parent)
}

/// Resolves a lexical identifier to its nearest visible binding.
fn local_definition<'a>(node: Node<'a>, name: &str, source: &str) -> Option<Node<'a>> {
    if is_binding_identifier(node) {
        return Some(node);
    }

    let mut child = node;
    while let Some(parent) = child.parent() {
        match parent.kind() {
            "block" => {
                let mut found = None;
                let mut cursor = parent.walk();
                for statement in parent.named_children(&mut cursor) {
                    if statement.start_byte() >= child.start_byte() {
                        break;
                    }
                    if let Some(binding) = statement_binding(statement, name, source) {
                        found = Some(binding);
                    }
                }
                if found.is_some() {
                    return found;
                }
            }
            "function_def" | "method_def" => {
                if let Some(parameters) = named_child(parent, "parameter_list") {
                    let mut cursor = parameters.walk();
                    for parameter in parameters
                        .named_children(&mut cursor)
                        .filter(|child| child.kind() == "parameter")
                    {
                        if let Some(pattern) = parameter.child_by_field_name("pattern")
                            && let Some(binding) = descendant_binding(pattern, name, source)
                        {
                            return Some(binding);
                        }
                    }
                }
            }
            "closure_expr" => {
                let mut cursor = parent.walk();
                for parameter in parent
                    .named_children(&mut cursor)
                    .filter(|child| child.kind() == "closure_param")
                {
                    if let Some(binding) = parameter.child_by_field_name("name")
                        && &source[binding.byte_range()] == name
                    {
                        return Some(binding);
                    }
                }
            }
            "for_statement" => {
                if let Some(body) = parent.child_by_field_name("body")
                    && body.start_byte() <= node.start_byte()
                    && node.end_byte() <= body.end_byte()
                    && let Some(binding) = parent.child_by_field_name("variable")
                    && &source[binding.byte_range()] == name
                {
                    return Some(binding);
                }
            }
            "try_statement" => {
                if let Some(handler) = parent.child_by_field_name("handler")
                    && handler.start_byte() <= node.start_byte()
                    && node.end_byte() <= handler.end_byte()
                    && let Some(binding) = parent.child_by_field_name("binding")
                    && &source[binding.byte_range()] == name
                {
                    return Some(binding);
                }
            }
            "match_arm" => {
                if let Some(body) = parent.child_by_field_name("body")
                    && body.start_byte() <= node.start_byte()
                    && node.end_byte() <= body.end_byte()
                    && let Some(pattern) = parent.child_by_field_name("pattern")
                    && let Some(binding) = match_binding(pattern, name, source)
                {
                    return Some(binding);
                }
            }
            _ => {}
        }
        child = parent;
    }
    None
}

fn path_definition(
    node: Node<'_>,
    name: &str,
    source: &str,
    document: &Document,
) -> Option<SourceSpan> {
    let path = node
        .parent()
        .filter(|parent| parent.kind() == "path_segment")?
        .parent()
        .filter(|parent| {
            matches!(
                parent.kind(),
                "path_expr" | "variant_pattern" | "unit_variant_pattern"
            )
        })?;
    let mut cursor = path.walk();
    let segments: Vec<Node<'_>> = path
        .named_children(&mut cursor)
        .filter(|child| child.kind() == "path_segment")
        .filter_map(|segment| segment.child_by_field_name("name"))
        .collect();
    let index = segments.iter().position(|segment| *segment == node)?;

    // The final segment of `Enum::Variant` names a variant. Resolve the owner
    // first so common names such as `None` do not jump to another enum.
    if index + 1 == segments.len() && index > 0 {
        let owner_name = &source[segments[index - 1].byte_range()];
        let owner_file = if index > 1 {
            let module = &source[segments[index - 2].byte_range()];
            document.module_files.get(module).copied()
        } else {
            document.source_map.root_file_id()
        };
        let owners: Vec<&ast::DefId> = document
            .type_defs
            .keys()
            .filter(|id| id.name == owner_name && Some(id.file) == owner_file)
            .collect();
        if owners.len() == 1
            && let Some(span) = document
                .variant_defs
                .get(&(owners[0].clone(), name.to_owned()))
        {
            return Some(*span);
        }
    }

    let expected_file = if index > 0 {
        let module = &source[segments[index - 1].byte_range()];
        document.module_files.get(module).copied()
    } else {
        document.source_map.root_file_id()
    };
    document
        .type_defs
        .iter()
        .find_map(|(id, span)| (id.name == name && Some(id.file) == expected_file).then_some(*span))
}

fn type_definition(
    node: Node<'_>,
    name: &str,
    source: &str,
    document: &Document,
) -> Option<SourceSpan> {
    let parent = node.parent()?;
    let file = match parent.kind() {
        "named_type" => document.source_map.root_file_id(),
        "qualified_type" if parent.child_by_field_name("name") == Some(node) => {
            let module = parent.child_by_field_name("module")?;
            document
                .module_files
                .get(&source[module.byte_range()])
                .copied()
        }
        _ => return None,
    };
    document
        .type_defs
        .iter()
        .find_map(|(id, span)| (id.name == name && Some(id.file) == file).then_some(*span))
}

fn statement_binding<'a>(statement: Node<'a>, name: &str, source: &str) -> Option<Node<'a>> {
    match statement.kind() {
        "let_statement" => {
            descendant_binding(statement.child_by_field_name("pattern")?, name, source)
        }
        "function_def" | "const_def" => {
            let binding = statement.child_by_field_name("name")?;
            (&source[binding.byte_range()] == name).then_some(binding)
        }
        _ => None,
    }
}

fn descendant_binding<'a>(pattern: Node<'a>, name: &str, source: &str) -> Option<Node<'a>> {
    if pattern.kind() == "identifier" && &source[pattern.byte_range()] == name {
        return Some(pattern);
    }
    let mut cursor = pattern.walk();
    for child in pattern.named_children(&mut cursor) {
        if let Some(binding) = descendant_binding(child, name, source) {
            return Some(binding);
        }
    }
    None
}

fn match_binding<'a>(pattern: Node<'a>, name: &str, source: &str) -> Option<Node<'a>> {
    match pattern.kind() {
        "variant_pattern" => {
            let binding = pattern.child_by_field_name("binding")?;
            (&source[binding.byte_range()] == name).then_some(binding)
        }
        "wildcard_pattern" => {
            let binding = pattern.child_by_field_name("name")?;
            (&source[binding.byte_range()] == name).then_some(binding)
        }
        _ => None,
    }
}

fn is_binding_identifier(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    matches!(
        parent.kind(),
        "parameter" | "closure_param" | "let_statement"
    ) && (parent.child_by_field_name("name") == Some(node)
        || parent.child_by_field_name("pattern") == Some(node))
        || matches!(parent.kind(), "for_statement" | "try_statement")
            && (parent.child_by_field_name("variable") == Some(node)
                || parent.child_by_field_name("binding") == Some(node))
        || matches!(parent.kind(), "variant_pattern" | "wildcard_pattern")
            && (parent.child_by_field_name("binding") == Some(node)
                || parent.child_by_field_name("name") == Some(node))
}

fn named_child<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|child| child.kind() == kind)
}

fn node_span(node: Node<'_>, file_id: u32) -> SourceSpan {
    let start = node.start_position();
    let end = node.end_position();
    SourceSpan {
        start: ast::SourcePos {
            line: start.row as u32,
            col: start.column as u32,
        },
        end: ast::SourcePos {
            line: end.row as u32,
            col: end.column as u32,
        },
        file_id,
    }
}

/// Finds call targets at a source position.
struct DefFinder<'a> {
    typed: &'a typed_ast::SourceFile,
    root_file: u32,
    cursor: (u32, u32),
    name: &'a str,
    anchor: Option<(u32, u32)>,
    generic_site: bool,
    function_defs: &'a HashMap<ast::DefId, Vec<SourceSpan>>,
    method_defs: &'a HashMap<String, Vec<SourceSpan>>,
    field_defs: &'a HashMap<(ast::DefId, String), SourceSpan>,
    variant_defs: &'a HashMap<(ast::DefId, String), SourceSpan>,
    type_defs: &'a HashMap<ast::DefId, SourceSpan>,
    global_defs: &'a HashMap<ast::DefId, SourceSpan>,
    field_init: bool,
    out: &'a mut Vec<SourceSpan>,
}

impl DefFinder<'_> {
    fn record(&mut self, function: &typed_ast::FuncId) {
        if self.generic_site {
            let candidates = if function.method {
                self.method_defs.get(&function.def.name)
            } else {
                self.function_defs.get(&function.def)
            };
            if let Some(candidates) = candidates {
                self.out.extend(candidates.iter().copied());
                return;
            }
        }
        if let Some(def) = self.typed.functions.get(function) {
            self.out.push(def.def_span);
        }
    }

    fn at(&self, span: SourceSpan, position: (u32, u32)) -> bool {
        span.file_id == self.root_file && (span.start.line, span.start.col) == position
    }

    fn walk_statement(&mut self, statement: &typed_ast::Statement) {
        use typed_ast::StatementKind;
        match &statement.kind {
            StatementKind::Let { value, .. }
            | StatementKind::Expression(value)
            | StatementKind::Return(value) => self.walk_expr(value),
            StatementKind::Assignment { target, value } => {
                self.walk_expr(target);
                self.walk_expr(value);
            }
            StatementKind::If {
                condition,
                body,
                else_body,
            } => {
                self.walk_expr(condition);
                for statement in body.iter().chain(else_body) {
                    self.walk_statement(statement);
                }
            }
            StatementKind::While { condition, body } => {
                self.walk_expr(condition);
                for statement in body {
                    self.walk_statement(statement);
                }
            }
            StatementKind::Break(value) => {
                if let Some(value) = value {
                    self.walk_expr(value);
                }
            }
            StatementKind::Continue => {}
        }
    }

    fn walk_expr(&mut self, expr: &typed_ast::Expr) {
        use typed_ast::ExprKind;
        match &expr.kind {
            ExprKind::FunctionRef(function) => {
                if self.at(expr.span, self.anchor.unwrap_or(self.cursor))
                    && function.def.name == self.name
                {
                    self.record(function);
                }
            }
            ExprKind::Call {
                function,
                arguments,
            } => {
                if self.at(expr.span, self.anchor.unwrap_or(self.cursor))
                    && function.def.name == self.name
                {
                    self.record(function);
                }
                for argument in arguments {
                    self.walk_expr(argument);
                }
            }
            ExprKind::CallIndirect { callee, arguments } => {
                self.walk_expr(callee);
                for argument in arguments {
                    self.walk_expr(argument);
                }
            }
            ExprKind::FieldAccess { object, field } => {
                if field == self.name
                    && self.at(expr.span, self.anchor.unwrap_or(self.cursor))
                    && let Some(owner) = struct_owner(&object.ty)
                    && let Some(span) = self.field_defs.get(&(owner, field.clone()))
                {
                    self.out.push(*span);
                }
                self.walk_expr(object);
            }
            ExprKind::Deref(object)
            | ExprKind::Reference(object)
            | ExprKind::Unique(object)
            | ExprKind::Not(object)
            | ExprKind::ArraySizeCoerce { expr: object, .. } => self.walk_expr(object),
            ExprKind::Index { object, index } => {
                self.walk_expr(object);
                self.walk_expr(index);
            }
            ExprKind::Slice { object, start, end } => {
                self.walk_expr(object);
                self.walk_expr(start);
                self.walk_expr(end);
            }
            ExprKind::StructLiteral { id, fields } => {
                if self.field_init
                    && fields.iter().any(|field| field.name == self.name)
                    && span_contains(expr.span, Some(self.root_file), self.cursor)
                    && let Some(span) = self.field_defs.get(&(id.def.clone(), self.name.to_owned()))
                {
                    self.out.push(*span);
                }
                if id.def.name == self.name
                    && self.at(expr.span, self.anchor.unwrap_or(self.cursor))
                    && let Some(span) = self.type_defs.get(&id.def)
                {
                    self.out.push(*span);
                }
                for field in fields {
                    self.walk_expr(&field.value);
                }
            }
            ExprKind::ArrayLiteral(values) => {
                for value in values {
                    self.walk_expr(value);
                }
            }
            ExprKind::Block(statements) | ExprKind::Loop(statements) => {
                for statement in statements {
                    self.walk_statement(statement);
                }
            }
            ExprKind::ArrayRepeat { element, count }
            | ExprKind::ArrayInit {
                init: element,
                count,
            } => {
                self.walk_expr(element);
                self.walk_expr(count);
            }
            ExprKind::BinaryOp { left, right, .. } => {
                self.walk_expr(left);
                self.walk_expr(right);
            }
            ExprKind::If {
                condition,
                then_body,
                else_body,
            } => {
                self.walk_expr(condition);
                for statement in then_body.iter().chain(else_body) {
                    self.walk_statement(statement);
                }
            }
            ExprKind::EnumVariant {
                enum_id,
                variant_name,
                value,
                ..
            } => {
                if self.at(expr.span, self.anchor.unwrap_or(self.cursor)) {
                    if variant_name == self.name
                        && let Some(span) = self
                            .variant_defs
                            .get(&(enum_id.def.clone(), variant_name.clone()))
                    {
                        self.out.push(*span);
                    } else if enum_id.def.name == self.name
                        && let Some(span) = self.type_defs.get(&enum_id.def)
                    {
                        self.out.push(*span);
                    }
                }
                if let Some(value) = value {
                    self.walk_expr(value);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                self.walk_expr(scrutinee);
                for arm in arms {
                    for statement in &arm.body {
                        self.walk_statement(statement);
                    }
                }
            }
            ExprKind::IntrinsicCall { arguments, .. } => {
                for argument in arguments {
                    self.walk_expr(argument);
                }
            }
            ExprKind::Global(id) => {
                if id.name == self.name
                    && self.at(expr.span, self.anchor.unwrap_or(self.cursor))
                    && let Some(span) = self.global_defs.get(id)
                {
                    self.out.push(*span);
                }
            }
            ExprKind::Identifier(_)
            | ExprKind::FloatLiteral(_)
            | ExprKind::IntegerLiteral(_)
            | ExprKind::BooleanLiteral(_)
            | ExprKind::NullLiteral
            | ExprKind::Closure { .. } => {}
        }
    }
}

/// Returns the identifier position represented by a typed expression.
fn definition_anchor(node: Node<'_>) -> Option<(u32, u32)> {
    let mut current = node;
    while let Some(parent) = current.parent() {
        let anchor = match parent.kind() {
            "generic_method_call" if parent.child_by_field_name("method") == Some(node) => {
                parent.child_by_field_name("receiver")
            }
            "field_access" if parent.child_by_field_name("field") == Some(node) => {
                if field_access_is_callee(node) {
                    parent.child_by_field_name("object")
                } else {
                    Some(parent)
                }
            }
            "generic_call_expr" if parent.child_by_field_name("function") == Some(node) => {
                Some(parent)
            }
            "call_expr" => {
                let function = parent.child_by_field_name("function")?;
                (function.start_byte() <= node.start_byte()
                    && node.end_byte() <= function.end_byte())
                .then_some(parent)
            }
            "struct_literal" if parent.child_by_field_name("name") == Some(node) => Some(parent),
            "path_expr" => Some(parent),
            _ => None,
        };
        if let Some(anchor) = anchor {
            // A path used as a callee is represented by the surrounding call,
            // not by the path itself.
            if parent.kind() == "path_expr"
                && let Some(call) = parent.parent()
                && call.kind() == "call_expr"
                && call.child_by_field_name("function") == Some(parent)
            {
                let start = call.start_position();
                return Some((start.row as u32, start.column as u32));
            }
            let start = anchor.start_position();
            return Some((start.row as u32, start.column as u32));
        }
        current = parent;
    }
    None
}

fn span_contains(span: SourceSpan, file: Option<u32>, position: (u32, u32)) -> bool {
    if file != Some(span.file_id) {
        return false;
    }
    let start = (span.start.line, span.start.col);
    let end = (span.end.line, span.end.col);
    start <= position && position < end
}

fn struct_owner(ty: &typed_ast::Type) -> Option<ast::DefId> {
    use typed_ast::Type;
    match ty {
        Type::Struct(id) => Some(id.def.clone()),
        Type::Ref(inner)
        | Type::RefUnsized(inner)
        | Type::NullableRef(inner)
        | Type::NullableRefUnsized(inner)
        | Type::Unique(inner)
        | Type::UniqueUnsized(inner) => struct_owner(inner),
        _ => None,
    }
}

/// Converts a declaration span to an LSP location.
fn span_to_location(span: SourceSpan, source_map: &SourceMap) -> Option<Value> {
    let (filename, file_source) = source_map.get(span.file_id)?;
    let line_text = file_source
        .lines()
        .nth(span.start.line as usize)
        .unwrap_or("");
    let character = utf16_column(line_text, span.start.col as usize);
    let position = json!({ "line": span.start.line, "character": character });
    Some(json!({
        "uri": path_to_file_uri(filename),
        "range": { "start": position, "end": position },
    }))
}

/// Percent-encode a filesystem path into a `file://` URI.
fn path_to_file_uri(path: &str) -> String {
    let mut uri = String::from("file://");
    for &byte in path.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                uri.push(byte as char)
            }
            _ => uri.push_str(&format!("%{byte:02X}")),
        }
    }
    uri
}

/// Global names used by semantic highlighting.
#[derive(Default)]
struct Names {
    /// Struct and enum names → `type`.
    types: HashSet<String>,
    /// Enum variant names → `enumMember`.
    variants: HashSet<String>,
}

/// Context for classifying syntax tokens.
struct Context<'a> {
    names: Option<&'a Names>,
    type_params: &'a HashSet<String>,
}

/// Cached compiler analysis for one document revision.
#[derive(Default)]
struct Document {
    /// Exact declaration span → `///` doc across the resolved program. Hover
    /// reaches this only through the shared symbol resolver.
    docs: HashMap<SpanKey, String>,
    /// Exact declaration span → source-level signature/metadata shown by hover,
    /// including declarations without doc comments.
    signatures: HashMap<SpanKey, String>,
    /// Free-function overload declarations keyed by resolved provenance.
    function_defs: HashMap<ast::DefId, Vec<SourceSpan>>,
    /// Method overload declarations keyed by their source name.
    method_defs: HashMap<String, Vec<SourceSpan>>,
    /// Struct fields keyed by owning type and field name.
    field_defs: HashMap<(ast::DefId, String), SourceSpan>,
    /// Enum variants keyed by owning type and variant name.
    variant_defs: HashMap<(ast::DefId, String), SourceSpan>,
    /// Struct/enum declarations keyed by resolved type identity.
    type_defs: HashMap<ast::DefId, SourceSpan>,
    /// Top-level const/static declarations keyed by resolved identity.
    global_defs: HashMap<ast::DefId, SourceSpan>,
    /// Source ranges of generic functions/methods in the root file.
    generic_bodies: Vec<SourceSpan>,
    /// Module aliases declared by the root source, resolved to source-map file
    /// identities for qualified type/path navigation.
    module_files: HashMap<String, u32>,
    /// `file_id` → path + source, to turn a definition's span into an LSP
    /// `Location` (URI + UTF-16 range) in whatever file it lives in.
    source_map: SourceMap,
    /// Type-check-derived facts for semantic highlighting. `None` when the
    /// buffer resolves but does not type-check — hover's docs survive either way.
    analysis: Option<Analysis>,
}

/// Type-checker facts used by semantic highlighting.
struct Analysis {
    typed: typed_ast::SourceFile,
    file_id: u32,
    names: Names,
}

/// Returns cached analysis for a document.
fn cached<'a>(cache: &'a mut HashMap<String, Document>, uri: &str, source: &str) -> &'a Document {
    if !cache.contains_key(uri) {
        let document = compute(uri, source);
        cache.insert(uri.to_owned(), document);
    }
    &cache[uri]
}

/// Analyzes an in-memory document.
fn compute(uri: &str, source: &str) -> Document {
    compute_with_diagnostics(uri, source).0
}

/// Analyzes an in-memory document once for diagnostics and language features.
fn compute_with_diagnostics(uri: &str, source: &str) -> (Document, HashMap<String, Vec<Value>>) {
    let Some(path) = file_uri_to_path(uri) else {
        return (Document::default(), HashMap::new());
    };
    let (ast, source_map) = match resolve::resolve_source(&path, source.to_owned()) {
        Ok(resolved) => resolved,
        Err((errors, source_map)) => {
            let diagnostics = diagnostics_from_errors(uri, source, &errors, &source_map);
            return (Document::default(), diagnostics);
        }
    };
    let definition_catalog = collect_definition_catalog(&ast, source_map.root_file_id());
    let module_files = collect_module_files(&path, source, &source_map);
    let signatures = collect_signatures(&ast, &source_map);
    let (analysis, diagnostics) = match typed_ast::lower(&ast) {
        Ok(typed) => (analysis_from_typed(typed, &source_map), HashMap::new()),
        Err(error) => (
            None,
            diagnostics_from_errors(uri, source, std::slice::from_ref(&error), &source_map),
        ),
    };
    let document = Document {
        docs: collect_docs(&ast),
        signatures,
        function_defs: definition_catalog.function_defs,
        method_defs: definition_catalog.method_defs,
        field_defs: definition_catalog.field_defs,
        variant_defs: definition_catalog.variant_defs,
        type_defs: definition_catalog.type_defs,
        global_defs: definition_catalog.global_defs,
        generic_bodies: definition_catalog.generic_bodies,
        module_files,
        analysis,
        source_map,
    };
    (document, diagnostics)
}

#[derive(Default)]
struct DefinitionCatalog {
    function_defs: HashMap<ast::DefId, Vec<SourceSpan>>,
    method_defs: HashMap<String, Vec<SourceSpan>>,
    field_defs: HashMap<(ast::DefId, String), SourceSpan>,
    variant_defs: HashMap<(ast::DefId, String), SourceSpan>,
    type_defs: HashMap<ast::DefId, SourceSpan>,
    global_defs: HashMap<ast::DefId, SourceSpan>,
    generic_bodies: Vec<SourceSpan>,
}

fn collect_definition_catalog(ast: &ast::SourceFile, root_file: Option<u32>) -> DefinitionCatalog {
    use solar::ast::TopLevelItem;
    let mut out = DefinitionCatalog::default();
    for item in &ast.items {
        match item {
            TopLevelItem::Function(def) => {
                let id = ast::DefId::new(def.span.file_id, def.name.clone());
                out.function_defs.entry(id).or_default().push(def.span);
                if !def.type_params.is_empty() && root_file == Some(def.span.file_id) {
                    out.generic_bodies.push(def.span);
                }
            }
            TopLevelItem::Method(def) => {
                out.method_defs
                    .entry(def.display_name.clone())
                    .or_default()
                    .push(def.span);
                if !def.type_params.is_empty() && root_file == Some(def.span.file_id) {
                    out.generic_bodies.push(def.span);
                }
            }
            TopLevelItem::Struct(def) => {
                out.type_defs.insert(def.def_id.clone(), def.span);
                for field in &def.fields {
                    out.field_defs
                        .insert((def.def_id.clone(), field.name.clone()), field.span);
                }
            }
            TopLevelItem::Enum(def) => {
                out.type_defs.insert(def.def_id.clone(), def.span);
                for variant in &def.variants {
                    out.variant_defs
                        .insert((def.def_id.clone(), variant.name.clone()), variant.span);
                }
            }
            TopLevelItem::Const(def) => {
                out.global_defs.insert(
                    ast::DefId::new(def.span.file_id, def.name.clone()),
                    def.span,
                );
            }
            TopLevelItem::Static(def) => {
                out.global_defs.insert(
                    ast::DefId::new(def.span.file_id, def.name.clone()),
                    def.span,
                );
            }
            TopLevelItem::TypeAlias(_) | TopLevelItem::Import(_) => {}
        }
    }
    out
}

fn collect_module_files(
    root_path: &std::path::Path,
    source: &str,
    source_map: &SourceMap,
) -> HashMap<String, u32> {
    use solar::ast::{ImportKind, TopLevelItem};
    let Ok(parsed) = solar::parser::parse(source) else {
        return HashMap::new();
    };
    let base = root_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let mut modules = HashMap::new();
    for item in parsed.items {
        let TopLevelItem::Import(import) = item else {
            continue;
        };
        let ImportKind::Module(alias) = import.kind else {
            continue;
        };
        if import.path.starts_with('@') {
            continue;
        }
        if let Some(file_id) = source_map.file_id_for_path(&base.join(import.path)) {
            modules.insert(alias, file_id);
        }
    }
    modules
}

/// Builds language-feature data from a type-checked program.
fn analysis_from_typed(typed: typed_ast::SourceFile, source_map: &SourceMap) -> Option<Analysis> {
    let file_id = source_map.root_file_id()?;

    // Collect every struct/enum name and every enum variant name across the
    // whole program (types are distinctively named, so a global table poses
    // little collision risk with locals and keeps imported types coloured
    // consistently too).
    let mut names = Names::default();
    for struct_def in typed.structs.values() {
        names.types.insert(struct_def.id.def.name.clone());
    }
    for enum_def in typed.enums.values() {
        names.types.insert(enum_def.id.def.name.clone());
        for variant in &enum_def.variants {
            names.variants.insert(variant.name.clone());
        }
    }

    Some(Analysis {
        typed,
        file_id,
        names,
    })
}

/// Collects typed expression classifications.
fn apply_typed_overlays(
    typed: &typed_ast::SourceFile,
    file_id: u32,
    source: &str,
    tokens: &mut [Token],
) {
    let mut overlays = Vec::new();
    for function in typed.functions.values() {
        for statement in &function.body {
            collect_statement_overlays(statement, file_id, &mut overlays);
        }
    }
    for static_item in &typed.statics {
        collect_expr_overlays(&static_item.init, file_id, &mut overlays);
    }

    for (span, kind) in overlays {
        let line = span.start.line;
        let line_text = source.lines().nth(line as usize).unwrap_or("");
        let start = utf16_column(line_text, span.start.col as usize);
        if let Some(token) = tokens
            .iter_mut()
            .find(|token| token.line == line && token.start == start)
        {
            token.kind = kind;
        }
    }
}

fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
    let path = uri.strip_prefix("file://")?;
    let mut decoded = Vec::with_capacity(path.len());
    let bytes = path.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    let decoded = String::from_utf8(decoded).ok()?;
    #[cfg(windows)]
    let decoded = decoded.strip_prefix('/').unwrap_or(&decoded).to_owned();
    Some(PathBuf::from(decoded))
}

fn collect_statement_overlays(
    statement: &typed_ast::Statement,
    file_id: u32,
    overlays: &mut Vec<(SourceSpan, u32)>,
) {
    use typed_ast::StatementKind;
    match &statement.kind {
        StatementKind::Let { value, .. }
        | StatementKind::Expression(value)
        | StatementKind::Return(value) => collect_expr_overlays(value, file_id, overlays),
        StatementKind::Assignment { target, value } => {
            collect_expr_overlays(target, file_id, overlays);
            collect_expr_overlays(value, file_id, overlays);
        }
        StatementKind::If {
            condition,
            body,
            else_body,
        } => {
            collect_expr_overlays(condition, file_id, overlays);
            for statement in body.iter().chain(else_body) {
                collect_statement_overlays(statement, file_id, overlays);
            }
        }
        StatementKind::While { condition, body } => {
            collect_expr_overlays(condition, file_id, overlays);
            for statement in body {
                collect_statement_overlays(statement, file_id, overlays);
            }
        }
        StatementKind::Break(value) => {
            if let Some(value) = value {
                collect_expr_overlays(value, file_id, overlays);
            }
        }
        StatementKind::Continue => {}
    }
}

fn collect_expr_overlays(
    expr: &typed_ast::Expr,
    file_id: u32,
    overlays: &mut Vec<(SourceSpan, u32)>,
) {
    use typed_ast::ExprKind;
    let root_span = expr.span.file_id == file_id;
    let push = |kind, overlays: &mut Vec<(SourceSpan, u32)>| {
        if root_span {
            overlays.push((expr.span, token_index(kind)));
        }
    };
    match &expr.kind {
        ExprKind::Identifier(_) | ExprKind::Global(_) => push("variable", overlays),
        ExprKind::FunctionRef(function) => push(
            if function.method {
                "method"
            } else {
                "function"
            },
            overlays,
        ),
        ExprKind::Call {
            function,
            arguments,
        } => {
            // A direct free call begins at the callee, so its expression span
            // identifies the exact token. Method calls begin at their receiver;
            // their CST classification remains the accurate one.
            if !function.method {
                push("function", overlays);
            }
            for argument in arguments {
                collect_expr_overlays(argument, file_id, overlays);
            }
        }
        ExprKind::CallIndirect { callee, arguments } => {
            collect_expr_overlays(callee, file_id, overlays);
            for argument in arguments {
                collect_expr_overlays(argument, file_id, overlays);
            }
        }
        ExprKind::FieldAccess { object, .. }
        | ExprKind::Deref(object)
        | ExprKind::Reference(object)
        | ExprKind::Unique(object)
        | ExprKind::Not(object)
        | ExprKind::ArraySizeCoerce { expr: object, .. } => {
            collect_expr_overlays(object, file_id, overlays)
        }
        ExprKind::Index { object, index } => {
            collect_expr_overlays(object, file_id, overlays);
            collect_expr_overlays(index, file_id, overlays);
        }
        ExprKind::Slice { object, start, end } => {
            collect_expr_overlays(object, file_id, overlays);
            collect_expr_overlays(start, file_id, overlays);
            collect_expr_overlays(end, file_id, overlays);
        }
        ExprKind::StructLiteral { fields, .. } => {
            for field in fields {
                collect_expr_overlays(&field.value, file_id, overlays);
            }
        }
        ExprKind::ArrayLiteral(_) | ExprKind::Block(_) | ExprKind::Loop(_) => {
            collect_sequence_overlays(&expr.kind, file_id, overlays);
        }
        ExprKind::ArrayRepeat { element, count }
        | ExprKind::ArrayInit {
            init: element,
            count,
        } => {
            collect_expr_overlays(element, file_id, overlays);
            collect_expr_overlays(count, file_id, overlays);
        }
        ExprKind::BinaryOp { left, right, .. } => {
            collect_expr_overlays(left, file_id, overlays);
            collect_expr_overlays(right, file_id, overlays);
        }
        ExprKind::If {
            condition,
            then_body,
            else_body,
        } => {
            collect_expr_overlays(condition, file_id, overlays);
            for statement in then_body.iter().chain(else_body) {
                collect_statement_overlays(statement, file_id, overlays);
            }
        }
        ExprKind::EnumVariant { value, .. } => {
            if let Some(value) = value {
                collect_expr_overlays(value, file_id, overlays);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            collect_expr_overlays(scrutinee, file_id, overlays);
            for arm in arms {
                for statement in &arm.body {
                    collect_statement_overlays(statement, file_id, overlays);
                }
            }
        }
        ExprKind::IntrinsicCall { arguments, .. } => {
            for argument in arguments {
                collect_expr_overlays(argument, file_id, overlays);
            }
        }
        ExprKind::FloatLiteral(_)
        | ExprKind::IntegerLiteral(_)
        | ExprKind::BooleanLiteral(_)
        | ExprKind::NullLiteral
        | ExprKind::Closure { .. } => {}
    }
}

fn collect_sequence_overlays(
    kind: &typed_ast::ExprKind,
    file_id: u32,
    overlays: &mut Vec<(SourceSpan, u32)>,
) {
    match kind {
        typed_ast::ExprKind::ArrayLiteral(values) => {
            for value in values {
                collect_expr_overlays(value, file_id, overlays);
            }
        }
        typed_ast::ExprKind::Block(statements) | typed_ast::ExprKind::Loop(statements) => {
            for statement in statements {
                collect_statement_overlays(statement, file_id, overlays);
            }
        }
        _ => unreachable!("only sequence expressions call this helper"),
    }
}

/// Collects declared type-parameter names.
fn collect_type_params(node: Node<'_>, source: &str, names: &mut HashSet<String>) {
    if node.kind() == "type_params" {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "identifier" {
                names.insert(source[child.byte_range()].to_owned());
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_type_params(child, source, names);
    }
}

fn collect_tokens(node: Node<'_>, source: &str, context: &Context, tokens: &mut Vec<Token>) {
    if node.child_count() == 0 {
        if let Some(kind) = token_kind(node, source, context) {
            let start = node.start_position();
            let end = node.end_position();
            if start.row == end.row {
                let line = source.lines().nth(start.row).unwrap_or("");
                let start_char = utf16_column(line, start.column);
                let end_char = utf16_column(line, end.column);
                if end_char > start_char {
                    tokens.push(Token {
                        line: start.row as u32,
                        start: start_char,
                        length: end_char - start_char,
                        kind,
                    });
                }
            }
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_tokens(child, source, context, tokens);
    }
}

fn utf16_column(line: &str, byte_column: usize) -> u32 {
    line.get(..byte_column)
        .unwrap_or(line)
        .encode_utf16()
        .count() as u32
}

fn token_kind(node: Node<'_>, source: &str, context: &Context) -> Option<u32> {
    let kind = node.kind();
    let parent = node.parent();
    let parent_kind = parent.map_or("", |node| node.kind());
    let text = &source[node.byte_range()];

    match kind {
        "comment" | "doc_comment" => Some(token_index("comment")),
        "string_literal" | "char_literal" => Some(token_index("string")),
        "integer_literal" | "float_literal" => Some(token_index("number")),
        "boolean_literal" => Some(token_index("keyword")),
        "identifier" => Some(refine(
            identifier_kind(node, parent_kind, text),
            text,
            context,
        )),
        _ if is_keyword(text) => Some(token_index("keyword")),
        _ if is_operator(text) => Some(token_index("operator")),
        _ => None,
    }
}

/// Applies canonical highlighting to known global names.
fn refine(base: u32, text: &str, context: &Context) -> u32 {
    if base == token_index("parameter") || base == token_index("property") {
        return base;
    }
    // A declared type parameter always outranks a same-named concrete type.
    if context.type_params.contains(text) {
        return token_index("typeParameter");
    }
    if let Some(names) = context.names {
        if names.types.contains(text) {
            return token_index("type");
        }
        if names.variants.contains(text) {
            return token_index("enumMember");
        }
    }
    base
}

fn token_index(name: &str) -> u32 {
    TOKEN_TYPES
        .iter()
        .position(|&kind| kind == name)
        .expect("known semantic token type") as u32
}

fn identifier_kind(node: Node<'_>, parent: &str, text: &str) -> u32 {
    let field = node
        .parent()
        .and_then(|parent| {
            (0..parent.child_count())
                .find(|&i| parent.child(i) == Some(node))
                .map(|i| parent.field_name_for_child(i as u32))
        })
        .flatten();
    match (parent, field) {
        ("function_def", Some("name")) => token_index("function"),
        ("method_def", Some("name")) => token_index("method"),
        ("struct_def" | "enum_def" | "type_alias_def", Some("name")) => token_index("type"),
        ("field_def" | "field_init" | "struct_pattern_field", Some("name" | "field_name")) => {
            token_index("property")
        }
        ("variant_def", Some("name")) => token_index("enumMember"),
        ("const_def", Some("name")) => token_index("variable"),
        (
            "parameter" | "closure_param" | "fn_type_param" | "argument",
            Some("pattern" | "name"),
        ) => token_index("parameter"),
        ("call_expr" | "generic_call_expr", Some("function")) => token_index("function"),
        ("generic_method_call", Some("method")) => token_index("method"),
        // `x.foo()` is a method call (the field access is a call's callee);
        // a bare `x.foo` reads a field, so it is a property.
        ("field_access", Some("field")) => {
            if field_access_is_callee(node) {
                token_index("method")
            } else {
                token_index("property")
            }
        }
        // A `named_type`'s only identifier child is the type name (its
        // `type_args` are nested type nodes), so it needs no field guard —
        // this is what colours primitives like `Int` as a type, consistently
        // with user structs and enums.
        ("named_type", _) | ("qualified_type", Some("name")) => token_index("type"),
        ("qualified_type", Some("module")) | ("import_statement", Some("module_name")) => {
            token_index("namespace")
        }
        ("type_params", _) => token_index("typeParameter"),
        // A struct literal / pattern names a type, so colour it as one (a
        // known type is confirmed by the name table; this is the fallback for
        // types the type-checker never saw).
        ("struct_literal" | "struct_pattern", Some("name")) => token_index("type"),
        ("struct_literal" | "struct_pattern", Some("module")) | ("import_path", _) => {
            token_index("namespace")
        }
        ("path_segment", Some("name")) => path_identifier_kind(node, text),
        _ => token_index("variable"),
    }
}

/// True when `node` is the `field` of a `field_access` that is itself the
/// callee of a `call_expr` — i.e. `x.foo()`, a method call — rather than a
/// bare field read `x.foo`.
fn field_access_is_callee(node: Node<'_>) -> bool {
    let Some(field_access) = node.parent() else {
        return false;
    };
    field_access.parent().is_some_and(|call| {
        call.kind() == "call_expr" && call.child_by_field_name("function") == Some(field_access)
    })
}

fn path_identifier_kind(node: Node<'_>, text: &str) -> u32 {
    let Some(segment) = node.parent() else {
        return token_index("variable");
    };
    let Some(path) = segment.parent() else {
        return token_index("variable");
    };
    let mut cursor = path.walk();
    let segments: Vec<_> = path
        .children(&mut cursor)
        .filter(|child| child.kind() == "path_segment")
        .collect();
    if segments.last().is_some_and(|last| *last == segment) {
        if path
            .parent()
            .is_some_and(|parent| parent.kind() == "call_expr")
        {
            token_index("function")
        } else if text.chars().next().is_some_and(char::is_uppercase) {
            token_index("enumMember")
        } else {
            token_index("variable")
        }
    } else {
        token_index("namespace")
    }
}

fn is_keyword(text: &str) -> bool {
    matches!(
        text,
        "struct"
            | "enum"
            | "type"
            | "const"
            | "static"
            | "pub"
            | "fn"
            | "method"
            | "let"
            | "import"
            | "from"
            | "if"
            | "else"
            | "match"
            | "while"
            | "for"
            | "loop"
            | "in"
            | "return"
            | "break"
            | "continue"
            | "try"
            | "catch"
            | "reflect"
            | "reflect_fields"
            | "reflect_fields_pair"
            | "reflect_variant"
            | "reflect_variant_pair"
            | "null"
    )
}

fn is_operator(text: &str) -> bool {
    matches!(
        text,
        "=" | "->"
            | "=>"
            | ".."
            | "@"
            | "&"
            | "^"
            | "?"
            | "\\"
            | "!"
            | "+"
            | "-"
            | "*"
            | "/"
            | "%"
            | "=="
            | "!="
            | "<"
            | "<="
            | ">"
            | ">="
            | "&&"
            | "||"
            | "|"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_document(relative: &str) -> (String, String, Document) {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
        let uri = format!("file://{}", path.display());
        let source = std::fs::read_to_string(&path).unwrap();
        let document = compute(&uri, &source);
        (uri, source, document)
    }

    fn occurrence_position(source: &str, needle: &str, occurrence: usize) -> (u32, u32) {
        let byte = source
            .match_indices(needle)
            .nth(occurrence)
            .map(|(byte, _)| byte)
            .unwrap();
        let prefix = &source[..byte];
        let line = prefix.bytes().filter(|byte| *byte == b'\n').count() as u32;
        let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
        let character = source[line_start..byte].encode_utf16().count() as u32;
        (line, character)
    }

    fn definition_locations(value: Value) -> Vec<Value> {
        match value {
            Value::Array(values) => values,
            value => vec![value],
        }
    }

    #[test]
    fn unknown_qualified_intrinsic_falls_back_without_panicking() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/multi_file/bad_qualified_intrinsic/main.solar");
        let uri = format!("file://{}", path.display());
        let source = r#"
import intrinsics from "@intrinsics";

fn main() {
    intrinsics::does_not_exist();
}
"#;

        let document = compute(&uri, source);

        assert!(document.analysis.is_none());
        assert!(document.docs.is_empty());
        assert!(document.signatures.is_empty());
    }

    #[test]
    fn check_document_reports_root_type_errors() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/multi_file/bad_qualified_intrinsic/main.solar");
        let uri = format!("file://{}", path.display());
        let diagnostics = check_document(
            &uri,
            r#"
fn main() {
    let value: Int = true;
}
"#,
        );

        let root = diagnostics.get(&uri).expect("root diagnostics");
        assert_eq!(root.len(), 1);
        assert_eq!(root[0]["severity"], 1);
        assert_eq!(root[0]["source"], "solar");
        assert!(
            root[0]["message"]
                .as_str()
                .unwrap()
                .contains("expected Int, got Bool")
        );
    }

    #[test]
    fn check_document_reports_root_parse_errors() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/multi_file/bad_qualified_intrinsic/main.solar");
        let uri = format!("file://{}", path.display());

        let diagnostics = check_document(&uri, "fn main( {");

        assert!(
            diagnostics
                .get(&uri)
                .is_some_and(|errors| !errors.is_empty())
        );
    }

    #[test]
    fn diagnostics_analysis_resolves_mutex_unlock() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/compile_only/mutex.solar");
        let uri = format!("file://{}", path.display());
        let source = std::fs::read_to_string(path).unwrap();

        let (document, diagnostics) = compute_with_diagnostics(&uri, &source);

        assert!(diagnostics.is_empty());
        let (line, character) = occurrence_position(&source, "unlock", 0);
        let location = definition(&source, line, character, &document)
            .expect("definition from diagnostics analysis");
        assert!(
            location["uri"]
                .as_str()
                .unwrap()
                .ends_with("/src/std/sync.solar")
        );
        assert_eq!(location["range"]["start"]["line"], 91);
    }

    #[test]
    fn diagnostic_columns_are_utf16() {
        let error = CompileError::new(
            "test".to_owned(),
            SourceSpan {
                start: ast::SourcePos { line: 0, col: 2 },
                end: ast::SourcePos { line: 0, col: 3 },
                file_id: 0,
            },
        );

        let diagnostic = compile_error_diagnostic(&error, "éx");

        assert_eq!(diagnostic["range"]["start"]["character"], 1);
        assert_eq!(diagnostic["range"]["end"]["character"], 2);
    }

    #[test]
    fn definition_resolves_imported_function_type_and_field() {
        let (_, source, document) = fixture_document("tests/multi_file/module_import/main.solar");

        for (needle, occurrence, target_line) in [("origin", 0, 5), ("Point", 0, 0), ("x", 0, 1)] {
            let (line, character) = occurrence_position(&source, needle, occurrence);
            let locations =
                definition(&source, line, character, &document).expect("definition location");
            let locations = definition_locations(locations);
            assert_eq!(locations.len(), 1, "{needle}");
            assert!(
                locations[0]["uri"]
                    .as_str()
                    .unwrap()
                    .ends_with("/module_import/lib.solar"),
                "{needle}: {}",
                locations[0]
            );
            assert_eq!(
                locations[0]["range"]["start"]["line"], target_line,
                "{needle}"
            );
        }
    }

    #[test]
    fn definition_selects_concrete_generic_overload() {
        let (_, source, document) =
            fixture_document("tests/multi_file/generic_overload_type_args/main.solar");

        for (occurrence, target_line) in [(1, 6), (2, 11)] {
            let (line, character) = occurrence_position(&source, "make", occurrence);
            let location =
                definition(&source, line, character, &document).expect("overload definition");
            let locations = definition_locations(location);
            assert_eq!(locations.len(), 1);
            assert_eq!(locations[0]["range"]["start"]["line"], target_line);
        }
    }

    #[test]
    fn definition_in_generic_body_returns_all_possible_overloads() {
        let (_, source, document) =
            fixture_document("tests/multi_file/lsp_generic_definition/main.solar");
        let (line, character) = occurrence_position(&source, "choose", 2);

        let locations = definition(&source, line, character, &document)
            .map(definition_locations)
            .expect("generic overload candidates");
        let mut lines: Vec<u64> = locations
            .iter()
            .map(|location| location["range"]["start"]["line"].as_u64().unwrap())
            .collect();
        lines.sort_unstable();
        lines.dedup();

        assert_eq!(lines, vec![3, 4]);
    }

    #[test]
    fn definition_distinguishes_free_function_from_same_named_method() {
        let (_, source, document) = fixture_document("tests/runtime/methods.solar");

        for (occurrence, target_line) in [(5, 41), (7, 8)] {
            let (line, character) = occurrence_position(&source, "double", occurrence);
            let location =
                definition(&source, line, character, &document).expect("double definition");
            let locations = definition_locations(location);
            assert_eq!(locations.len(), 1);
            assert_eq!(locations[0]["range"]["start"]["line"], target_line);
        }
    }

    #[test]
    fn definition_resolves_enum_constructor_and_match_variant() {
        let (_, source, document) = fixture_document("tests/runtime/enums.solar");

        // `Shape::Circle` in the first match arm: Shape resolves to the enum and
        // Circle resolves to the variant declaration.
        for (needle, occurrence, target_line) in [("Shape", 3, 0), ("Circle", 1, 1)] {
            let (line, character) = occurrence_position(&source, needle, occurrence);
            let location =
                definition(&source, line, character, &document).expect("enum definition");
            let locations = definition_locations(location);
            assert_eq!(locations.len(), 1, "{needle}");
            assert_eq!(locations[0]["range"]["start"]["line"], target_line);
        }
    }

    #[test]
    fn definition_resolves_shadowed_local_bindings() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/runtime/methods.solar");
        let uri = format!("file://{}", path.display());
        let source = r#"fn use_value(x: Int) {
    let before = x;
    if true {
        let x = 2;
        println(x);
    }
    println(x);
}

fn main() { use_value(1); }
"#;
        let document = compute(&uri, source);

        for (occurrence, target_line, target_character) in [
            (1, 0, 13), // `x` in `before = x` → parameter
            (3, 3, 12), // inner use → shadowing let
            (4, 0, 13), // use after block → parameter
        ] {
            let (line, character) = occurrence_position(source, "x", occurrence);
            let location = definition(source, line, character, &document)
                .unwrap_or_else(|| panic!("local definition for occurrence {occurrence}"));
            assert_eq!(location["range"]["start"]["line"], target_line);
            assert_eq!(location["range"]["start"]["character"], target_character);
        }
    }

    #[test]
    fn definition_resolves_example_blue_variant() {
        let (_, source, document) = fixture_document("examples/example.solar");
        let (line, character) = occurrence_position(&source, "Blue", 2);

        let location = definition(&source, line, character, &document).expect("Blue definition");

        assert!(
            location["uri"]
                .as_str()
                .unwrap()
                .ends_with("/examples/example.solar"),
            "{location}"
        );
        assert_eq!(location["range"]["start"]["line"], 80, "{location}");
        assert_eq!(location["range"]["start"]["character"], 2, "{location}");
    }

    #[test]
    fn hover_and_definition_resolve_the_same_overload() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/runtime/methods.solar");
        let uri = format!("file://{}", path.display());
        let source = r#"/// Method documentation.
method ping(self: Int) -> Int { self }

/// Free documentation.
fn ping(x: Int) -> Int { x }

fn main() {
    println(1.ping());
    println(ping(1));
}
"#;
        let document = compute(&uri, source);

        for (occurrence, target_line, expected_doc, rejected_doc) in [
            (2, 1, "Method documentation.", "Free documentation."),
            (3, 4, "Free documentation.", "Method documentation."),
        ] {
            let (line, character) = occurrence_position(source, "ping", occurrence);
            let target = definition(source, line, character, &document).expect("definition target");
            assert_eq!(target["range"]["start"]["line"], target_line);

            let hover = hover(source, line, character, &document).expect("hover docs");
            let contents = hover["contents"]["value"].as_str().unwrap();
            assert!(contents.contains("ping("), "{contents}");
            assert!(contents.contains(expected_doc), "{contents}");
            assert!(!contents.contains(rejected_doc), "{contents}");
        }
    }

    #[test]
    fn hover_shows_signature_without_docstring() {
        let (_, source, document) = fixture_document("tests/multi_file/module_import/main.solar");
        let (line, character) = occurrence_position(&source, "origin", 0);

        let hover = hover(&source, line, character, &document).expect("signature hover");
        let contents = hover["contents"]["value"].as_str().unwrap();

        assert!(contents.contains("```solar"), "{contents}");
        assert!(contents.contains("pub fn origin() -> Point"), "{contents}");
    }

    #[test]
    fn hover_shows_every_generic_overload_candidate() {
        let (_, source, document) =
            fixture_document("tests/multi_file/lsp_generic_definition/main.solar");
        let (line, character) = occurrence_position(&source, "choose", 2);

        let hover = hover(&source, line, character, &document).expect("candidate hover");
        let contents = hover["contents"]["value"].as_str().unwrap();

        assert!(contents.contains("fn choose(x: A) -> Int"), "{contents}");
        assert!(contents.contains("fn choose(x: B) -> Int"), "{contents}");
        assert!(contents.contains("\n\n---\n\n"), "{contents}");
    }

    #[test]
    fn intrinsic_call_never_falls_back_to_same_named_wrapper() {
        let (_, source, document) = fixture_document("src/std/lib.solar");
        let (line, character) = occurrence_position(&source, "count_trailing_zeros(self)", 0);

        assert!(definition(&source, line, character, &document).is_none());
        assert!(hover(&source, line, character, &document).is_none());
    }
}
