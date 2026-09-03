//! Language Server Protocol support for Solar.

use serde_json::{Value, json};
use solar::{
    ast::{self, SourceSpan},
    error::{CompileError, SourceMap},
    fmt::format_source,
    intrinsics::Intrinsic,
    resolve, typed_ast,
};
use std::{
    collections::{HashMap, HashSet},
    io::{self, BufRead, Write},
    path::PathBuf,
};
use tree_sitter::{Node, Parser};

const TOKEN_TYPES: &[&str] = &[
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
    // tokens share one resolve and type-check per document revision. Type facts
    // from the last successful revision survive failed edits for completion and
    // inlay hints.
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
                        "documentFormattingProvider": true,
                        "completionProvider": { "triggerCharacters": [".", ":"] },
                        "signatureHelpProvider": {
                            "triggerCharacters": ["(", ","],
                            "retriggerCharacters": [","]
                        },
                        "hoverProvider": true,
                        "definitionProvider": true,
                        "inlayHintProvider": true,
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
                    update_cached_document(&mut cache, &uri, document);
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
                    update_cached_document(&mut cache, uri, document);
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
            Some("textDocument/formatting") => {
                let uri = params.pointer("/textDocument/uri").and_then(Value::as_str);
                let edits = uri
                    .and_then(|uri| documents.get(uri))
                    .map_or_else(Vec::new, |text| formatting_edits(text));
                respond(&mut output, id, json!(edits));
            }
            Some("textDocument/completion") => {
                let uri = params.pointer("/textDocument/uri").and_then(Value::as_str);
                let line = params.pointer("/position/line").and_then(Value::as_u64);
                let character = params
                    .pointer("/position/character")
                    .and_then(Value::as_u64);
                let target = uri.and_then(|uri| documents.get(uri).map(|text| (uri, text)));
                let items = match (target, line, character) {
                    (Some((uri, text)), Some(line), Some(character)) => {
                        let document = cached(&mut cache, uri, text);
                        completions(text, line as u32, character as u32, uri, document)
                    }
                    _ => Vec::new(),
                };
                respond(&mut output, id, json!(items));
            }
            Some("textDocument/signatureHelp") => {
                let uri = params.pointer("/textDocument/uri").and_then(Value::as_str);
                let line = params.pointer("/position/line").and_then(Value::as_u64);
                let character = params
                    .pointer("/position/character")
                    .and_then(Value::as_u64);
                let target = uri.and_then(|uri| documents.get(uri).map(|text| (uri, text)));
                let result = match (target, line, character) {
                    (Some((uri, text)), Some(line), Some(character)) => {
                        let document = cached(&mut cache, uri, text);
                        signature_help(text, line as u32, character as u32, uri, document)
                    }
                    _ => None,
                };
                respond(&mut output, id, result.unwrap_or(Value::Null));
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
            Some("textDocument/inlayHint") => {
                let uri = params.pointer("/textDocument/uri").and_then(Value::as_str);
                let range = inlay_hint_range(&params);
                let target = uri.and_then(|uri| documents.get(uri).map(|text| (uri, text)));
                let result = match target {
                    Some((uri, text)) => {
                        let document = cached(&mut cache, uri, text);
                        inlay_hints(text, document, range)
                    }
                    None => Vec::new(),
                };
                respond(&mut output, id, json!(result));
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

fn formatting_edits(source: &str) -> Vec<Value> {
    let Ok(formatted) = format_source(source) else {
        return Vec::new();
    };
    if formatted == source {
        return Vec::new();
    }

    let line = source.bytes().filter(|byte| *byte == b'\n').count();
    let line_start = source.rfind('\n').map_or(0, |index| index + 1);
    let character = source[line_start..].encode_utf16().count();
    vec![json!({
        "range": {
            "start": { "line": 0, "character": 0 },
            "end": { "line": line, "character": character },
        },
        "newText": formatted,
    })]
}

#[derive(Clone)]
struct SignatureArgument {
    ordinal: usize,
    keyword: Option<String>,
    value_span: SourceSpan,
    argument_start_byte: usize,
}

struct SignatureHelpContext {
    name: String,
    namespace_segments: Vec<String>,
    call_start_byte: usize,
    call_end_byte: usize,
    has_close: bool,
    open_byte: usize,
    active_argument: usize,
    active_keyword: Option<String>,
    explicit_type_argument_count: Option<usize>,
    receiver_span: Option<SourceSpan>,
    receiver_end_byte: Option<usize>,
    arguments: Vec<SignatureArgument>,
    commas: Vec<usize>,
    probe_cutoff: usize,
}

/// Returns LSP signature help for the innermost function or method call whose
/// argument list contains the cursor. Completed arguments narrow the overload
/// set; an incomplete argument is deliberately treated as unknown.
fn signature_help(
    source: &str,
    line: u32,
    character: u32,
    uri: &str,
    document: &Document,
) -> Option<Value> {
    let context = signature_help_context(source, line, character).or_else(|| {
        let cursor_byte = position_to_byte(source, line, character)?;
        let mut repaired = source.to_owned();
        repaired.insert(cursor_byte, ')');
        let mut context = signature_help_context(&repaired, line, character)?;
        context.call_end_byte = cursor_byte;
        context.has_close = false;
        Some(context)
    })?;
    let probe_documents = if document.analysis.is_some()
        && (!document.completion_functions.is_empty() || !document.completion_methods.is_empty())
    {
        Vec::new()
    } else {
        let probe_source = signature_argument_probe_source(source, &context);
        completion_probe_documents(uri, &probe_source, context.open_byte + 2)
    };
    let probe_document = probe_documents
        .iter()
        .find(|probe| probe.analysis.is_some());
    let tooling_document =
        if !document.completion_functions.is_empty() || !document.completion_methods.is_empty() {
            document
        } else {
            probe_document?
        };

    let argument_types = context
        .arguments
        .iter()
        .map(|argument| {
            expression_types_for_signature(
                argument.value_span,
                document.analysis.as_ref(),
                probe_document.and_then(|probe| probe.analysis.as_ref()),
                source,
                context.open_byte,
            )
        })
        .collect::<Vec<_>>();
    let receiver_types = context.receiver_span.map_or_else(Vec::new, |span| {
        let current = document
            .analysis
            .as_ref()
            .map_or_else(Vec::new, |analysis| {
                expression_types_at(&analysis.typed, analysis.file_id, span)
            });
        if !current.is_empty() {
            return current;
        }
        let Some(receiver_end_byte) = context.receiver_end_byte else {
            return Vec::new();
        };
        let member_context = MemberCompletionContext {
            receiver_span: span,
            dot_byte: receiver_end_byte,
            probe_end_byte: context.call_end_byte,
            prefix: String::new(),
            prefix_start_character: 0,
        };
        let receiver_probe = completion_probe_source(source, &member_context);
        completion_probe_documents(uri, &receiver_probe, receiver_end_byte)
            .iter()
            .find_map(|probe| {
                let analysis = probe.analysis.as_ref()?;
                let types = expression_types_at(&analysis.typed, analysis.file_id, span);
                (!types.is_empty()).then_some(types)
            })
            .unwrap_or_default()
    });

    let mut signatures = Vec::new();
    let mut seen = HashSet::new();
    if context.receiver_span.is_some() {
        for candidate in &tooling_document.completion_methods {
            if candidate.def.name != context.name
                || !signature_candidate_matches(
                    &candidate.def,
                    true,
                    &context,
                    &argument_types,
                    &receiver_types,
                    &tooling_document.completion_aliases,
                )
            {
                continue;
            }
            let signature = signature_information(&candidate.def, true, &context);
            if seen.insert(signature["label"].as_str()?.to_owned()) {
                signatures.push(signature);
            }
        }
    } else {
        let namespace_defs = if context.namespace_segments.is_empty() {
            None
        } else {
            let file_id = namespace_file(tooling_document, &context.namespace_segments)?;
            Some(exported_completion_defs(
                file_id,
                &tooling_document.source_map,
                &mut HashMap::new(),
                &mut HashSet::new(),
            ))
        };
        for candidate in &tooling_document.completion_functions {
            let in_scope = namespace_defs
                .as_ref()
                .map_or(candidate.visible_unqualified, |defs| {
                    defs.contains(&candidate.id)
                });
            if !in_scope
                || candidate.def.name != context.name && candidate.def.display_name != context.name
                || !signature_candidate_matches(
                    &candidate.def,
                    false,
                    &context,
                    &argument_types,
                    &receiver_types,
                    &tooling_document.completion_aliases,
                )
            {
                continue;
            }
            let signature = signature_information(&candidate.def, false, &context);
            if seen.insert(signature["label"].as_str()?.to_owned()) {
                signatures.push(signature);
            }
        }
    }
    (!signatures.is_empty()).then(|| {
        let active_parameter = signatures[0]["activeParameter"].clone();
        json!({
            "signatures": signatures,
            "activeSignature": 0,
            "activeParameter": active_parameter,
        })
    })
}

fn expression_types_for_signature(
    span: SourceSpan,
    current: Option<&Analysis>,
    probe: Option<&Analysis>,
    source: &str,
    open_byte: usize,
) -> Vec<typed_ast::Type> {
    for (analysis, target) in [
        current.map(|analysis| (analysis, span)),
        probe.map(|analysis| (analysis, signature_probe_span(span, source, open_byte))),
    ]
    .into_iter()
    .flatten()
    {
        let types = expression_types_at(&analysis.typed, analysis.file_id, target);
        if !types.is_empty() {
            return types;
        }
    }
    Vec::new()
}

fn signature_help_context(source: &str, line: u32, character: u32) -> Option<SignatureHelpContext> {
    let cursor_byte = position_to_byte(source, line, character)?;
    if completion_is_in_comment_or_string(source, cursor_byte) {
        return None;
    }
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_solar::LANGUAGE.into())
        .expect("Solar grammar must load");
    let tree = parser.parse(source, None)?;
    let mut node = tree
        .root_node()
        .descendant_for_byte_range(cursor_byte.saturating_sub(1), cursor_byte.min(source.len()))?;
    while !matches!(
        node.kind(),
        "call_expr" | "generic_call_expr" | "generic_method_call"
    ) {
        node = node.parent()?;
    }

    let (name_node, receiver, namespace_segments) = match node.kind() {
        "generic_method_call" => (
            node.child_by_field_name("method")?,
            Some(node.child_by_field_name("receiver")?),
            Vec::new(),
        ),
        "generic_call_expr" => (node.child_by_field_name("function")?, None, Vec::new()),
        "call_expr" => {
            let function = node.child_by_field_name("function")?;
            if function.kind() == "field_access"
                && function
                    .child_by_field_name("field")
                    .is_some_and(|field| field.kind() == "identifier")
            {
                (
                    function.child_by_field_name("field")?,
                    Some(function.child_by_field_name("object")?),
                    Vec::new(),
                )
            } else {
                let name_node = last_identifier(function)?;
                let namespace_segments = if function.kind() == "path_expr" {
                    let mut walk = function.walk();
                    function
                        .named_children(&mut walk)
                        .filter(|child| child.kind() == "path_segment")
                        .filter_map(|segment| segment.child_by_field_name("name"))
                        .filter(|name| *name != name_node)
                        .map(|name| source[name.byte_range()].to_owned())
                        .collect()
                } else {
                    Vec::new()
                };
                (name_node, None, namespace_segments)
            }
        }
        _ => unreachable!(),
    };
    let head_end = node
        .child_by_field_name("type_args")
        .map_or(name_node.end_byte(), |type_args| type_args.end_byte());
    let explicit_type_argument_count = node.child_by_field_name("type_args").map(|type_args| {
        let mut walk = type_args.walk();
        type_args.named_children(&mut walk).count()
    });
    let open_byte = head_end
        + source
            .get(head_end..node.end_byte().max(cursor_byte).min(source.len()))?
            .find('(')?;
    let mut walk = node.walk();
    let close_byte = node
        .children(&mut walk)
        .find(|child| child.kind() == ")" && child.start_byte() > open_byte)
        .map(|child| child.start_byte());
    if cursor_byte <= open_byte || close_byte.is_some_and(|close| cursor_byte > close) {
        return None;
    }

    let argument_list = named_child(node, "argument_list");
    let mut arguments = Vec::new();
    let mut commas = Vec::new();
    if let Some(argument_list) = argument_list {
        let mut walk = argument_list.walk();
        for child in argument_list.children(&mut walk) {
            match child.kind() {
                "," => commas.push(child.start_byte()),
                "argument" => {
                    let value = child.child_by_field_name("value")?;
                    arguments.push(SignatureArgument {
                        ordinal: arguments.len(),
                        keyword: child
                            .child_by_field_name("name")
                            .map(|name| source[name.byte_range()].to_owned()),
                        value_span: node_span(value, 0),
                        argument_start_byte: child.start_byte(),
                    });
                }
                _ => {}
            }
        }
    }
    let active_argument = commas.iter().filter(|comma| **comma < cursor_byte).count();
    let active_keyword = arguments
        .iter()
        .find(|argument| argument.ordinal == active_argument)
        .and_then(|argument| argument.keyword.clone());
    let completed_arguments = arguments
        .iter()
        .filter(|argument| {
            let end = span_end_byte(source, argument.value_span);
            end <= cursor_byte && argument.ordinal <= active_argument
        })
        .cloned()
        .collect::<Vec<_>>();
    let probe_cutoff = arguments
        .iter()
        .find(|argument| {
            argument.ordinal == active_argument
                && span_end_byte(source, argument.value_span) > cursor_byte
        })
        .map_or(cursor_byte, |argument| argument.argument_start_byte);

    Some(SignatureHelpContext {
        name: source[name_node.byte_range()].to_owned(),
        namespace_segments,
        call_start_byte: node.start_byte(),
        call_end_byte: close_byte.map_or(node.end_byte().max(cursor_byte), |close| close + 1),
        has_close: close_byte.is_some(),
        open_byte,
        active_argument,
        active_keyword,
        explicit_type_argument_count,
        receiver_span: receiver.map(|receiver| node_span(receiver, 0)),
        receiver_end_byte: receiver.map(|receiver| receiver.end_byte()),
        arguments: completed_arguments,
        commas,
        probe_cutoff,
    })
}

fn last_identifier(mut node: Node<'_>) -> Option<Node<'_>> {
    loop {
        if node.kind() == "identifier" {
            return Some(node);
        }
        let mut walk = node.walk();
        node = node.named_children(&mut walk).last()?;
    }
}

fn span_end_byte(source: &str, span: SourceSpan) -> usize {
    let offsets = source_line_offsets(source);
    offsets[span.end.line as usize] + span.end.col as usize
}

fn signature_argument_probe_source(source: &str, context: &SignatureHelpContext) -> String {
    let mut probe = source.as_bytes().to_vec();
    blank_probe_range(&mut probe, context.call_start_byte, context.open_byte);
    probe.insert(context.open_byte + 1, b'{');
    for comma in &context.commas {
        if *comma < context.probe_cutoff {
            probe[*comma + 1] = b';';
        }
    }
    for argument in &context.arguments {
        if argument.keyword.is_some() {
            let value_start = source_line_offsets(source)[argument.value_span.start.line as usize]
                + argument.value_span.start.col as usize;
            blank_probe_range(
                &mut probe,
                argument.argument_start_byte + 1,
                value_start + 1,
            );
        }
    }
    let probe_cutoff = context.probe_cutoff + 1;
    let call_end = context.call_end_byte.min(source.len()) + 1 - usize::from(context.has_close);
    blank_probe_range(&mut probe, probe_cutoff, call_end);
    let has_argument = !context.arguments.is_empty();
    let already_separated = probe[context.open_byte + 2..probe_cutoff]
        .iter()
        .rev()
        .find(|byte| !byte.is_ascii_whitespace())
        .is_some_and(|byte| *byte == b';');
    let tail = if !has_argument || already_separated {
        " loop {}}"
    } else {
        "; loop {}}"
    };
    let tail = if context.has_close {
        tail.to_owned()
    } else {
        format!("{tail})")
    };
    probe.splice(probe_cutoff..probe_cutoff, tail.bytes());
    String::from_utf8(probe).unwrap()
}

fn signature_probe_span(mut span: SourceSpan, source: &str, open_byte: usize) -> SourceSpan {
    let prefix = &source[..open_byte];
    let open = (
        prefix.bytes().filter(|byte| *byte == b'\n').count() as u32,
        prefix
            .rfind('\n')
            .map_or(open_byte, |line| open_byte - line - 1) as u32,
    );
    if span.start.line == open.0 && span.start.col > open.1 {
        span.start.col += 1;
    }
    if span.end.line == open.0 && span.end.col > open.1 {
        span.end.col += 1;
    }
    span
}

fn signature_candidate_matches(
    def: &ast::FunctionDef,
    method: bool,
    context: &SignatureHelpContext,
    argument_types: &[Vec<typed_ast::Type>],
    receiver_types: &[typed_ast::Type],
    aliases: &HashMap<ast::DefId, (Vec<String>, ast::Type)>,
) -> bool {
    match context.explicit_type_argument_count {
        Some(count) if count != def.out_type_params.len() => return false,
        None if !def.out_type_params.is_empty() => return false,
        _ => {}
    }
    let offset = usize::from(method);
    if def.parameters.len() < offset {
        return false;
    }
    let type_parameters = def
        .type_params
        .iter()
        .chain(&def.out_type_params)
        .cloned()
        .collect::<Vec<_>>();
    let mut states = vec![HashMap::new()];
    if method {
        let Some(receiver_parameter) = def.parameters.first() else {
            return false;
        };
        if !receiver_types.is_empty() {
            states = signature_match_type(
                states,
                &receiver_parameter.ty,
                receiver_types,
                &type_parameters,
                aliases,
            );
            if states.is_empty() {
                return false;
            }
        }
    }

    let supplied = &def.parameters[offset..];
    let required = supplied
        .iter()
        .take_while(|parameter| parameter.default.is_none())
        .count();
    let mut positional = 0;
    let mut keyword_parameters = HashSet::new();
    for (argument, types) in context.arguments.iter().zip(argument_types) {
        let parameter_index = if let Some(keyword) = &argument.keyword {
            let Some(index) = supplied.iter().position(|parameter| {
                parameter_name(&parameter.pattern).is_some_and(|name| name == keyword)
            }) else {
                return false;
            };
            if index < required || !keyword_parameters.insert(index) {
                return false;
            }
            index
        } else {
            let index = positional;
            positional += 1;
            if index >= required {
                return false;
            }
            index
        };
        if !types.is_empty() {
            states = signature_match_type(
                states,
                &supplied[parameter_index].ty,
                types,
                &type_parameters,
                aliases,
            );
            if states.is_empty() {
                return false;
            }
        }
    }

    let active_parameter = if let Some(keyword) = &context.active_keyword {
        supplied.iter().position(|parameter| {
            parameter_name(&parameter.pattern).is_some_and(|name| name == keyword)
        })
    } else {
        Some(
            context
                .arguments
                .iter()
                .filter(|argument| {
                    argument.ordinal < context.active_argument && argument.keyword.is_none()
                })
                .count(),
        )
    };
    active_parameter.is_some_and(|index| index < supplied.len()) || supplied.is_empty()
}

fn signature_match_type(
    states: Vec<HashMap<String, typed_ast::Type>>,
    expected: &ast::Type,
    actual_types: &[typed_ast::Type],
    type_parameters: &[String],
    aliases: &HashMap<ast::DefId, (Vec<String>, ast::Type)>,
) -> Vec<HashMap<String, typed_ast::Type>> {
    let expected = expand_completion_aliases(expected, aliases, 0);
    let mut matched = Vec::new();
    for state in states {
        for actual in actual_types {
            let mut next = state.clone();
            if ast_type_matches(&expected, actual, type_parameters, &mut next) {
                matched.push(next);
            }
        }
    }
    matched
}

fn signature_information(
    def: &ast::FunctionDef,
    method: bool,
    context: &SignatureHelpContext,
) -> Value {
    let parameter_labels = def
        .parameters
        .iter()
        .map(|parameter| {
            format!(
                "{}: {}",
                parameter_name(&parameter.pattern).unwrap_or("_"),
                ast_type_label(&parameter.ty)
            )
        })
        .collect::<Vec<_>>();
    let kind = match (def.is_unsafe, method) {
        (true, true) => "unsafe method",
        (true, false) => "unsafe fn",
        (false, true) => "method",
        (false, false) => "fn",
    };
    let mut generic_parameters = def.type_params.clone();
    generic_parameters.extend(
        def.out_type_params
            .iter()
            .map(|parameter| format!("out {parameter}")),
    );
    let generics = if generic_parameters.is_empty() {
        String::new()
    } else {
        format!("#[{}]", generic_parameters.join(", "))
    };
    let mut label = format!(
        "{kind} {}{generics}({})",
        def.display_name,
        parameter_labels.join(", ")
    );
    if let Some(return_type) = &def.return_type {
        label.push_str(&format!(" -> {}", ast_type_label(return_type)));
    }
    let offset = usize::from(method);
    let supplied = &def.parameters[offset..];
    let active_supplied = context
        .active_keyword
        .as_ref()
        .and_then(|keyword| {
            supplied.iter().position(|parameter| {
                parameter_name(&parameter.pattern).is_some_and(|name| name == keyword)
            })
        })
        .unwrap_or_else(|| {
            context
                .arguments
                .iter()
                .filter(|argument| {
                    argument.ordinal < context.active_argument && argument.keyword.is_none()
                })
                .count()
        });
    let active_parameter = (offset + active_supplied).min(parameter_labels.len().saturating_sub(1));
    let parameters = parameter_labels
        .into_iter()
        .map(|label| json!({ "label": label }))
        .collect::<Vec<_>>();
    let mut result = json!({
        "label": label,
        "parameters": parameters,
        "activeParameter": active_parameter,
    });
    if let Some(doc) = &def.doc {
        result["documentation"] = json!({ "kind": "markdown", "value": doc });
    }
    result
}

fn parameter_name(pattern: &ast::DestructurePattern) -> Option<&str> {
    match pattern {
        ast::DestructurePattern::Name(ast::Ident::User(name) | ast::Ident::Synthetic(name)) => {
            Some(name)
        }
        _ => None,
    }
}

fn ast_type_label(ty: &ast::Type) -> String {
    match ty {
        ast::Type::Named(name) => name.to_string(),
        ast::Type::Generic { name, type_args } => format!(
            "{}#[{}]",
            name,
            type_args
                .iter()
                .map(ast_type_label)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        ast::Type::Reference(inner) => format!("&{}", ast_type_label(inner)),
        ast::Type::NullableReference(inner) => format!("&?{}", ast_type_label(inner)),
        ast::Type::Unique(inner) => format!("^{}", ast_type_label(inner)),
        ast::Type::Slice(inner) => format!("[{}]", ast_type_label(inner)),
        ast::Type::FixedArray(inner, size) => format!("[{}; {size}]", ast_type_label(inner)),
        ast::Type::Function {
            params,
            return_type,
        } => {
            let mut label = format!(
                "fn({})",
                params
                    .iter()
                    .map(|(name, ty)| name.as_ref().map_or_else(
                        || ast_type_label(ty),
                        |name| format!("{name}: {}", ast_type_label(ty))
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            if let Some(return_type) = return_type {
                label.push_str(&format!(" -> {}", ast_type_label(return_type)));
            }
            label
        }
        ast::Type::Tuple(types) => format!(
            "({})",
            types
                .iter()
                .map(ast_type_label)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        ast::Type::Infer => "_".to_owned(),
    }
}

/// Returns type-aware member completions at `receiver.<cursor>`. When the
/// incomplete member access prevents the current revision from type-checking,
/// a private probe removes that access and analyzes the receiver expression.
fn completions(
    source: &str,
    line: u32,
    character: u32,
    uri: &str,
    document: &Document,
) -> Vec<Value> {
    if let Some(context) = namespace_completion_context(source, line, character) {
        return namespace_completions(source, line, character, uri, document, &context);
    }
    let Some(context) = member_completion_context(source, line, character) else {
        return ordinary_completions(source, line, character, uri, document);
    };

    let current_types = document
        .analysis
        .as_ref()
        .map_or_else(Vec::new, |analysis| {
            expression_types_at(&analysis.typed, analysis.file_id, context.receiver_span)
        });
    let probe_documents;
    let (receiver_types, tooling_document) = if current_types.is_empty() {
        let probe_source = completion_probe_source(source, &context);
        probe_documents = completion_probe_documents(uri, &probe_source, context.dot_byte);
        if let Some((types, probe)) = probe_documents.iter().find_map(|probe| {
            let types = probe.analysis.as_ref().map_or_else(Vec::new, |analysis| {
                expression_types_at(&analysis.typed, analysis.file_id, context.receiver_span)
            });
            (!types.is_empty()).then_some((types, probe))
        }) {
            (types, probe)
        } else {
            let cached_types =
                document
                    .last_successful_types
                    .as_ref()
                    .map_or_else(Vec::new, |types| {
                        expression_types_at(
                            &types.analysis.typed,
                            types.analysis.file_id,
                            context.receiver_span,
                        )
                    });
            let tooling_document = if document.resolved.is_some() {
                document
            } else {
                &probe_documents[0]
            };
            (cached_types, tooling_document)
        }
    } else {
        (current_types, document)
    };
    if receiver_types.is_empty() {
        return Vec::new();
    }

    let replace_range = json!({
        "start": { "line": line, "character": context.prefix_start_character },
        "end": { "line": line, "character": character },
    });
    let dot_position = byte_position(source, context.dot_byte);
    let mut seen = HashSet::new();
    let mut items = Vec::new();
    for receiver_type in &receiver_types {
        let typed_ast::Type::Struct(id) = receiver_type else {
            continue;
        };
        let Some(fields) = tooling_document.completion_fields.get(&id.def) else {
            continue;
        };
        for field in fields {
            if !field.name.starts_with(&context.prefix)
                || (!field.is_pub
                    && tooling_document.source_map.root_file_id() != Some(field.file_id))
                || !seen.insert((field.name.clone(), String::new(), field.detail.clone()))
            {
                continue;
            }
            let mut item = json!({
                "label": field.name,
                "kind": 5,
                "sortText": format!("0{}", field.name),
                "textEdit": { "range": replace_range, "newText": field.name },
            });
            if let Some(detail) = &field.detail {
                item["detail"] = json!(detail);
            }
            items.push(item);
        }
    }
    let mut monomorphization_requests = Vec::new();
    let mut monomorphization_keys = Vec::new();
    let mut requested = HashSet::new();
    for method in &tooling_document.completion_methods {
        if !method.def.name.starts_with(&context.prefix)
            || method.def.type_params.is_empty()
            || !method.def.out_type_params.is_empty()
        {
            continue;
        }
        let Some(self_type) = method.def.parameters.first().map(|parameter| &parameter.ty) else {
            continue;
        };
        let self_type =
            expand_completion_aliases(self_type, &tooling_document.completion_aliases, 0);
        for receiver_type in &receiver_types {
            let Some(receiver_match) =
                method_receiver_match(receiver_type, &self_type, &method.def.type_params)
            else {
                continue;
            };
            let Some(type_arguments) = method
                .def
                .type_params
                .iter()
                .map(|parameter| receiver_match.substitutions.get(parameter).cloned())
                .collect::<Option<Vec<_>>>()
            else {
                // A later call argument could bind the remaining parameter, so
                // this specialization still has the potential to type-check.
                continue;
            };
            let key = (span_key(method.def.span), type_arguments.clone());
            if requested.insert(key.clone()) {
                monomorphization_keys.push(key);
                monomorphization_requests.push(typed_ast::MethodMonomorphization {
                    definition_span: method.def.span,
                    type_arguments,
                });
            }
        }
    }
    let mut rejected_monomorphizations = HashSet::new();
    if !monomorphization_requests.is_empty()
        && let Some(resolved) = &tooling_document.resolved
        && let Ok(results) =
            typed_ast::method_monomorphizations_typecheck(resolved, &monomorphization_requests)
    {
        for (key, typechecks) in monomorphization_keys.into_iter().zip(results) {
            if !typechecks {
                rejected_monomorphizations.insert(key);
            }
        }
    }
    for method in &tooling_document.completion_methods {
        if !method.def.name.starts_with(&context.prefix) {
            continue;
        }
        let Some(self_type) = method.def.parameters.first().map(|parameter| &parameter.ty) else {
            continue;
        };
        for receiver_type in &receiver_types {
            let type_parameters: Vec<String> = method
                .def
                .type_params
                .iter()
                .chain(&method.def.out_type_params)
                .cloned()
                .collect();
            let self_type =
                expand_completion_aliases(self_type, &tooling_document.completion_aliases, 0);
            let Some(receiver_match) =
                method_receiver_match(receiver_type, &self_type, &type_parameters)
            else {
                continue;
            };
            if method.def.out_type_params.is_empty()
                && let Some(type_arguments) = method
                    .def
                    .type_params
                    .iter()
                    .map(|parameter| receiver_match.substitutions.get(parameter).cloned())
                    .collect::<Option<Vec<_>>>()
                && rejected_monomorphizations.contains(&(span_key(method.def.span), type_arguments))
            {
                continue;
            }
            let adjustment = receiver_match.adjustment;
            let key = (
                method.def.name.clone(),
                adjustment.clone(),
                method.detail.clone(),
            );
            if !seen.insert(key) {
                continue;
            }
            let mut item = json!({
                "label": method.def.name,
                "kind": 2,
                "sortText": format!("{}{}", if adjustment.is_empty() { 0 } else { 1 }, method.def.name),
                "textEdit": { "range": replace_range, "newText": method.def.name },
            });
            if let Some(detail) = &method.detail {
                item["detail"] = json!(detail);
            }
            if !adjustment.is_empty() {
                item["additionalTextEdits"] = json!([{
                    "range": { "start": dot_position, "end": dot_position },
                    "newText": adjustment,
                }]);
            }
            if let Some(doc) = &method.def.doc {
                item["documentation"] = json!({ "kind": "markdown", "value": doc });
            }
            items.push(item);
        }
    }
    items
}

struct NamespaceCompletionContext {
    segments: Vec<String>,
    prefix: String,
    path_start_byte: usize,
    prefix_start_character: u32,
}

fn namespace_completion_context(
    source: &str,
    line: u32,
    character: u32,
) -> Option<NamespaceCompletionContext> {
    let cursor_byte = position_to_byte(source, line, character)?;
    if completion_is_in_comment_or_string(source, cursor_byte) {
        return None;
    }

    let mut prefix_start = cursor_byte;
    while prefix_start > 0 {
        let ch = source[..prefix_start].chars().next_back()?;
        if ch == '_' || ch.is_alphanumeric() {
            prefix_start -= ch.len_utf8();
        } else {
            break;
        }
    }
    let qualifier_end = prefix_start.checked_sub(2)?;
    if source.get(qualifier_end..prefix_start)? != "::" {
        return None;
    }

    let mut path_start = qualifier_end;
    while path_start > 0 {
        let ch = source[..path_start].chars().next_back()?;
        if ch == '_' || ch == ':' || ch.is_alphanumeric() {
            path_start -= ch.len_utf8();
        } else {
            break;
        }
    }
    let qualifier = source.get(path_start..qualifier_end)?;
    let segments = qualifier
        .split("::")
        .filter(|segment| !segment.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if segments.is_empty() || segments.join("::") != qualifier {
        return None;
    }

    let line_start = source[..prefix_start]
        .rfind('\n')
        .map_or(0, |index| index + 1);
    Some(NamespaceCompletionContext {
        segments,
        prefix: source[prefix_start..cursor_byte].to_owned(),
        path_start_byte: path_start,
        prefix_start_character: source[line_start..prefix_start].encode_utf16().count() as u32,
    })
}

fn namespace_completions(
    source: &str,
    line: u32,
    character: u32,
    uri: &str,
    document: &Document,
    context: &NamespaceCompletionContext,
) -> Vec<Value> {
    let probe;
    let statement_probe;
    let (file_id, tooling_document) =
        if let Some(file_id) = namespace_file(document, &context.segments) {
            (file_id, document)
        } else {
            let cursor_byte = position_to_byte(source, line, character).unwrap();
            let mut probe_source = source.to_owned();
            probe_source.replace_range(context.path_start_byte..cursor_byte, "Int");
            probe = compute(uri, &probe_source);
            if let Some(file_id) = namespace_file(&probe, &context.segments) {
                (file_id, &probe)
            } else {
                // A qualified path at the end of a statement needs a delimiter
                // before the following line can parse. Keep the delimiter out
                // of the first probe so completion inside calls and other value
                // positions continues to work.
                probe_source.replace_range(
                    context.path_start_byte..context.path_start_byte + "Int".len(),
                    "Int;",
                );
                statement_probe = compute(uri, &probe_source);
                let Some(file_id) = namespace_file(&statement_probe, &context.segments) else {
                    return Vec::new();
                };
                (file_id, &statement_probe)
            }
        };
    let Some(symbols) = tooling_document.namespace_symbols.get(&file_id) else {
        return Vec::new();
    };

    let replace_range = json!({
        "start": { "line": line, "character": context.prefix_start_character },
        "end": { "line": line, "character": character },
    });
    symbols
        .iter()
        .filter(|symbol| symbol.label.starts_with(&context.prefix))
        .map(|symbol| {
            let mut item = json!({
                "label": symbol.label,
                "kind": symbol.kind,
                "textEdit": { "range": replace_range, "newText": symbol.label },
            });
            if let Some(detail) = &symbol.detail {
                item["detail"] = json!(detail);
            }
            if let Some(documentation) = &symbol.documentation {
                item["documentation"] = json!({
                    "kind": "markdown",
                    "value": documentation,
                });
            }
            item
        })
        .collect()
}

fn namespace_file(document: &Document, segments: &[String]) -> Option<u32> {
    let mut file_id = document.source_map.root_file_id()?;
    for (index, segment) in segments.iter().enumerate() {
        let module = document.module_imports.get(&(file_id, segment.clone()))?;
        if index > 0 && !module.is_pub {
            return None;
        }
        file_id = module.file_id;
    }
    Some(file_id)
}

struct MemberCompletionContext {
    receiver_span: SourceSpan,
    prefix: String,
    prefix_start_character: u32,
    dot_byte: usize,
    probe_end_byte: usize,
}

fn member_completion_context(
    source: &str,
    line: u32,
    character: u32,
) -> Option<MemberCompletionContext> {
    let cursor_byte = position_to_byte(source, line, character)?;
    let mut prefix_start = cursor_byte;
    while prefix_start > 0 {
        let ch = source[..prefix_start].chars().next_back()?;
        if ch == '_' || ch.is_alphanumeric() {
            prefix_start -= ch.len_utf8();
        } else {
            break;
        }
    }
    let mut dot_byte = prefix_start;
    while dot_byte > 0 && source.as_bytes()[dot_byte - 1].is_ascii_whitespace() {
        dot_byte -= 1;
    }
    dot_byte = dot_byte.checked_sub(1)?;
    if source.as_bytes().get(dot_byte) != Some(&b'.')
        || source.as_bytes().get(dot_byte.wrapping_sub(1)) == Some(&b'.')
    {
        return None;
    }

    let mut receiver_end = dot_byte;
    while receiver_end > 0 && source.as_bytes()[receiver_end - 1].is_ascii_whitespace() {
        receiver_end -= 1;
    }
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_solar::LANGUAGE.into())
        .expect("Solar grammar must load");
    let tree = parser.parse(source, None)?;
    let mut node = tree
        .root_node()
        .descendant_for_byte_range(receiver_end.saturating_sub(1), receiver_end)?;
    let mut receiver = is_expression_node(node.kind())
        .then_some(node)
        .filter(|node| node.end_byte() == receiver_end);
    while let Some(parent) = node.parent() {
        if parent.end_byte() > receiver_end {
            break;
        }
        node = parent;
        if node.end_byte() == receiver_end && is_expression_node(node.kind()) {
            receiver = Some(node);
        }
    }
    let receiver = receiver?;
    let start = receiver.start_position();
    let end = receiver.end_position();

    Some(MemberCompletionContext {
        receiver_span: SourceSpan {
            start: ast::SourcePos {
                line: start.row as u32,
                col: start.column as u32,
            },
            end: ast::SourcePos {
                line: end.row as u32,
                col: end.column as u32,
            },
            file_id: 0,
        },
        prefix: source[prefix_start..cursor_byte].to_owned(),
        prefix_start_character: utf16_column(
            source.lines().nth(line as usize).unwrap_or(""),
            prefix_start - source_line_offsets(source)[line as usize],
        ),
        dot_byte,
        probe_end_byte: completion_call_end(source, cursor_byte),
    })
}

fn is_expression_node(kind: &str) -> bool {
    kind.ends_with("_expr")
        || kind.ends_with("_expression")
        || matches!(
            kind,
            "identifier"
                | "integer_literal"
                | "float_literal"
                | "boolean_literal"
                | "string_literal"
                | "char_literal"
                | "struct_literal"
                | "array_literal"
                | "tuple_literal"
                | "block"
        )
}

fn completion_call_end(source: &str, cursor_byte: usize) -> usize {
    let mut start = cursor_byte;
    while source
        .as_bytes()
        .get(start)
        .is_some_and(u8::is_ascii_whitespace)
    {
        start += 1;
    }
    if source.as_bytes().get(start) != Some(&b'(') {
        return cursor_byte;
    }
    let mut depth = 0usize;
    for (offset, ch) in source[start..].char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return start + offset + 1;
                }
            }
            _ => {}
        }
    }
    cursor_byte
}

fn completion_probe_source(source: &str, context: &MemberCompletionContext) -> String {
    let after = source[context.probe_end_byte..].trim_start().chars().next();
    let replacement = if matches!(after, Some(';' | ',' | ')' | ']' | '}')) {
        ""
    } else {
        ";"
    };
    let mut probe = source.to_owned();
    probe.replace_range(context.dot_byte..context.probe_end_byte, replacement);
    probe
}

fn completion_probe_documents(uri: &str, source: &str, cursor_byte: usize) -> Vec<Document> {
    let mut documents = vec![compute(uri, source)];
    if documents[0].analysis.is_some() {
        return documents;
    }
    for probe in completion_scope_probe_sources(source, cursor_byte) {
        let document = compute(uri, &probe);
        let succeeded = document.analysis.is_some();
        documents.push(document);
        if succeeded {
            break;
        }
    }
    documents
}

fn completion_scope_probe_sources(source: &str, cursor_byte: usize) -> Vec<String> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_solar::LANGUAGE.into())
        .expect("Solar grammar must load");
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };
    let Some(mut node) = tree
        .root_node()
        .descendant_for_byte_range(cursor_byte.saturating_sub(1), cursor_byte.min(source.len()))
    else {
        return Vec::new();
    };
    while node.kind() != "block" {
        let Some(parent) = node.parent() else {
            return Vec::new();
        };
        node = parent;
    }

    let mut block = node;
    let mut probe = source.as_bytes().to_vec();
    let mut sources = Vec::new();
    loop {
        let mut owner = block;
        let outer_block = loop {
            let Some(parent) = owner.parent() else {
                return sources;
            };
            if matches!(
                parent.kind(),
                "function_def" | "method_def" | "closure_expr"
            ) {
                return sources;
            }
            if parent.kind() == "block" {
                break parent;
            }
            owner = parent;
        };

        blank_probe_range(&mut probe, owner.start_byte(), block.start_byte() + 1);
        blank_probe_range(&mut probe, block.end_byte() - 1, owner.end_byte());
        if block.child_by_field_name("tail").is_some() {
            probe[block.end_byte() - 1] = b';';
        }
        sources.push(String::from_utf8(probe.clone()).unwrap());
        block = outer_block;
    }
}

fn blank_probe_range(source: &mut [u8], start: usize, end: usize) {
    for byte in &mut source[start..end] {
        if !matches!(*byte, b'\n' | b'\r') {
            *byte = b' ';
        }
    }
}

fn byte_position(source: &str, byte: usize) -> Value {
    let prefix = &source[..byte];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() as u32;
    let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
    json!({
        "line": line,
        "character": source[line_start..byte].encode_utf16().count(),
    })
}

fn ordinary_completions(
    source: &str,
    line: u32,
    character: u32,
    uri: &str,
    document: &Document,
) -> Vec<Value> {
    let Some(cursor_byte) = position_to_byte(source, line, character) else {
        return Vec::new();
    };
    if completion_is_in_comment_or_string(source, cursor_byte) {
        return Vec::new();
    }
    let mut prefix_start = cursor_byte;
    while prefix_start > 0 {
        let Some(ch) = source[..prefix_start].chars().next_back() else {
            break;
        };
        if ch == '_' || ch.is_alphanumeric() {
            prefix_start -= ch.len_utf8();
        } else {
            break;
        }
    }
    let prefix = &source[prefix_start..cursor_byte];
    let line_start = source[..prefix_start]
        .rfind('\n')
        .map_or(0, |index| index + 1);
    let start_character = source[line_start..prefix_start].encode_utf16().count() as u32;
    let replace_range = json!({
        "start": { "line": line, "character": start_character },
        "end": { "line": line, "character": character },
    });
    let probe;
    let tooling_document = if document.completion_symbols.is_empty() {
        let line_prefix = &source[line_start..prefix_start];
        let replacement =
            if line_prefix.trim().is_empty() && !source[..prefix_start].trim_end().ends_with('{') {
                ""
            } else {
                "0"
            };
        let mut probe_source = source.to_owned();
        probe_source.replace_range(prefix_start..cursor_byte, replacement);
        probe = compute(uri, &probe_source);
        &probe
    } else {
        document
    };
    let mut seen = HashSet::new();
    let mut items = Vec::new();

    for (label, detail, kind) in visible_bindings(source, cursor_byte, tooling_document) {
        if label.starts_with(prefix) && seen.insert((label.clone(), kind)) {
            let mut item = json!({
                "label": label,
                "kind": kind,
                "sortText": format!("0{label}"),
                "textEdit": { "range": replace_range, "newText": label },
            });
            if let Some(detail) = detail {
                item["detail"] = json!(detail);
            }
            items.push(item);
        }
    }
    for symbol in &tooling_document.completion_symbols {
        if symbol.label.starts_with(prefix) && seen.insert((symbol.label.clone(), symbol.kind)) {
            let mut item = json!({
                "label": symbol.label,
                "kind": symbol.kind,
                "sortText": format!("1{}", symbol.label),
                "textEdit": { "range": replace_range, "newText": symbol.label },
            });
            if let Some(detail) = &symbol.detail {
                item["detail"] = json!(detail);
            }
            if let Some(doc) = &symbol.documentation {
                item["documentation"] = json!({ "kind": "markdown", "value": doc });
            }
            items.push(item);
        }
    }

    if let Some(root_file) = tooling_document.source_map.root_file_id() {
        let mut aliases = tooling_document
            .module_imports
            .keys()
            .filter_map(|(file_id, alias)| (*file_id == root_file).then_some(alias))
            .collect::<Vec<_>>();
        aliases.sort();
        aliases.dedup();
        for alias in aliases {
            if alias.starts_with(prefix) && seen.insert((alias.clone(), 9)) {
                items.push(json!({
                    "label": alias,
                    "kind": 9,
                    "sortText": format!("1{alias}"),
                    "textEdit": { "range": replace_range, "newText": alias },
                }));
            }
        }
    }

    if let Ok(parsed) = solar::parser::parse(source) {
        for item in parsed.items {
            let ast::TopLevelItem::Import(import) = item else {
                continue;
            };
            let ast::ImportKind::Module(alias) = import.kind else {
                continue;
            };
            if alias.starts_with(prefix) && seen.insert((alias.clone(), 9)) {
                items.push(json!({
                    "label": alias,
                    "kind": 9,
                    "sortText": format!("1{alias}"),
                    "textEdit": { "range": replace_range, "newText": alias },
                }));
            }
        }
    }

    const KEYWORDS: &[&str] = &[
        "break", "catch", "const", "continue", "else", "enum", "false", "fn", "for", "from", "if",
        "import", "in", "let", "loop", "match", "method", "out", "pub", "return", "static",
        "struct", "true", "try", "unsafe", "while",
    ];
    for &keyword in KEYWORDS {
        if keyword.starts_with(prefix) && seen.insert((keyword.to_owned(), 14)) {
            items.push(json!({
                "label": keyword,
                "kind": 14,
                "sortText": format!("2{keyword}"),
                "textEdit": { "range": replace_range, "newText": keyword },
            }));
        }
    }
    items
}

fn completion_is_in_comment_or_string(source: &str, cursor_byte: usize) -> bool {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_solar::LANGUAGE.into())
        .expect("Solar grammar must load");
    let Some(tree) = parser.parse(source, None) else {
        return false;
    };
    let Some(mut node) = tree
        .root_node()
        .descendant_for_byte_range(cursor_byte.saturating_sub(1), cursor_byte)
    else {
        return false;
    };
    loop {
        if matches!(node.kind(), "comment" | "doc_comment" | "string_literal") {
            return true;
        }
        let Some(parent) = node.parent() else {
            return false;
        };
        node = parent;
    }
}

fn visible_bindings(
    source: &str,
    cursor_byte: usize,
    document: &Document,
) -> Vec<(String, Option<String>, u32)> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_solar::LANGUAGE.into())
        .expect("Solar grammar must load");
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };
    let mut bindings = Vec::new();
    collect_visible_bindings(
        tree.root_node(),
        source,
        cursor_byte,
        document,
        &mut bindings,
    );
    bindings.sort_by(|left, right| left.0.cmp(&right.0));
    bindings.dedup_by(|left, right| left.0 == right.0 && left.2 == right.2);
    bindings
}

fn collect_visible_bindings(
    node: Node<'_>,
    source: &str,
    cursor_byte: usize,
    document: &Document,
    out: &mut Vec<(String, Option<String>, u32)>,
) {
    if node.start_byte() < cursor_byte && is_binding_identifier(node) {
        if let Some(scope) = binding_scope(node)
            && scope.start_byte() <= cursor_byte
            && cursor_byte <= scope.end_byte()
        {
            let detail = document
                .completion_type_file_id()
                .and_then(|file_id| {
                    document
                        .completion_binding_signatures()
                        .get(&span_key(node_span(node, file_id)))
                })
                .map(|signatures| signatures.join(" | "));
            out.push((source[node.byte_range()].to_owned(), detail, 6));
        }
    } else if matches!(node.kind(), "function_def" | "const_def")
        && node.start_byte() < cursor_byte
        && node.parent().is_some_and(|parent| parent.kind() == "block")
        && let Some(name) = node.child_by_field_name("name")
        && node.parent().is_some_and(|scope| {
            scope.start_byte() <= cursor_byte && cursor_byte <= scope.end_byte()
        })
    {
        out.push((
            source[name.byte_range()].to_owned(),
            None,
            if node.kind() == "function_def" { 3 } else { 21 },
        ));
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_visible_bindings(child, source, cursor_byte, document, out);
    }
}

fn binding_scope(mut node: Node<'_>) -> Option<Node<'_>> {
    while let Some(parent) = node.parent() {
        match parent.kind() {
            "parameter" => {
                let function = parent.parent()?.parent()?;
                return function.child_by_field_name("body");
            }
            "let_statement" => {
                return parent.parent().filter(|scope| scope.kind() == "block");
            }
            "closure_param" => {
                let closure = parent.parent()?.parent()?;
                return (closure.kind() == "closure_expr")
                    .then(|| closure.child_by_field_name("body"))
                    .flatten();
            }
            "for_statement"
            | "reflect_fields_statement"
            | "reflect_fields_pair_statement"
            | "reflect_variant_statement"
            | "reflect_variant_pair_statement" => return parent.child_by_field_name("body"),
            "try_statement" => return parent.child_by_field_name("handler"),
            "match_arm" => return parent.child_by_field_name("body"),
            _ => node = parent,
        }
    }
    None
}

/// Returns the shortest explicit postfix adjustment that makes `actual` match
/// a method's declared receiver type.
fn expand_completion_aliases(
    ty: &ast::Type,
    aliases: &HashMap<ast::DefId, (Vec<String>, ast::Type)>,
    depth: usize,
) -> ast::Type {
    fn expand(
        ty: &ast::Type,
        aliases: &HashMap<ast::DefId, (Vec<String>, ast::Type)>,
        substitutions: &HashMap<String, ast::Type>,
        depth: usize,
    ) -> ast::Type {
        if depth > 64 {
            return ty.clone();
        }
        match ty {
            ast::Type::Named(id) => {
                if let Some(replacement) = substitutions.get(&id.name) {
                    return expand(replacement, aliases, substitutions, depth + 1);
                }
                aliases.get(id).map_or_else(
                    || ty.clone(),
                    |(_, target)| expand(target, aliases, substitutions, depth + 1),
                )
            }
            ast::Type::Generic { name, type_args } => {
                if let Some((parameters, target)) = aliases.get(name)
                    && parameters.len() == type_args.len()
                {
                    let mut nested = substitutions.clone();
                    for (parameter, argument) in parameters.iter().zip(type_args) {
                        nested.insert(
                            parameter.clone(),
                            expand(argument, aliases, substitutions, depth + 1),
                        );
                    }
                    return expand(target, aliases, &nested, depth + 1);
                }
                ast::Type::Generic {
                    name: name.clone(),
                    type_args: type_args
                        .iter()
                        .map(|argument| expand(argument, aliases, substitutions, depth + 1))
                        .collect(),
                }
            }
            ast::Type::Reference(inner) => {
                ast::Type::Reference(Box::new(expand(inner, aliases, substitutions, depth + 1)))
            }
            ast::Type::NullableReference(inner) => ast::Type::NullableReference(Box::new(expand(
                inner,
                aliases,
                substitutions,
                depth + 1,
            ))),
            ast::Type::Unique(inner) => {
                ast::Type::Unique(Box::new(expand(inner, aliases, substitutions, depth + 1)))
            }
            ast::Type::Slice(inner) => {
                ast::Type::Slice(Box::new(expand(inner, aliases, substitutions, depth + 1)))
            }
            ast::Type::FixedArray(inner, size) => ast::Type::FixedArray(
                Box::new(expand(inner, aliases, substitutions, depth + 1)),
                *size,
            ),
            ast::Type::Function {
                params,
                return_type,
            } => ast::Type::Function {
                params: params
                    .iter()
                    .map(|(name, ty)| (name.clone(), expand(ty, aliases, substitutions, depth + 1)))
                    .collect(),
                return_type: return_type
                    .as_ref()
                    .map(|ty| Box::new(expand(ty, aliases, substitutions, depth + 1))),
            },
            ast::Type::Tuple(types) => ast::Type::Tuple(
                types
                    .iter()
                    .map(|ty| expand(ty, aliases, substitutions, depth + 1))
                    .collect(),
            ),
            ast::Type::Infer => ast::Type::Infer,
        }
    }

    expand(ty, aliases, &HashMap::new(), depth)
}

struct MethodReceiverMatch {
    adjustment: String,
    substitutions: HashMap<String, typed_ast::Type>,
}

fn method_receiver_match(
    actual: &typed_ast::Type,
    expected: &ast::Type,
    type_parameters: &[String],
) -> Option<MethodReceiverMatch> {
    let candidates = ["", "&", "@", "@@", "@@@"];
    candidates.into_iter().find_map(|adjustment| {
        let adjusted = adjusted_receiver_type(actual, adjustment)?;
        let mut substitutions = HashMap::new();
        ast_type_matches(expected, &adjusted, type_parameters, &mut substitutions).then(|| {
            MethodReceiverMatch {
                adjustment: adjustment.to_owned(),
                substitutions,
            }
        })
    })
}

fn adjusted_receiver_type(actual: &typed_ast::Type, adjustment: &str) -> Option<typed_ast::Type> {
    let mut ty = actual.clone();
    for operator in adjustment.chars() {
        ty = match operator {
            '&' => typed_ast::Type::Ref(Box::new(ty)),
            '@' => match ty {
                typed_ast::Type::Ref(inner)
                | typed_ast::Type::RefUnsized(inner)
                | typed_ast::Type::NullableRef(inner)
                | typed_ast::Type::NullableRefUnsized(inner)
                | typed_ast::Type::Unique(inner)
                | typed_ast::Type::UniqueUnsized(inner) => *inner,
                _ => return None,
            },
            _ => unreachable!(),
        };
    }
    Some(ty)
}

fn ast_type_matches(
    expected: &ast::Type,
    actual: &typed_ast::Type,
    type_parameters: &[String],
    substitutions: &mut HashMap<String, typed_ast::Type>,
) -> bool {
    use typed_ast::Type;
    if *actual == Type::Never {
        return true;
    }
    match expected {
        ast::Type::Named(id) if type_parameters.contains(&id.name) => {
            match substitutions.get(&id.name) {
                Some(bound) => bound == actual,
                None => {
                    substitutions.insert(id.name.clone(), actual.clone());
                    true
                }
            }
        }
        ast::Type::Named(id) => primitive_type(&id.name).map_or_else(
            || {
                matches!(actual, Type::Struct(actual_id) | Type::Enum(actual_id) if actual_id.def == *id && actual_id.args.is_empty())
            },
            |primitive| primitive == *actual,
        ),
        ast::Type::Generic { name, type_args } => {
            let actual_id = match actual {
                Type::Struct(id) | Type::Enum(id) if id.def == *name => id,
                _ => return false,
            };
            type_args.len() == actual_id.args.len()
                && type_args
                    .iter()
                    .zip(&actual_id.args)
                    .all(|(expected, actual)| {
                        ast_type_matches(expected, actual, type_parameters, substitutions)
                    })
        }
        ast::Type::Reference(inner) => match actual {
            Type::Ref(actual) | Type::RefUnsized(actual) => {
                ast_type_matches(inner, actual, type_parameters, substitutions)
            }
            _ => false,
        },
        ast::Type::NullableReference(inner) => match actual {
            Type::NullableRef(actual)
            | Type::NullableRefUnsized(actual)
            | Type::Ref(actual)
            | Type::RefUnsized(actual) => {
                ast_type_matches(inner, actual, type_parameters, substitutions)
            }
            _ => false,
        },
        ast::Type::Unique(inner) => match actual {
            Type::Unique(actual) | Type::UniqueUnsized(actual) => {
                ast_type_matches(inner, actual, type_parameters, substitutions)
            }
            _ => false,
        },
        ast::Type::Slice(inner) => match actual {
            Type::Array(actual) | Type::FixedArray(actual, _) => {
                ast_type_matches(inner, actual, type_parameters, substitutions)
            }
            _ => false,
        },
        ast::Type::FixedArray(inner, _) => match actual {
            Type::Array(actual) | Type::FixedArray(actual, _) => {
                ast_type_matches(inner, actual, type_parameters, substitutions)
            }
            _ => false,
        },
        ast::Type::Function {
            params,
            return_type,
        } => match actual {
            Type::Function {
                params: actual_params,
                return_type: actual_return,
            } => {
                params.len() == actual_params.len()
                    && params.iter().zip(actual_params).all(|((_, expected), actual)| {
                        ast_type_matches(expected, actual, type_parameters, substitutions)
                    })
                    && return_type.as_deref().map_or_else(
                        || **actual_return == Type::Unit,
                        |expected| {
                            ast_type_matches(
                                expected,
                                actual_return,
                                type_parameters,
                                substitutions,
                            )
                        },
                    )
            }
            _ => false,
        },
        ast::Type::Tuple(expected) => match actual {
            Type::Struct(id)
                if id.def.file == ast::SYNTHETIC_FILE
                    && id.def.name == "0tuple" =>
            {
                expected.len() == id.args.len()
                    && expected.iter().zip(&id.args).all(|(expected, actual)| {
                        ast_type_matches(expected, actual, type_parameters, substitutions)
                    })
            }
            _ => false,
        },
        ast::Type::Infer => true,
    }
}

fn primitive_type(name: &str) -> Option<typed_ast::Type> {
    use typed_ast::Type;
    Some(match ast::PrimitiveType::from_name(name)? {
        ast::PrimitiveType::Int8 => Type::Int8,
        ast::PrimitiveType::Int16 => Type::Int16,
        ast::PrimitiveType::Int32 => Type::Int32,
        ast::PrimitiveType::Int64 => Type::Int64,
        ast::PrimitiveType::Int => Type::Int,
        ast::PrimitiveType::Uint8 => Type::Uint8,
        ast::PrimitiveType::Uint16 => Type::Uint16,
        ast::PrimitiveType::Uint32 => Type::Uint32,
        ast::PrimitiveType::Uint64 => Type::Uint64,
        ast::PrimitiveType::Uint => Type::Uint,
        ast::PrimitiveType::Float32 => Type::Float32,
        ast::PrimitiveType::Float64 => Type::Float64,
        ast::PrimitiveType::Bool => Type::Bool,
        ast::PrimitiveType::FileDesc => Type::FileDesc,
        ast::PrimitiveType::Any => Type::Any,
        ast::PrimitiveType::Unit => Type::Unit,
        ast::PrimitiveType::Never => Type::Never,
    })
}

fn expression_types_at(
    typed: &typed_ast::SourceFile,
    file_id: u32,
    mut target: SourceSpan,
) -> Vec<typed_ast::Type> {
    target.file_id = file_id;
    let mut types = HashSet::new();
    for function in typed.functions.values() {
        if function.def_span.file_id != file_id {
            continue;
        }
        for statement in &function.body {
            collect_expression_types(statement, target, &mut types);
        }
    }
    for item in &typed.statics {
        collect_expression_type(&item.init, target, &mut types);
    }
    types.into_iter().collect()
}

fn collect_expression_types(
    statement: &typed_ast::Statement,
    target: SourceSpan,
    out: &mut HashSet<typed_ast::Type>,
) {
    use typed_ast::StatementKind;
    match &statement.kind {
        StatementKind::Let { value, .. }
        | StatementKind::Expression(value)
        | StatementKind::Return(value) => collect_expression_type(value, target, out),
        StatementKind::Assignment {
            target: left,
            value,
        } => {
            collect_expression_type(left, target, out);
            collect_expression_type(value, target, out);
        }
        StatementKind::If {
            condition,
            body,
            else_body,
        } => {
            collect_expression_type(condition, target, out);
            for statement in body.iter().chain(else_body) {
                collect_expression_types(statement, target, out);
            }
        }
        StatementKind::While { condition, body } => {
            collect_expression_type(condition, target, out);
            for statement in body {
                collect_expression_types(statement, target, out);
            }
        }
        StatementKind::Break(value) => {
            if let Some(value) = value {
                collect_expression_type(value, target, out);
            }
        }
        StatementKind::Continue => {}
    }
}

fn collect_expression_type(
    expr: &typed_ast::Expr,
    target: SourceSpan,
    out: &mut HashSet<typed_ast::Type>,
) {
    use typed_ast::ExprKind;
    if span_key(expr.span) == span_key(target) {
        out.insert(expr.ty.clone());
    }
    match &expr.kind {
        ExprKind::Call { arguments, .. } | ExprKind::IntrinsicCall { arguments, .. } => {
            for argument in arguments {
                collect_expression_type(argument, target, out);
            }
        }
        ExprKind::CallIndirect { callee, arguments } => {
            collect_expression_type(callee, target, out);
            for argument in arguments {
                collect_expression_type(argument, target, out);
            }
        }
        ExprKind::FieldAccess { object, .. }
        | ExprKind::Deref(object)
        | ExprKind::Reference(object)
        | ExprKind::Unique(object)
        | ExprKind::Not(object)
        | ExprKind::ArraySizeCoerce { expr: object, .. } => {
            collect_expression_type(object, target, out)
        }
        ExprKind::Index { object, index } => {
            collect_expression_type(object, target, out);
            collect_expression_type(index, target, out);
        }
        ExprKind::Slice { object, start, end } => {
            collect_expression_type(object, target, out);
            collect_expression_type(start, target, out);
            collect_expression_type(end, target, out);
        }
        ExprKind::StructLiteral { fields, .. } => {
            for field in fields {
                collect_expression_type(&field.value, target, out);
            }
        }
        ExprKind::ArrayLiteral(values) => {
            for value in values {
                collect_expression_type(value, target, out);
            }
        }
        ExprKind::Block(statements) | ExprKind::Loop(statements) => {
            for statement in statements {
                collect_expression_types(statement, target, out);
            }
        }
        ExprKind::ArrayRepeat { element, count }
        | ExprKind::ArrayInit {
            init: element,
            count,
        } => {
            collect_expression_type(element, target, out);
            collect_expression_type(count, target, out);
        }
        ExprKind::BinaryOp { left, right, .. } => {
            collect_expression_type(left, target, out);
            collect_expression_type(right, target, out);
        }
        ExprKind::If {
            condition,
            then_body,
            else_body,
        } => {
            collect_expression_type(condition, target, out);
            for statement in then_body.iter().chain(else_body) {
                collect_expression_types(statement, target, out);
            }
        }
        ExprKind::EnumVariant { value, .. } => {
            if let Some(value) = value {
                collect_expression_type(value, target, out);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            collect_expression_type(scrutinee, target, out);
            for arm in arms {
                for statement in &arm.body {
                    collect_expression_types(statement, target, out);
                }
            }
        }
        ExprKind::Identifier(_)
        | ExprKind::FunctionRef(_)
        | ExprKind::FloatLiteral(_)
        | ExprKind::Global(_)
        | ExprKind::IntegerLiteral(_)
        | ExprKind::BooleanLiteral(_)
        | ExprKind::NullLiteral
        | ExprKind::Closure { .. } => {}
    }
}

type InlayHintRange = ((u32, u32), (u32, u32));

fn inlay_hint_range(params: &Value) -> Option<InlayHintRange> {
    Some((
        (
            params.pointer("/range/start/line")?.as_u64()? as u32,
            params.pointer("/range/start/character")?.as_u64()? as u32,
        ),
        (
            params.pointer("/range/end/line")?.as_u64()? as u32,
            params.pointer("/range/end/character")?.as_u64()? as u32,
        ),
    ))
}

/// Returns inferred-type hints for declarations whose annotations are omitted.
fn inlay_hints(source: &str, document: &Document, range: Option<InlayHintRange>) -> Vec<Value> {
    let Some((analysis, inferred_binding_types)) = document.inlay_type_facts() else {
        return Vec::new();
    };
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_solar::LANGUAGE.into())
        .expect("Solar grammar must load");
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };

    let mut return_types = HashMap::<SpanKey, Vec<String>>::new();
    for function in analysis.typed.functions.values() {
        if function.def_span.file_id == analysis.file_id
            && function.return_type != typed_ast::Type::Unit
        {
            return_types
                .entry(span_key(function.def_span))
                .or_default()
                .push(function.return_type.to_string());
        }
    }
    normalize_type_options(&mut return_types);

    let static_types: HashMap<&str, &typed_ast::Type> = analysis
        .typed
        .statics
        .iter()
        .filter(|item| item.id.file == analysis.file_id)
        .map(|item| (item.id.name.as_str(), &item.ty))
        .collect();
    let context = InlayHintContext {
        source,
        file_id: analysis.file_id,
        binding_types: inferred_binding_types,
        return_types: &return_types,
        static_types: &static_types,
        range,
    };
    let mut hints = Vec::new();
    collect_inlay_hints(tree.root_node(), &context, &mut hints);
    hints.sort_by_key(|hint| {
        let label = hint["label"].as_str().unwrap_or_default();
        let type_order = if label.starts_with(": ") { 0 } else { 1 };
        (
            hint["position"]["line"].as_u64().unwrap_or_default(),
            hint["position"]["character"].as_u64().unwrap_or_default(),
            type_order,
            label.to_owned(),
        )
    });
    hints.dedup();
    hints
}

struct InlayHintContext<'a> {
    source: &'a str,
    file_id: u32,
    binding_types: &'a HashMap<SpanKey, Vec<String>>,
    return_types: &'a HashMap<SpanKey, Vec<String>>,
    static_types: &'a HashMap<&'a str, &'a typed_ast::Type>,
    range: Option<InlayHintRange>,
}

fn collect_inlay_hints(node: Node<'_>, context: &InlayHintContext<'_>, out: &mut Vec<Value>) {
    match node.kind() {
        "let_statement" if node.child_by_field_name("type").is_none() => {
            if let Some(pattern) = node.child_by_field_name("pattern") {
                collect_pattern_inlay_hints(pattern, context, out);
            }
        }
        "parameter" if node.child_by_field_name("type").is_none() => {
            if let Some(pattern) = node.child_by_field_name("pattern") {
                collect_pattern_inlay_hints(pattern, context, out);
                let key = span_key(node_span(pattern, context.file_id));
                if pattern.kind() == "identifier"
                    && !context.binding_types.contains_key(&key)
                    && let Some(default) = node.child_by_field_name("default")
                    && let Some(ty) = inferred_literal_type(default, context.source)
                {
                    push_inlay_hint(
                        node_end_position(pattern, context.source),
                        format!(": {ty}"),
                        context,
                        out,
                    );
                }
            }
        }
        "closure_param" if node.child_by_field_name("type").is_none() => {
            if let Some(name) = node.child_by_field_name("name") {
                push_binding_inlay_hint(name, context, out);
            }
        }
        "for_statement" => {
            if let Some(variable) = node.child_by_field_name("variable") {
                push_binding_inlay_hint(variable, context, out);
            }
        }
        "try_statement" if node.child_by_field_name("binding_type").is_none() => {
            if let Some(binding) = node.child_by_field_name("binding") {
                push_binding_inlay_hint(binding, context, out);
            }
        }
        "match_arm" => {
            if let Some(pattern) = node.child_by_field_name("pattern") {
                collect_pattern_inlay_hints(pattern, context, out);
            }
        }
        "reflect_fields_statement" | "reflect_fields_pair_statement" => {
            if let Some(pattern) = node.child_by_field_name("variable") {
                collect_pattern_inlay_hints(pattern, context, out);
            }
        }
        "reflect_variant_statement" | "reflect_variant_pair_statement" => {
            if let Some(pattern) = node.child_by_field_name("pattern") {
                collect_pattern_inlay_hints(pattern, context, out);
            }
        }
        "const_def" if node.child_by_field_name("type").is_none() => {
            if let (Some(name), Some(value)) = (
                node.child_by_field_name("name"),
                node.child_by_field_name("value"),
            ) && let Some(ty) = inferred_literal_type(value, context.source)
            {
                push_inlay_hint(
                    node_end_position(name, context.source),
                    format!(": {ty}"),
                    context,
                    out,
                );
            }
        }
        "static_def" if node.child_by_field_name("type").is_none() => {
            if let Some(name) = node.child_by_field_name("name") {
                let text = &context.source[name.byte_range()];
                if let Some(ty) = context.static_types.get(text) {
                    push_inlay_hint(
                        node_end_position(name, context.source),
                        format!(": {ty}"),
                        context,
                        out,
                    );
                }
            }
        }
        "function_def" | "method_def" if node.child_by_field_name("return_type").is_none() => {
            push_return_inlay_hint(node, function_return_anchor(node), context, out);
        }
        "closure_expr" if node.child_by_field_name("return_type").is_none() => {
            let anchor = named_child(node, "closure_param_list").or_else(|| {
                (0..node.child_count())
                    .filter_map(|index| node.child(index))
                    .find(|child| child.kind() == "\\")
            });
            push_return_inlay_hint(node, anchor, context, out);
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_inlay_hints(child, context, out);
    }
}

fn function_return_anchor(node: Node<'_>) -> Option<Node<'_>> {
    let body_start = node.child_by_field_name("body")?.start_byte();
    (0..node.child_count())
        .filter_map(|index| node.child(index))
        .rfind(|child| child.kind() == ")" && child.end_byte() <= body_start)
}

fn push_return_inlay_hint(
    declaration: Node<'_>,
    anchor: Option<Node<'_>>,
    context: &InlayHintContext<'_>,
    out: &mut Vec<Value>,
) {
    let Some(anchor) = anchor else {
        return;
    };
    let key = span_key(node_span(declaration, context.file_id));
    let Some(types) = context.return_types.get(&key) else {
        return;
    };
    push_inlay_hint(
        node_end_position(anchor, context.source),
        format_type_hint(" -> ", types),
        context,
        out,
    );
}

fn collect_pattern_inlay_hints(
    pattern: Node<'_>,
    context: &InlayHintContext<'_>,
    out: &mut Vec<Value>,
) {
    if pattern.kind() == "identifier" {
        push_binding_inlay_hint(pattern, context, out);
        return;
    }
    if matches!(pattern.kind(), "variant_pattern" | "wildcard_pattern") {
        let field = if pattern.kind() == "variant_pattern" {
            "binding"
        } else {
            "name"
        };
        if let Some(binding) = pattern.child_by_field_name(field) {
            push_binding_inlay_hint(binding, context, out);
        }
        return;
    }
    if pattern.kind() == "struct_pattern_field" {
        if let Some(inner) = pattern.child_by_field_name("pattern") {
            collect_pattern_inlay_hints(inner, context, out);
        } else if let Some(name) = pattern.child_by_field_name("field_name") {
            push_binding_inlay_hint(name, context, out);
        }
        return;
    }
    let mut cursor = pattern.walk();
    for child in pattern.named_children(&mut cursor) {
        if matches!(
            pattern.kind(),
            "tuple_pattern" | "array_pattern" | "match_pattern"
        ) || pattern.kind() == "struct_pattern" && child.kind() == "struct_pattern_field"
        {
            collect_pattern_inlay_hints(child, context, out);
        }
    }
}

fn push_binding_inlay_hint(
    binding: Node<'_>,
    context: &InlayHintContext<'_>,
    out: &mut Vec<Value>,
) {
    if &context.source[binding.byte_range()] == "_" {
        return;
    }
    let key = span_key(node_span(binding, context.file_id));
    let Some(types) = context.binding_types.get(&key) else {
        return;
    };
    push_inlay_hint(
        node_end_position(binding, context.source),
        format_type_hint(": ", types),
        context,
        out,
    );
}

fn push_inlay_hint(
    position: (u32, u32),
    label: String,
    context: &InlayHintContext<'_>,
    out: &mut Vec<Value>,
) {
    if context
        .range
        .is_some_and(|(start, end)| position < start || position >= end)
    {
        return;
    }
    out.push(json!({
        "position": { "line": position.0, "character": position.1 },
        "label": label,
        "kind": 1,
    }));
}

fn node_end_position(node: Node<'_>, source: &str) -> (u32, u32) {
    let end = node.end_position();
    let line = source.lines().nth(end.row).unwrap_or("");
    (end.row as u32, utf16_column(line, end.column))
}

fn format_type_hint(prefix: &str, types: &[String]) -> String {
    format!("{prefix}{}", types.join(" | "))
}

fn normalize_type_options(types: &mut HashMap<SpanKey, Vec<String>>) {
    for options in types.values_mut() {
        options.sort();
        options.dedup();
    }
}

fn inferred_literal_type(node: Node<'_>, source: &str) -> Option<String> {
    let text = &source[node.byte_range()];
    match node.kind() {
        "integer_literal" => Some(
            [
                ("i8", "Int8"),
                ("i16", "Int16"),
                ("i32", "Int32"),
                ("i64", "Int64"),
                ("u8", "Uint8"),
                ("u16", "Uint16"),
                ("u32", "Uint32"),
                ("u64", "Uint64"),
                ("u", "Uint"),
            ]
            .into_iter()
            .find_map(|(suffix, ty)| text.ends_with(suffix).then_some(ty))
            .unwrap_or("Int")
            .to_owned(),
        ),
        "float_literal" => Some(if text.ends_with("f32") {
            "Float32".to_owned()
        } else {
            "Float64".to_owned()
        }),
        "boolean_literal" => Some("Bool".to_owned()),
        "char_literal" => Some("Uint8".to_owned()),
        "string_literal" => Some("[Uint8]".to_owned()),
        "null_expr" => {
            let type_args = node.child_by_field_name("type_args")?;
            let ty = first_named_code_child(type_args)?;
            Some(format!("&?{}", &source[ty.byte_range()]))
        }
        "reference_expr" => Some(format!(
            "&{}",
            inferred_literal_type(node.child_by_field_name("operand")?, source)?
        )),
        "unique_expr" => Some(format!(
            "^{}",
            inferred_literal_type(node.child_by_field_name("operand")?, source)?
        )),
        "array_literal" => {
            let element = if let Some(type_args) = node.child_by_field_name("type_args") {
                let ty = first_named_code_child(type_args)?;
                source[ty.byte_range()].to_owned()
            } else {
                let list = named_child(node, "element_list")?;
                inferred_literal_type(first_named_code_child(list)?, source)?
            };
            Some(format!("[{element}]"))
        }
        "array_repeat" => Some(format!(
            "[{}]",
            inferred_literal_type(node.child_by_field_name("element")?, source)?
        )),
        "parenthesized_expression" => inferred_literal_type(first_named_code_child(node)?, source),
        _ => None,
    }
}

fn first_named_code_child(node: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|child| !matches!(child.kind(), "comment" | "doc_comment"))
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
    let context = DiagnosticContext {
        root_uri,
        root_source,
        root_path: root_path.as_deref(),
        source_map,
    };
    for error in errors {
        let mut chain = Vec::new();
        collect_compile_error_chain(error, &mut chain);
        for (index, error) in chain.iter().enumerate() {
            let related = chain
                .iter()
                .enumerate()
                .filter(|(related_index, _)| *related_index != index)
                .map(|(related_index, related_error)| {
                    let (uri, source) = diagnostic_source(related_error, &context);
                    json!({
                        "location": {
                            "uri": uri,
                            "range": compile_error_range(related_error.span, source),
                        },
                        "message": if related_index < index {
                            related_error.message.as_str()
                        } else {
                            "from"
                        },
                    })
                })
                .collect();
            let (uri, source) = diagnostic_source(error, &context);
            diagnostics
                .entry(uri)
                .or_default()
                .push(compile_error_diagnostic(
                    error,
                    source,
                    if index + 1 == chain.len() { 1 } else { 3 },
                    related,
                ));
        }
    }
    diagnostics
}

struct DiagnosticContext<'a> {
    root_uri: &'a str,
    root_source: &'a str,
    root_path: Option<&'a std::path::Path>,
    source_map: &'a SourceMap,
}

fn collect_compile_error_chain<'a>(error: &'a CompileError, chain: &mut Vec<&'a CompileError>) {
    if let Some(cause) = &error.caused_by {
        collect_compile_error_chain(cause, chain);
    }
    chain.push(error);
}

fn diagnostic_source<'a>(
    error: &CompileError,
    context: &DiagnosticContext<'a>,
) -> (String, &'a str) {
    // Parsing can fail before resolve_source has recorded root_file_id, so
    // also recognize the root by its canonical filename in the SourceMap.
    let mapped_source = context.source_map.get(error.span.file_id);
    let is_root = context.source_map.root_file_id() == Some(error.span.file_id)
        || mapped_source.is_some_and(|(filename, _)| {
            let path = PathBuf::from(filename);
            let path = path.canonicalize().unwrap_or(path);
            context.root_path == Some(path.as_path())
        });
    let (uri, file_source) = if is_root {
        (context.root_uri.to_owned(), context.root_source)
    } else if let Some((filename, source)) = mapped_source {
        (path_to_file_uri(filename), source)
    } else {
        (context.root_uri.to_owned(), context.root_source)
    };
    (uri, file_source)
}

fn compile_error_range(span: SourceSpan, source: &str) -> Value {
    let position = |pos: ast::SourcePos| {
        let line = source.lines().nth(pos.line as usize).unwrap_or("");
        json!({
            "line": pos.line,
            "character": utf16_column(line, pos.col as usize),
        })
    };
    json!({
        "start": position(span.start),
        "end": position(span.end),
    })
}

fn compile_error_diagnostic(
    error: &CompileError,
    source: &str,
    severity: u32,
    related: Vec<Value>,
) -> Value {
    let mut diagnostic = json!({
        "range": compile_error_range(error.span, source),
        "severity": severity,
        "source": "solar",
        "message": error.message,
    });
    if !related.is_empty() {
        diagnostic["relatedInformation"] = json!(related);
    }
    diagnostic
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
        match target {
            SymbolTarget::Source(target) => {
                let target_key = span_key(target);
                if !seen.insert(target_key) {
                    continue;
                }
                if let Some(signatures) = document.binding_signatures.get(&target_key) {
                    entries.extend(
                        signatures
                            .iter()
                            .map(|signature| format!("```solar\n{signature}\n```")),
                    );
                    continue;
                }
                let signature = document
                    .signatures
                    .get(&target_key)
                    .cloned()
                    .or_else(|| span_source_text(target, &document.source_map));
                let doc = document.docs.get(&target_key);
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
            SymbolTarget::BuiltIn(signature) => {
                entries.push(format!("```solar\n{signature}\n```\n\nbuilt-in"));
            }
        }
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

fn span_key_contains(outer: SpanKey, inner: SpanKey) -> bool {
    outer.0 == inner.0
        && (outer.1, outer.2) <= (inner.1, inner.2)
        && (inner.3, inner.4) <= (outer.3, outer.4)
}

fn collect_docs(ast: &solar::resolved_ast::SourceFile) -> HashMap<SpanKey, String> {
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

fn collect_signatures(
    ast: &solar::resolved_ast::SourceFile,
    source_map: &SourceMap,
) -> HashMap<SpanKey, String> {
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
    for target in targets {
        if let SymbolTarget::Source(span) = target
            && let Some(location) = span_to_location(span, &document.source_map)
        {
            locations.push(location);
        }
    }
    match locations.len() {
        0 => None,
        1 => locations.pop(),
        _ => Some(Value::Array(locations)),
    }
}

/// A source declaration or a compiler-provided signature resolved at a cursor.
enum SymbolTarget {
    Source(SourceSpan),
    BuiltIn(String),
}

/// Resolves a cursor position to source declarations and compiler-provided items.
fn symbol_targets(
    source: &str,
    line: u32,
    character: u32,
    document: &Document,
) -> Vec<SymbolTarget> {
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
    if node.kind() == "string_literal" {
        return import_path_definition(node, source, document)
            .map(SymbolTarget::Source)
            .into_iter()
            .collect();
    }
    let operator = operator_definition_context(node, source);
    if node.kind() != "identifier" && operator.is_none() {
        return Vec::new();
    }
    let (name, cursor, operator_anchor) = if let Some((name, anchor)) = operator {
        let start = node.start_position();
        (name, (start.row as u32, start.column as u32), Some(anchor))
    } else {
        let start = node.start_position();
        (
            &source[node.byte_range()],
            (start.row as u32, start.column as u32),
            None,
        )
    };

    if operator_anchor.is_none() {
        if let Some(parent) = declaration_parent(node)
            && let Some(file_id) = document.source_map.root_file_id()
        {
            return vec![SymbolTarget::Source(node_span(parent, file_id))];
        }
        if let Some(target) = import_fragment_definition(node, source, document) {
            return vec![SymbolTarget::Source(target)];
        }
        if let Some(binding) = local_definition(node, name, source)
            && let Some(file_id) = document.source_map.root_file_id()
        {
            let target = node_span(binding, file_id);
            return vec![SymbolTarget::Source(target)];
        }
        if let Some(target) = type_definition(node, name, source, document) {
            return vec![SymbolTarget::Source(target)];
        }
        if let Some(target) = path_definition(node, name, source, document) {
            return vec![SymbolTarget::Source(target)];
        }
    }

    let anchor = operator_anchor.or_else(|| definition_anchor(node));
    let field_access = document.source_map.root_file_id().and_then(|file_id| {
        node.parent()
            .filter(|parent| {
                parent.kind() == "field_access"
                    && parent.child_by_field_name("field") == Some(node)
                    && !field_access_is_callee(node)
            })
            .map(|access| span_key(node_span(access, file_id)))
    });
    let generic_site = document
        .generic_bodies
        .iter()
        .any(|span| span_contains(*span, document.source_map.root_file_id(), cursor));

    // Precise pass: resolve the specific overload(s) via the typed AST. Only
    // calls in functions defined in this file can sit at the cursor, so the walk
    // is restricted to them (which also prunes the entire stdlib).
    let mut targets = Vec::new();
    if let Some(analysis) = &document.analysis {
        let mut finder = DefFinder {
            typed: &analysis.typed,
            root_file: analysis.file_id,
            cursor,
            name,
            anchor,
            field_access,
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

    let mut source_seen = HashSet::new();
    let mut built_in_seen = HashSet::new();
    targets.retain(|target| match target {
        SymbolTarget::Source(span) => source_seen.insert(span_key(*span)),
        SymbolTarget::BuiltIn(signature) => built_in_seen.insert(signature.clone()),
    });
    targets
}

fn operator_definition_context(node: Node<'_>, source: &str) -> Option<(&'static str, (u32, u32))> {
    let binary = node.parent()?;
    if binary.kind() != "binary_expression" || binary.child_by_field_name("operator") != Some(node)
    {
        return None;
    }
    let operator = match &source[node.byte_range()] {
        "+" => ast::BinOp::Add,
        "-" => ast::BinOp::Sub,
        "*" => ast::BinOp::Mul,
        "/" => ast::BinOp::Div,
        "%" => ast::BinOp::Mod,
        "==" => ast::BinOp::Eq,
        "!=" => ast::BinOp::Ne,
        "<" => ast::BinOp::Lt,
        "<=" => ast::BinOp::Le,
        ">" => ast::BinOp::Gt,
        ">=" => ast::BinOp::Ge,
        "&&" => ast::BinOp::And,
        "||" => ast::BinOp::Or,
        "&" => ast::BinOp::BitAnd,
        "|" => ast::BinOp::BitOr,
        "^" => ast::BinOp::BitXor,
        "<<" => ast::BinOp::Shl,
        ">>" => ast::BinOp::Shr,
        "++" => ast::BinOp::WrapAdd,
        "--" => ast::BinOp::WrapSub,
        "**" => ast::BinOp::WrapMul,
        _ => return None,
    };
    let start = binary.start_position();
    Some((
        operator.method_name(),
        (start.row as u32, start.column as u32),
    ))
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
        let owner_file = module_path_file(&segments[..index - 1], source, document);
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

    let expected_file = module_path_file(&segments[..index], source, document);
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
            module_path_file(&[module], source, document)
        }
        _ => return None,
    };
    document
        .type_defs
        .iter()
        .find_map(|(id, span)| (id.name == name && Some(id.file) == file).then_some(*span))
}

/// Resolves a module fragment in a qualified path to the import statement
/// that introduced it. Later fragments walk imports in the preceding module,
/// so `a::b::item` maps `a` to the root import and `b` to the import in `a`.
fn import_fragment_definition(
    node: Node<'_>,
    source: &str,
    document: &Document,
) -> Option<SourceSpan> {
    let parent = node.parent()?;
    let segments = if parent.kind() == "path_segment" {
        let path = parent.parent().filter(|path| {
            matches!(
                path.kind(),
                "path_expr" | "variant_pattern" | "unit_variant_pattern"
            )
        })?;
        let mut cursor = path.walk();
        path.named_children(&mut cursor)
            .filter(|child| child.kind() == "path_segment")
            .filter_map(|segment| segment.child_by_field_name("name"))
            .collect::<Vec<_>>()
    } else if matches!(
        parent.kind(),
        "qualified_type" | "struct_literal" | "struct_pattern"
    ) && parent.child_by_field_name("module") == Some(node)
    {
        vec![node]
    } else {
        return None;
    };
    let index = segments.iter().position(|segment| *segment == node)?;
    let mut file_id = document.source_map.root_file_id()?;
    for (segment_index, segment) in segments.iter().enumerate().take(index + 1) {
        let name = source.get(segment.byte_range())?;
        let import = document.module_imports.get(&(file_id, name.to_owned()))?;
        if segment_index == index {
            return Some(import.span);
        }
        file_id = import.file_id;
    }
    None
}

fn module_path_file(segments: &[Node<'_>], source: &str, document: &Document) -> Option<u32> {
    let mut file_id = document.source_map.root_file_id()?;
    for segment in segments {
        let name = source.get(segment.byte_range())?;
        file_id = document
            .module_imports
            .get(&(file_id, name.to_owned()))?
            .file_id;
    }
    Some(file_id)
}

fn import_path_definition(node: Node<'_>, source: &str, document: &Document) -> Option<SourceSpan> {
    if node.parent()?.kind() != "import_statement" {
        return None;
    }
    let path = source[node.byte_range()]
        .strip_prefix('"')?
        .strip_suffix('"')?;
    if path.starts_with('@') {
        return None;
    }
    let root_file = document.source_map.root_file_id()?;
    let (root_path, _) = document.source_map.get(root_file)?;
    let base = std::path::Path::new(root_path).parent()?;
    let file_id = document.source_map.file_id_for_path(&base.join(path))?;
    Some(SourceSpan {
        file_id,
        ..SourceSpan::default()
    })
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
        "match_pattern" => {
            let mut cursor = pattern.walk();
            pattern
                .named_children(&mut cursor)
                .find_map(|child| match_binding(child, name, source))
        }
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
    if node.kind() != "identifier" {
        return false;
    }
    let mut child = node;
    while let Some(parent) = child.parent() {
        let contains = |ancestor: Node<'_>| {
            ancestor.start_byte() <= node.start_byte() && node.end_byte() <= ancestor.end_byte()
        };
        match parent.kind() {
            "parameter" | "let_statement" => {
                return parent.child_by_field_name("pattern").is_some_and(contains);
            }
            "closure_param" => return parent.child_by_field_name("name") == Some(node),
            "for_statement"
            | "reflect_fields_statement"
            | "reflect_fields_pair_statement"
            | "reflect_variant_statement"
            | "reflect_variant_pair_statement" => {
                return parent
                    .child_by_field_name("variable")
                    .or_else(|| parent.child_by_field_name("pattern"))
                    .is_some_and(contains);
            }
            "try_statement" => return parent.child_by_field_name("binding") == Some(node),
            "variant_pattern" => return parent.child_by_field_name("binding") == Some(node),
            "wildcard_pattern" => return parent.child_by_field_name("name") == Some(node),
            "tuple_pattern" | "array_pattern" => {}
            "struct_pattern_field" => {
                if let Some(pattern) = parent.child_by_field_name("pattern") {
                    if !contains(pattern) {
                        return false;
                    }
                } else if parent.child_by_field_name("field_name") != Some(node) {
                    return false;
                }
            }
            "struct_pattern" => {
                if parent.child_by_field_name("name").is_some_and(contains)
                    || parent.child_by_field_name("module").is_some_and(contains)
                {
                    return false;
                }
            }
            _ => return false,
        }
        child = parent;
    }
    false
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

/// Finds typed symbol targets at a source position.
struct DefFinder<'a> {
    typed: &'a typed_ast::SourceFile,
    root_file: u32,
    cursor: (u32, u32),
    name: &'a str,
    anchor: Option<(u32, u32)>,
    field_access: Option<SpanKey>,
    generic_site: bool,
    function_defs: &'a HashMap<ast::DefId, Vec<SourceSpan>>,
    method_defs: &'a HashMap<String, Vec<SourceSpan>>,
    field_defs: &'a HashMap<(ast::DefId, String), SourceSpan>,
    variant_defs: &'a HashMap<(ast::DefId, String), SourceSpan>,
    type_defs: &'a HashMap<ast::DefId, SourceSpan>,
    global_defs: &'a HashMap<ast::DefId, SourceSpan>,
    field_init: bool,
    out: &'a mut Vec<SymbolTarget>,
}

fn function_signature(function: &typed_ast::FunctionDef) -> String {
    let kind = match (function.is_unsafe, function.id.method) {
        (true, true) => "unsafe method",
        (true, false) => "unsafe fn",
        (false, true) => "method",
        (false, false) => "fn",
    };
    call_signature(
        kind,
        &function.id.def.name,
        function.parameters.iter().map(|parameter| {
            let name = match &parameter.name {
                ast::Ident::User(name) | ast::Ident::Synthetic(name) => name.as_str(),
            };
            (name, &parameter.ty)
        }),
        &function.return_type,
    )
}

fn call_signature<'a>(
    kind: &str,
    name: &str,
    parameters: impl IntoIterator<Item = (&'a str, &'a typed_ast::Type)>,
    return_type: &typed_ast::Type,
) -> String {
    let parameters = parameters
        .into_iter()
        .map(|(name, ty)| format!("{name}: {ty}"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut signature = format!("{kind} {name}({parameters})");
    if *return_type != typed_ast::Type::Unit {
        signature.push_str(&format!(" -> {return_type}"));
    }
    signature
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
                let source_candidates = candidates
                    .iter()
                    .copied()
                    .filter(|span| span.file_id != ast::SYNTHETIC_FILE)
                    .map(SymbolTarget::Source)
                    .collect::<Vec<_>>();
                if !source_candidates.is_empty() {
                    self.out.extend(source_candidates);
                    return;
                }
            }
        }
        if let Some(def) = self.typed.functions.get(function) {
            if def.def_span.file_id == ast::SYNTHETIC_FILE {
                self.out
                    .push(SymbolTarget::BuiltIn(function_signature(def)));
            } else {
                self.out.push(SymbolTarget::Source(def.def_span));
            }
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
                    && self.field_access == Some(span_key(expr.span))
                    && let Some(owner) = struct_owner(&object.ty)
                    && let Some(span) = self.field_defs.get(&(owner, field.clone()))
                {
                    self.out.push(SymbolTarget::Source(*span));
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
                    self.out.push(SymbolTarget::Source(*span));
                }
                if id.def.name == self.name
                    && self.at(expr.span, self.anchor.unwrap_or(self.cursor))
                    && let Some(span) = self.type_defs.get(&id.def)
                {
                    self.out.push(SymbolTarget::Source(*span));
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
            ExprKind::BinaryOp { op, left, right } => {
                if op.method_name() == self.name
                    && self.at(expr.span, self.anchor.unwrap_or(self.cursor))
                {
                    let receiver = typed_ast::Type::Ref(Box::new(left.ty.clone()));
                    let other = typed_ast::Type::Ref(Box::new(right.ty.clone()));
                    self.out.push(SymbolTarget::BuiltIn(call_signature(
                        "method",
                        self.name,
                        [("self", &receiver), ("other", &other)],
                        &expr.ty,
                    )));
                }
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
                        self.out.push(SymbolTarget::Source(*span));
                    } else if enum_id.def.name == self.name
                        && let Some(span) = self.type_defs.get(&enum_id.def)
                    {
                        self.out.push(SymbolTarget::Source(*span));
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
            ExprKind::IntrinsicCall {
                intrinsic,
                arguments,
            } => {
                let name_matches = self.name == intrinsic.name()
                    || matches!(intrinsic, Intrinsic::Cast(..)) && self.name.starts_with("cast_");
                if name_matches && self.at(expr.span, self.anchor.unwrap_or(self.cursor)) {
                    self.out.push(SymbolTarget::BuiltIn(call_signature(
                        "fn",
                        self.name,
                        arguments.iter().map(|argument| ("_", &argument.ty)),
                        &expr.ty,
                    )));
                }
                for argument in arguments {
                    self.walk_expr(argument);
                }
            }
            ExprKind::Global(id) => {
                if id.name == self.name
                    && self.at(expr.span, self.anchor.unwrap_or(self.cursor))
                    && let Some(span) = self.global_defs.get(id)
                {
                    self.out.push(SymbolTarget::Source(*span));
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
    /// Exact local-binding span → concrete `name: Type` signatures.
    binding_signatures: HashMap<SpanKey, Vec<String>>,
    /// Exact local-binding span → concrete inferred type(s), used for inlay
    /// hints only when the corresponding syntax omits an annotation.
    inferred_binding_types: HashMap<SpanKey, Vec<String>>,
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
    /// Source method declarations used by receiver-aware completion. Unlike
    /// the monomorphized typed AST, this retains methods that have not yet been
    /// called and therefore includes every generic candidate.
    completion_methods: Vec<CompletionMethod>,
    /// Visible free-function declarations retained separately so signature
    /// help can present every overload before a partial call type-checks.
    completion_functions: Vec<CompletionFunction>,
    /// Visible top-level declarations and struct fields used by completion.
    completion_symbols: Vec<CompletionSymbol>,
    completion_fields: HashMap<ast::DefId, Vec<CompletionField>>,
    /// Public declarations and nested namespaces keyed by module file.
    namespace_symbols: HashMap<u32, Vec<CompletionSymbol>>,
    /// Type aliases expanded while matching method receiver declarations.
    completion_aliases: HashMap<ast::DefId, (Vec<String>, ast::Type)>,
    /// The resolved program retained for speculative generic-method
    /// monomorphization during member completion.
    resolved: Option<solar::resolved_ast::SourceFile>,
    /// Module aliases declared throughout the resolved import graph. Each
    /// entry records both the imported file and the import statement so path
    /// fragments can navigate through re-exported modules.
    module_imports: HashMap<(u32, String), ModuleImport>,
    /// `file_id` → path + source, to turn a definition's span into an LSP
    /// `Location` (URI + UTF-16 range) in whatever file it lives in.
    source_map: SourceMap,
    /// Type-check-derived facts for semantic highlighting. `None` when the
    /// buffer resolves but does not type-check — hover's docs survive either way.
    analysis: Option<Analysis>,
    /// Type-check-derived facts from the most recent successful revision. This
    /// is populated only while the current revision fails and is consulted only
    /// by completion and inlay hints.
    last_successful_types: Option<ResolvedTypeCache>,
}

struct ResolvedTypeCache {
    analysis: Analysis,
    binding_signatures: HashMap<SpanKey, Vec<String>>,
    inferred_binding_types: HashMap<SpanKey, Vec<String>>,
}

impl Document {
    fn take_resolved_types(&mut self) -> Option<ResolvedTypeCache> {
        if let Some(analysis) = self.analysis.take() {
            Some(ResolvedTypeCache {
                analysis,
                binding_signatures: std::mem::take(&mut self.binding_signatures),
                inferred_binding_types: std::mem::take(&mut self.inferred_binding_types),
            })
        } else {
            self.last_successful_types.take()
        }
    }

    fn completion_binding_signatures(&self) -> &HashMap<SpanKey, Vec<String>> {
        if self.analysis.is_some() {
            &self.binding_signatures
        } else {
            self.last_successful_types
                .as_ref()
                .map_or(&self.binding_signatures, |types| &types.binding_signatures)
        }
    }

    fn completion_type_file_id(&self) -> Option<u32> {
        self.analysis
            .as_ref()
            .map(|analysis| analysis.file_id)
            .or_else(|| {
                self.last_successful_types
                    .as_ref()
                    .map(|types| types.analysis.file_id)
            })
    }

    fn inlay_type_facts(&self) -> Option<(&Analysis, &HashMap<SpanKey, Vec<String>>)> {
        if let Some(analysis) = &self.analysis {
            Some((analysis, &self.inferred_binding_types))
        } else {
            self.last_successful_types
                .as_ref()
                .map(|types| (&types.analysis, &types.inferred_binding_types))
        }
    }
}

/// Type-checker facts used by semantic highlighting.
struct Analysis {
    typed: typed_ast::SourceFile,
    file_id: u32,
    names: Names,
}

#[derive(Clone)]
struct CompletionMethod {
    def: ast::FunctionDef,
    detail: Option<String>,
}

#[derive(Clone)]
struct CompletionFunction {
    id: ast::DefId,
    def: ast::FunctionDef,
    visible_unqualified: bool,
}

#[derive(Clone)]
struct CompletionSymbol {
    label: String,
    kind: u32,
    detail: Option<String>,
    documentation: Option<String>,
}

#[derive(Clone)]
struct CompletionField {
    name: String,
    detail: Option<String>,
    is_pub: bool,
    file_id: u32,
}

struct CompletionCatalog {
    symbols: Vec<CompletionSymbol>,
    fields: HashMap<ast::DefId, Vec<CompletionField>>,
    definitions: HashMap<ast::DefId, Vec<CompletionSymbol>>,
}

#[derive(Clone, Copy)]
struct ModuleImport {
    file_id: u32,
    span: SourceSpan,
    is_pub: bool,
}

/// Returns cached analysis for a document.
fn cached<'a>(cache: &'a mut HashMap<String, Document>, uri: &str, source: &str) -> &'a Document {
    if !cache.contains_key(uri) {
        let document = compute(uri, source);
        update_cached_document(cache, uri, document);
    }
    &cache[uri]
}

fn update_cached_document(
    cache: &mut HashMap<String, Document>,
    uri: &str,
    mut document: Document,
) {
    if document.analysis.is_none() {
        document.last_successful_types = cache.get_mut(uri).and_then(Document::take_resolved_types);
    }
    cache.insert(uri.to_owned(), document);
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
    let module_imports = collect_module_imports(&source_map);
    let signatures = collect_signatures(&ast, &source_map);
    let completion_methods = ast
        .items
        .iter()
        .filter_map(|item| {
            let ast::TopLevelItem::Method(def) = item else {
                return None;
            };
            Some(CompletionMethod {
                def: def.clone(),
                detail: signatures.get(&span_key(def.span)).cloned(),
            })
        })
        .collect();
    let completion_functions = collect_completion_functions(&ast, &source_map);
    let CompletionCatalog {
        symbols: completion_symbols,
        fields: completion_fields,
        definitions: all_completion_symbols,
    } = collect_completion_catalog(&ast, &source_map, &signatures);
    let namespace_symbols =
        collect_namespace_completion_catalog(&source_map, &module_imports, &all_completion_symbols);
    let completion_aliases = ast
        .items
        .iter()
        .filter_map(|item| {
            let ast::TopLevelItem::TypeAlias(def) = item else {
                return None;
            };
            Some((
                ast::DefId::new(def.span.file_id, def.name.clone()),
                (def.type_params.clone(), def.target_type.clone()),
            ))
        })
        .collect();
    let (analysis, diagnostics) = match typed_ast::lower(&ast) {
        Ok(typed) => (analysis_from_typed(typed, &source_map), HashMap::new()),
        Err(error) => (
            None,
            diagnostics_from_errors(uri, source, std::slice::from_ref(&error), &source_map),
        ),
    };
    let binding_facts = analysis
        .as_ref()
        .map_or_else(BindingFacts::default, |analysis| {
            collect_binding_facts(analysis, source)
        });
    let document = Document {
        docs: collect_docs(&ast),
        signatures,
        binding_signatures: binding_facts.signatures,
        inferred_binding_types: binding_facts.types,
        function_defs: definition_catalog.function_defs,
        method_defs: definition_catalog.method_defs,
        field_defs: definition_catalog.field_defs,
        variant_defs: definition_catalog.variant_defs,
        type_defs: definition_catalog.type_defs,
        global_defs: definition_catalog.global_defs,
        generic_bodies: definition_catalog.generic_bodies,
        completion_methods,
        completion_functions,
        completion_symbols,
        completion_fields,
        namespace_symbols,
        completion_aliases,
        resolved: Some(ast),
        module_imports,
        analysis,
        last_successful_types: None,
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

fn collect_completion_functions(
    ast: &solar::resolved_ast::SourceFile,
    source_map: &SourceMap,
) -> Vec<CompletionFunction> {
    let visible_defs = visible_completion_defs(source_map);
    ast.items
        .iter()
        .filter_map(|item| {
            let ast::TopLevelItem::Function(def) = item else {
                return None;
            };
            let id = ast::DefId::new(def.span.file_id, def.name.clone());
            Some(CompletionFunction {
                visible_unqualified: visible_defs.contains(&id)
                    || def.span.file_id == ast::SYNTHETIC_FILE,
                id,
                def: def.clone(),
            })
        })
        .collect()
}

fn collect_completion_catalog(
    ast: &solar::resolved_ast::SourceFile,
    source_map: &SourceMap,
    signatures: &HashMap<SpanKey, String>,
) -> CompletionCatalog {
    use solar::ast::TopLevelItem;
    let visible_defs = visible_completion_defs(source_map);
    let mut symbols = Vec::new();
    let mut fields = HashMap::<ast::DefId, Vec<CompletionField>>::new();
    let mut all_symbols = HashMap::<ast::DefId, Vec<CompletionSymbol>>::new();
    for item in &ast.items {
        let (label, kind, doc, span) = match item {
            TopLevelItem::Function(def) => (&def.name, 3, &def.doc, def.span),
            TopLevelItem::Struct(def) => {
                fields.insert(
                    def.def_id.clone(),
                    def.fields
                        .iter()
                        .map(|field| CompletionField {
                            name: field.name.clone(),
                            detail: signatures.get(&span_key(field.span)).cloned(),
                            is_pub: field.is_pub,
                            file_id: field.span.file_id,
                        })
                        .collect(),
                );
                (&def.name, 22, &def.doc, def.span)
            }
            TopLevelItem::Enum(def) => (&def.name, 13, &def.doc, def.span),
            TopLevelItem::TypeAlias(def) => (&def.name, 7, &def.doc, def.span),
            TopLevelItem::Const(def) => (&def.name, 21, &def.doc, def.span),
            TopLevelItem::Static(def) => (&def.name, 6, &def.doc, def.span),
            TopLevelItem::Method(_) | TopLevelItem::Import(_) => continue,
        };
        let def = ast::DefId::new(span.file_id, label);
        let symbol = CompletionSymbol {
            label: label.clone(),
            kind,
            detail: signatures.get(&span_key(span)).cloned(),
            documentation: doc.clone(),
        };
        all_symbols
            .entry(def.clone())
            .or_default()
            .push(symbol.clone());
        if visible_defs.contains(&def) || span.file_id == ast::SYNTHETIC_FILE {
            symbols.push(symbol);
        }
    }
    symbols.sort_by(|left, right| left.label.cmp(&right.label));
    symbols.dedup_by(|left, right| {
        left.label == right.label && left.kind == right.kind && left.detail == right.detail
    });
    CompletionCatalog {
        symbols,
        fields,
        definitions: all_symbols,
    }
}

fn collect_namespace_completion_catalog(
    source_map: &SourceMap,
    module_imports: &HashMap<(u32, String), ModuleImport>,
    definitions: &HashMap<ast::DefId, Vec<CompletionSymbol>>,
) -> HashMap<u32, Vec<CompletionSymbol>> {
    let namespace_files: HashSet<u32> = module_imports
        .values()
        .map(|module| module.file_id)
        .collect();
    let mut export_cache = HashMap::new();
    let mut catalog = HashMap::new();
    for file_id in namespace_files {
        let mut symbols = Vec::new();
        for def in
            exported_completion_defs(file_id, source_map, &mut export_cache, &mut HashSet::new())
        {
            if let Some(entries) = definitions.get(&def) {
                symbols.extend(entries.iter().cloned());
            }
        }
        symbols.extend(
            module_imports
                .iter()
                .filter(|((owner_file, _), module)| *owner_file == file_id && module.is_pub)
                .map(|((_, alias), _)| CompletionSymbol {
                    label: alias.clone(),
                    kind: 9,
                    detail: None,
                    documentation: None,
                }),
        );
        symbols.sort_by(|left, right| {
            (&left.label, left.kind, &left.detail).cmp(&(&right.label, right.kind, &right.detail))
        });
        symbols.dedup_by(|left, right| {
            left.label == right.label && left.kind == right.kind && left.detail == right.detail
        });
        catalog.insert(file_id, symbols);
    }
    catalog
}

fn visible_completion_defs(source_map: &SourceMap) -> HashSet<ast::DefId> {
    use solar::ast::{ImportKind, TopLevelItem};
    let mut visible = HashSet::new();
    let Some(root_file) = source_map.root_file_id() else {
        return visible;
    };

    // Every user file receives an implicit wildcard import from the std root.
    let mut export_cache = HashMap::new();
    let mut visiting = HashSet::new();
    visible.extend(exported_completion_defs(
        0,
        source_map,
        &mut export_cache,
        &mut visiting,
    ));

    let Some((_, source)) = source_map.get(root_file) else {
        return visible;
    };
    let Ok(parsed) = solar::parser::parse(source) else {
        return visible;
    };
    for item in parsed.items {
        match item {
            TopLevelItem::Function(def) | TopLevelItem::Method(def) => {
                visible.insert(ast::DefId::new(root_file, def.name));
            }
            TopLevelItem::Struct(def) => {
                visible.insert(ast::DefId::new(root_file, def.name));
            }
            TopLevelItem::Enum(def) => {
                visible.insert(ast::DefId::new(root_file, def.name));
            }
            TopLevelItem::TypeAlias(def) => {
                visible.insert(ast::DefId::new(root_file, def.name));
            }
            TopLevelItem::Const(def) => {
                visible.insert(ast::DefId::new(root_file, def.name));
            }
            TopLevelItem::Static(def) => {
                visible.insert(ast::DefId::new(root_file, def.name));
            }
            TopLevelItem::Import(import) => match import.kind {
                ImportKind::Named(names) => {
                    if let Some(imported_file) =
                        imported_completion_file(source_map, root_file, &import.path)
                    {
                        for name in names {
                            let Some(defining_file) =
                                imported_completion_name_file(source_map, imported_file, &name)
                            else {
                                continue;
                            };
                            visible.extend(
                                exported_completion_defs(
                                    defining_file,
                                    source_map,
                                    &mut export_cache,
                                    &mut visiting,
                                )
                                .into_iter()
                                .filter(|def| def.name == name.local_name()),
                            );
                        }
                    }
                }
                ImportKind::Wildcard => {
                    if let Some(file_id) =
                        imported_completion_file(source_map, root_file, &import.path)
                    {
                        visible.extend(exported_completion_defs(
                            file_id,
                            source_map,
                            &mut export_cache,
                            &mut visiting,
                        ));
                    }
                }
                ImportKind::Module(_) => {}
            },
        }
    }
    visible
}

fn exported_completion_defs(
    file_id: u32,
    source_map: &SourceMap,
    cache: &mut HashMap<u32, HashSet<ast::DefId>>,
    visiting: &mut HashSet<u32>,
) -> HashSet<ast::DefId> {
    use solar::ast::{ImportKind, TopLevelItem};
    if let Some(names) = cache.get(&file_id) {
        return names.clone();
    }
    if !visiting.insert(file_id) {
        return HashSet::new();
    }
    let mut defs = HashSet::new();
    if let Some((_, source)) = source_map.get(file_id)
        && let Ok(parsed) = solar::parser::parse(source)
    {
        for item in parsed.items {
            match item {
                TopLevelItem::Function(def) | TopLevelItem::Method(def) if def.is_pub => {
                    defs.insert(ast::DefId::new(file_id, def.name));
                }
                TopLevelItem::Struct(def) if def.is_pub => {
                    defs.insert(ast::DefId::new(file_id, def.name));
                }
                TopLevelItem::Enum(def) if def.is_pub => {
                    defs.insert(ast::DefId::new(file_id, def.name));
                }
                TopLevelItem::TypeAlias(def) if def.is_pub => {
                    defs.insert(ast::DefId::new(file_id, def.name));
                }
                TopLevelItem::Const(def) if def.is_pub => {
                    defs.insert(ast::DefId::new(file_id, def.name));
                }
                TopLevelItem::Static(def) if def.is_pub => {
                    defs.insert(ast::DefId::new(file_id, def.name));
                }
                TopLevelItem::Import(import) if import.is_pub => match import.kind {
                    ImportKind::Named(imported) => {
                        if let Some(imported_file) =
                            imported_completion_file(source_map, file_id, &import.path)
                        {
                            for name in imported {
                                let Some(defining_file) =
                                    imported_completion_name_file(source_map, imported_file, &name)
                                else {
                                    continue;
                                };
                                defs.extend(
                                    exported_completion_defs(
                                        defining_file,
                                        source_map,
                                        cache,
                                        visiting,
                                    )
                                    .into_iter()
                                    .filter(|def| def.name == name.local_name()),
                                );
                            }
                        }
                    }
                    ImportKind::Wildcard => {
                        if let Some(imported_file) =
                            imported_completion_file(source_map, file_id, &import.path)
                        {
                            defs.extend(exported_completion_defs(
                                imported_file,
                                source_map,
                                cache,
                                visiting,
                            ));
                        }
                    }
                    ImportKind::Module(_) => {}
                },
                _ => {}
            }
        }
    }
    visiting.remove(&file_id);
    cache.insert(file_id, defs.clone());
    defs
}

fn imported_completion_name_file(
    source_map: &SourceMap,
    imported_file: u32,
    name: &ast::ImportName,
) -> Option<u32> {
    use solar::ast::{ImportKind, TopLevelItem};
    let mut file_id = imported_file;
    for segment in name.module_segments() {
        let (_, source) = source_map.get(file_id)?;
        let parsed = solar::parser::parse(source).ok()?;
        let module = parsed.items.into_iter().find_map(|item| {
            let TopLevelItem::Import(import) = item else {
                return None;
            };
            matches!(&import.kind, ImportKind::Module(alias) if alias == segment).then_some(import)
        })?;
        file_id = imported_completion_file(source_map, file_id, &module.path)?;
    }
    Some(file_id)
}

fn imported_completion_file(
    source_map: &SourceMap,
    from_file: u32,
    import_path: &str,
) -> Option<u32> {
    if import_path == "@std" {
        return Some(0);
    }
    if import_path.starts_with('@') {
        return None;
    }
    let (filename, _) = source_map.get(from_file)?;
    let base = std::path::Path::new(filename).parent()?;
    source_map.file_id_for_path(&base.join(import_path))
}

fn collect_definition_catalog(
    ast: &solar::resolved_ast::SourceFile,
    root_file: Option<u32>,
) -> DefinitionCatalog {
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

fn collect_module_imports(source_map: &SourceMap) -> HashMap<(u32, String), ModuleImport> {
    use solar::ast::{ImportKind, TopLevelItem};
    let mut modules = HashMap::new();
    let Some(root_file) = source_map.root_file_id() else {
        return modules;
    };
    let mut pending = vec![root_file, 0];
    let mut visited = HashSet::new();

    while let Some(file_id) = pending.pop() {
        if !visited.insert(file_id) {
            continue;
        }
        let Some((_, source)) = source_map.get(file_id) else {
            continue;
        };
        let Ok(parsed) = solar::parser::parse(source) else {
            continue;
        };

        for item in parsed.items {
            let TopLevelItem::Import(mut import) = item else {
                continue;
            };
            if import.path == "@intrinsics" {
                continue;
            }
            let Some(imported_file) = imported_completion_file(source_map, file_id, &import.path)
            else {
                continue;
            };
            pending.push(imported_file);
            if let ImportKind::Module(alias) = import.kind {
                import.span.file_id = file_id;
                modules.insert(
                    (file_id, alias),
                    ModuleImport {
                        file_id: imported_file,
                        span: import.span,
                        is_pub: import.is_pub,
                    },
                );
            }
        }
    }

    // User files implicitly wildcard-import the standard-library root. Module
    // aliases are not ordinary exported definitions, so mirror the resolver's
    // propagation of public module imports into the root namespace. Do the same
    // for explicit wildcard imports in the root file.
    let mut wildcard_sources = vec![0];
    if let Some((_, source)) = source_map.get(root_file)
        && let Ok(parsed) = solar::parser::parse(source)
    {
        for item in parsed.items {
            let TopLevelItem::Import(import) = item else {
                continue;
            };
            if matches!(import.kind, ImportKind::Wildcard)
                && let Some(file_id) = imported_completion_file(source_map, root_file, &import.path)
            {
                wildcard_sources.push(file_id);
            }
        }
    }
    for source_file in wildcard_sources {
        let Some((_, source)) = source_map.get(source_file) else {
            continue;
        };
        let Ok(parsed) = solar::parser::parse(source) else {
            continue;
        };
        for item in parsed.items {
            let TopLevelItem::Import(import) = item else {
                continue;
            };
            if !import.is_pub {
                continue;
            }
            let ImportKind::Module(alias) = import.kind else {
                continue;
            };
            if let Some(module) = modules.get(&(source_file, alias.clone())).copied() {
                modules.entry((root_file, alias)).or_insert(module);
            }
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

type BindingDeclarationKey = (SpanKey, String);

#[derive(Default)]
struct BindingFacts {
    signatures: HashMap<SpanKey, Vec<String>>,
    types: HashMap<SpanKey, Vec<String>>,
}

fn collect_binding_facts(analysis: &Analysis, source: &str) -> BindingFacts {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_solar::LANGUAGE.into())
        .expect("Solar grammar must load");
    let Some(tree) = parser.parse(source, None) else {
        return BindingFacts::default();
    };
    let root = tree.root_node();
    let mut declarations = HashMap::new();
    collect_binding_declarations(root, analysis.file_id, source, &mut declarations);

    let mut collector = BindingSignatureCollector {
        file_id: analysis.file_id,
        scope: None,
        declarations: &declarations,
        facts: BindingFacts::default(),
    };
    for function in analysis.typed.functions.values() {
        if function.def_span.file_id != analysis.file_id {
            continue;
        }
        collector.scope = Some(function.def_span);
        for parameter in &function.parameters {
            collector.record_declaration(parameter.span, &parameter.name, &parameter.ty);
        }
        for statement in &function.body {
            collector.walk_statement(statement);
        }
    }
    collector.scope = None;
    for static_item in &analysis.typed.statics {
        collector.walk_expr(&static_item.init);
    }
    for signatures in collector.facts.signatures.values_mut() {
        signatures.sort();
        signatures.dedup();
    }
    normalize_type_options(&mut collector.facts.types);
    collector.facts
}

fn collect_binding_declarations(
    node: Node<'_>,
    file_id: u32,
    source: &str,
    out: &mut HashMap<BindingDeclarationKey, Vec<SpanKey>>,
) {
    let pattern = match node.kind() {
        "parameter" | "let_statement" => node.child_by_field_name("pattern"),
        "closure_param" => node.child_by_field_name("name"),
        "for_statement" | "reflect_fields_statement" | "reflect_fields_pair_statement" => {
            node.child_by_field_name("variable")
        }
        "reflect_variant_statement" | "reflect_variant_pair_statement" => {
            node.child_by_field_name("pattern")
        }
        "try_statement" => node.child_by_field_name("binding"),
        "match_arm" => node.child_by_field_name("pattern"),
        _ => None,
    };
    if let Some(pattern) = pattern {
        let container_node = if node.kind() == "match_arm" {
            node.child_by_field_name("body").unwrap_or(node)
        } else {
            node
        };
        let container = span_key(node_span(container_node, file_id));
        collect_pattern_declarations(pattern, container, file_id, source, out);
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_binding_declarations(child, file_id, source, out);
    }
}

fn collect_pattern_declarations(
    pattern: Node<'_>,
    container: SpanKey,
    file_id: u32,
    source: &str,
    out: &mut HashMap<BindingDeclarationKey, Vec<SpanKey>>,
) {
    if pattern.kind() == "identifier" {
        let name = source[pattern.byte_range()].to_owned();
        out.entry((container, name))
            .or_default()
            .push(span_key(node_span(pattern, file_id)));
        return;
    }
    if matches!(pattern.kind(), "variant_pattern" | "wildcard_pattern") {
        let field = if pattern.kind() == "variant_pattern" {
            "binding"
        } else {
            "name"
        };
        if let Some(binding) = pattern.child_by_field_name(field) {
            collect_pattern_declarations(binding, container, file_id, source, out);
        }
        return;
    }
    if pattern.kind() == "struct_pattern_field" {
        if let Some(inner) = pattern.child_by_field_name("pattern") {
            collect_pattern_declarations(inner, container, file_id, source, out);
        } else if let Some(name) = pattern.child_by_field_name("field_name") {
            collect_pattern_declarations(name, container, file_id, source, out);
        }
        return;
    }
    let mut cursor = pattern.walk();
    for child in pattern.named_children(&mut cursor) {
        if matches!(
            pattern.kind(),
            "tuple_pattern" | "array_pattern" | "match_pattern"
        ) || pattern.kind() == "struct_pattern" && child.kind() == "struct_pattern_field"
        {
            collect_pattern_declarations(child, container, file_id, source, out);
        }
    }
}

struct BindingSignatureCollector<'a> {
    file_id: u32,
    scope: Option<SourceSpan>,
    declarations: &'a HashMap<BindingDeclarationKey, Vec<SpanKey>>,
    facts: BindingFacts,
}

impl BindingSignatureCollector<'_> {
    fn insert(&mut self, target: SpanKey, name: &str, ty: &typed_ast::Type) {
        self.facts
            .signatures
            .entry(target)
            .or_default()
            .push(format!("{name}: {ty}"));
        self.facts
            .types
            .entry(target)
            .or_default()
            .push(ty.to_string());
    }

    fn record_declaration(
        &mut self,
        span: SourceSpan,
        ident: &solar::ast::Ident,
        ty: &typed_ast::Type,
    ) {
        let solar::ast::Ident::User(name) = ident else {
            return;
        };
        let span = SourceSpan {
            file_id: self.file_id,
            ..span
        };
        let key = (span_key(span), name.to_owned());
        let targets = self.declarations.get(&key).cloned().or_else(|| {
            let scope = span_key(self.scope?);
            let mut targets: Vec<SpanKey> = self
                .declarations
                .iter()
                .filter(|((container, candidate), _)| {
                    candidate == name && span_key_contains(scope, *container)
                })
                .flat_map(|(_, targets)| targets.iter().copied())
                .collect();
            targets.sort_unstable();
            targets.dedup();
            (targets.len() == 1).then_some(targets)
        });
        for target in targets.into_iter().flatten() {
            self.insert(target, name, ty);
        }
    }

    fn walk_statement(&mut self, statement: &typed_ast::Statement) {
        use typed_ast::StatementKind;
        match &statement.kind {
            StatementKind::Let { name, ty, value } => {
                self.record_declaration(statement.span, name, ty);
                self.walk_expr(value);
            }
            StatementKind::Expression(value) | StatementKind::Return(value) => {
                self.walk_expr(value)
            }
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
            ExprKind::Call { arguments, .. } | ExprKind::IntrinsicCall { arguments, .. } => {
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
            ExprKind::FieldAccess { object, .. }
            | ExprKind::Deref(object)
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
            ExprKind::StructLiteral { fields, .. } => {
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
            ExprKind::EnumVariant { value, .. } => {
                if let Some(value) = value {
                    self.walk_expr(value);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                self.walk_expr(scrutinee);
                for arm in arms {
                    let span = arm
                        .body
                        .first()
                        .map_or(expr.span, |statement| statement.span);
                    match &arm.pattern {
                        typed_ast::TypedPattern::Variant {
                            binding: Some((name, ty)),
                            ..
                        }
                        | typed_ast::TypedPattern::Wildcard(name, ty) => {
                            self.record_declaration(span, name, ty);
                        }
                        typed_ast::TypedPattern::IntegerLiteral(_) => {}
                        typed_ast::TypedPattern::Variant { binding: None, .. } => {}
                    }
                    for statement in &arm.body {
                        self.walk_statement(statement);
                    }
                }
            }
            ExprKind::Identifier(_)
            | ExprKind::Global(_)
            | ExprKind::FunctionRef(_)
            | ExprKind::FloatLiteral(_)
            | ExprKind::IntegerLiteral(_)
            | ExprKind::BooleanLiteral(_)
            | ExprKind::NullLiteral
            | ExprKind::Closure { .. } => {}
        }
    }
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
    if matches!(node.kind(), "type_params" | "function_type_params") {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            let identifier = match child.kind() {
                "identifier" => Some(child),
                "out_type_param" => child.child_by_field_name("name"),
                _ => None,
            };
            if let Some(identifier) = identifier {
                names.insert(source[identifier.byte_range()].to_owned());
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
        "identifier" => Some(refine(
            identifier_kind(node, parent_kind, text),
            text,
            context,
        )),
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

    fn hint_labels(hints: &[Value]) -> Vec<&str> {
        hints
            .iter()
            .map(|hint| hint["label"].as_str().unwrap())
            .collect()
    }

    fn completion_items(source: &str, needle: &str) -> Vec<Value> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/runtime/completion_probe.solar");
        completion_items_for_path(&path, source, needle)
    }

    fn completion_items_for_path(path: &std::path::Path, source: &str, needle: &str) -> Vec<Value> {
        let uri = format!("file://{}", path.display());
        let document = compute(&uri, source);
        let (line, character) = occurrence_position(source, needle, 0);
        completions(
            source,
            line,
            character + needle.encode_utf16().count() as u32,
            &uri,
            &document,
        )
    }

    fn signatures_at(source: &str, needle: &str) -> Value {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/runtime/signature_help_probe.solar");
        signatures_at_for_path(&path, source, needle)
    }

    fn signatures_at_for_path(path: &std::path::Path, source: &str, needle: &str) -> Value {
        let uri = format!("file://{}", path.display());
        let document = compute(&uri, source);
        let (line, character) = occurrence_position(source, needle, 0);
        signature_help(
            source,
            line,
            character + needle.encode_utf16().count() as u32,
            &uri,
            &document,
        )
        .expect("signature help")
    }

    #[test]
    fn signature_help_filters_function_overloads_by_completed_arguments() {
        let source = r#"
fn select(prefix: Int, value: Bool) {}
fn select(prefix: Bool, value: Int) {}
fn select(prefix: Int, value: Uint) {}
fn main() { select(1, ); }
"#;
        let help = signatures_at(source, "select(1, ");
        let signatures = help["signatures"].as_array().unwrap();

        assert_eq!(signatures.len(), 2, "{help}");
        assert!(
            signatures
                .iter()
                .any(|signature| signature["parameters"][1]["label"] == "value: Bool"),
            "{help}"
        );
        assert!(
            signatures
                .iter()
                .any(|signature| signature["parameters"][1]["label"] == "value: Uint"),
            "{help}"
        );
        assert!(signatures.iter().all(|signature| {
            signature["activeParameter"] == 1
                && signature["parameters"][0]["label"] == "prefix: Int"
        }));

        let help = signatures_at(source, "main() { select(");
        assert_eq!(help["signatures"].as_array().unwrap().len(), 3, "{help}");

        let source = source.replace("select(1, );", "select(1, ");
        let help = signatures_at(&source, "select(1, ");
        assert_eq!(help["signatures"].as_array().unwrap().len(), 2, "{help}");

        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/multi_file/pub_field/signature_help.solar");
        let source = "import lib from \"lib.solar\";\nfn main() { lib::make_pair(1, }\n";
        let help = signatures_at_for_path(&path, source, "lib::make_pair(1, ");
        assert_eq!(help["signatures"].as_array().unwrap().len(), 1, "{help}");
        assert_eq!(help["signatures"][0]["parameters"][1]["label"], "b: Int");
    }

    #[test]
    fn signature_help_filters_method_overloads_and_accounts_for_self() {
        let source = r#"
struct Box { value: Int }
struct Other { value: Int }
method update(self: Box, prefix: Int, value: Bool) {}
method update(self: Box, prefix: Bool, value: Int) {}
method update(self: Other, prefix: Int, value: Uint) {}
fn main() {
    let boxed = Box { value: 1 };
    boxed.update(1, );
}
"#;
        let help = signatures_at(source, "boxed.update(1, ");
        let signatures = help["signatures"].as_array().unwrap();

        assert_eq!(signatures.len(), 1, "{help}");
        assert_eq!(signatures[0]["parameters"][2]["label"], "value: Bool");
        assert_eq!(signatures[0]["activeParameter"], 2);

        let source = source.replace("boxed.update(1, );", "boxed.update(1, ");
        let help = signatures_at(&source, "boxed.update(1, ");
        assert_eq!(help["signatures"].as_array().unwrap().len(), 1, "{help}");
    }

    #[test]
    fn completion_suggests_receiver_reference_and_dereference_edits() {
        let source = r#"
struct Point { x: Int }
method inspect(self: Point) -> Int { self.x }
method set_x(self: &Point, value: Int) { self@.x = value; }
fn main() {
    let point = Point { x: 1 };
    point.se;
}
"#;
        let items = completion_items(source, "point.se");
        let set_x = items
            .iter()
            .find(|item| item["label"] == "set_x")
            .expect("set_x completion");
        assert_eq!(set_x["additionalTextEdits"][0]["newText"], "&");

        let source = source.replace("point.se;", "let reference = point&;\n    reference.ins;");
        let items = completion_items(&source, "reference.ins");
        let inspect = items
            .iter()
            .find(|item| item["label"] == "inspect")
            .expect("inspect completion");
        assert_eq!(inspect["additionalTextEdits"][0]["newText"], "@");
    }

    #[test]
    fn completion_unwraps_failed_enclosing_blocks_until_the_function_body() {
        let source = r#"
struct Point { value: Int }
method read(self: Point) -> Int { self.value }
fn main() {
    for ignored in true {
        while 1 {
            let point = Point { value: 1 };
            point.re;
        }
    }
}
"#;
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/runtime/completion_probe.solar");
        let uri = format!("file://{}", path.display());
        let document = compute(&uri, source);
        assert!(document.analysis.is_none());

        let (line, character) = occurrence_position(source, "point.re", 0);
        let items = completions(
            source,
            line,
            character + "point.re".len() as u32,
            &uri,
            &document,
        );
        assert!(
            items.iter().any(|item| item["label"] == "read"),
            "{items:?}"
        );
    }

    #[test]
    fn completion_includes_uninstantiated_generic_methods() {
        let source = r#"
struct Box#[T] { value: T }
method get#[T](self: Box#[T]) -> T { self.value }
fn main() {
    let boxed = Box#[Int] { value: 1 };
    boxed.ge;
}
"#;
        let items = completion_items(source, "boxed.ge");
        let get = items
            .iter()
            .find(|item| item["label"] == "get")
            .expect("generic get completion");
        assert!(get.get("additionalTextEdits").is_none());
        assert!(
            get["detail"]
                .as_str()
                .unwrap()
                .starts_with("method get#[T]")
        );

        let source = r#"
struct Item { value: Int }
type ItemRef = &Item;
method read(self: ItemRef) -> Int { self@.value }
fn main() {
    let item = Item { value: 1 };
    item.re;
}
"#;
        let items = completion_items(source, "item.re");
        let read = items
            .iter()
            .find(|item| item["label"] == "read")
            .expect("aliased receiver completion");
        assert_eq!(read["additionalTextEdits"][0]["newText"], "&");
    }

    #[test]
    fn completion_excludes_generic_methods_whose_specialization_does_not_typecheck() {
        let source = r#"
fn main() {
    [1, 2, 3].count_;
    println(0);
}
"#;
        let items = completion_items(source, "[1, 2, 3].count_");
        assert!(
            items
                .iter()
                .all(|item| !item["label"].as_str().unwrap().starts_with("count_"))
        );

        let blank_source = source.replace("[1, 2, 3].count_", "[1, 2, 3].");
        let items = completion_items(&blank_source, "[1, 2, 3].");
        assert!(
            items
                .iter()
                .all(|item| !item["label"].as_str().unwrap().starts_with("count_"))
        );
        assert!(items.iter().any(|item| item["label"] == "len"));

        let valid_source = source.replace("[1, 2, 3].count_", "[1, 2, 3].le");
        let items = completion_items(&valid_source, "[1, 2, 3].le");
        assert!(items.iter().any(|item| item["label"] == "len"));

        let source = source.replace("[1, 2, 3].count_", "1.count_");
        let items = completion_items(&source, "1.count_");
        for name in ["count_leading_zeros", "count_trailing_zeros", "count_ones"] {
            assert!(items.iter().any(|item| item["label"] == name), "{name}");
        }
    }

    #[test]
    fn completion_includes_fields_locals_globals_and_keywords() {
        let source = r#"
struct Point { x_coordinate: Int }
fn helper() {}
fn main() {
    let point = Point { x_coordinate: 1 };
    point.x_co;
}
"#;
        let items = completion_items(source, "point.x_co");
        assert!(
            items
                .iter()
                .any(|item| item["label"] == "x_coordinate" && item["kind"] == 5)
        );

        let local_source = source.replace("point.x_co;", "poi;");
        let items = completion_items(&local_source, "poi");
        assert!(
            items
                .iter()
                .any(|item| item["label"] == "point" && item["kind"] == 6)
        );

        let global_source = source.replace("point.x_co;", "help;");
        let items = completion_items(&global_source, "help");
        assert!(
            items
                .iter()
                .any(|item| item["label"] == "helper" && item["kind"] == 3)
        );

        let keyword_source = source.replace("point.x_co;", "ret;");
        let items = completion_items(&keyword_source, "ret");
        assert!(
            items
                .iter()
                .any(|item| item["label"] == "return" && item["kind"] == 14)
        );

        let source = "struct Point {}\nPoi";
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/runtime/completion_probe.solar");
        let uri = format!("file://{}", path.display());
        let document = compute(&uri, source);
        let (line, character) = occurrence_position(source, "Poi", 1);
        let items = completions(source, line, character + 3, &uri, &document);
        assert!(
            items
                .iter()
                .any(|item| item["label"] == "Point" && item["kind"] == 22),
            "{items:?}"
        );
    }

    #[test]
    fn completion_lists_namespace_exports_and_reexported_modules() {
        let source = "fn main() { process::a; }\n";
        let items = completion_items(source, "process::a");
        assert!(
            items.iter().any(|item| {
                item["label"] == "args"
                    && item["kind"] == 3
                    && item["detail"] == "pub fn args() -> &[&[Uint8]]"
            }),
            "{items:?}"
        );
        assert!(items.iter().all(|item| item["label"] != "env"));

        let items = completion_items(
            "fn main() {\n    process::\n    println(0);\n}\n",
            "process::",
        );
        let labels = items
            .iter()
            .map(|item| item["label"].as_str().unwrap())
            .collect::<HashSet<_>>();
        assert_eq!(labels, HashSet::from(["args", "env"]));

        let items = completion_items("fn main() { pro; }\n", "pro");
        assert!(
            items
                .iter()
                .any(|item| { item["label"] == "process" && item["kind"] == 9 }),
            "{items:?}"
        );

        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/multi_file/many_modules/main.solar");
        let source = "import d from \"d.solar\";\nfn main() { d::c::b::a::; }\n";
        let items = completion_items_for_path(&path, source, "d::c::b::a::");
        assert!(
            items
                .iter()
                .any(|item| { item["label"] == "Enum" && item["kind"] == 13 }),
            "{items:?}"
        );
    }

    #[test]
    fn namespace_completion_excludes_private_transitive_declarations() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/multi_file/transitive_function_visibility/main.solar");
        let source = "import lib from \"lib.solar\";\nfn main() { lib::; }\n";
        let items = completion_items_for_path(&path, source, "lib::");
        let labels = items
            .iter()
            .map(|item| item["label"].as_str().unwrap())
            .collect::<HashSet<_>>();

        assert!(labels.contains("visible_fn"), "{items:?}");
        assert!(labels.contains("reexported_fn"), "{items:?}");
        assert!(!labels.contains("hidden_fn"), "{items:?}");
        assert!(!labels.contains("hidden"), "{items:?}");
    }

    #[test]
    fn failed_revision_reuses_last_successful_types_for_completion_and_inlay_hints() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/runtime/completion_probe.solar");
        let uri = format!("file://{}", path.display());
        let successful_source = r#"
struct Point { x: Int }
method read(self: Point) -> Int { self.x }
fn main() {
    let point = Point { x: 1 };
    point.read();
}
"#;
        let failed_source = successful_source.replace(
            "point.read();",
            "point.re;\n    poi;\n    let broken: Int = true;",
        );
        let mut cache = HashMap::new();
        update_cached_document(&mut cache, &uri, compute(&uri, successful_source));
        update_cached_document(&mut cache, &uri, compute(&uri, &failed_source));

        let document = cached(&mut cache, &uri, &failed_source);
        assert!(document.analysis.is_none());
        assert!(document.last_successful_types.is_some());

        let (line, character) = occurrence_position(&failed_source, "point.re", 0);
        let items = completions(
            &failed_source,
            line,
            character + "point.re".encode_utf16().count() as u32,
            &uri,
            document,
        );
        assert!(
            items.iter().any(|item| item["label"] == "read"),
            "{items:?}"
        );

        let (line, character) = occurrence_position(&failed_source, "poi", 0);
        let items = completions(
            &failed_source,
            line,
            character + "poi".len() as u32,
            &uri,
            document,
        );
        assert!(
            items
                .iter()
                .any(|item| { item["label"] == "point" && item["detail"] == "point: Point" }),
            "{items:?}"
        );

        let hints = inlay_hints(&failed_source, document, None);
        let point_position = occurrence_position(&failed_source, "point", 0);
        assert!(hints.iter().any(|hint| {
            hint["position"]
                == json!({
                    "line": point_position.0,
                    "character": point_position.1 + "point".len() as u32,
                })
                && hint["label"] == ": Point"
        }));
    }

    #[test]
    fn completion_excludes_functions_from_private_transitive_modules() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/multi_file/transitive_function_visibility/main.solar");
        let uri = format!("file://{}", path.display());
        let source = std::fs::read_to_string(path).unwrap();
        let document = compute(&uri, &source);
        let labels: HashSet<&str> = document
            .completion_symbols
            .iter()
            .map(|symbol| symbol.label.as_str())
            .collect();

        assert!(labels.contains("visible_fn"));
        assert!(labels.contains("reexported_fn"));
        assert!(!labels.contains("hidden_fn"));
    }

    #[test]
    fn formatting_returns_a_full_document_utf16_edit() {
        let source = "fn f(){println(\"😀\"&);}";
        let edits = formatting_edits(source);

        assert_eq!(edits.len(), 1);
        assert_eq!(
            edits[0]["range"]["start"],
            json!({ "line": 0, "character": 0 })
        );
        assert_eq!(
            edits[0]["range"]["end"],
            json!({ "line": 0, "character": source.encode_utf16().count() })
        );
        assert_eq!(edits[0]["newText"], "fn f() { println(\"😀\"&); }\n");
    }

    #[test]
    fn formatting_returns_no_edits_for_clean_or_invalid_source() {
        assert!(formatting_edits("fn f() {}\n").is_empty());
        assert!(formatting_edits("fn broken( {").is_empty());
    }

    #[test]
    fn semantic_tokens_leave_lexical_tokens_to_textmate() {
        let data = semantic_tokens("import intrinsics from \"@intrinsics\";\n", None);

        assert_eq!(
            data.get(..5),
            Some(&[0, 7, 10, token_index("namespace"), 0][..])
        );
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
    fn diagnostics_report_monomorphization_causes_as_information() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/typecheck/monomorphization_error_chain.solar");
        let uri = format!("file://{}", path.display());
        let source = std::fs::read_to_string(path).unwrap();

        let diagnostics = check_document(&uri, &source);
        let root = diagnostics.get(&uri).expect("root diagnostics");
        assert_eq!(root.len(), 3);
        assert_eq!(root[0]["severity"], 3);
        assert!(root[0]["message"].as_str().unwrap().contains("got Bool"));
        assert_eq!(root[1]["severity"], 3);
        assert!(root[1]["message"].as_str().unwrap().contains("`inner`"));
        assert_eq!(root[2]["severity"], 1);
        assert!(root[2]["message"].as_str().unwrap().contains("`outer`"));

        let inner_related = root[0]["relatedInformation"].as_array().unwrap();
        assert_eq!(inner_related.len(), 2);
        assert!(inner_related.iter().all(|item| item["message"] == "from"));
        assert_eq!(inner_related[0]["location"]["uri"], uri);
        assert_eq!(inner_related[0]["location"]["range"]["start"]["line"], 2);
        assert_eq!(inner_related[1]["location"]["range"]["start"]["line"], 4);

        let middle_related = root[1]["relatedInformation"].as_array().unwrap();
        assert!(
            middle_related[0]["message"]
                .as_str()
                .unwrap()
                .contains("got Bool")
        );
        assert_eq!(middle_related[1]["message"], "from");
        assert_eq!(middle_related[1]["location"]["range"]["start"]["line"], 4);

        let outer_related = root[2]["relatedInformation"].as_array().unwrap();
        assert!(
            outer_related[0]["message"]
                .as_str()
                .unwrap()
                .contains("got Bool")
        );
        assert!(
            outer_related[1]["message"]
                .as_str()
                .unwrap()
                .contains("`inner`")
        );
        assert_eq!(outer_related[0]["location"]["uri"], uri);
        assert_eq!(outer_related[0]["location"]["range"]["start"]["line"], 0);
        assert_eq!(outer_related[1]["location"]["range"]["start"]["line"], 2);
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
        assert_eq!(location["range"]["start"]["line"], 106);
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

        let diagnostic = compile_error_diagnostic(&error, "éx", 1, Vec::new());

        assert_eq!(diagnostic["range"]["start"]["character"], 1);
        assert_eq!(diagnostic["range"]["end"]["character"], 2);
    }

    #[test]
    fn definition_resolves_imported_function_type_and_field() {
        let (_, source, document) = fixture_document("tests/multi_file/module_import/main.solar");

        for (needle, occurrence, target_line) in [("origin", 0, 2), ("Point", 0, 0), ("x", 0, 0)] {
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
    fn definition_resolves_import_file_path() {
        let (_, source, document) = fixture_document("tests/multi_file/module_import/main.solar");
        let (line, character) = occurrence_position(&source, "lib.solar", 0);

        let location = definition(&source, line, character, &document).expect("import target");

        assert!(
            location["uri"]
                .as_str()
                .unwrap()
                .ends_with("/tests/multi_file/module_import/lib.solar"),
            "{location}"
        );
        assert_eq!(location["range"]["start"]["line"], 0);
        assert_eq!(location["range"]["start"]["character"], 0);
    }

    #[test]
    fn definition_and_hover_resolve_imported_path_fragments() {
        let (_, source, document) = fixture_document("tests/multi_file/many_modules/main.solar");

        for (needle, target_file, import_statement) in [
            (
                "d::c::b::a::Enum::Variant",
                "many_modules/main.solar",
                "import d from \"d.solar\";",
            ),
            (
                "c::b::a::Enum::Variant",
                "many_modules/d.solar",
                "pub import c from \"c.solar\";",
            ),
            (
                "b::a::Enum::Variant",
                "many_modules/c.solar",
                "pub import b from \"b.solar\";",
            ),
            (
                "a::Enum::Variant",
                "many_modules/b.solar",
                "pub import a from \"a.solar\";",
            ),
        ] {
            let (line, character) = occurrence_position(&source, needle, 0);
            let location = definition(&source, line, character, &document)
                .expect("import fragment definition");
            assert!(
                location["uri"].as_str().unwrap().ends_with(target_file),
                "{needle}: {location}"
            );
            assert_eq!(location["range"]["start"]["line"], 0, "{needle}");

            let hover = hover(&source, line, character, &document).expect("import fragment hover");
            let contents = hover["contents"]["value"].as_str().unwrap();
            assert!(contents.contains(import_statement), "{needle}: {contents}");
        }
    }

    #[test]
    fn definition_and_hover_resolve_exact_chained_field() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/runtime/methods.solar");
        let uri = format!("file://{}", path.display());
        let source = r#"struct Leaf {
    item: Int,
}

struct Branch {
    item: Leaf,
}

fn main() {
    let branch = Branch { item: Leaf { item: 1, }, };
    println(branch.item.item);
}
"#;
        let document = compute(&uri, source);

        for (occurrence, target_line, signature) in [(4, 5, "item: Leaf"), (5, 1, "item: Int")] {
            let (line, character) = occurrence_position(source, "item", occurrence);
            let locations = definition(source, line, character, &document)
                .map(definition_locations)
                .expect("field definition");
            assert_eq!(locations.len(), 1, "item occurrence {occurrence}");
            assert_eq!(locations[0]["range"]["start"]["line"], target_line);

            let hover = hover(source, line, character, &document).expect("field hover");
            let contents = hover["contents"]["value"].as_str().unwrap();
            assert!(contents.contains(signature), "{contents}");
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

        for (occurrence, target_line) in [(5, 38), (7, 7)] {
            let (line, character) = occurrence_position(&source, "double", occurrence);
            let location =
                definition(&source, line, character, &document).expect("double definition");
            let locations = definition_locations(location);
            assert_eq!(locations.len(), 1);
            assert_eq!(locations[0]["range"]["start"]["line"], target_line);
        }
    }

    #[test]
    fn definition_resolves_overloaded_operator_method_only() {
        let (_, source, document) = fixture_document("tests/runtime/operator_overload.solar");
        assert!(document.analysis.is_some());

        for (expression, prefix, target_line) in [
            ("a + b", "a ", 6),
            ("b - a", "b ", 10),
            ("a * 3", "a ", 19),
            ("sum == c", "sum ", 14),
        ] {
            let (line, start) = occurrence_position(&source, expression, 0);
            let operator = start + prefix.encode_utf16().count() as u32;
            let location = definition(&source, line, operator, &document)
                .unwrap_or_else(|| panic!("definition for {expression}"));
            assert_eq!(location["range"]["start"]["line"], target_line);

            let hover = hover(&source, line, operator, &document)
                .unwrap_or_else(|| panic!("hover for {expression}"));
            let contents = hover["contents"]["value"].as_str().unwrap();
            assert!(!contents.contains("built-in"), "{expression}: {contents}");
        }

        let (line, start) = occurrence_position(&source, "1 + 2", 0);
        let operator = start + "1 ".encode_utf16().count() as u32;
        assert!(definition(&source, line, operator, &document).is_none());

        let hover = hover(&source, line, operator, &document).expect("primitive operator hover");
        assert_eq!(
            hover["contents"]["value"],
            "```solar\nmethod operator_add(self: &Int, other: &Int) -> Int\n```\n\nbuilt-in"
        );
    }

    #[test]
    fn definition_resolves_enum_constructor_and_match_variant() {
        let (_, source, document) = fixture_document("tests/runtime/enums.solar");

        // `Shape::Circle` in the first match arm: Shape resolves to the enum and
        // Circle resolves to the variant declaration.
        for (needle, occurrence, target_line) in [("Shape", 3, 0), ("Circle", 1, 0)] {
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
        assert_eq!(location["range"]["start"]["line"], 76, "{location}");
        assert_eq!(location["range"]["start"]["character"], 25, "{location}");
    }

    #[test]
    fn definition_resolves_std_method_called_from_another_file() {
        let (_, source, document) = fixture_document("src/std/hashbrown/group.solar");
        let (line, character) = occurrence_position(&source, "eq_mask", 0);
        assert!(document.analysis.is_some(), "group.solar must type-check");

        let location =
            definition(&source, line, character, &document).expect("eq_mask method definition");

        assert!(
            location["uri"]
                .as_str()
                .unwrap()
                .ends_with("/src/std/simd.solar"),
            "{location}"
        );
        assert_eq!(location["range"]["start"]["line"], 17, "{location}");
    }

    #[test]
    fn hover_and_definition_resolve_the_same_overload() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/runtime/methods.solar");
        let uri = format!("file://{}", path.display());
        let source = r#"struct Number { value: Int, }

/// Method documentation.
method ping(self: Number) -> Int { self.value }

/// Free documentation.
fn ping(x: Int) -> Int { x }

fn main() {
    println((Number { value: 1 }).ping());
    println(ping(1));
}
"#;
        let document = compute(&uri, source);

        for (occurrence, target_line, expected_doc, rejected_doc) in [
            (2, 3, "Method documentation.", "Free documentation."),
            (3, 6, "Free documentation.", "Method documentation."),
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
    fn hover_shows_binding_types() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/runtime/methods.solar");
        let uri = format!("file://{}", path.display());
        let source = r#"fn copy#[T](input: T) -> T {
    let output = input;
    output
}

fn main() {
    copy(1);
    copy(true);
}
"#;
        let document = compute(&uri, source);

        for (name, occurrences) in [("input", [0, 1]), ("output", [0, 1])] {
            for occurrence in occurrences {
                let (line, character) = occurrence_position(source, name, occurrence);
                let hover = hover(source, line, character, &document)
                    .unwrap_or_else(|| panic!("missing hover for {name} occurrence {occurrence}"));
                let contents = hover["contents"]["value"].as_str().unwrap();
                assert!(contents.contains(&format!("{name}: Bool")), "{contents}");
                assert!(contents.contains(&format!("{name}: Int")), "{contents}");
            }
        }
    }

    #[test]
    fn hover_shows_destructured_binding_types() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/runtime/destructure.solar");
        let uri = format!("file://{}", path.display());
        let source = r#"fn main() {
    let (left, right) = (1, 2u);
    println(left);
    println(right);
}
"#;
        let document = compute(&uri, source);

        for (name, ty) in [("left", "Int"), ("right", "Uint")] {
            for occurrence in 0..2 {
                let (line, character) = occurrence_position(source, name, occurrence);
                let hover = hover(source, line, character, &document).expect("binding hover");
                let contents = hover["contents"]["value"].as_str().unwrap();
                assert!(contents.contains(&format!("{name}: {ty}")), "{contents}");
            }
        }
    }

    #[test]
    fn hover_shows_loop_and_match_binding_types() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/runtime/enums.solar");
        let uri = format!("file://{}", path.display());
        let source = r#"enum Shape {
    Circle(Int),
    Empty,
}

fn main() {
    let shape = Shape::Circle(1);
    let radius = match shape {
        Shape::Circle(value) => value,
        Shape::Empty => 0,
    };
    for index in 0..radius {
        println(index);
    }
    try {
        println(radius);
    } catch (message) {
        println(message);
    }
}
"#;
        let document = compute(&uri, source);
        for (name, ty) in [("value", "Int"), ("index", "Int"), ("message", "&[Uint8]")] {
            for occurrence in 0..2 {
                let (line, character) = occurrence_position(source, name, occurrence);
                let hover = hover(source, line, character, &document)
                    .unwrap_or_else(|| panic!("missing hover for {name} occurrence {occurrence}"));
                let contents = hover["contents"]["value"].as_str().unwrap();
                assert!(contents.contains(&format!("{name}: {ty}")), "{contents}");
            }
        }
    }

    #[test]
    fn inlay_hints_cover_omitted_binding_and_return_types() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/runtime/enums.solar");
        let uri = format!("file://{}", path.display());
        let source = r#"enum Shape {
    Circle(Int),
    Empty,
}

fn main() {
    let shape = Shape::Circle(1);
    let radius = match shape {
        Shape::Circle(value) => value,
        Shape::Empty => 0,
    };
    for index in 0..radius {
        println(index);
    }
    try {
        println(radius);
    } catch (message) {
        println(message);
    }
}
"#;
        let document = compute(&uri, source);

        let hints = inlay_hints(source, &document, None);
        let labels = hint_labels(&hints);

        for expected in [": Shape", ": Int", ": Int", ": Int", ": &[Uint8]"] {
            assert!(labels.contains(&expected), "missing {expected}: {labels:?}");
        }
        assert!(!labels.contains(&" -> ()"), "{labels:?}");
    }

    #[test]
    fn inlay_hints_cover_globals_defaults_and_contextual_closures() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/runtime/methods.solar");
        let uri = format!("file://{}", path.display());
        let source = r#"const LIMIT = 4u;
static ENABLED = true;

fn apply(function: fn(Int) -> Int, debug = false) -> Int {
    if debug { function(1) } else { 0 }
}

fn main() {
    let result = apply(\value value);
    let explicit: Int = result;
    if ENABLED { println(result); }
}
"#;
        let document = compute(&uri, source);

        let hints = inlay_hints(source, &document, None);
        let labels = hint_labels(&hints);

        assert!(labels.contains(&": Uint"), "{labels:?}");
        assert_eq!(labels.iter().filter(|label| **label == ": Bool").count(), 2);
        assert_eq!(labels.iter().filter(|label| **label == ": Int").count(), 2);
        assert!(labels.contains(&" -> Int"), "{labels:?}");
        assert!(!labels.contains(&" -> ()"), "{labels:?}");

        let explicit = occurrence_position(source, "explicit", 0);
        assert!(!hints.iter().any(|hint| {
            hint["position"]["line"] == explicit.0
                && hint["position"]["character"] == explicit.1 + "explicit".len() as u32
        }));
    }

    #[test]
    fn inlay_hint_positions_and_ranges_use_utf16() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/runtime/methods.solar");
        let uri = format!("file://{}", path.display());
        let source = "fn main() { println(\"😀\"&); let value = 1; }\n";
        let document = compute(&uri, source);
        let (line, start) = occurrence_position(source, "value", 0);
        let position = (line, start + "value".encode_utf16().count() as u32);

        let range_end = (position.0, position.1 + 1);
        let hints = inlay_hints(source, &document, Some((position, range_end)));

        assert_eq!(hints.len(), 1, "{hints:?}");
        assert_eq!(
            hints[0]["position"],
            json!({ "line": 0, "character": position.1 })
        );
        assert_eq!(hints[0]["label"], ": Int");
    }

    #[test]
    fn inlay_hints_cover_reflection_pattern_bindings() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/runtime/methods.solar");
        let uri = format!("file://{}", path.display());
        let source = r#"struct Pair { number: Int, flag: Bool, }

fn main() {
    let pair = Pair { number: 1, flag: true, };
    for.reflect_fields (name, value) in pair& {
        println(name);
    }
}
"#;
        let document = compute(&uri, source);

        let hints = inlay_hints(source, &document, None);
        let labels = hint_labels(&hints);

        assert!(labels.contains(&": &[Uint8]"), "{labels:?}");
        assert!(
            labels.contains(&": &Bool | &Int") || labels.contains(&": &Int | &Bool"),
            "{labels:?}"
        );
    }

    #[test]
    fn closure_parameter_hint_precedes_return_hint_at_the_same_position() {
        let (_, source, document) = fixture_document("examples/threads_loop.solar");
        let (line, closure_start) = occurrence_position(&source, "\\i thread::spawn(test)", 0);
        let position = (line, closure_start + "\\i".encode_utf16().count() as u32);

        let hints = inlay_hints(
            &source,
            &document,
            Some((position, (position.0, position.1 + 1))),
        );
        let labels = hint_labels(&hints);

        assert_eq!(labels.len(), 2, "{hints:?}");
        assert_eq!(labels[0], ": Uint");
        assert!(labels[1].starts_with(" -> "), "{labels:?}");
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
    fn intrinsic_hover_shows_concrete_built_in_signatures() {
        let (_, source, document) = fixture_document("src/std/lib.solar");
        let (line, character) = occurrence_position(&source, "count_trailing_zeros(self)", 0);

        assert!(definition(&source, line, character, &document).is_none());
        let hover = hover(&source, line, character, &document).expect("intrinsic hover");
        let contents = hover["contents"]["value"].as_str().unwrap();
        assert!(
            contents.contains("fn count_trailing_zeros(_: "),
            "{contents}"
        );
        assert!(contents.contains(" -> Uint"), "{contents}");
        assert!(contents.contains("built-in"), "{contents}");
        assert!(
            !contents.contains("pub method count_trailing_zeros"),
            "{contents}"
        );
    }

    #[test]
    fn numeric_constructor_hover_shows_generated_signature() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/runtime/methods.solar");
        let uri = format!("file://{}", path.display());
        let source = "fn convert#[T](x: T) -> Int64 { Int64(x) }\nfn main() { convert(1); }\n";
        let document = compute(&uri, source);
        let (line, character) = occurrence_position(source, "Int64(x)", 0);

        assert!(definition(source, line, character, &document).is_none());
        let hover = hover(source, line, character, &document).expect("constructor hover");
        assert_eq!(
            hover["contents"]["value"],
            "```solar\nfn Int64(x: Int) -> Int64\n```\n\nbuilt-in"
        );
    }
}
