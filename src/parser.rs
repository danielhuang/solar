use crate::ast::*;
use std::fmt;

/// Generate overloaded constructor functions for each numeric type.
/// e.g. `fn Int(x: Uint) -> Int { IntrinsicCall(Cast(Uint, Int), [x]) }`
pub fn generate_numeric_constructors(items: &mut Vec<TopLevelItem>) {
    const TYPES: &[&str] = &[
        "Int", "Uint", "Int8", "Int16", "Int32", "Int64", "Uint8", "Uint16", "Uint32", "Uint64",
        "Float32", "Float64",
    ];

    let span = SourceSpan::default();

    for &target_name in TYPES {
        for &from_name in TYPES {
            if target_name == from_name {
                continue;
            }
            let intrinsic = Intrinsic::Cast(
                NumericType::from_name(from_name).unwrap(),
                NumericType::from_name(target_name).unwrap(),
            );
            items.push(TopLevelItem::Function(FunctionDef {
                name: target_name.to_string(),
                display_name: target_name.to_string(),
                type_params: vec![],
                parameters: vec![Parameter {
                    pattern: DestructurePattern::Name("x".to_string()),
                    ty: Type::Named(from_name.to_string()),
                    default: None,
                    span,
                }],
                return_type: Some(Type::Named(target_name.to_string())),
                return_type_span: None,
                body: vec![Statement {
                    kind: StatementKind::Expression(Expr {
                        kind: ExprKind::IntrinsicCall {
                            intrinsic,
                            arguments: vec![Expr {
                                kind: ExprKind::Identifier("x".to_string()),
                                span,
                            }],
                        },
                        span,
                    }),
                    span,
                }],
                is_pub: false,
                inline_hint: false,
                span,
            }));
        }
    }
}

#[derive(Debug)]
pub struct ParseError {
    pub line: usize,
    pub column: usize,
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.line + 1, self.column + 1, self.message)
    }
}

pub fn parse(source: &str) -> Result<SourceFile, Vec<ParseError>> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_solar::LANGUAGE.into())
        .expect("failed to set tree-sitter language");

    let tree = parser
        .parse(source, None)
        .expect("tree-sitter parse failed");
    let root = tree.root_node();

    let mut errors = Vec::new();
    collect_errors(root, source, &mut errors);

    if !errors.is_empty() {
        return Err(errors);
    }

    let source_file = convert_source_file(root, source);
    Ok(source_file)
}

fn collect_errors(node: tree_sitter::Node, source: &str, errors: &mut Vec<ParseError>) {
    if node.is_error() {
        let start = node.start_position();
        let text = node_text(node, source);
        errors.push(ParseError {
            line: start.row,
            column: start.column,
            message: format!("unexpected: {text}"),
        });
    } else if node.is_missing() {
        let start = node.start_position();
        errors.push(ParseError {
            line: start.row,
            column: start.column,
            message: format!("missing: {}", node.kind()),
        });
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_errors(child, source, errors);
    }
}

fn has_pub_keyword(node: tree_sitter::Node, source: &str) -> bool {
    for i in 0..node.child_count() {
        let child = node.child(i).unwrap();
        if !child.is_named() && node_text(child, source) == "pub" {
            return true;
        }
    }
    false
}

fn node_text<'a>(node: tree_sitter::Node, source: &'a str) -> &'a str {
    &source[node.byte_range()]
}

fn source_span(node: tree_sitter::Node) -> SourceSpan {
    let start = node.start_position();
    let end = node.end_position();
    SourceSpan {
        start: SourcePos {
            line: start.row as u32,
            col: start.column as u32,
        },
        end: SourcePos {
            line: end.row as u32,
            col: end.column as u32,
        },
        file_id: 0,
    }
}

fn convert_source_file(node: tree_sitter::Node, source: &str) -> SourceFile {
    let mut items = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "struct_def" => items.push(TopLevelItem::Struct(convert_struct_def(child, source))),
            "function_def" => {
                items.push(TopLevelItem::Function(convert_function_def(child, source)))
            }
            "enum_def" => items.push(TopLevelItem::Enum(convert_enum_def(child, source))),
            "method_def" => items.push(TopLevelItem::Method(convert_function_def(child, source))),
            "import_statement" => items.push(TopLevelItem::Import(convert_import_statement(
                child, source,
            ))),
            "type_alias_def" => items.push(TopLevelItem::TypeAlias(convert_type_alias_def(
                child, source,
            ))),
            "const_def" => items.push(TopLevelItem::Const(convert_const_def(child, source))),
            "static_def" => items.push(TopLevelItem::Static(convert_static_def(child, source))),
            _ => {}
        }
    }
    SourceFile { items }
}

fn convert_import_statement(node: tree_sitter::Node, source: &str) -> ImportDef {
    let is_pub = has_pub_keyword(node, source);
    let span = source_span(node);

    // Find the string literal for the path
    let string_node = named_child_by_kind(node, "string_literal").unwrap();
    let path_text = node_text(string_node, source);
    let path = path_text[1..path_text.len() - 1].to_string(); // strip quotes

    // Determine import kind
    let kind = if let Some(module_name) = node.child_by_field_name("module_name") {
        ImportKind::Module(node_text(module_name, source).to_string())
    } else if let Some(name_list) = named_child_by_kind(node, "import_name_list") {
        let mut names = Vec::new();
        let mut cursor = name_list.walk();
        for child in name_list.named_children(&mut cursor) {
            if child.kind() == "import_path" {
                let mut segments = Vec::new();
                let mut path_cursor = child.walk();
                for seg in child.named_children(&mut path_cursor) {
                    if seg.kind() == "identifier" {
                        segments.push(node_text(seg, source).to_string());
                    }
                }
                names.push(ImportName { segments });
            }
        }
        ImportKind::Named(names)
    } else {
        // Must be wildcard (*)
        ImportKind::Wildcard
    };

    ImportDef {
        kind,
        path,
        is_pub,
        span,
    }
}

fn convert_type_params(node: tree_sitter::Node, source: &str) -> Vec<String> {
    let mut params = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "identifier" {
            params.push(node_text(child, source).to_string());
        }
    }
    params
}

fn convert_type_args(node: tree_sitter::Node, source: &str) -> Vec<Type> {
    let mut args = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        args.push(convert_type(child, source));
    }
    args
}

fn convert_struct_def(node: tree_sitter::Node, source: &str) -> StructDef {
    let name = node_text(node.child_by_field_name("name").unwrap(), source).to_string();
    let is_pub = has_pub_keyword(node, source);
    let type_params = node
        .child_by_field_name("type_params")
        .map(|n| convert_type_params(n, source))
        .unwrap_or_default();
    let mut fields = Vec::new();
    if let Some(field_list) = named_child_by_kind(node, "field_list") {
        let mut cursor = field_list.walk();
        for child in field_list.named_children(&mut cursor) {
            if child.kind() == "field_def" {
                if !is_pub && has_pub_keyword(child, source) {
                    let field_name = node_text(child.child_by_field_name("name").unwrap(), source);
                    let pos = child.start_position();
                    panic!(
                        "{}:{}: `pub` field `{}` in non-pub struct `{}`",
                        pos.row + 1,
                        pos.column + 1,
                        field_name,
                        name
                    );
                }
                fields.push(convert_field_def(child, source));
            }
        }
    }
    StructDef {
        name,
        type_params,
        fields,
        is_pub,
        span: source_span(node),
    }
}

fn convert_const_def(node: tree_sitter::Node, source: &str) -> ConstDef {
    let name = node_text(node.child_by_field_name("name").unwrap(), source).to_string();
    let is_pub = has_pub_keyword(node, source);
    let ty = node
        .child_by_field_name("type")
        .map(|n| convert_type(n, source));
    let value = Box::new(convert_expr(
        node.child_by_field_name("value").unwrap(),
        source,
    ));
    ConstDef {
        name,
        ty,
        value,
        is_pub,
        span: source_span(node),
    }
}

fn convert_static_def(node: tree_sitter::Node, source: &str) -> StaticDef {
    let name = node_text(node.child_by_field_name("name").unwrap(), source).to_string();
    let is_pub = has_pub_keyword(node, source);
    let ty = node
        .child_by_field_name("type")
        .map(|n| convert_type(n, source));
    let value = Box::new(convert_expr(
        node.child_by_field_name("value").unwrap(),
        source,
    ));
    StaticDef {
        name,
        ty,
        value,
        is_pub,
        span: source_span(node),
    }
}

fn convert_type_alias_def(node: tree_sitter::Node, source: &str) -> TypeAliasDef {
    let name = node_text(node.child_by_field_name("name").unwrap(), source).to_string();
    let is_pub = has_pub_keyword(node, source);
    let type_params = node
        .child_by_field_name("type_params")
        .map(|n| convert_type_params(n, source))
        .unwrap_or_default();
    let target_type = convert_type(node.child_by_field_name("target_type").unwrap(), source);
    TypeAliasDef {
        name,
        type_params,
        target_type,
        is_pub,
        span: source_span(node),
    }
}

fn convert_enum_def(node: tree_sitter::Node, source: &str) -> EnumDef {
    let name = node_text(node.child_by_field_name("name").unwrap(), source).to_string();
    let is_pub = has_pub_keyword(node, source);
    let type_params = node
        .child_by_field_name("type_params")
        .map(|n| convert_type_params(n, source))
        .unwrap_or_default();
    let mut variants = Vec::new();
    if let Some(variant_list) = named_child_by_kind(node, "variant_list") {
        let mut cursor = variant_list.walk();
        for child in variant_list.named_children(&mut cursor) {
            if child.kind() == "variant_def" {
                let vname =
                    node_text(child.child_by_field_name("name").unwrap(), source).to_string();
                let inner_type = child
                    .child_by_field_name("inner_type")
                    .map(|n| convert_type(n, source));
                variants.push(VariantDef {
                    name: vname,
                    inner_type,
                    span: source_span(child),
                });
            }
        }
    }
    EnumDef {
        name,
        type_params,
        variants,
        is_pub,
        span: source_span(node),
    }
}

fn convert_field_def(node: tree_sitter::Node, source: &str) -> FieldDef {
    let name = node_text(node.child_by_field_name("name").unwrap(), source).to_string();
    let ty = convert_type(node.child_by_field_name("type").unwrap(), source);
    let is_pub = has_pub_keyword(node, source);
    FieldDef {
        name,
        ty,
        is_pub,
        span: source_span(node),
    }
}

fn convert_function_def(node: tree_sitter::Node, source: &str) -> FunctionDef {
    let name = node_text(node.child_by_field_name("name").unwrap(), source).to_string();
    let is_pub = has_pub_keyword(node, source);
    let type_params = node
        .child_by_field_name("type_params")
        .map(|n| convert_type_params(n, source))
        .unwrap_or_default();

    let mut parameters = Vec::new();
    if let Some(param_list) = named_child_by_kind(node, "parameter_list") {
        let mut cursor = param_list.walk();
        for child in param_list.named_children(&mut cursor) {
            if child.kind() == "parameter" {
                parameters.push(convert_parameter(child, source));
            }
        }
    }

    let return_type_node = node.child_by_field_name("return_type");
    let return_type = return_type_node.map(|n| convert_type(n, source));
    let return_type_span = return_type_node.map(source_span);

    let body_node = node.child_by_field_name("body").unwrap();
    let body = convert_block(body_node, source);

    // `fn(inline)` / `method(inline)`
    let inline_hint = node.child_by_field_name("attr").is_some();

    FunctionDef {
        display_name: name.clone(),
        name,
        type_params,
        parameters,
        return_type,
        return_type_span,
        body,
        is_pub,
        inline_hint,
        span: source_span(node),
    }
}

fn convert_parameter(node: tree_sitter::Node, source: &str) -> Parameter {
    let pattern_node = node.child_by_field_name("pattern").unwrap();
    let pattern = convert_destructure_pattern(pattern_node, source);
    // An explicit type is present unless this is the inferred-type keyword form
    // (`name = default`).
    let ty = match node.child_by_field_name("type") {
        Some(type_node) => convert_type(type_node, source),
        None => Type::Infer,
    };
    let default = node
        .child_by_field_name("default")
        .map(|n| Box::new(convert_expr(n, source)));
    Parameter {
        pattern,
        ty,
        default,
        span: source_span(node),
    }
}

/// Desugar `try { body } catch (e[: T]) { handler }` into a call of the `try`
/// intrinsic with two closures: `intrinsics::try(\ { body }, \ e: &[Uint8] { handler })`.
/// The handler's parameter type is synthesized as `&[Uint8]` when omitted; an
/// explicit annotation is kept (and the intrinsic's signature enforces it must
/// be `&[Uint8]`).
fn convert_try_statement(node: tree_sitter::Node, source: &str) -> Statement {
    let span = source_span(node);
    let body = convert_block(node.child_by_field_name("body").unwrap(), source);
    let handler_body = convert_block(node.child_by_field_name("handler").unwrap(), source);
    let binding = node_text(node.child_by_field_name("binding").unwrap(), source).to_string();
    let binding_ty = match node.child_by_field_name("binding_type") {
        Some(t) => convert_type(t, source),
        // `&[Uint8]`
        None => Type::Reference(Box::new(Type::Slice(Box::new(Type::Named(
            "Uint8".to_string(),
        ))))),
    };

    let body_closure = Expr {
        kind: ExprKind::Closure {
            parameters: Vec::new(),
            return_type: None,
            body,
        },
        span,
    };
    let handler_closure = Expr {
        kind: ExprKind::Closure {
            parameters: vec![Parameter {
                pattern: DestructurePattern::Name(binding),
                ty: binding_ty,
                default: None,
                span,
            }],
            return_type: None,
            body: handler_body,
        },
        span,
    };

    Statement {
        kind: StatementKind::Expression(Expr {
            kind: ExprKind::IntrinsicCall {
                intrinsic: Intrinsic::Try,
                arguments: vec![body_closure, handler_closure],
            },
            span,
        }),
        span,
    }
}

fn convert_closure_parameter(node: tree_sitter::Node, source: &str) -> Parameter {
    let name = node_text(node.child_by_field_name("name").unwrap(), source).to_string();
    let ty = match node.child_by_field_name("type") {
        Some(type_node) => convert_type(type_node, source),
        None => Type::Infer,
    };
    Parameter {
        pattern: DestructurePattern::Name(name),
        ty,
        default: None,
        span: source_span(node),
    }
}

/// Convert an `argument_list` node into positional arguments and keyword
/// arguments (`name = value`).
fn convert_arguments(node: tree_sitter::Node, source: &str) -> (Vec<Expr>, Vec<(String, Expr)>) {
    let mut positional = Vec::new();
    let mut kwargs = Vec::new();
    if let Some(arg_list) = named_child_by_kind(node, "argument_list") {
        let mut cursor = arg_list.walk();
        for arg in arg_list.named_children(&mut cursor) {
            if arg.kind() != "argument" {
                continue;
            }
            let value = convert_expr(arg.child_by_field_name("value").unwrap(), source);
            match arg.child_by_field_name("name") {
                Some(name_node) => {
                    kwargs.push((node_text(name_node, source).to_string(), value));
                }
                None => positional.push(value),
            }
        }
    }
    (positional, kwargs)
}

fn convert_destructure_pattern(node: tree_sitter::Node, source: &str) -> DestructurePattern {
    match node.kind() {
        "identifier" => DestructurePattern::Name(node_text(node, source).to_string()),
        "tuple_pattern" => {
            let mut elements = Vec::new();
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                elements.push(convert_destructure_pattern(child, source));
            }
            DestructurePattern::Tuple(elements)
        }
        "struct_pattern" => {
            let module = node
                .child_by_field_name("module")
                .map(|n| node_text(n, source).to_string());
            let name = node_text(node.child_by_field_name("name").unwrap(), source).to_string();
            let mut fields = Vec::new();
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if child.kind() == "struct_pattern_field" {
                    fields.push(convert_struct_pattern_field(child, source));
                }
            }
            DestructurePattern::Struct {
                module,
                name,
                fields,
            }
        }
        "array_pattern" => {
            let mut elements = Vec::new();
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                elements.push(convert_destructure_pattern(child, source));
            }
            DestructurePattern::Array(elements)
        }
        other => panic!("unexpected destructure pattern node kind: {other}"),
    }
}

fn convert_struct_pattern_field(node: tree_sitter::Node, source: &str) -> DestructureField {
    let field_name = node_text(node.child_by_field_name("field_name").unwrap(), source).to_string();
    let pattern = if let Some(pat_node) = node.child_by_field_name("pattern") {
        convert_destructure_pattern(pat_node, source)
    } else {
        // Shorthand: `field_name` binds to same name
        DestructurePattern::Name(field_name.clone())
    };
    DestructureField {
        field_name,
        pattern,
    }
}

fn convert_block(node: tree_sitter::Node, source: &str) -> Vec<Statement> {
    let mut stmts = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "let_statement" => stmts.push(convert_let_statement(child, source)),
            "assignment_statement" => stmts.push(convert_assignment_statement(child, source)),
            "expression_statement" => stmts.push(convert_expression_statement(child, source)),
            "if_statement" => stmts.push(convert_if_statement(child, source)),
            "while_statement" => stmts.push(convert_while_statement(child, source)),
            "for_statement" => stmts.push(convert_for_statement(child, source)),
            "reflect_fields_statement" => {
                stmts.push(convert_reflect_fields_statement(child, source, false))
            }
            "reflect_fields_pair_statement" => {
                stmts.push(convert_reflect_fields_statement(child, source, true))
            }
            "reflect_variant_statement" => {
                stmts.push(convert_reflect_variant_statement(child, source, false))
            }
            "reflect_variant_pair_statement" => {
                stmts.push(convert_reflect_variant_statement(child, source, true))
            }
            "return_statement" => stmts.push(convert_return_statement(child, source)),
            "break_statement" => stmts.push(Statement {
                kind: StatementKind::Break(
                    child
                        .child_by_field_name("value")
                        .map(|n| convert_expr(n, source)),
                ),
                span: source_span(child),
            }),
            "continue_statement" => stmts.push(Statement {
                kind: StatementKind::Continue,
                span: source_span(child),
            }),
            "try_statement" => stmts.push(convert_try_statement(child, source)),
            "function_def" => {
                let span = source_span(child);
                stmts.push(Statement {
                    kind: StatementKind::NestedFunction(convert_function_def(child, source)),
                    span,
                });
            }
            "const_def" => {
                let span = source_span(child);
                stmts.push(Statement {
                    kind: StatementKind::Const(convert_const_def(child, source)),
                    span,
                });
            }
            _ => {}
        }
    }
    // A tail expression (no semicolon) becomes a normal Expression statement
    if let Some(tail) = node.child_by_field_name("tail") {
        let span = source_span(tail);
        stmts.push(Statement {
            kind: StatementKind::Expression(convert_expr(tail, source)),
            span,
        });
    }
    stmts
}

fn convert_let_statement(node: tree_sitter::Node, source: &str) -> Statement {
    let span = source_span(node);
    let pattern_node = node.child_by_field_name("pattern").unwrap();
    let pattern = convert_destructure_pattern(pattern_node, source);
    let ty = node
        .child_by_field_name("type")
        .map(|n| convert_type(n, source));
    let value = convert_expr(node.child_by_field_name("value").unwrap(), source);
    Statement {
        kind: StatementKind::Let { pattern, ty, value },
        span,
    }
}

fn convert_assignment_statement(node: tree_sitter::Node, source: &str) -> Statement {
    let span = source_span(node);
    let target = convert_expr(node.child_by_field_name("target").unwrap(), source);
    let value = convert_expr(node.child_by_field_name("value").unwrap(), source);
    Statement {
        kind: StatementKind::Assignment { target, value },
        span,
    }
}

fn convert_if_statement(node: tree_sitter::Node, source: &str) -> Statement {
    let span = source_span(node);
    let condition = convert_expr(node.child_by_field_name("condition").unwrap(), source);
    let body_node = node.child_by_field_name("body").unwrap();
    let body = convert_block(body_node, source);
    let else_body = if let Some(else_node) = node.child_by_field_name("else_body") {
        match else_node.kind() {
            "block" => convert_block(else_node, source),
            "if_statement" => vec![convert_if_statement(else_node, source)],
            _ => panic!("unexpected else_body kind: {}", else_node.kind()),
        }
    } else {
        Vec::new()
    };
    Statement {
        kind: StatementKind::If {
            condition,
            body,
            else_body,
        },
        span,
    }
}

fn convert_while_statement(node: tree_sitter::Node, source: &str) -> Statement {
    let span = source_span(node);
    let condition = convert_expr(node.child_by_field_name("condition").unwrap(), source);
    let body_node = node.child_by_field_name("body").unwrap();
    let body = convert_block(body_node, source);
    Statement {
        kind: StatementKind::While { condition, body },
        span,
    }
}

fn convert_for_statement(node: tree_sitter::Node, source: &str) -> Statement {
    let span = source_span(node);
    let variable = node_text(node.child_by_field_name("variable").unwrap(), source).to_string();
    let body_node = node.child_by_field_name("body").unwrap();
    let body = convert_block(body_node, source);

    if let Some(start_node) = node.child_by_field_name("start") {
        // Range form: for i in start..end { ... }
        let start = convert_expr(start_node, source);
        let end = convert_expr(node.child_by_field_name("end").unwrap(), source);
        Statement {
            kind: StatementKind::ForRange {
                variable,
                start,
                end,
                body,
            },
            span,
        }
    } else {
        // Iterable form: for x in list { ... }
        let iterable = convert_expr(node.child_by_field_name("iterable").unwrap(), source);
        Statement {
            kind: StatementKind::ForIn {
                variable,
                iterable,
                body,
            },
            span,
        }
    }
}

fn convert_reflect_fields_statement(
    node: tree_sitter::Node,
    source: &str,
    paired: bool,
) -> Statement {
    let span = source_span(node);
    let pattern =
        convert_destructure_pattern(node.child_by_field_name("variable").unwrap(), source);
    let object = convert_expr(node.child_by_field_name("object").unwrap(), source);
    let body = convert_block(node.child_by_field_name("body").unwrap(), source);
    Statement {
        kind: StatementKind::ForReflectFields {
            pattern,
            object,
            body,
            paired,
        },
        span,
    }
}

fn convert_reflect_variant_statement(
    node: tree_sitter::Node,
    source: &str,
    paired: bool,
) -> Statement {
    let span = source_span(node);
    let pattern = convert_destructure_pattern(node.child_by_field_name("pattern").unwrap(), source);
    let object = convert_expr(node.child_by_field_name("object").unwrap(), source);
    let body = convert_block(node.child_by_field_name("body").unwrap(), source);
    Statement {
        kind: StatementKind::MatchReflectVariant {
            pattern,
            object,
            body,
            paired,
        },
        span,
    }
}

fn convert_expression_statement(node: tree_sitter::Node, source: &str) -> Statement {
    let span = source_span(node);
    let mut cursor = node.walk();
    let expr_node = node.named_children(&mut cursor).next().unwrap();
    Statement {
        kind: StatementKind::Expression(convert_expr(expr_node, source)),
        span,
    }
}

fn convert_return_statement(node: tree_sitter::Node, source: &str) -> Statement {
    let span = source_span(node);
    let value = convert_expr(node.child_by_field_name("value").unwrap(), source);
    Statement {
        kind: StatementKind::Return(value),
        span,
    }
}

fn convert_if_expression(node: tree_sitter::Node, source: &str) -> Expr {
    let span = source_span(node);
    let condition = convert_expr(node.child_by_field_name("condition").unwrap(), source);
    let then_body_node = node.child_by_field_name("then_body").unwrap();
    let then_body = convert_block(then_body_node, source);
    let else_node = node.child_by_field_name("else_body").unwrap();
    let else_body = match else_node.kind() {
        "block" => convert_block(else_node, source),
        "if_expression" => {
            let else_span = source_span(else_node);
            vec![Statement {
                kind: StatementKind::Expression(convert_if_expression(else_node, source)),
                span: else_span,
            }]
        }
        _ => panic!("unexpected else_body kind: {}", else_node.kind()),
    };
    Expr {
        kind: ExprKind::If {
            condition: Box::new(condition),
            then_body,
            else_body,
        },
        span,
    }
}

fn convert_expr(node: tree_sitter::Node, source: &str) -> Expr {
    let span = source_span(node);
    let kind = match node.kind() {
        "identifier" => ExprKind::Identifier(node_text(node, source).to_string()),
        "integer_literal" => {
            let text = node_text(node, source);
            let (num_str, int_ty) = parse_integer_suffix(text);
            // The grammar guarantees valid digits for the radix, so the sole
            // possible parse error is overflow; saturate and let the type checker
            // report out-of-range.
            let value = parse_integer_value(num_str);
            ExprKind::IntegerLiteral(value, int_ty)
        }
        "float_literal" => {
            let text = node_text(node, source);
            let (num_str, float_ty) = if let Some(n) = text.strip_suffix("f32") {
                (n, FloatType::Float32)
            } else if let Some(n) = text.strip_suffix("f64") {
                (n, FloatType::Float64)
            } else {
                (text.strip_suffix('f').unwrap(), FloatType::Float64)
            };
            // Parse f32 literals in f32 precision (then widen exactly) so the
            // value is correctly rounded once, not double-rounded through f64.
            let value = match float_ty {
                FloatType::Float32 => num_str.parse::<f32>().unwrap() as f64,
                FloatType::Float64 => num_str.parse::<f64>().unwrap(),
            };
            ExprKind::FloatLiteral(value, float_ty)
        }
        "boolean_literal" => {
            let text = node_text(node, source);
            ExprKind::BooleanLiteral(text == "true")
        }
        "string_literal" => {
            let text = node_text(node, source);
            let inner = &text[1..text.len() - 1]; // strip quotes
            let bytes = unescape_string(inner);
            ExprKind::ArrayLiteral(
                bytes
                    .into_iter()
                    .map(|b| Expr {
                        kind: ExprKind::IntegerLiteral(b as i128, IntegerType::Uint8),
                        span,
                    })
                    .collect(),
                // implicit annotation so the empty string works like []#[Uint8]
                Some(Type::Named("Uint8".to_string())),
            )
        }
        "field_access" => {
            let object = convert_expr(node.child_by_field_name("object").unwrap(), source);
            let field_node = node.child_by_field_name("field").unwrap();
            let field = if field_node.kind() == "integer_literal" {
                format!("_{}", node_text(field_node, source))
            } else {
                node_text(field_node, source).to_string()
            };
            ExprKind::FieldAccess {
                object: Box::new(object),
                field,
            }
        }
        "deref_expr" => {
            let operand = convert_expr(node.child_by_field_name("operand").unwrap(), source);
            ExprKind::Deref(Box::new(operand))
        }
        "reference_expr" => {
            let operand = convert_expr(node.child_by_field_name("operand").unwrap(), source);
            ExprKind::Reference(Box::new(operand))
        }
        "unique_expr" => {
            let operand = convert_expr(node.child_by_field_name("operand").unwrap(), source);
            ExprKind::Unique(Box::new(operand))
        }
        "null_expr" => {
            let type_args =
                convert_type_args(node.child_by_field_name("type_args").unwrap(), source);
            ExprKind::NullLiteral(type_args.into_iter().next().unwrap())
        }
        "call_expr" => {
            let func_node = node.child_by_field_name("function").unwrap();
            let (arguments, kwargs) = convert_arguments(node, source);
            // a.b(c) parses as call_expr(field_access(a, b), [c])
            // Convert to MethodCall when the callee is a field_access with an identifier field
            // (not a numeric field like .0 from tuple access)
            if func_node.kind() == "field_access"
                && func_node.child_by_field_name("field").unwrap().kind() == "identifier"
            {
                let receiver =
                    convert_expr(func_node.child_by_field_name("object").unwrap(), source);
                let method =
                    node_text(func_node.child_by_field_name("field").unwrap(), source).to_string();
                ExprKind::MethodCall {
                    receiver: Box::new(receiver),
                    method,
                    type_args: Vec::new(),
                    arguments,
                    kwargs,
                }
            } else if func_node.kind() == "path_expr" {
                // path_expr as call: sync::Channel#[Int]() — extract type_args from last segment
                let mut segments: Vec<(String, Vec<Type>)> = Vec::new();
                let mut cursor = func_node.walk();
                for child in func_node.named_children(&mut cursor) {
                    if child.kind() == "path_segment" {
                        let name = node_text(child.child_by_field_name("name").unwrap(), source)
                            .to_string();
                        let ta = child
                            .child_by_field_name("type_args")
                            .map(|n| convert_type_args(n, source))
                            .unwrap_or_default();
                        segments.push((name, ta));
                    }
                }
                let (last_name, last_type_args) = segments.pop().unwrap();
                let (enum_name, enum_type_args) = segments.pop().unwrap();
                let module_path: Vec<String> = segments.into_iter().map(|(n, _)| n).collect();
                let function = Expr {
                    kind: ExprKind::EnumVariant {
                        module_path,
                        enum_name,
                        type_args: enum_type_args,
                        variant_name: last_name,
                    },
                    span: source_span(func_node),
                };
                ExprKind::Call {
                    function: Box::new(function),
                    type_args: last_type_args,
                    arguments,
                    kwargs,
                }
            } else {
                let function = convert_expr(func_node, source);
                ExprKind::Call {
                    function: Box::new(function),
                    type_args: Vec::new(),
                    arguments,
                    kwargs,
                }
            }
        }
        "generic_call_expr" => {
            let name = node_text(node.child_by_field_name("function").unwrap(), source).to_string();
            let type_args = node
                .child_by_field_name("type_args")
                .map(|n| convert_type_args(n, source))
                .unwrap_or_default();
            let (arguments, kwargs) = convert_arguments(node, source);
            ExprKind::Call {
                function: Box::new(Expr {
                    kind: ExprKind::Identifier(name),
                    span,
                }),
                type_args,
                arguments,
                kwargs,
            }
        }
        "struct_literal" => {
            let module = node
                .child_by_field_name("module")
                .map(|n| node_text(n, source).to_string());
            let name = node_text(node.child_by_field_name("name").unwrap(), source).to_string();
            let type_args = node
                .child_by_field_name("type_args")
                .map(|n| convert_type_args(n, source))
                .unwrap_or_default();
            let mut fields = Vec::new();
            if let Some(init_list) = named_child_by_kind(node, "field_init_list") {
                let mut cursor = init_list.walk();
                for child in init_list.named_children(&mut cursor) {
                    if child.kind() == "field_init" {
                        let fname = node_text(child.child_by_field_name("name").unwrap(), source)
                            .to_string();
                        let value =
                            convert_expr(child.child_by_field_name("value").unwrap(), source);
                        fields.push(FieldInit { name: fname, value });
                    }
                }
            }
            ExprKind::StructLiteral {
                module,
                name,
                type_args,
                fields,
            }
        }
        "index_expr" => {
            let object = convert_expr(node.child_by_field_name("object").unwrap(), source);
            let index = convert_expr(node.child_by_field_name("index").unwrap(), source);
            ExprKind::Index {
                object: Box::new(object),
                index: Box::new(index),
            }
        }
        "slice_expr" => {
            let object = convert_expr(node.child_by_field_name("object").unwrap(), source);
            let start = convert_expr(node.child_by_field_name("start").unwrap(), source);
            let end = convert_expr(node.child_by_field_name("end").unwrap(), source);
            ExprKind::Slice {
                object: Box::new(object),
                start: Box::new(start),
                end: Box::new(end),
            }
        }
        "array_literal" => {
            let mut elements = Vec::new();
            if let Some(elem_list) = named_child_by_kind(node, "element_list") {
                let mut cursor = elem_list.walk();
                for child in elem_list.named_children(&mut cursor) {
                    elements.push(convert_expr(child, source));
                }
            }
            let elem_ty = node
                .child_by_field_name("type_args")
                .map(|ta| convert_type_args(ta, source).into_iter().next().unwrap());
            ExprKind::ArrayLiteral(elements, elem_ty)
        }
        "array_repeat" => {
            let element = convert_expr(node.child_by_field_name("element").unwrap(), source);
            let count = convert_expr(node.child_by_field_name("count").unwrap(), source);
            ExprKind::ArrayRepeat {
                element: Box::new(element),
                count: Box::new(count),
            }
        }
        "parenthesized_expression" => {
            let mut cursor = node.walk();
            let inner = node.named_children(&mut cursor).next().unwrap();
            return convert_expr(inner, source);
        }
        "tuple_literal" => {
            let mut elements = Vec::new();
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                elements.push(convert_expr(child, source));
            }
            ExprKind::TupleLiteral(elements)
        }
        "if_expression" => return convert_if_expression(node, source),
        "path_expr" => {
            // Collect all path_segment children
            let mut segments: Vec<(String, Vec<Type>)> = Vec::new();
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if child.kind() == "path_segment" {
                    let name =
                        node_text(child.child_by_field_name("name").unwrap(), source).to_string();
                    let type_args = child
                        .child_by_field_name("type_args")
                        .map(|n| convert_type_args(n, source))
                        .unwrap_or_default();
                    segments.push((name, type_args));
                }
            }
            // Last segment = variant_name, second-to-last = enum_name, rest = module path
            let (variant_name, _) = segments.pop().unwrap();
            let (enum_name, type_args) = segments.pop().unwrap();
            let module_path: Vec<String> = segments.into_iter().map(|(n, _)| n).collect();
            ExprKind::EnumVariant {
                module_path,
                enum_name,
                type_args,
                variant_name,
            }
        }
        "match_expression" => {
            let scrutinee = convert_expr(node.child_by_field_name("scrutinee").unwrap(), source);
            let mut arms = Vec::new();
            if let Some(arm_list) = named_child_by_kind(node, "match_arm_list") {
                let mut cursor = arm_list.walk();
                for child in arm_list.named_children(&mut cursor) {
                    if child.kind() == "match_arm" {
                        arms.push(convert_match_arm(child, source));
                    }
                }
            }
            ExprKind::Match {
                scrutinee: Box::new(scrutinee),
                arms,
            }
        }
        "reflect_match_expression" => {
            let ty = convert_type(node.child_by_field_name("type").unwrap(), source);
            let mut arms = Vec::new();
            if let Some(arm_list) = named_child_by_kind(node, "reflect_match_arm_list") {
                let mut cursor = arm_list.walk();
                for child in arm_list.named_children(&mut cursor) {
                    if child.kind() == "reflect_match_arm" {
                        arms.push(convert_reflect_arm(child, source));
                    }
                }
            }
            ExprKind::MatchReflect { ty, arms }
        }
        "generic_method_call" => {
            let receiver = convert_expr(node.child_by_field_name("receiver").unwrap(), source);
            let method = node_text(node.child_by_field_name("method").unwrap(), source).to_string();
            let type_args = node
                .child_by_field_name("type_args")
                .map(|n| convert_type_args(n, source))
                .unwrap_or_default();
            let (arguments, kwargs) = convert_arguments(node, source);
            ExprKind::MethodCall {
                receiver: Box::new(receiver),
                method,
                type_args,
                arguments,
                kwargs,
            }
        }
        "block" => ExprKind::Block(convert_block(node, source)),
        "loop_expression" => {
            let body_node = node.child_by_field_name("body").unwrap();
            ExprKind::Loop(convert_block(body_node, source))
        }
        "closure_expr" => {
            let mut parameters = Vec::new();
            if let Some(param_list) = named_child_by_kind(node, "closure_param_list") {
                let mut cursor = param_list.walk();
                for child in param_list.named_children(&mut cursor) {
                    if child.kind() == "closure_param" {
                        parameters.push(convert_closure_parameter(child, source));
                    }
                }
            }
            let return_type = node
                .child_by_field_name("return_type")
                .map(|n| convert_type(n, source));
            let body_node = node.child_by_field_name("body").unwrap();
            let body = if body_node.kind() == "block" {
                convert_block(body_node, source)
            } else {
                let body_span = source_span(body_node);
                vec![Statement {
                    kind: StatementKind::Expression(convert_expr(body_node, source)),
                    span: body_span,
                }]
            };
            ExprKind::Closure {
                parameters,
                return_type,
                body,
            }
        }
        "binary_expression" => {
            let left = convert_expr(node.child_by_field_name("left").unwrap(), source);
            let right = convert_expr(node.child_by_field_name("right").unwrap(), source);
            let op_node = node.child_by_field_name("operator").unwrap();
            let op = match node_text(op_node, source) {
                "+" => BinOp::Add,
                "-" => BinOp::Sub,
                "*" => BinOp::Mul,
                "/" => BinOp::Div,
                "%" => BinOp::Mod,
                "==" => BinOp::Eq,
                "!=" => BinOp::Ne,
                "<" => BinOp::Lt,
                "<=" => BinOp::Le,
                ">" => BinOp::Gt,
                ">=" => BinOp::Ge,
                "&&" => BinOp::And,
                "||" => BinOp::Or,
                "&" => BinOp::BitAnd,
                "|" => BinOp::BitOr,
                "^" => BinOp::BitXor,
                "<<" => BinOp::Shl,
                ">>" => BinOp::Shr,
                "++" => BinOp::WrapAdd,
                "--" => BinOp::WrapSub,
                "**" => BinOp::WrapMul,
                other => panic!("unexpected operator: {other}"),
            };
            ExprKind::BinaryOp {
                op,
                left: Box::new(left),
                right: Box::new(right),
            }
        }
        "not_expression" => {
            let operand = convert_expr(node.child_by_field_name("operand").unwrap(), source);
            ExprKind::Not(Box::new(operand))
        }
        other => panic!("unexpected expression node kind: {other}"),
    };
    Expr { kind, span }
}

fn convert_match_arm(node: tree_sitter::Node, source: &str) -> MatchArm {
    let pattern = convert_pattern(node.child_by_field_name("pattern").unwrap(), source);
    let body = convert_expr(node.child_by_field_name("body").unwrap(), source);
    MatchArm { pattern, body }
}

fn convert_reflect_arm(node: tree_sitter::Node, source: &str) -> ReflectArm {
    let pattern_node = node.child_by_field_name("pattern").unwrap();
    // String literal patterns are special-cased: they stay strings instead of
    // desugaring to byte array literals like string literal expressions do.
    let pattern = if pattern_node.kind() == "string_literal" {
        let text = node_text(pattern_node, source);
        ReflectPattern::Kind(text[1..text.len() - 1].to_string())
    } else {
        ReflectPattern::Wildcard
    };
    let body = convert_expr(node.child_by_field_name("body").unwrap(), source);
    ReflectArm { pattern, body }
}

fn convert_pattern(node: tree_sitter::Node, source: &str) -> Pattern {
    match node.kind() {
        "match_pattern" => {
            // Wrapper node — unwrap to the actual pattern child
            let mut cursor = node.walk();
            let child = node.named_children(&mut cursor).next().unwrap();
            convert_pattern(child, source)
        }
        "variant_pattern" => {
            // Collect all path_segment children
            let mut segments: Vec<(String, Vec<Type>)> = Vec::new();
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if child.kind() == "path_segment" {
                    let name =
                        node_text(child.child_by_field_name("name").unwrap(), source).to_string();
                    let type_args = child
                        .child_by_field_name("type_args")
                        .map(|n| convert_type_args(n, source))
                        .unwrap_or_default();
                    segments.push((name, type_args));
                }
            }
            let (variant_name, _) = segments.pop().unwrap();
            let (enum_name, type_args) = segments.pop().unwrap();
            let module_path: Vec<String> = segments.into_iter().map(|(n, _)| n).collect();
            let binding =
                node_text(node.child_by_field_name("binding").unwrap(), source).to_string();
            Pattern::Variant {
                module_path,
                enum_name,
                type_args,
                variant_name,
                binding: Some(binding),
            }
        }
        "unit_variant_pattern" => {
            let mut segments: Vec<(String, Vec<Type>)> = Vec::new();
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if child.kind() == "path_segment" {
                    let name =
                        node_text(child.child_by_field_name("name").unwrap(), source).to_string();
                    let type_args = child
                        .child_by_field_name("type_args")
                        .map(|n| convert_type_args(n, source))
                        .unwrap_or_default();
                    segments.push((name, type_args));
                }
            }
            let (variant_name, _) = segments.pop().unwrap();
            let (enum_name, type_args) = segments.pop().unwrap();
            let module_path: Vec<String> = segments.into_iter().map(|(n, _)| n).collect();
            Pattern::Variant {
                module_path,
                enum_name,
                type_args,
                variant_name,
                binding: None,
            }
        }
        "wildcard_pattern" => {
            let name = node_text(node.child_by_field_name("name").unwrap(), source).to_string();
            Pattern::Wildcard(name)
        }
        other => panic!("unexpected pattern node kind: {other}"),
    }
}

fn convert_type(node: tree_sitter::Node, source: &str) -> Type {
    match node.kind() {
        "named_type" => {
            let mut cursor = node.walk();
            let ident = node.named_children(&mut cursor).next().unwrap();
            let name = node_text(ident, source).to_string();
            if let Some(ta_node) = node.child_by_field_name("type_args") {
                let type_args = convert_type_args(ta_node, source);
                Type::Generic { name, type_args }
            } else {
                Type::Named(name)
            }
        }
        "qualified_type" => {
            let module = node_text(node.child_by_field_name("module").unwrap(), source);
            let name = node_text(node.child_by_field_name("name").unwrap(), source);
            // Encode qualified type as "module::Name" — resolve stage will rewrite
            if let Some(ta_node) = node.child_by_field_name("type_args") {
                let type_args = convert_type_args(ta_node, source);
                Type::Generic {
                    name: format!("{module}::{name}"),
                    type_args,
                }
            } else {
                Type::Named(format!("{module}::{name}"))
            }
        }
        "reference_type" => {
            let inner = convert_type(node.child_by_field_name("inner").unwrap(), source);
            Type::Reference(Box::new(inner))
        }
        "nullable_reference_type" => {
            let inner = convert_type(node.child_by_field_name("inner").unwrap(), source);
            Type::NullableReference(Box::new(inner))
        }
        "unique_type" => {
            let inner = convert_type(node.child_by_field_name("inner").unwrap(), source);
            Type::Unique(Box::new(inner))
        }
        "slice_type" => {
            let element = convert_type(node.child_by_field_name("element").unwrap(), source);
            Type::Slice(Box::new(element))
        }
        "fixed_array_type" => {
            let element = convert_type(node.child_by_field_name("element").unwrap(), source);
            let size_text = node_text(node.child_by_field_name("size").unwrap(), source);
            // The array size literal may carry a suffix and a radix prefix.
            let (num_str, _) = parse_integer_suffix(size_text);
            let size = parse_integer_value(num_str) as u64;
            Type::FixedArray(Box::new(element), size)
        }
        "tuple_type" => {
            let mut types = Vec::new();
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                types.push(convert_type(child, source));
            }
            Type::Tuple(types)
        }
        "function_type" => {
            let mut params = Vec::new();
            if let Some(param_list) = named_child_by_kind(node, "fn_type_param_list") {
                let mut cursor = param_list.walk();
                for child in param_list.named_children(&mut cursor) {
                    if child.kind() == "fn_type_param" {
                        let name = child
                            .child_by_field_name("name")
                            .map(|n| node_text(n, source).to_string());
                        let ty = convert_type(child.child_by_field_name("type").unwrap(), source);
                        params.push((name, ty));
                    }
                }
            }
            let return_type = node
                .child_by_field_name("return_type")
                .map(|n| Box::new(convert_type(n, source)));
            Type::Function {
                params,
                return_type,
            }
        }
        other => panic!("unexpected type node kind: {other}"),
    }
}

/// Parse an integer literal body (suffix already stripped), honoring a `0b`
/// binary, `0o` octal, or `0x` hex prefix. Saturates on overflow.
fn parse_integer_value(num_str: &str) -> i128 {
    let (digits, radix) = if let Some(d) = num_str.strip_prefix("0b") {
        (d, 2)
    } else if let Some(d) = num_str.strip_prefix("0o") {
        (d, 8)
    } else if let Some(d) = num_str.strip_prefix("0x") {
        (d, 16)
    } else {
        (num_str, 10)
    };
    i128::from_str_radix(digits, radix).unwrap_or(i128::MAX)
}

fn parse_integer_suffix(text: &str) -> (&str, IntegerType) {
    let suffixes: &[(&str, IntegerType)] = &[
        ("i8", IntegerType::Int8),
        ("i16", IntegerType::Int16),
        ("i32", IntegerType::Int32),
        ("i64", IntegerType::Int64),
        ("u8", IntegerType::Uint8),
        ("u16", IntegerType::Uint16),
        ("u32", IntegerType::Uint32),
        ("u64", IntegerType::Uint64),
        ("u", IntegerType::Uint),
    ];
    for &(suffix, ty) in suffixes {
        if let Some(num) = text.strip_suffix(suffix) {
            return (num, ty);
        }
    }
    (text, IntegerType::Int)
}

fn named_child_by_kind<'a>(
    node: tree_sitter::Node<'a>,
    kind: &str,
) -> Option<tree_sitter::Node<'a>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).find(|c| c.kind() == kind)
}

fn unescape_string(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next().expect("unexpected end of string escape") {
                '\\' => out.push(b'\\'),
                '"' => out.push(b'"'),
                'n' => out.push(b'\n'),
                't' => out.push(b'\t'),
                '0' => out.push(0),
                other => panic!("unknown escape sequence: \\{other}"),
            }
        } else {
            let mut buf = [0u8; 4];
            let encoded = c.encode_utf8(&mut buf);
            out.extend_from_slice(encoded.as_bytes());
        }
    }
    out
}
