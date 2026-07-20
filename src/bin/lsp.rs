//! A small, syntax-only Language Server Protocol implementation for Solar.
//!
//! It deliberately keeps no compiler state: semantic tokens are derived from
//! the same tree-sitter grammar the compiler uses, so incomplete documents are
//! still highlighted while they are being edited.

use serde_json::{Value, json};
use solar::{ast::SourceSpan, resolve, typed_ast};
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
                    documents.insert(uri, text);
                }
            }
            Some("textDocument/didChange") => {
                let uri = params.pointer("/textDocument/uri").and_then(Value::as_str);
                let text = params
                    .pointer("/contentChanges/0/text")
                    .and_then(Value::as_str);
                if let (Some(uri), Some(text)) = (uri, text) {
                    documents.insert(uri.to_owned(), text.to_owned());
                }
            }
            Some("textDocument/didClose") => {
                if let Some(uri) = params.pointer("/textDocument/uri").and_then(Value::as_str) {
                    documents.remove(uri);
                }
            }
            Some("textDocument/semanticTokens/full") => {
                let uri = params.pointer("/textDocument/uri").and_then(Value::as_str);
                let data = uri
                    .and_then(|uri| documents.get(uri))
                    .map(|text| semantic_tokens(uri.unwrap_or_default(), text))
                    .unwrap_or_default();
                respond(&mut output, id, json!({ "data": data }));
            }
            Some("textDocument/hover") => {
                let uri = params.pointer("/textDocument/uri").and_then(Value::as_str);
                let line = params.pointer("/position/line").and_then(Value::as_u64);
                let character = params
                    .pointer("/position/character")
                    .and_then(Value::as_u64);
                let result = match (uri.and_then(|uri| documents.get(uri)), line, character) {
                    (Some(text), Some(line), Some(character)) => {
                        hover(text, line as u32, character as u32)
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

fn semantic_tokens(uri: &str, source: &str) -> Vec<u32> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_solar::LANGUAGE.into())
        .expect("Solar grammar must load");
    let Some(tree) = parser.parse(source, None) else {
        return vec![];
    };

    // A single resolve+typecheck feeds both passes below: the name tables (so
    // every reference to a global type/variant is coloured the same, wherever
    // it appears) and the per-expression overlays (so a call is a function, a
    // field read a property, etc.). Any parse/resolve/type error simply leaves
    // the CST's error-tolerant classification in place.
    let analysis = analyze(uri, source);

    // Type-parameter names are gathered straight from the syntax tree (they
    // survive no further than monomorphization, so type checking can't report
    // them) and colour a `T` the same at its `#[T]` declaration and every use.
    let mut type_params = HashSet::new();
    collect_type_params(tree.root_node(), source, &mut type_params);

    let context = Context {
        names: analysis.as_ref().map(|analysis| &analysis.names),
        type_params: &type_params,
    };

    let mut tokens = Vec::new();
    collect_tokens(tree.root_node(), source, &context, &mut tokens);
    if let Some(analysis) = &analysis {
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

/// A `textDocument/hover` response for the identifier under the cursor: the
/// `///` doc comment of the top-level item (or method) it names, if any. The
/// lookup is by name against the current file's own declarations, so it works
/// on the definition and on every use site, even while the buffer has type
/// errors elsewhere.
fn hover(source: &str, line: u32, character: u32) -> Option<Value> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_solar::LANGUAGE.into())
        .expect("Solar grammar must load");
    let tree = parser.parse(source, None)?;
    let byte = position_to_byte(source, line, character)?;
    let node = tree.root_node().descendant_for_byte_range(byte, byte)?;
    if node.kind() != "identifier" {
        return None;
    }
    let name = &source[node.byte_range()];
    let doc = collect_docs(source).remove(name)?;
    Some(json!({
        "contents": {
            "kind": "markdown",
            "value": format!("```solar\n{name}\n```\n\n{doc}"),
        }
    }))
}

/// Map every top-level declaration (and method) that carries a `///` doc
/// comment to its documentation, keyed by the name a hover would see. Built
/// from the current file alone — no resolve, so it is cheap and tolerant of a
/// partially-broken buffer. The first declaration of a name wins.
fn collect_docs(source: &str) -> HashMap<String, String> {
    use solar::ast::TopLevelItem;
    let mut docs = HashMap::new();
    let Ok(ast) = solar::parser::parse(source) else {
        return docs;
    };
    for item in &ast.items {
        let (name, doc) = match item {
            TopLevelItem::Function(function) | TopLevelItem::Method(function) => {
                (&function.display_name, &function.doc)
            }
            TopLevelItem::Struct(def) => (&def.name, &def.doc),
            TopLevelItem::Enum(def) => (&def.name, &def.doc),
            TopLevelItem::Const(def) => (&def.name, &def.doc),
            TopLevelItem::Static(def) => (&def.name, &def.doc),
            TopLevelItem::TypeAlias(def) => (&def.name, &def.doc),
            TopLevelItem::Import(_) => continue,
        };
        if let Some(doc) = doc {
            docs.entry(name.clone()).or_insert_with(|| doc.clone());
        }
    }
    docs
}

/// Convert an LSP `(line, character)` position — `character` in UTF-16 code
/// units — to a byte offset into `source`.
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

/// Names that must be coloured the same wherever they appear, extracted from
/// the resolved program. Because these are global entities (unlike locals,
/// whose role is inherent to their binding site), one occurrence in a type
/// annotation and another in a struct literal or path should look identical.
#[derive(Default)]
struct Names {
    /// Struct and enum names → `type`.
    types: HashSet<String>,
    /// Enum variant names → `enumMember`.
    variants: HashSet<String>,
}

/// Everything the CST classifier consults to give an identifier its canonical
/// colour: the type-checker's name tables (absent on a broken buffer) and the
/// syntactically-collected type-parameter names.
struct Context<'a> {
    names: Option<&'a Names>,
    type_params: &'a HashSet<String>,
}

/// The result of resolving and type-checking the current buffer, shared by the
/// name-table and per-expression classification passes.
struct Analysis {
    typed: typed_ast::SourceFile,
    file_id: u32,
    names: Names,
}

/// Resolve and type-check the in-editor buffer. Returns `None` on any
/// parse/resolve/type error, in which case the CST classification stands alone.
/// The resolver accepts the current buffer, so this works before the editor
/// writes the document to disk.
fn analyze(uri: &str, source: &str) -> Option<Analysis> {
    let path = file_uri_to_path(uri)?;
    let (ast, source_map) = resolve::resolve_source(&path, source.to_owned()).ok()?;
    let typed = typed_ast::lower(&ast).ok()?;
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

/// Type checking supplies facts which syntax alone cannot know — for example,
/// whether a direct call is a free function or a method.
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

/// Record the names declared in every `#[T, …]` type-parameter list.
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
        "comment" => Some(token_index("comment")),
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

/// Force a reference to a known global entity to its canonical colour, so it
/// looks the same wherever it appears — a type in an annotation, a struct
/// literal, a pattern, or a path; a type parameter at its declaration and every
/// use. Binding-position tokens (`parameter`, `property`) keep their local
/// role, so a local named like a type is not recoloured.
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
