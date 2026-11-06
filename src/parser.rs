use crate::ast::*;
use std::fmt;

/// A syntax error reported by the parser.
#[derive(Debug)]
pub struct ParseError {
    /// Zero-indexed line.
    pub line: usize,
    /// Zero-indexed byte column.
    pub column: usize,
    /// Error description.
    pub message: String,
}

impl ParseError {
    fn at(node: tree_sitter::Node, message: impl Into<String>) -> Self {
        let start = node.start_position();
        Self {
            line: start.row,
            column: start.column,
            message: message.into(),
        }
    }

    fn in_literal(
        node: tree_sitter::Node,
        source: &str,
        offset: usize,
        message: impl Into<String>,
    ) -> Self {
        let prefix = &source[..node.start_byte() + offset];
        Self {
            line: prefix.bytes().filter(|&byte| byte == b'\n').count(),
            column: prefix.len() - prefix.rfind('\n').map_or(0, |newline| newline + 1),
            message: message.into(),
        }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.line + 1, self.column + 1, self.message)
    }
}

/// Parses a Solar source file, returning located syntax and AST-conversion errors.
pub fn parse(source: &str) -> Result<SourceFile, Vec<ParseError>> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_solar::LANGUAGE.into())
        .map_err(|error| {
            vec![ParseError {
                line: 0,
                column: 0,
                message: format!("failed to set tree-sitter language: {error}"),
            }]
        })?;

    let tree = parser.parse(source, None).ok_or_else(|| {
        vec![ParseError {
            line: 0,
            column: 0,
            message: "tree-sitter parse failed".to_string(),
        }]
    })?;
    let root = tree.root_node();

    let mut errors = Vec::new();
    collect_errors(root, source, &mut errors);
    collect_visibility_errors(root, source, &mut errors);

    if !errors.is_empty() {
        return Err(errors);
    }

    convert_source_file(root, source).map_err(|error| vec![error])
}

fn collect_visibility_errors(node: tree_sitter::Node, source: &str, errors: &mut Vec<ParseError>) {
    if node.kind() == "struct_def" && !has_pub_keyword(node, source) {
        if let Some(field_list) = named_child_by_kind(node, "field_list") {
            for field in code_children(field_list) {
                if field.kind() == "field_def" && has_pub_keyword(field, source) {
                    let start = field.start_position();
                    let name = field
                        .child_by_field_name("name")
                        .map(|name| node_text(name, source))
                        .unwrap_or("<unknown>");
                    let struct_name = node
                        .child_by_field_name("name")
                        .map(|name| node_text(name, source))
                        .unwrap_or("<unknown>");
                    errors.push(ParseError {
                        line: start.row,
                        column: start.column,
                        message: format!("`pub` field `{name}` in non-pub struct `{struct_name}`"),
                    });
                }
            }
        }
        if let Some(tuple_body) = named_child_by_kind(node, "tuple_struct_body")
            && let Some(field_list) = named_child_by_kind(tuple_body, "tuple_struct_field_list")
        {
            for field in code_children(field_list) {
                if field.kind() == "tuple_struct_field" && has_pub_keyword(field, source) {
                    let start = field.start_position();
                    let struct_name = node
                        .child_by_field_name("name")
                        .map(|name| node_text(name, source))
                        .unwrap_or("<unknown>");
                    errors.push(ParseError {
                        line: start.row,
                        column: start.column,
                        message: format!("`pub` field in non-pub struct `{struct_name}`"),
                    });
                }
            }
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_visibility_errors(child, source, errors);
    }
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
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if !child.is_named() && node_text(child, source) == "pub" {
            return true;
        }
    }
    false
}

fn node_text<'a>(node: tree_sitter::Node, source: &'a str) -> &'a str {
    &source[node.byte_range()]
}

fn required_field<'a>(
    node: tree_sitter::Node<'a>,
    name: &str,
) -> Result<tree_sitter::Node<'a>, ParseError> {
    node.child_by_field_name(name)
        .ok_or_else(|| ParseError::at(node, format!("missing {name} in {}", node.kind())))
}

fn required_named_child<'a>(
    node: tree_sitter::Node<'a>,
    kind: &str,
) -> Result<tree_sitter::Node<'a>, ParseError> {
    named_child_by_kind(node, kind)
        .ok_or_else(|| ParseError::at(node, format!("missing {kind} in {}", node.kind())))
}

fn first_code_child(node: tree_sitter::Node) -> Result<tree_sitter::Node, ParseError> {
    code_children(node)
        .into_iter()
        .next()
        .ok_or_else(|| ParseError::at(node, format!("expected a child in {}", node.kind())))
}

fn literal_contents<'a>(node: tree_sitter::Node, source: &'a str) -> Result<&'a str, ParseError> {
    let text = node_text(node, source);
    text.get(1..text.len().saturating_sub(1))
        .ok_or_else(|| ParseError::at(node, "expected a quoted literal"))
}

/// Gather the `///` doc comment attached to `node`: the run of consecutive
/// `doc_comment` siblings immediately preceding it (doc comments are `extra`
/// nodes, so they appear as siblings, with only whitespace — which produces no
/// node — between them and the item). Each line drops its `///` and one
/// optional leading space; multiple lines join with `\n`. Returns `None` when
/// there is no doc comment.
fn leading_doc(node: tree_sitter::Node, source: &str) -> Option<String> {
    let mut lines = Vec::new();
    let mut sibling = node.prev_sibling();
    while let Some(current) = sibling {
        if current.kind() != "doc_comment" {
            break;
        }
        let text = node_text(current, source);
        let body = text.strip_prefix("///").unwrap_or(text);
        lines.push(body.strip_prefix(' ').unwrap_or(body).trim_end());
        sibling = current.prev_sibling();
    }
    if lines.is_empty() {
        return None;
    }
    lines.reverse();
    Some(lines.join("\n"))
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

fn convert_source_file(node: tree_sitter::Node, source: &str) -> Result<SourceFile, ParseError> {
    let mut items = Vec::new();
    for child in code_children(node) {
        match child.kind() {
            "struct_def" => items.push(TopLevelItem::Struct(convert_struct_def(child, source)?)),
            "function_def" => {
                items.push(TopLevelItem::Function(convert_function_def(child, source)?))
            }
            "enum_def" => items.push(TopLevelItem::Enum(convert_enum_def(child, source)?)),
            "method_def" => items.push(TopLevelItem::Method(convert_function_def(child, source)?)),
            "import_statement" => items.push(TopLevelItem::Import(convert_import_statement(
                child, source,
            )?)),
            "type_alias_def" => items.push(TopLevelItem::TypeAlias(convert_type_alias_def(
                child, source,
            )?)),
            "const_def" => items.push(TopLevelItem::Const(convert_const_def(child, source)?)),
            "static_def" => items.push(TopLevelItem::Static(convert_static_def(child, source)?)),
            _ => {}
        }
    }
    Ok(SourceFile { items })
}

fn convert_import_statement(
    node: tree_sitter::Node,
    source: &str,
) -> Result<ImportDef, ParseError> {
    let is_pub = has_pub_keyword(node, source);
    let span = source_span(node);

    // Find the string literal for the path
    let string_node = required_named_child(node, "string_literal")?;
    unescape_string(string_node, source)?;
    let path = literal_contents(string_node, source)?.to_string();

    // Determine import kind
    let kind = if let Some(module_name) = node.child_by_field_name("module_name") {
        ImportKind::Module(node_text(module_name, source).to_string())
    } else if let Some(name_list) = named_child_by_kind(node, "import_name_list") {
        let mut names = Vec::new();
        for child in code_children(name_list) {
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

    Ok(ImportDef {
        is_c: node.child_by_field_name("language").is_some(),
        kind,
        path,
        is_pub,
        span,
    })
}

fn convert_type_params(node: tree_sitter::Node, source: &str) -> Vec<String> {
    let mut params = Vec::new();
    for child in code_children(node) {
        if child.kind() == "identifier" {
            params.push(node_text(child, source).to_string());
        }
    }
    params
}

fn convert_function_type_params(
    node: tree_sitter::Node,
    source: &str,
) -> Result<(Vec<String>, Vec<String>), ParseError> {
    let mut inferred = Vec::new();
    let mut output = Vec::new();
    for child in code_children(node) {
        match child.kind() {
            "identifier" => inferred.push(node_text(child, source).to_string()),
            "out_type_param" => {
                output.push(node_text(required_field(child, "name")?, source).to_string())
            }
            _ => {}
        }
    }
    Ok((inferred, output))
}

fn convert_type_args(node: tree_sitter::Node, source: &str) -> Result<Vec<Type>, ParseError> {
    let mut args = Vec::new();
    for child in code_children(node) {
        args.push(convert_type(child, source)?);
    }
    Ok(args)
}

fn convert_single_type_arg(node: tree_sitter::Node, source: &str) -> Result<Type, ParseError> {
    let mut args = convert_type_args(node, source)?;
    if args.len() != 1 {
        return Err(ParseError::at(node, "expected exactly one type argument"));
    }
    args.pop()
        .ok_or_else(|| ParseError::at(node, "expected a type argument"))
}

fn convert_struct_def(node: tree_sitter::Node, source: &str) -> Result<StructDef, ParseError> {
    let name = node_text(required_field(node, "name")?, source).to_string();
    let is_pub = has_pub_keyword(node, source);
    let type_params = node
        .child_by_field_name("type_params")
        .map(|n| convert_type_params(n, source))
        .unwrap_or_default();
    let mut fields = Vec::new();
    if let Some(field_list) = named_child_by_kind(node, "field_list") {
        for child in code_children(field_list) {
            if child.kind() == "field_def" {
                fields.push(convert_field_def(child, source)?);
            }
        }
    } else if let Some(tuple_body) = named_child_by_kind(node, "tuple_struct_body")
        && let Some(field_list) = named_child_by_kind(tuple_body, "tuple_struct_field_list")
    {
        {
            for (index, child) in code_children(field_list).into_iter().enumerate() {
                if child.kind() != "tuple_struct_field" {
                    return Err(ParseError::at(child, "expected a tuple struct field"));
                }
                let is_field_pub = has_pub_keyword(child, source);
                fields.push(FieldDef {
                    name: index.to_string(),
                    ty: convert_type(required_field(child, "type")?, source)?,
                    is_pub: is_field_pub,
                    span: source_span(child),
                });
            }
        }
    }
    Ok(StructDef {
        def_id: DefId::new(0, name.clone()),
        name,
        type_params,
        fields,
        is_tuple: named_child_by_kind(node, "tuple_struct_body").is_some(),
        repr_c: node
            .child_by_field_name("attr")
            .is_some_and(|attr| named_child_by_kind(attr, "struct_repr_attr").is_some()),
        opaque: node
            .child_by_field_name("attr")
            .is_some_and(|attr| named_child_by_kind(attr, "struct_opaque_attr").is_some()),
        is_pub,
        doc: leading_doc(node, source),
        span: source_span(node),
    })
}

fn convert_const_def(node: tree_sitter::Node, source: &str) -> Result<ConstDef, ParseError> {
    let name = node_text(required_field(node, "name")?, source).to_string();
    let is_pub = has_pub_keyword(node, source);
    let ty = node
        .child_by_field_name("type")
        .map(|n| convert_type(n, source))
        .transpose()?;
    let value = Box::new(convert_expr(required_field(node, "value")?, source)?);
    Ok(ConstDef {
        name,
        ty,
        value,
        is_pub,
        doc: leading_doc(node, source),
        span: source_span(node),
    })
}

fn convert_static_def(node: tree_sitter::Node, source: &str) -> Result<StaticDef, ParseError> {
    let name = node_text(required_field(node, "name")?, source).to_string();
    let is_pub = has_pub_keyword(node, source);
    let ty = node
        .child_by_field_name("type")
        .map(|n| convert_type(n, source))
        .transpose()?;
    let value = Box::new(convert_expr(required_field(node, "value")?, source)?);
    Ok(StaticDef {
        name,
        ty,
        value,
        thread_local: node.child_by_field_name("attr").is_some(),
        is_pub,
        doc: leading_doc(node, source),
        span: source_span(node),
    })
}

fn convert_type_alias_def(
    node: tree_sitter::Node,
    source: &str,
) -> Result<TypeAliasDef, ParseError> {
    let name = node_text(required_field(node, "name")?, source).to_string();
    let is_pub = has_pub_keyword(node, source);
    let type_params = node
        .child_by_field_name("type_params")
        .map(|n| convert_type_params(n, source))
        .unwrap_or_default();
    let target_type = convert_type(required_field(node, "target_type")?, source)?;
    Ok(TypeAliasDef {
        name,
        type_params,
        target_type,
        is_pub,
        doc: leading_doc(node, source),
        span: source_span(node),
    })
}

fn convert_enum_def(node: tree_sitter::Node, source: &str) -> Result<EnumDef, ParseError> {
    let name = node_text(required_field(node, "name")?, source).to_string();
    let is_pub = has_pub_keyword(node, source);
    let type_params = node
        .child_by_field_name("type_params")
        .map(|n| convert_type_params(n, source))
        .unwrap_or_default();
    let mut variants = Vec::new();
    if let Some(variant_list) = named_child_by_kind(node, "variant_list") {
        for child in code_children(variant_list) {
            if child.kind() == "variant_def" {
                let vname = node_text(required_field(child, "name")?, source).to_string();
                let inner_type = child
                    .child_by_field_name("inner_type")
                    .map(|n| convert_type(n, source))
                    .transpose()?;
                variants.push(VariantDef {
                    name: vname,
                    inner_type,
                    span: source_span(child),
                });
            }
        }
    }
    Ok(EnumDef {
        def_id: DefId::new(0, name.clone()),
        name,
        type_params,
        variants,
        is_pub,
        doc: leading_doc(node, source),
        span: source_span(node),
    })
}

fn convert_field_def(node: tree_sitter::Node, source: &str) -> Result<FieldDef, ParseError> {
    let name = node_text(required_field(node, "name")?, source).to_string();
    let ty = convert_type(required_field(node, "type")?, source)?;
    let is_pub = has_pub_keyword(node, source);
    Ok(FieldDef {
        name,
        ty,
        is_pub,
        span: source_span(node),
    })
}

fn convert_function_def(node: tree_sitter::Node, source: &str) -> Result<FunctionDef, ParseError> {
    let name = node
        .child_by_field_name("name")
        .map(|name| node_text(name, source).to_string())
        .unwrap_or_default();
    let associated_type = node
        .child_by_field_name("associated_type")
        .map(|owner| convert_type(owner, source))
        .transpose()?;
    let associated_type_params = node
        .child_by_field_name("associated_type_params")
        .map(|params| convert_type_params(params, source))
        .unwrap_or_default();
    let is_pub = has_pub_keyword(node, source);
    let is_unsafe = node.child_by_field_name("unsafe").is_some();
    let (type_params, out_type_params) = node
        .child_by_field_name("type_params")
        .map(|n| convert_function_type_params(n, source))
        .transpose()?
        .unwrap_or_default();

    let mut parameters = Vec::new();
    if let Some(param_list) = named_child_by_kind(node, "parameter_list") {
        for child in code_children(param_list) {
            if child.kind() == "parameter" {
                parameters.push(convert_parameter(child, source)?);
            }
        }
    }

    let return_type_node = node.child_by_field_name("return_type");
    let return_type = return_type_node
        .map(|n| convert_type(n, source))
        .transpose()?;
    let return_type_span = return_type_node.map(source_span);

    let body_node = required_field(node, "body")?;
    let body = convert_block(body_node, source)?;

    let has_attribute = |name| {
        node.child_by_field_name("attr").is_some_and(|attr| {
            attr.children(&mut attr.walk())
                .any(|child| child.kind() == name)
        })
    };
    let inline_hint = has_attribute("inline");
    let is_extension = has_attribute("ext");
    let is_overlap = has_attribute("overlap");
    if is_extension && is_overlap {
        return Err(ParseError::at(
            node,
            "method attributes `overlap` and `ext` cannot be combined",
        ));
    }

    Ok(FunctionDef {
        associated_type_params,
        associated_type,
        display_name: name.clone(),
        name: Ident::user(name),
        type_params,
        out_type_params,
        parameters,
        return_type,
        return_type_span,
        body,
        is_pub,
        is_unsafe,
        inline_hint,
        is_extension,
        is_overlap,
        doc: leading_doc(node, source),
        span: source_span(node),
    })
}

fn convert_parameter(node: tree_sitter::Node, source: &str) -> Result<Parameter, ParseError> {
    let pattern_node = required_field(node, "pattern")?;
    let pattern = convert_destructure_pattern(pattern_node, source)?;
    // An explicit type is present unless this is the inferred-type keyword form
    // (`name = default`).
    let ty = match node.child_by_field_name("type") {
        Some(type_node) => convert_type(type_node, source)?,
        None => Type::Infer,
    };
    let default = node
        .child_by_field_name("default")
        .map(|n| convert_expr(n, source).map(Box::new))
        .transpose()?;
    Ok(Parameter {
        pattern,
        ty,
        default,
        span: source_span(node),
    })
}

fn convert_try_statement(node: tree_sitter::Node, source: &str) -> Result<Statement, ParseError> {
    let span = source_span(node);
    let body = convert_block(required_field(node, "body")?, source)?;
    let handler = convert_block(required_field(node, "handler")?, source)?;
    let binding = Ident::user(node_text(required_field(node, "binding")?, source));
    let binding_type = node
        .child_by_field_name("binding_type")
        .map(|ty| convert_type(ty, source))
        .transpose()?;

    Ok(Statement {
        kind: StatementKind::Try {
            body,
            binding,
            binding_type,
            handler,
        },
        span,
    })
}

fn convert_closure_parameter(
    node: tree_sitter::Node,
    source: &str,
) -> Result<Parameter, ParseError> {
    let name = node_text(required_field(node, "name")?, source).to_string();
    let ty = match node.child_by_field_name("type") {
        Some(type_node) => convert_type(type_node, source)?,
        None => Type::Infer,
    };
    Ok(Parameter {
        pattern: DestructurePattern::Name(Ident::user(name)),
        ty,
        default: None,
        span: source_span(node),
    })
}

type Arguments = (Vec<Expr>, Vec<(String, Expr)>);

/// Convert an `argument_list` node into positional arguments and keyword
/// arguments (`name = value`).
fn convert_arguments(node: tree_sitter::Node, source: &str) -> Result<Arguments, ParseError> {
    let mut positional = Vec::new();
    let mut kwargs = Vec::new();
    if let Some(arg_list) = named_child_by_kind(node, "argument_list") {
        for arg in code_children(arg_list) {
            if arg.kind() != "argument" {
                continue;
            }
            let value = convert_expr(required_field(arg, "value")?, source)?;
            match arg.child_by_field_name("name") {
                Some(name_node) => {
                    kwargs.push((node_text(name_node, source).to_string(), value));
                }
                None => positional.push(value),
            }
        }
    }
    Ok((positional, kwargs))
}

fn convert_destructure_pattern(
    node: tree_sitter::Node,
    source: &str,
) -> Result<DestructurePattern, ParseError> {
    Ok(match node.kind() {
        "identifier" => DestructurePattern::Name(Ident::user(node_text(node, source))),
        "tuple_pattern" => {
            let mut elements = Vec::new();
            for child in code_children(node) {
                elements.push(convert_destructure_pattern(child, source)?);
            }
            DestructurePattern::Tuple(elements)
        }
        "struct_pattern" => {
            let module = node
                .child_by_field_name("module")
                .map(|n| node_text(n, source).to_string());
            let name = node_text(required_field(node, "name")?, source).to_string();
            let mut fields = Vec::new();
            for child in code_children(node) {
                if child.kind() == "struct_pattern_field" {
                    fields.push(convert_struct_pattern_field(child, source)?);
                }
            }
            DestructurePattern::Struct {
                module,
                name: DefId::new(0, name),
                fields,
            }
        }
        "array_pattern" => {
            let mut elements = Vec::new();
            for child in code_children(node) {
                elements.push(convert_destructure_pattern(child, source)?);
            }
            DestructurePattern::Array(elements)
        }
        other => {
            return Err(ParseError::at(
                node,
                format!("unexpected destructure pattern node kind: {other}"),
            ));
        }
    })
}

fn convert_struct_pattern_field(
    node: tree_sitter::Node,
    source: &str,
) -> Result<DestructureField, ParseError> {
    let field_name = node_text(required_field(node, "field_name")?, source).to_string();
    let pattern = if let Some(pat_node) = node.child_by_field_name("pattern") {
        convert_destructure_pattern(pat_node, source)?
    } else {
        // Shorthand: `field_name` binds to same name
        DestructurePattern::Name(Ident::user(field_name.clone()))
    };
    Ok(DestructureField {
        field_name,
        pattern,
    })
}

fn convert_block(node: tree_sitter::Node, source: &str) -> Result<Vec<Statement>, ParseError> {
    let mut stmts = Vec::new();
    for child in code_children(node) {
        match child.kind() {
            "let_statement" => stmts.push(convert_let_statement(child, source)?),
            "assignment_statement" => stmts.push(convert_assignment_statement(child, source)?),
            "expression_statement" => stmts.push(convert_expression_statement(child, source)?),
            "unsafe_block_statement" => {
                let unsafe_block = required_named_child(child, "unsafe_block")?;
                stmts.push(Statement {
                    kind: StatementKind::Expression(convert_expr(unsafe_block, source)?),
                    span: source_span(child),
                });
            }
            "if_statement" => stmts.push(convert_if_statement(child, source)?),
            "while_statement" => stmts.push(convert_while_statement(child, source)?),
            "for_statement" => stmts.push(Statement {
                kind: StatementKind::Expression(convert_expr(
                    required_named_child(child, "for_expression")?,
                    source,
                )?),
                span: source_span(child),
            }),
            "reflect_fields_statement" => {
                stmts.push(convert_reflect_fields_statement(child, source, false)?)
            }
            "reflect_fields_pair_statement" => {
                stmts.push(convert_reflect_fields_statement(child, source, true)?)
            }
            "reflect_variant_statement" => {
                stmts.push(convert_reflect_variant_statement(child, source, false)?)
            }
            "reflect_variant_pair_statement" => {
                stmts.push(convert_reflect_variant_statement(child, source, true)?)
            }
            "return_statement" => stmts.push(convert_return_statement(child, source)?),
            "break_statement" => stmts.push(Statement {
                kind: StatementKind::Break(
                    child
                        .child_by_field_name("value")
                        .map(|n| convert_expr(n, source))
                        .transpose()?,
                ),
                span: source_span(child),
            }),
            "continue_statement" => stmts.push(Statement {
                kind: StatementKind::Continue,
                span: source_span(child),
            }),
            "try_statement" => stmts.push(convert_try_statement(child, source)?),
            "function_def" => {
                let span = source_span(child);
                stmts.push(Statement {
                    kind: StatementKind::NestedFunction(convert_function_def(child, source)?),
                    span,
                });
            }
            "const_def" => {
                let span = source_span(child);
                stmts.push(Statement {
                    kind: StatementKind::Const(convert_const_def(child, source)?),
                    span,
                });
            }
            _ => {}
        }
    }
    // Only an unterminated tail expression supplies the block's value.
    if let Some(tail) = node.child_by_field_name("tail") {
        let span = source_span(tail);
        stmts.push(Statement {
            kind: StatementKind::Expression(convert_expr(tail, source)?),
            span,
        });
    } else {
        let mut span = source_span(node);
        span.start = span.end;
        span.start.col -= 1;
        span.end = span.start;
        stmts.push(Statement {
            kind: StatementKind::Expression(Expr {
                kind: ExprKind::TupleLiteral(Vec::new()),
                span,
            }),
            span,
        });
    }
    Ok(stmts)
}

fn convert_let_statement(node: tree_sitter::Node, source: &str) -> Result<Statement, ParseError> {
    let span = source_span(node);
    let pattern_node = required_field(node, "pattern")?;
    let pattern = convert_destructure_pattern(pattern_node, source)?;
    let ty = node
        .child_by_field_name("type")
        .map(|n| convert_type(n, source))
        .transpose()?;
    let value = convert_expr(required_field(node, "value")?, source)?;
    Ok(Statement {
        kind: StatementKind::Let { pattern, ty, value },
        span,
    })
}

fn convert_assignment_statement(
    node: tree_sitter::Node,
    source: &str,
) -> Result<Statement, ParseError> {
    let span = source_span(node);
    let target = convert_expr(required_field(node, "target")?, source)?;
    let value = convert_expr(required_field(node, "value")?, source)?;
    Ok(Statement {
        kind: StatementKind::Assignment { target, value },
        span,
    })
}

fn convert_if_statement(node: tree_sitter::Node, source: &str) -> Result<Statement, ParseError> {
    let span = source_span(node);
    let condition = convert_expr(required_field(node, "condition")?, source)?;
    let body_node = required_field(node, "body")?;
    let body = convert_block(body_node, source)?;
    let else_body = if let Some(else_node) = node.child_by_field_name("else_body") {
        match else_node.kind() {
            "block" => convert_block(else_node, source)?,
            "if_statement" => vec![convert_if_statement(else_node, source)?],
            _ => {
                return Err(ParseError::at(
                    node,
                    format!("unexpected else_body kind: {}", else_node.kind()),
                ));
            }
        }
    } else {
        Vec::new()
    };
    Ok(Statement {
        kind: StatementKind::If {
            condition,
            body,
            else_body,
        },
        span,
    })
}

fn convert_while_statement(node: tree_sitter::Node, source: &str) -> Result<Statement, ParseError> {
    let span = source_span(node);
    let condition = convert_expr(required_field(node, "condition")?, source)?;
    let body_node = required_field(node, "body")?;
    let body = convert_block(body_node, source)?;
    Ok(Statement {
        kind: StatementKind::While { condition, body },
        span,
    })
}

fn convert_for_expression(node: tree_sitter::Node, source: &str) -> Result<ExprKind, ParseError> {
    let variable = Ident::user(node_text(required_field(node, "variable")?, source));
    let body = convert_block(required_field(node, "body")?, source)?;
    Ok(if let Some(start) = node.child_by_field_name("start") {
        ExprKind::ForRange {
            variable,
            start: Box::new(convert_expr(start, source)?),
            end: Box::new(convert_expr(required_field(node, "end")?, source)?),
            body,
        }
    } else {
        ExprKind::ForIn {
            variable,
            iterable: Box::new(convert_expr(required_field(node, "iterable")?, source)?),
            body,
        }
    })
}

fn convert_reflect_fields_statement(
    node: tree_sitter::Node,
    source: &str,
    paired: bool,
) -> Result<Statement, ParseError> {
    let span = source_span(node);
    let pattern = convert_destructure_pattern(required_field(node, "variable")?, source)?;
    let object = convert_expr(required_field(node, "object")?, source)?;
    let body = convert_block(required_field(node, "body")?, source)?;
    Ok(Statement {
        kind: StatementKind::ForReflectFields {
            pattern,
            object,
            body,
            paired,
        },
        span,
    })
}

fn convert_reflect_variant_statement(
    node: tree_sitter::Node,
    source: &str,
    paired: bool,
) -> Result<Statement, ParseError> {
    let span = source_span(node);
    let pattern = convert_destructure_pattern(required_field(node, "pattern")?, source)?;
    let object = convert_expr(required_field(node, "object")?, source)?;
    let body = convert_block(required_field(node, "body")?, source)?;
    Ok(Statement {
        kind: StatementKind::MatchReflectVariant {
            pattern,
            object,
            body,
            paired,
        },
        span,
    })
}

fn convert_expression_statement(
    node: tree_sitter::Node,
    source: &str,
) -> Result<Statement, ParseError> {
    let span = source_span(node);
    let expr_node = first_code_child(node)?;
    Ok(Statement {
        kind: StatementKind::Expression(convert_expr(expr_node, source)?),
        span,
    })
}

fn convert_return_statement(
    node: tree_sitter::Node,
    source: &str,
) -> Result<Statement, ParseError> {
    let span = source_span(node);
    let value = node.child_by_field_name("value").map_or_else(
        || {
            let mut value_span = span;
            value_span.start = value_span.end;
            Ok(Expr {
                kind: ExprKind::TupleLiteral(Vec::new()),
                span: value_span,
            })
        },
        |value| convert_expr(value, source),
    )?;
    let kind = StatementKind::Return(value);
    Ok(Statement { kind, span })
}

fn convert_if_expression(node: tree_sitter::Node, source: &str) -> Result<Expr, ParseError> {
    let span = source_span(node);
    let condition = convert_expr(required_field(node, "condition")?, source)?;
    let then_body_node = required_field(node, "then_body")?;
    let then_body = convert_block(then_body_node, source)?;
    let else_node = required_field(node, "else_body")?;
    let else_body = match else_node.kind() {
        "block" => convert_block(else_node, source)?,
        "if_expression" => {
            let else_span = source_span(else_node);
            vec![Statement {
                kind: StatementKind::Expression(convert_if_expression(else_node, source)?),
                span: else_span,
            }]
        }
        _ => {
            return Err(ParseError::at(
                node,
                format!("unexpected else_body kind: {}", else_node.kind()),
            ));
        }
    };
    Ok(Expr {
        kind: ExprKind::If {
            condition: Box::new(condition),
            then_body,
            else_body,
        },
        span,
    })
}

fn convert_expr(node: tree_sitter::Node, source: &str) -> Result<Expr, ParseError> {
    let span = source_span(node);
    let kind = match node.kind() {
        "identifier" => ExprKind::Identifier(Ident::user(node_text(node, source))),
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
            } else if let Some(n) = text.strip_suffix('f') {
                (n, FloatType::Float64)
            } else {
                return Err(ParseError::at(node, "expected a floating-point suffix"));
            };
            // Parse f32 literals in f32 precision (then widen exactly) so the
            // value is correctly rounded once, not double-rounded through f64.
            let value = match float_ty {
                FloatType::Float32 => num_str.parse::<f32>().map(f64::from),
                FloatType::Float64 => num_str.parse::<f64>(),
            }
            .map_err(|error| ParseError::at(node, format!("invalid float literal: {error}")))?;
            ExprKind::FloatLiteral(value, float_ty)
        }
        "boolean_literal" => {
            let text = node_text(node, source);
            ExprKind::BooleanLiteral(text == "true")
        }
        "string_literal" => ExprKind::StringLiteral(unescape_string(node, source)?),
        "char_literal" => {
            let bytes = unescape_string(node, source)?;
            let [byte] = bytes.as_slice() else {
                return Err(ParseError::at(
                    node,
                    "character literal must be a single byte",
                ));
            };
            ExprKind::CharLiteral(*byte)
        }
        "field_access" => {
            let object = convert_expr(required_field(node, "object")?, source)?;
            let field_node = required_field(node, "field")?;
            let field = node_text(field_node, source).to_string();
            ExprKind::FieldAccess {
                object: Box::new(object),
                field,
            }
        }
        "deref_expr" => {
            let operand = convert_expr(required_field(node, "operand")?, source)?;
            ExprKind::Deref(Box::new(operand))
        }
        "reference_expr" => {
            let operand = convert_expr(required_field(node, "operand")?, source)?;
            ExprKind::Reference(Box::new(operand))
        }
        "unique_expr" => {
            let operand = convert_expr(required_field(node, "operand")?, source)?;
            ExprKind::Unique(Box::new(operand))
        }
        "null_expr" => ExprKind::NullLiteral(convert_single_type_arg(
            required_field(node, "type_args")?,
            source,
        )?),
        "call_expr" => {
            let func_node = required_field(node, "function")?;
            let (arguments, kwargs) = convert_arguments(node, source)?;
            // a.b(c) parses as call_expr(field_access(a, b), [c])
            // Convert to MethodCall when the callee is a field_access with an identifier field
            // (not a numeric field like .0 from tuple access)
            if matches!(func_node.kind(), "reference_expr" | "unique_expr")
                && arguments.len() == 1
                && kwargs.is_empty()
                && node
                    .parent()
                    .is_none_or(|parent| parent.kind() != "binary_expression")
            {
                // `a & (b)` / `a ^ (b)` is parsed as the invalid calls `(a&)(b)` /
                // `(a^)(b)` because postfix operators have higher precedence.
                // A direct call through a referenced function is parenthesized
                // (`(f&)(x)`), so this unparenthesized CST shape is the binary
                // expression the source spelling denotes.
                let left = convert_expr(required_field(func_node, "operand")?, source)?;
                ExprKind::BinaryOp {
                    op: if func_node.kind() == "reference_expr" {
                        BinOp::BitAnd
                    } else {
                        BinOp::BitXor
                    },
                    left: Box::new(left),
                    right: Box::new(
                        arguments
                            .into_iter()
                            .next()
                            .ok_or_else(|| ParseError::at(node, "expected a binary operand"))?,
                    ),
                }
            } else if func_node.kind() == "field_access"
                && required_field(func_node, "field")?.kind() == "identifier"
            {
                let receiver = convert_expr(required_field(func_node, "object")?, source)?;
                let method = node_text(required_field(func_node, "field")?, source).to_string();
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
                for child in code_children(func_node) {
                    if child.kind() == "path_segment" {
                        let name = node_text(required_field(child, "name")?, source).to_string();
                        let ta = child
                            .child_by_field_name("type_args")
                            .map(|n| convert_type_args(n, source))
                            .transpose()?
                            .unwrap_or_default();
                        segments.push((name, ta));
                    }
                }
                let (last_name, last_type_args) = segments.pop().ok_or_else(|| {
                    ParseError::at(node, "expected a qualified path with at least two segments")
                })?;
                let (enum_name, enum_type_args) = segments.pop().ok_or_else(|| {
                    ParseError::at(node, "expected a qualified path with at least two segments")
                })?;
                let module_path: Vec<String> = segments.into_iter().map(|(n, _)| n).collect();
                let function = Expr {
                    kind: ExprKind::EnumVariant {
                        module_path,
                        enum_name: DefId::new(0, enum_name),
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
                let function = convert_expr(func_node, source)?;
                ExprKind::Call {
                    function: Box::new(function),
                    type_args: Vec::new(),
                    arguments,
                    kwargs,
                }
            }
        }
        "generic_call_expr" => {
            let function_node = required_field(node, "function")?;
            let name = node_text(function_node, source).to_string();
            let type_args = node
                .child_by_field_name("type_args")
                .map(|n| convert_type_args(n, source))
                .transpose()?
                .unwrap_or_default();
            let (arguments, kwargs) = convert_arguments(node, source)?;
            ExprKind::Call {
                function: Box::new(Expr {
                    kind: ExprKind::Identifier(Ident::user(name)),
                    span: source_span(function_node),
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
            let name = node_text(required_field(node, "name")?, source).to_string();
            let type_args = node
                .child_by_field_name("type_args")
                .map(|n| convert_type_args(n, source))
                .transpose()?
                .unwrap_or_default();
            let mut fields = Vec::new();
            if let Some(init_list) = named_child_by_kind(node, "field_init_list") {
                for child in code_children(init_list) {
                    if child.kind() == "field_init" {
                        let fname = node_text(required_field(child, "name")?, source).to_string();
                        let value = convert_expr(required_field(child, "value")?, source)?;
                        fields.push(FieldInit { name: fname, value });
                    }
                }
            }
            ExprKind::StructLiteral {
                module,
                name: DefId::new(0, name),
                type_args,
                fields,
            }
        }
        "index_expr" => {
            let object = convert_expr(required_field(node, "object")?, source)?;
            let index = convert_expr(required_field(node, "index")?, source)?;
            ExprKind::Index {
                object: Box::new(object),
                index: Box::new(index),
            }
        }
        "slice_expr" => {
            let object = convert_expr(required_field(node, "object")?, source)?;
            let start = convert_expr(required_field(node, "start")?, source)?;
            let end = convert_expr(required_field(node, "end")?, source)?;
            ExprKind::Slice {
                object: Box::new(object),
                start: Box::new(start),
                end: Box::new(end),
            }
        }
        "array_literal" => {
            let mut elements = Vec::new();
            if let Some(elem_list) = named_child_by_kind(node, "element_list") {
                for child in code_children(elem_list) {
                    elements.push(convert_expr(child, source)?);
                }
            }
            let elem_ty = node
                .child_by_field_name("type_args")
                .map(|ta| convert_single_type_arg(ta, source))
                .transpose()?;
            ExprKind::ArrayLiteral(elements, elem_ty)
        }
        "array_repeat" => {
            let element = convert_expr(required_field(node, "element")?, source)?;
            let count = convert_expr(required_field(node, "count")?, source)?;
            ExprKind::ArrayRepeat {
                element: Box::new(element),
                count: Box::new(count),
            }
        }
        "parenthesized_expression" => {
            let inner = first_code_child(node)?;
            return convert_expr(inner, source);
        }
        "tuple_literal" => {
            let mut elements = Vec::new();
            for child in code_children(node) {
                elements.push(convert_expr(child, source)?);
            }
            ExprKind::TupleLiteral(elements)
        }
        "if_expression" => return convert_if_expression(node, source),
        "path_expr" => {
            // Collect all path_segment children
            let mut segments: Vec<(String, Vec<Type>)> = Vec::new();
            for child in code_children(node) {
                if child.kind() == "path_segment" {
                    let name = node_text(required_field(child, "name")?, source).to_string();
                    let type_args = child
                        .child_by_field_name("type_args")
                        .map(|n| convert_type_args(n, source))
                        .transpose()?
                        .unwrap_or_default();
                    segments.push((name, type_args));
                }
            }
            // Last segment = variant_name, second-to-last = enum_name, rest = module path
            let (variant_name, _) = segments.pop().ok_or_else(|| {
                ParseError::at(node, "expected a qualified path with at least two segments")
            })?;
            let (enum_name, type_args) = segments.pop().ok_or_else(|| {
                ParseError::at(node, "expected a qualified path with at least two segments")
            })?;
            let module_path: Vec<String> = segments.into_iter().map(|(n, _)| n).collect();
            ExprKind::EnumVariant {
                module_path,
                enum_name: DefId::new(0, enum_name),
                type_args,
                variant_name,
            }
        }
        "match_expression" => {
            let scrutinee = convert_expr(required_field(node, "scrutinee")?, source)?;
            let mut arms = Vec::new();
            if let Some(arm_list) = named_child_by_kind(node, "match_arm_list") {
                for child in code_children(arm_list) {
                    if child.kind() == "match_arm" {
                        arms.push(convert_match_arm(child, source)?);
                    }
                }
            }
            ExprKind::Match {
                scrutinee: Box::new(scrutinee),
                arms,
            }
        }
        "reflect_match_expression" => {
            let ty = convert_type(required_field(node, "type")?, source)?;
            let mut arms = Vec::new();
            if let Some(arm_list) = named_child_by_kind(node, "reflect_match_arm_list") {
                for child in code_children(arm_list) {
                    if child.kind() == "reflect_match_arm" {
                        arms.push(convert_reflect_arm(child, source)?);
                    }
                }
            }
            ExprKind::MatchReflect { ty, arms }
        }
        "generic_method_call" => {
            let receiver = convert_expr(required_field(node, "receiver")?, source)?;
            let method = node_text(required_field(node, "method")?, source).to_string();
            let type_args = node
                .child_by_field_name("type_args")
                .map(|n| convert_type_args(n, source))
                .transpose()?
                .unwrap_or_default();
            let (arguments, kwargs) = convert_arguments(node, source)?;
            ExprKind::MethodCall {
                receiver: Box::new(receiver),
                method,
                type_args,
                arguments,
                kwargs,
            }
        }
        "block" => ExprKind::Block(convert_block(node, source)?),
        "unsafe_block" => {
            ExprKind::UnsafeBlock(convert_block(required_field(node, "body")?, source)?)
        }
        "for_expression" => convert_for_expression(node, source)?,
        "loop_expression" => {
            let body_node = required_field(node, "body")?;
            ExprKind::Loop(convert_block(body_node, source)?)
        }
        "coroutine_expression" => ExprKind::Coroutine {
            parameter: node
                .child_by_field_name("parameter")
                .map(|p| convert_closure_parameter(p, source).map(Box::new))
                .transpose()?,
            body: convert_block(required_field(node, "body")?, source)?,
        },
        "await_expression" => ExprKind::Await(Box::new(convert_expr(
            required_field(node, "value")?,
            source,
        )?)),
        "yield_expression" => ExprKind::Yield(Box::new(convert_expr(
            required_field(node, "value")?,
            source,
        )?)),
        "closure_expr" => {
            let mut parameters = Vec::new();
            if let Some(param_list) = named_child_by_kind(node, "closure_param_list") {
                for child in code_children(param_list) {
                    if child.kind() == "closure_param" {
                        parameters.push(convert_closure_parameter(child, source)?);
                    }
                }
            }
            let return_type = node
                .child_by_field_name("return_type")
                .map(|n| convert_type(n, source))
                .transpose()?;
            let body_node = required_field(node, "body")?;
            // `\ f(x)` has two valid CST interpretations: a zero-parameter
            // closure whose body calls `f`, or a one-parameter closure named
            // `f` whose body is the parenthesized expression `x`. Tree-sitter's
            // static postfix precedence selects the latter. It is almost never
            // useful and made call-shaped zero-parameter closures fail type
            // inference, so reinterpret that exact ambiguous shape as a call.
            // A typed parameter (`\f: T (x)`) remains unambiguous.
            let ambiguous_call = parameters.len() == 1
                && matches!(parameters[0].ty, Type::Infer)
                && return_type.is_none()
                && matches!(
                    body_node.kind(),
                    "parenthesized_expression" | "tuple_literal"
                );
            let body = if ambiguous_call {
                let parameter = parameters
                    .pop()
                    .ok_or_else(|| ParseError::at(node, "expected a closure parameter"))?;
                let DestructurePattern::Name(function) = parameter.pattern else {
                    return Err(ParseError::at(node, "expected a named closure parameter"));
                };
                let arguments = code_children(body_node)
                    .into_iter()
                    .map(|argument| convert_expr(argument, source))
                    .collect::<Result<_, _>>()?;
                vec![Statement {
                    kind: StatementKind::Expression(Expr {
                        kind: ExprKind::Call {
                            function: Box::new(Expr {
                                kind: ExprKind::Identifier(function),
                                span: parameter.span,
                            }),
                            type_args: Vec::new(),
                            arguments,
                            kwargs: Vec::new(),
                        },
                        span: source_span(body_node),
                    }),
                    span: source_span(body_node),
                }]
            } else if body_node.kind() == "block" {
                convert_block(body_node, source)?
            } else {
                let body_span = source_span(body_node);
                vec![Statement {
                    kind: StatementKind::Expression(convert_expr(body_node, source)?),
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
            let left = convert_expr(required_field(node, "left")?, source)?;
            let right = convert_expr(required_field(node, "right")?, source)?;
            let op_node = required_field(node, "operator")?;
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
                other => {
                    return Err(ParseError::at(
                        node,
                        format!("unexpected operator: {other}"),
                    ));
                }
            };
            ExprKind::BinaryOp {
                op,
                left: Box::new(left),
                right: Box::new(right),
            }
        }
        "not_expression" => {
            let operand = convert_expr(required_field(node, "operand")?, source)?;
            ExprKind::Not(Box::new(operand))
        }
        other => {
            return Err(ParseError::at(
                node,
                format!("unexpected expression node kind: {other}"),
            ));
        }
    };
    Ok(Expr { kind, span })
}

fn convert_match_arm(node: tree_sitter::Node, source: &str) -> Result<MatchArm, ParseError> {
    let pattern = convert_pattern(required_field(node, "pattern")?, source)?;
    let body = convert_expr(required_field(node, "body")?, source)?;
    Ok(MatchArm { pattern, body })
}

fn convert_reflect_arm(node: tree_sitter::Node, source: &str) -> Result<ReflectArm, ParseError> {
    let pattern_node = required_field(node, "pattern")?;
    // String literal patterns are special-cased: they stay strings instead of
    // desugaring to byte array literals like string literal expressions do.
    let pattern = if pattern_node.kind() == "string_literal" {
        unescape_string(pattern_node, source)?;
        ReflectPattern::Kind(literal_contents(pattern_node, source)?.to_string())
    } else {
        ReflectPattern::Wildcard
    };
    let body = convert_expr(required_field(node, "body")?, source)?;
    Ok(ReflectArm { pattern, body })
}

fn convert_pattern(node: tree_sitter::Node, source: &str) -> Result<Pattern, ParseError> {
    Ok(match node.kind() {
        "match_pattern" => {
            // Wrapper node — unwrap to the actual pattern child
            let child = first_code_child(node)?;
            convert_pattern(child, source)?
        }
        "variant_pattern" => {
            // Collect all path_segment children
            let mut segments: Vec<(String, Vec<Type>)> = Vec::new();
            for child in code_children(node) {
                if child.kind() == "path_segment" {
                    let name = node_text(required_field(child, "name")?, source).to_string();
                    let type_args = child
                        .child_by_field_name("type_args")
                        .map(|n| convert_type_args(n, source))
                        .transpose()?
                        .unwrap_or_default();
                    segments.push((name, type_args));
                }
            }
            let (variant_name, _) = segments.pop().ok_or_else(|| {
                ParseError::at(node, "expected a qualified path with at least two segments")
            })?;
            let (enum_name, type_args) = segments.pop().ok_or_else(|| {
                ParseError::at(node, "expected a qualified path with at least two segments")
            })?;
            let module_path: Vec<String> = segments.into_iter().map(|(n, _)| n).collect();
            let binding = Ident::user(node_text(required_field(node, "binding")?, source));
            Pattern::Variant {
                module_path,
                enum_name: DefId::new(0, enum_name),
                type_args,
                variant_name,
                binding: Some(binding),
            }
        }
        "unit_variant_pattern" => {
            let mut segments: Vec<(String, Vec<Type>)> = Vec::new();
            for child in code_children(node) {
                if child.kind() == "path_segment" {
                    let name = node_text(required_field(child, "name")?, source).to_string();
                    let type_args = child
                        .child_by_field_name("type_args")
                        .map(|n| convert_type_args(n, source))
                        .transpose()?
                        .unwrap_or_default();
                    segments.push((name, type_args));
                }
            }
            let (variant_name, _) = segments.pop().ok_or_else(|| {
                ParseError::at(node, "expected a qualified path with at least two segments")
            })?;
            let (enum_name, type_args) = segments.pop().ok_or_else(|| {
                ParseError::at(node, "expected a qualified path with at least two segments")
            })?;
            let module_path: Vec<String> = segments.into_iter().map(|(n, _)| n).collect();
            Pattern::Variant {
                module_path,
                enum_name: DefId::new(0, enum_name),
                type_args,
                variant_name,
                binding: None,
            }
        }
        "wildcard_pattern" => {
            let name = node_text(required_field(node, "name")?, source);
            Pattern::Wildcard(Ident::user(name))
        }
        "integer_literal" => {
            let text = node_text(node, source);
            let (num_str, int_ty) = parse_integer_suffix(text);
            Pattern::IntegerLiteral(parse_integer_value(num_str), int_ty)
        }
        other => {
            return Err(ParseError::at(
                node,
                format!("unexpected pattern node kind: {other}"),
            ));
        }
    })
}

fn convert_type(node: tree_sitter::Node, source: &str) -> Result<Type, ParseError> {
    Ok(match node.kind() {
        "named_type" => {
            let ident = first_code_child(node)?;
            let name = node_text(ident, source).to_string();
            if let Some(ta_node) = node.child_by_field_name("type_args") {
                let type_args = convert_type_args(ta_node, source)?;
                Type::Generic {
                    name: DefId::new(0, name),
                    type_args,
                }
            } else {
                Type::Named(DefId::new(0, name))
            }
        }
        "qualified_type" => {
            let module = node_text(required_field(node, "module")?, source);
            let name = node_text(required_field(node, "name")?, source);
            // Encode qualified type as "module::Name" — resolve stage will rewrite
            if let Some(ta_node) = node.child_by_field_name("type_args") {
                let type_args = convert_type_args(ta_node, source)?;
                Type::Generic {
                    name: DefId::new(0, format!("{module}::{name}")),
                    type_args,
                }
            } else {
                Type::Named(DefId::new(0, format!("{module}::{name}")))
            }
        }
        "reference_type" => {
            let inner = convert_type(required_field(node, "inner")?, source)?;
            Type::Reference(Box::new(inner))
        }
        "nullable_reference_type" => {
            let inner = convert_type(required_field(node, "inner")?, source)?;
            Type::NullableReference(Box::new(inner))
        }
        "unique_type" => {
            let inner = convert_type(required_field(node, "inner")?, source)?;
            Type::Unique(Box::new(inner))
        }
        "slice_type" => {
            let element = convert_type(required_field(node, "element")?, source)?;
            Type::Slice(Box::new(element))
        }
        "fixed_array_type" => {
            let element = convert_type(required_field(node, "element")?, source)?;
            let size_text = node_text(required_field(node, "size")?, source);
            // The array size literal may carry a suffix and a radix prefix.
            let (num_str, _) = parse_integer_suffix(size_text);
            let size = parse_integer_value(num_str) as u64;
            Type::FixedArray(Box::new(element), size)
        }
        "tuple_type" => {
            let mut types = Vec::new();
            for child in code_children(node) {
                types.push(convert_type(child, source)?);
            }
            Type::Tuple(types)
        }
        "function_type" => {
            let mut params = Vec::new();
            if let Some(param_list) = named_child_by_kind(node, "fn_type_param_list") {
                for child in code_children(param_list) {
                    if child.kind() == "fn_type_param" {
                        let name = child
                            .child_by_field_name("name")
                            .map(|n| node_text(n, source).to_string());
                        let ty = convert_type(required_field(child, "type")?, source)?;
                        params.push((name, ty));
                    }
                }
            }
            let return_type = node
                .child_by_field_name("return_type")
                .map(|n| convert_type(n, source).map(Box::new))
                .transpose()?;
            Type::Function {
                params,
                return_type,
            }
        }
        other => {
            return Err(ParseError::at(
                node,
                format!("unexpected type node kind: {other}"),
            ));
        }
    })
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

/// Named children of `node` with `extra` nodes (comments) filtered out.
/// Comments are `extra` in the grammar, so tree-sitter may attach them as
/// named children of ANY node — including expression/argument/field lists,
/// whose converters would otherwise choke on them.
fn code_children<'a>(node: tree_sitter::Node<'a>) -> Vec<tree_sitter::Node<'a>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|c| !matches!(c.kind(), "comment" | "doc_comment"))
        .collect()
}

fn named_child_by_kind<'a>(
    node: tree_sitter::Node<'a>,
    kind: &str,
) -> Option<tree_sitter::Node<'a>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).find(|c| c.kind() == kind)
}

fn unescape_string(node: tree_sitter::Node, source: &str) -> Result<Vec<u8>, ParseError> {
    let s = literal_contents(node, source)?;
    let mut out = Vec::new();
    let mut chars = s.char_indices();
    while let Some((offset, c)) = chars.next() {
        if c == '\\' {
            let (_, escaped) = chars.next().ok_or_else(|| {
                ParseError::in_literal(node, source, offset + 1, "unexpected end of string escape")
            })?;
            match escaped {
                '\\' => out.push(b'\\'),
                '"' => out.push(b'"'),
                '\'' => out.push(b'\''),
                'n' => out.push(b'\n'),
                'r' => out.push(b'\r'),
                't' => out.push(b'\t'),
                '0' => out.push(0),
                other => {
                    return Err(ParseError::in_literal(
                        node,
                        source,
                        offset + 1,
                        format!("unknown escape sequence: \\{other}"),
                    ));
                }
            }
        } else {
            let mut buf = [0u8; 4];
            let encoded = c.encode_utf8(&mut buf);
            out.extend_from_slice(encoded.as_bytes());
        }
    }
    Ok(out)
}
