//! Solar source formatting.

mod syntax;

use crate::ast::*;
use syntax::{CodeBlock, Comment, Parentheses, Trivia};

const MAX_WIDTH: usize = 80;
const TAB_WIDTH: usize = 4;

#[derive(Clone)]
enum Doc {
    Nil,
    Text(String),
    Line(&'static str),
    HardLine,
    BreakParent,
    Measure(usize),
    Concat(Vec<Doc>),
    Nest(Box<Doc>),
    Group(Box<Doc>),
    IfBreak(&'static str),
}

#[derive(Clone, Copy)]
enum Mode {
    Flat,
    Break,
}

#[derive(Clone)]
struct Command {
    indent: usize,
    mode: Mode,
    strict_flat: bool,
    doc: Doc,
}

struct SourceContext<'a> {
    source: &'a str,
    line_starts: Vec<usize>,
    trivia: &'a [Trivia],
    parentheses: &'a [Parentheses],
    blocks: &'a [CodeBlock],
}

struct SpannedDoc {
    span: SourceSpan,
    doc: Doc,
    multiline_block: bool,
}

impl Doc {
    fn text(text: impl Into<String>) -> Self {
        Self::Text(text.into())
    }

    fn concat(docs: impl IntoIterator<Item = Self>) -> Self {
        let docs = docs
            .into_iter()
            .filter(|doc| !matches!(doc, Self::Nil))
            .collect::<Vec<_>>();
        match docs.len() {
            0 => Self::Nil,
            1 => docs.into_iter().next().unwrap(),
            _ => Self::Concat(docs),
        }
    }

    fn nest(self) -> Self {
        Self::Nest(Box::new(self))
    }

    fn group(self) -> Self {
        Self::Group(Box::new(self))
    }
}

impl<'a> SourceContext<'a> {
    fn new(
        source: &'a str,
        trivia: &'a [Trivia],
        parentheses: &'a [Parentheses],
        blocks: &'a [CodeBlock],
    ) -> Self {
        let mut line_starts = vec![0];
        for (index, byte) in source.bytes().enumerate() {
            if byte == b'\n' {
                line_starts.push(index + 1);
            }
        }
        Self {
            source,
            line_starts,
            trivia,
            parentheses,
            blocks,
        }
    }

    fn offset(&self, position: SourcePos) -> usize {
        self.line_starts[position.line as usize] + position.col as usize
    }

    fn text(&self, span: SourceSpan) -> &'a str {
        &self.source[self.offset(span.start)..self.offset(span.end)]
    }
}

fn position(position: SourcePos) -> (u32, u32) {
    (position.line, position.col)
}

fn trivia_span(trivia: &Trivia) -> SourceSpan {
    match trivia {
        Trivia::Comment(comment) => comment.span,
        Trivia::BlankLine(span) => *span,
    }
}

fn in_gap(span: SourceSpan, start: SourcePos, end: SourcePos) -> bool {
    position(span.start) >= position(start) && position(span.start) < position(end)
}

fn gap_trivia<'a>(
    context: &'a SourceContext<'_>,
    start: SourcePos,
    end: SourcePos,
) -> Vec<&'a Trivia> {
    context
        .trivia
        .iter()
        .filter(|trivia| in_gap(trivia_span(trivia), start, end))
        .collect()
}

fn comment_doc(comment: &Comment) -> Doc {
    Doc::concat([Doc::text(&comment.text), Doc::BreakParent])
}

fn hard_lines(count: usize) -> Doc {
    Doc::concat(std::iter::repeat_n(Doc::HardLine, count))
}

fn format_gap(
    trivia: &[&Trivia],
    previous: Option<SourceSpan>,
    default: Doc,
    require_blank: bool,
) -> Doc {
    if trivia.is_empty() {
        return if require_blank {
            hard_lines(2)
        } else {
            default
        };
    }

    let mut docs = Vec::new();
    let mut pending_blank = false;
    let mut wrote_comment = false;
    for trivia in trivia {
        match trivia {
            Trivia::BlankLine(_) => pending_blank = true,
            Trivia::Comment(comment) => {
                let trailing = !wrote_comment
                    && previous.is_some_and(|span| span.end.line == comment.span.start.line);
                if trailing {
                    docs.push(Doc::text(" "));
                } else if wrote_comment || previous.is_some() {
                    let blank = pending_blank || (!wrote_comment && require_blank);
                    docs.push(hard_lines(usize::from(blank) + 1));
                }
                docs.push(comment_doc(comment));
                wrote_comment = true;
                pending_blank = false;
            }
        }
    }
    if wrote_comment {
        docs.push(hard_lines(usize::from(pending_blank) + 1));
    } else if previous.is_some() {
        docs.push(if pending_blank || require_blank {
            hard_lines(2)
        } else {
            default
        });
    }
    Doc::concat(docs)
}

fn format_spanned_items(
    items: &[SpannedDoc],
    boundary: SourceSpan,
    context: &SourceContext<'_>,
    _top_level: bool,
    commas: bool,
) -> Doc {
    let mut docs = Vec::new();
    let mut previous = None;
    for (index, item) in items.iter().enumerate() {
        let start = previous.map_or(boundary.start, |span: SourceSpan| span.end);
        let trivia = gap_trivia(context, start, item.span.start);
        if index == 0 {
            docs.push(format_gap(&trivia, None, Doc::Nil, false));
        } else {
            if commas {
                docs.push(Doc::text(","));
            }
            let previous_item = &items[index - 1];
            let require_blank = !commas && (previous_item.multiline_block || item.multiline_block);
            let default = Doc::Line(" ");
            docs.push(format_gap(&trivia, previous, default, require_blank));
        }
        docs.push(item.doc.clone());
        previous = Some(item.span);
    }

    if let Some(previous) = previous {
        let trivia = gap_trivia(context, previous.end, boundary.end);
        let trailing = trivia
			.iter()
			.take_while(|trivia| {
				matches!(trivia, Trivia::Comment(comment) if comment.span.start.line == previous.end.line)
			})
			.copied()
			.collect::<Vec<_>>();
        if !trailing.is_empty() {
            for trivia in trailing {
                let Trivia::Comment(comment) = trivia else {
                    unreachable!();
                };
                docs.push(Doc::text(" "));
                docs.push(comment_doc(comment));
            }
        }
    }
    if commas && !items.is_empty() {
        docs.push(Doc::IfBreak(","));
    }

    Doc::concat(docs)
}

fn delimited(open: &str, content: Doc, close: &str, padding: bool, force_break: bool) -> Doc {
    if matches!(content, Doc::Nil) {
        return Doc::text(format!("{open}{close}"));
    }
    let padding = if padding { " " } else { "" };
    Doc::concat([
        Doc::text(open),
        Doc::concat([Doc::Line(padding), content]).nest(),
        Doc::Line(padding),
        Doc::text(close),
        if force_break {
            Doc::BreakParent
        } else {
            Doc::Nil
        },
    ])
    .group()
}

fn list(docs: impl IntoIterator<Item = Doc>, open: &str, close: &str) -> Doc {
    list_with_padding(docs, open, close, false)
}

fn list_with_padding(
    docs: impl IntoIterator<Item = Doc>,
    open: &str,
    close: &str,
    padding: bool,
) -> Doc {
    let docs = docs.into_iter().collect::<Vec<_>>();
    if docs.is_empty() {
        return Doc::text(format!("{open}{close}"));
    }
    let mut content = Vec::new();
    for (index, doc) in docs.into_iter().enumerate() {
        if index != 0 {
            content.push(Doc::text(","));
            content.push(Doc::Line(" "));
        }
        content.push(doc);
    }
    content.push(Doc::IfBreak(","));
    delimited(open, Doc::concat(content), close, padding, false)
}

fn hugged(open: &str, content: Doc, close: &str) -> Doc {
    Doc::concat([Doc::text(open), content, Doc::text(close)])
}

fn source_file_doc(file: &SourceFile, context: &SourceContext<'_>) -> Doc {
    let end_line = context.source.lines().count().saturating_sub(1) as u32;
    let boundary = SourceSpan {
        start: SourcePos { line: 0, col: 0 },
        end: SourcePos {
            line: end_line,
            col: context.source.lines().last().map_or(0, |line| line.len()) as u32,
        },
        file_id: 0,
    };
    let items = file
        .items
        .iter()
        .map(|item| SpannedDoc {
            span: top_level_span(item),
            doc: top_level_doc(item, context),
            multiline_block: top_level_has_block(item)
                && top_level_span(item).start.line != top_level_span(item).end.line,
        })
        .collect::<Vec<_>>();
    format_spanned_items(&items, boundary, context, true, false)
}

fn top_level_span(item: &TopLevelItem) -> SourceSpan {
    match item {
        TopLevelItem::Struct(item) => item.span,
        TopLevelItem::Function(item) | TopLevelItem::Method(item) => item.span,
        TopLevelItem::Enum(item) => item.span,
        TopLevelItem::Import(item) => item.span,
        TopLevelItem::TypeAlias(item) => item.span,
        TopLevelItem::Const(item) => item.span,
        TopLevelItem::Static(item) => item.span,
    }
}

fn top_level_has_block(item: &TopLevelItem) -> bool {
    matches!(
        item,
        TopLevelItem::Struct(_)
            | TopLevelItem::Function(_)
            | TopLevelItem::Method(_)
            | TopLevelItem::Enum(_)
    )
}

fn visibility(is_pub: bool) -> Doc {
    if is_pub { Doc::text("pub ") } else { Doc::Nil }
}

fn type_parameters(parameters: &[String]) -> Doc {
    if parameters.is_empty() {
        Doc::Nil
    } else {
        list(parameters.iter().map(Doc::text), "#[", "]")
    }
}

fn function_type_parameters(parameters: &[String], output_parameters: &[String]) -> Doc {
    if parameters.is_empty() && output_parameters.is_empty() {
        return Doc::Nil;
    }
    list(
        parameters.iter().map(Doc::text).chain(
            output_parameters
                .iter()
                .map(|name| Doc::concat([Doc::text("out "), Doc::text(name)])),
        ),
        "#[",
        "]",
    )
}

fn top_level_doc(item: &TopLevelItem, context: &SourceContext<'_>) -> Doc {
    match item {
        TopLevelItem::Struct(definition) => struct_doc(definition, context),
        TopLevelItem::Function(definition) => function_doc(definition, "fn", context),
        TopLevelItem::Method(definition) => function_doc(definition, "method", context),
        TopLevelItem::Enum(definition) => enum_doc(definition, context),
        TopLevelItem::Import(definition) => import_doc(definition),
        TopLevelItem::TypeAlias(definition) => type_alias_doc(definition),
        TopLevelItem::Const(definition) => const_doc(definition, context),
        TopLevelItem::Static(definition) => static_doc(definition, context),
    }
}

fn struct_doc(definition: &StructDef, context: &SourceContext<'_>) -> Doc {
    let header = Doc::concat([
        visibility(definition.is_pub),
        if definition.repr_c {
            Doc::text("struct(repr(C)) ")
        } else {
            Doc::text("struct ")
        },
        Doc::text(&definition.name),
        type_parameters(&definition.type_params),
    ]);
    let fields = definition
        .fields
        .iter()
        .map(|field| SpannedDoc {
            span: field.span,
            doc: if definition.is_tuple {
                Doc::concat([visibility(field.is_pub), type_doc(&field.ty)])
            } else {
                Doc::concat([
                    visibility(field.is_pub),
                    Doc::text(&field.name),
                    Doc::text(": "),
                    type_doc(&field.ty),
                ])
            },
            multiline_block: false,
        })
        .collect::<Vec<_>>();
    let body = format_spanned_items(&fields, definition.span, context, false, true);
    let delimiters = if definition.is_tuple {
        ("(", ")")
    } else {
        ("{", "}")
    };
    Doc::concat([
        header,
        if definition.is_tuple {
            Doc::Nil
        } else {
            Doc::text(" ")
        },
        delimited(
            delimiters.0,
            body,
            delimiters.1,
            !definition.is_tuple,
            false,
        ),
        if definition.is_tuple {
            Doc::text(";")
        } else {
            Doc::Nil
        },
    ])
}

fn enum_doc(definition: &EnumDef, context: &SourceContext<'_>) -> Doc {
    let variants = definition
        .variants
        .iter()
        .map(|variant| SpannedDoc {
            span: variant.span,
            doc: Doc::concat([
                Doc::text(&variant.name),
                variant.inner_type.as_ref().map_or(Doc::Nil, |ty| {
                    delimited("(", type_doc(ty), ")", false, false)
                }),
            ]),
            multiline_block: false,
        })
        .collect::<Vec<_>>();
    let body = format_spanned_items(&variants, definition.span, context, false, true);
    Doc::concat([
        visibility(definition.is_pub),
        Doc::text("enum "),
        Doc::text(&definition.name),
        type_parameters(&definition.type_params),
        Doc::text(" "),
        delimited("{", body, "}", true, false),
    ])
}

fn function_doc(definition: &FunctionDef, keyword: &str, context: &SourceContext<'_>) -> Doc {
    let parameters = definition
        .parameters
        .iter()
        .map(|parameter| SpannedDoc {
            span: parameter.span,
            doc: parameter_doc(parameter, context),
            multiline_block: false,
        })
        .collect::<Vec<_>>();
    let parameter_docs = format_spanned_items(&parameters, definition.span, context, false, true);
    let signature = Doc::concat([
        visibility(definition.is_pub),
        if definition.is_unsafe {
            Doc::text("unsafe ")
        } else {
            Doc::Nil
        },
        Doc::text(keyword),
        if definition.inline_hint {
            Doc::text("(inline) ")
        } else {
            Doc::text(" ")
        },
        Doc::text(&definition.display_name),
        function_type_parameters(&definition.type_params, &definition.out_type_params),
        delimited("(", parameter_docs, ")", false, false),
        definition.return_type.as_ref().map_or(Doc::Nil, |ty| {
            Doc::concat([Doc::text(" -> "), type_doc(ty)])
        }),
        Doc::text(" "),
        Doc::Measure(1),
    ])
    .group();
    Doc::concat([
        signature,
        block_doc(&definition.body, definition.span, context),
    ])
}

fn parameter_doc(parameter: &Parameter, context: &SourceContext<'_>) -> Doc {
    let mut docs = vec![destructure_doc(&parameter.pattern)];
    if !matches!(parameter.ty, Type::Infer) {
        docs.push(Doc::text(": "));
        docs.push(type_doc(&parameter.ty));
    }
    if let Some(default) = &parameter.default {
        docs.push(Doc::text(" = "));
        docs.push(expr_doc(default, context));
    }
    Doc::concat(docs)
}

fn import_doc(definition: &ImportDef) -> Doc {
    let names = match &definition.kind {
        ImportKind::Named(names) => list(
            names.iter().map(|name| Doc::text(name.segments.join("::"))),
            "{",
            "}",
        ),
        ImportKind::Module(name) => Doc::text(name),
        ImportKind::Wildcard => Doc::text("*"),
    };
    Doc::concat([
        visibility(definition.is_pub),
        Doc::text("import "),
        names,
        Doc::text(" from "),
        Doc::text(format!("{:?}", definition.path)),
        Doc::text(";"),
    ])
}

fn type_alias_doc(definition: &TypeAliasDef) -> Doc {
    Doc::concat([
        visibility(definition.is_pub),
        Doc::text("type "),
        Doc::text(&definition.name),
        type_parameters(&definition.type_params),
        Doc::text(" = "),
        type_doc(&definition.target_type),
        Doc::text(";"),
    ])
}

fn const_doc(definition: &ConstDef, context: &SourceContext<'_>) -> Doc {
    Doc::concat([
        visibility(definition.is_pub),
        Doc::text("const "),
        Doc::text(&definition.name),
        definition
            .ty
            .as_ref()
            .map_or(Doc::Nil, |ty| Doc::concat([Doc::text(": "), type_doc(ty)])),
        Doc::text(" = "),
        expr_doc(&definition.value, context),
        Doc::text(";"),
    ])
    .group()
}

fn static_doc(definition: &StaticDef, context: &SourceContext<'_>) -> Doc {
    Doc::concat([
        visibility(definition.is_pub),
        Doc::text("static"),
        if definition.thread_local {
            Doc::text("(thread_local)")
        } else {
            Doc::Nil
        },
        Doc::text(" "),
        Doc::text(&definition.name),
        definition
            .ty
            .as_ref()
            .map_or(Doc::Nil, |ty| Doc::concat([Doc::text(": "), type_doc(ty)])),
        Doc::text(" = "),
        expr_doc(&definition.value, context),
        Doc::text(";"),
    ])
    .group()
}

fn block_doc(statements: &[Statement], boundary: SourceSpan, context: &SourceContext<'_>) -> Doc {
    let boundary = block_span(statements, boundary, context);
    if statements.is_empty() {
        let comments = gap_trivia(context, boundary.start, boundary.end)
            .into_iter()
            .filter_map(|trivia| match trivia {
                Trivia::Comment(comment) => Some(comment_doc(comment)),
                Trivia::BlankLine(_) => None,
            })
            .collect::<Vec<_>>();
        if comments.is_empty() {
            return Doc::text("{}");
        }
        return delimited("{", Doc::concat(comments), "}", true, true);
    }
    let items = statements
        .iter()
        .map(|statement| SpannedDoc {
            span: statement.span,
            doc: statement_doc(statement, context),
            multiline_block: statement_has_block(statement)
                && statement.span.start.line != statement.span.end.line,
        })
        .collect::<Vec<_>>();
    let body = format_spanned_items(&items, boundary, context, false, false);
    delimited(
        "{",
        body,
        "}",
        true,
        boundary.start.line != boundary.end.line,
    )
}

fn block_span(
    statements: &[Statement],
    parent: SourceSpan,
    context: &SourceContext<'_>,
) -> SourceSpan {
    context
        .blocks
        .iter()
        .filter(|block| {
            position(block.span.start) >= position(parent.start)
                && position(block.span.end) <= position(parent.end)
        })
        .filter(|block| {
            statements.first().is_none_or(|first| {
                position(block.span.start) <= position(first.span.start)
                    && position(block.span.end) >= position(statements.last().unwrap().span.end)
            })
        })
        .min_by_key(|block| context.offset(block.span.end) - context.offset(block.span.start))
        .map_or(parent, |block| block.span)
}

fn statement_has_block(statement: &Statement) -> bool {
    matches!(
        statement.kind,
        StatementKind::If { .. }
            | StatementKind::While { .. }
            | StatementKind::ForRange { .. }
            | StatementKind::ForIn { .. }
            | StatementKind::Try { .. }
            | StatementKind::ForReflectFields { .. }
            | StatementKind::MatchReflectVariant { .. }
            | StatementKind::NestedFunction(_)
    ) || matches!(
        &statement.kind,
        StatementKind::Expression(Expr {
            kind: ExprKind::Block(_)
                | ExprKind::If { .. }
                | ExprKind::Loop(_)
                | ExprKind::Closure { .. },
            ..
        })
    )
}

fn statement_doc(statement: &Statement, context: &SourceContext<'_>) -> Doc {
    match &statement.kind {
        StatementKind::Let { pattern, ty, value } => Doc::concat([
            Doc::text("let "),
            destructure_doc(pattern),
            ty.as_ref()
                .map_or(Doc::Nil, |ty| Doc::concat([Doc::text(": "), type_doc(ty)])),
            Doc::text(" = "),
            expr_doc(value, context),
            Doc::text(";"),
        ])
        .group(),
        StatementKind::Assignment { target, value } => Doc::concat([
            expr_doc(target, context),
            Doc::text(" = "),
            expr_doc(value, context),
            Doc::text(";"),
        ])
        .group(),
        StatementKind::If {
            condition,
            body,
            else_body,
        } => {
            let mut doc = Doc::concat([
                Doc::text("if "),
                expr_doc(condition, context),
                Doc::text(" "),
                block_doc(body, statement.span, context),
            ]);
            if !else_body.is_empty() {
                doc = Doc::concat([
                    doc,
                    Doc::text(" else "),
                    if else_body.len() == 1 && matches!(else_body[0].kind, StatementKind::If { .. })
                    {
                        statement_doc(&else_body[0], context)
                    } else {
                        block_doc(else_body, statement.span, context)
                    },
                ]);
            }
            doc.group()
        }
        StatementKind::While { condition, body } => Doc::concat([
            Doc::text("while "),
            expr_doc(condition, context),
            Doc::text(" "),
            block_doc(body, statement.span, context),
        ])
        .group(),
        StatementKind::ForRange {
            variable,
            start,
            end,
            body,
        } => Doc::concat([
            Doc::text("for "),
            ident_doc(variable),
            Doc::text(" in "),
            expr_doc(start, context),
            Doc::text(".."),
            expr_doc(end, context),
            Doc::text(" "),
            block_doc(body, statement.span, context),
        ])
        .group(),
        StatementKind::ForIn {
            variable,
            iterable,
            body,
        } => Doc::concat([
            Doc::text("for "),
            ident_doc(variable),
            Doc::text(" in "),
            expr_doc(iterable, context),
            Doc::text(" "),
            block_doc(body, statement.span, context),
        ])
        .group(),
        StatementKind::Try {
            body,
            binding,
            binding_type,
            handler,
        } => Doc::concat([
            Doc::text("try "),
            block_doc(body, statement.span, context),
            Doc::text(" catch ("),
            ident_doc(binding),
            binding_type
                .as_ref()
                .map_or(Doc::Nil, |ty| Doc::concat([Doc::text(": "), type_doc(ty)])),
            Doc::text(") "),
            block_doc(handler, statement.span, context),
        ])
        .group(),
        StatementKind::ForReflectFields {
            pattern,
            object,
            body,
            paired,
        } => Doc::concat([
            Doc::text(if *paired {
                "for.reflect_fields_pair "
            } else {
                "for.reflect_fields "
            }),
            destructure_doc(pattern),
            Doc::text(" in "),
            expr_doc(object, context),
            Doc::text(" "),
            block_doc(body, statement.span, context),
        ])
        .group(),
        StatementKind::MatchReflectVariant {
            pattern,
            object,
            body,
            paired,
        } => Doc::concat([
            Doc::text(if *paired {
                "match.reflect_variant_pair "
            } else {
                "match.reflect_variant "
            }),
            destructure_doc(pattern),
            Doc::text(" in "),
            expr_doc(object, context),
            Doc::text(" "),
            block_doc(body, statement.span, context),
        ])
        .group(),
        StatementKind::Expression(expression) => Doc::concat([
            expr_doc(expression, context),
            if context.text(statement.span).trim_end().ends_with(';') {
                Doc::text(";")
            } else {
                Doc::Nil
            },
        ]),
        StatementKind::Return(expression) => Doc::concat([
            Doc::text("return "),
            expr_doc(expression, context),
            Doc::text(";"),
        ]),
        StatementKind::ReturnVoid => Doc::text("return;"),
        StatementKind::Break(value) => Doc::concat([
            Doc::text("break"),
            value.as_ref().map_or(Doc::Nil, |value| {
                Doc::concat([Doc::text(" "), expr_doc(value, context)])
            }),
            if context.text(statement.span).trim_end().ends_with(';') {
                Doc::text(";")
            } else {
                Doc::Nil
            },
        ]),
        StatementKind::Continue => Doc::concat([
            Doc::text("continue"),
            if context.text(statement.span).trim_end().ends_with(';') {
                Doc::text(";")
            } else {
                Doc::Nil
            },
        ]),
        StatementKind::NestedFunction(function) => function_doc(function, "fn", context),
        StatementKind::Const(definition) => const_doc(definition, context),
    }
}

fn expr_doc(expression: &Expr, context: &SourceContext<'_>) -> Doc {
    let doc = match &expression.kind {
        ExprKind::Identifier(identifier) => ident_doc(identifier),
        ExprKind::GlobalRef(definition) => Doc::text(&definition.name),
        ExprKind::IntegerLiteral(value, ty) => literal_doc(expression, context, || {
            format!("{value}{}", integer_suffix(*ty))
        }),
        ExprKind::FloatLiteral(value, ty) => literal_doc(expression, context, || {
            format!(
                "{value}{}",
                match ty {
                    FloatType::Float32 => "f32",
                    FloatType::Float64 => "f",
                }
            )
        }),
        ExprKind::BooleanLiteral(value) => Doc::text(value.to_string()),
        ExprKind::StringLiteral(value) => literal_doc(expression, context, || {
            format!("{:?}", String::from_utf8_lossy(value))
        }),
        ExprKind::CharLiteral(value) => literal_doc(expression, context, || {
            format!("{:?}", *value as char).replace('"', "'")
        }),
        ExprKind::FieldAccess { object, field } => {
            Doc::concat([expr_doc(object, context), Doc::text("."), Doc::text(field)])
        }
        ExprKind::Deref(operand) => Doc::concat([expr_doc(operand, context), Doc::text("@")]),
        ExprKind::Reference(operand) => Doc::concat([expr_doc(operand, context), Doc::text("&")]),
        ExprKind::Unique(operand) => Doc::concat([expr_doc(operand, context), Doc::text("^")]),
        ExprKind::Not(operand) => Doc::concat([Doc::text("!"), expr_doc(operand, context)]),
        ExprKind::NullLiteral(ty) => {
            Doc::concat([Doc::text("null#["), type_doc(ty), Doc::text("]")])
        }
        ExprKind::Call {
            function,
            type_args,
            arguments,
            kwargs,
        } => call_doc(function, type_args, arguments, kwargs, context),
        ExprKind::StructLiteral {
            module,
            name,
            type_args,
            fields,
        } => {
            let fields = fields.iter().map(|field| {
                Doc::concat([
                    Doc::text(&field.name),
                    Doc::text(": "),
                    expr_doc(&field.value, context),
                ])
            });
            Doc::concat([
                module
                    .as_ref()
                    .map_or(Doc::Nil, |module| Doc::text(format!("{module}::"))),
                Doc::text(&name.name),
                type_arguments(type_args),
                Doc::text(" "),
                list_with_padding(fields, "{", "}", true),
            ])
        }
        ExprKind::Index { object, index } => Doc::concat([
            expr_doc(object, context),
            Doc::text("["),
            expr_doc(index, context),
            Doc::text("]"),
        ]),
        ExprKind::Slice { object, start, end } => Doc::concat([
            expr_doc(object, context),
            Doc::text("["),
            expr_doc(start, context),
            Doc::text(".."),
            expr_doc(end, context),
            Doc::text("]"),
        ]),
        ExprKind::ArrayLiteral(elements, ty) => Doc::concat([
            expression_list(elements, "[", "]", context),
            ty.as_ref().map_or(Doc::Nil, |ty| {
                Doc::concat([Doc::text("#["), type_doc(ty), Doc::text("]")])
            }),
        ]),
        ExprKind::ArrayRepeat { element, count } => Doc::concat([
            Doc::text("["),
            expr_doc(element, context),
            Doc::text("; "),
            expr_doc(count, context),
            Doc::text("]"),
        ]),
        ExprKind::Loop(body) => Doc::concat([
            Doc::text("loop "),
            block_doc(body, expression.span, context),
        ]),
        ExprKind::BinaryOp { op, left, right } => Doc::concat([
            expr_doc(left, context),
            Doc::text(" "),
            Doc::text(binary_operator(*op)),
            Doc::concat([Doc::Line(" "), expr_doc(right, context)]).nest(),
        ])
        .group(),
        ExprKind::If {
            condition,
            then_body,
            else_body,
        } => Doc::concat([
            Doc::text("if "),
            expr_doc(condition, context),
            Doc::text(" "),
            block_doc(then_body, expression.span, context),
            Doc::text(" else "),
            if else_body.len() == 1
                && matches!(
                    else_body[0].kind,
                    StatementKind::Expression(Expr {
                        kind: ExprKind::If { .. },
                        ..
                    })
                )
            {
                statement_doc(&else_body[0], context)
            } else {
                block_doc(else_body, expression.span, context)
            },
        ])
        .group(),
        ExprKind::Block(body) => block_doc(body, expression.span, context),
        ExprKind::UnsafeBlock(body) => Doc::concat([
            Doc::text("unsafe "),
            block_doc(body, expression.span, context),
        ]),
        ExprKind::Closure {
            parameters,
            return_type,
            body,
        } => closure_doc(expression, parameters, return_type.as_ref(), body, context),
        ExprKind::EnumVariant {
            module_path,
            enum_name,
            type_args,
            variant_name,
        } => {
            let mut path = module_path.clone();
            path.push(enum_name.name.clone());
            Doc::concat([
                Doc::text(path.join("::")),
                type_arguments(type_args),
                Doc::text("::"),
                Doc::text(variant_name),
            ])
        }
        ExprKind::Match { scrutinee, arms } => {
            let arms = arms.iter().map(|arm| {
                Doc::concat([
                    pattern_doc(&arm.pattern),
                    Doc::text(" => "),
                    expr_doc(&arm.body, context),
                ])
            });
            Doc::concat([
                Doc::text("match "),
                expr_doc(scrutinee, context),
                Doc::text(" "),
                list_with_padding(arms, "{", "}", true),
            ])
        }
        ExprKind::MatchReflect { ty, arms } => {
            let arms = arms.iter().map(|arm| {
                Doc::concat([
                    reflect_pattern_doc(&arm.pattern),
                    Doc::text(" => "),
                    expr_doc(&arm.body, context),
                ])
            });
            Doc::concat([
                Doc::text("match.reflect "),
                type_doc(ty),
                Doc::text(" "),
                list_with_padding(arms, "{", "}", true),
            ])
        }
        ExprKind::MethodCall {
            receiver,
            method,
            type_args,
            arguments,
            kwargs,
        } => Doc::concat([
            expr_doc(receiver, context),
            Doc::text("."),
            Doc::text(method),
            type_arguments(type_args),
            argument_list(arguments, kwargs, context),
        ]),
        ExprKind::TupleLiteral(elements) => expression_list(elements, "(", ")", context),
        ExprKind::IntrinsicCall {
            intrinsic,
            type_args,
            arguments,
        } => Doc::concat([
            Doc::text(intrinsic.name()),
            type_arguments(type_args),
            expression_list(arguments, "(", ")", context),
        ]),
    };
    wrap_parentheses(expression, doc, context)
}

fn call_doc(
    function: &Expr,
    type_args: &[Type],
    arguments: &[Expr],
    kwargs: &[(String, Expr)],
    context: &SourceContext<'_>,
) -> Doc {
    Doc::concat([
        expr_doc(function, context),
        type_arguments(type_args),
        argument_list(arguments, kwargs, context),
    ])
}

fn argument_list(
    arguments: &[Expr],
    kwargs: &[(String, Expr)],
    context: &SourceContext<'_>,
) -> Doc {
    let docs = arguments
        .iter()
        .map(|argument| expr_doc(argument, context))
        .chain(kwargs.iter().map(|(name, value)| {
            Doc::concat([Doc::text(name), Doc::text(" = "), expr_doc(value, context)])
        }))
        .collect::<Vec<_>>();
    let sole_block = match (arguments, kwargs) {
        ([argument], []) => expression_is_huggable_block(argument),
        ([], [(_, value)]) => expression_is_huggable_block(value),
        _ => false,
    };
    if sole_block {
        hugged("(", docs.into_iter().next().unwrap(), ")")
    } else {
        list(docs, "(", ")")
    }
}

fn expression_list(
    expressions: &[Expr],
    open: &str,
    close: &str,
    context: &SourceContext<'_>,
) -> Doc {
    if let [expression] = expressions
        && expression_is_huggable_block(expression)
    {
        hugged(open, expr_doc(expression, context), close)
    } else {
        list(
            expressions
                .iter()
                .map(|expression| expr_doc(expression, context)),
            open,
            close,
        )
    }
}

fn expression_is_huggable_block(expression: &Expr) -> bool {
    match &expression.kind {
        ExprKind::Block(_)
        | ExprKind::UnsafeBlock(_)
        | ExprKind::Closure { .. }
        | ExprKind::ArrayLiteral(..)
        | ExprKind::TupleLiteral(_) => true,
        ExprKind::Call {
            arguments, kwargs, ..
        }
        | ExprKind::MethodCall {
            arguments, kwargs, ..
        } => match (arguments.as_slice(), kwargs.as_slice()) {
            ([argument], []) => expression_is_huggable_block(argument),
            ([], [(_, value)]) => expression_is_huggable_block(value),
            _ => false,
        },
        ExprKind::IntrinsicCall { arguments, .. } => {
            matches!(arguments.as_slice(), [argument] if expression_is_huggable_block(argument))
        }
        _ => false,
    }
}

fn closure_doc(
    expression: &Expr,
    parameters: &[Parameter],
    return_type: Option<&Type>,
    body: &[Statement],
    context: &SourceContext<'_>,
) -> Doc {
    let mut docs = vec![Doc::text("\\")];
    if parameters.is_empty() {
        docs.push(Doc::text(" "));
    } else {
        for (index, parameter) in parameters.iter().enumerate() {
            if index != 0 {
                docs.push(Doc::text(", "));
            }
            docs.push(parameter_doc(parameter, context));
        }
        docs.push(Doc::text(" "));
    }
    if let Some(return_type) = return_type {
        docs.push(Doc::text("-> "));
        docs.push(type_doc(return_type));
        docs.push(Doc::text(" "));
    }
    let source = context.text(expression.span);
    let block = source.contains('{');
    if block {
        docs.push(block_doc(body, expression.span, context));
    } else if body.len() == 1 {
        match &body[0].kind {
            StatementKind::Expression(body) => docs.push(expr_doc(body, context)),
            _ => docs.push(block_doc(body, expression.span, context)),
        }
    } else {
        docs.push(block_doc(body, expression.span, context));
    }
    Doc::concat(docs)
}

fn type_arguments(arguments: &[Type]) -> Doc {
    if arguments.is_empty() {
        Doc::Nil
    } else {
        list(arguments.iter().map(type_doc), "#[", "]")
    }
}

fn type_doc(ty: &Type) -> Doc {
    match ty {
        Type::Named(name) => Doc::text(&name.name),
        Type::Generic { name, type_args } => {
            Doc::concat([Doc::text(&name.name), type_arguments(type_args)])
        }
        Type::Reference(inner) => Doc::concat([Doc::text("&"), type_doc(inner)]),
        Type::NullableReference(inner) => Doc::concat([Doc::text("&?"), type_doc(inner)]),
        Type::Unique(inner) => Doc::concat([Doc::text("^"), type_doc(inner)]),
        Type::Slice(element) => Doc::concat([Doc::text("["), type_doc(element), Doc::text("]")]),
        Type::FixedArray(element, size) => Doc::concat([
            Doc::text("["),
            type_doc(element),
            Doc::text("; "),
            Doc::text(size.to_string()),
            Doc::text("]"),
        ]),
        Type::Function {
            params,
            return_type,
        } => Doc::concat([
            Doc::text("fn"),
            list(
                params.iter().map(|(name, ty)| {
                    Doc::concat([
                        name.as_ref()
                            .map_or(Doc::Nil, |name| Doc::text(format!("{name}: "))),
                        type_doc(ty),
                    ])
                }),
                "(",
                ")",
            ),
            return_type.as_ref().map_or(Doc::Nil, |ty| {
                Doc::concat([Doc::text(" -> "), type_doc(ty)])
            }),
        ]),
        Type::Tuple(types) => list(types.iter().map(type_doc), "(", ")"),
        Type::Infer => Doc::text("_"),
    }
}

fn destructure_doc(pattern: &DestructurePattern) -> Doc {
    match pattern {
        DestructurePattern::Name(name) => ident_doc(name),
        DestructurePattern::Tuple(elements) => list(elements.iter().map(destructure_doc), "(", ")"),
        DestructurePattern::Struct {
            module,
            name,
            fields,
        } => Doc::concat([
            module
                .as_ref()
                .map_or(Doc::Nil, |module| Doc::text(format!("{module}::"))),
            Doc::text(&name.name),
            Doc::text(" "),
            list_with_padding(
                fields.iter().map(|field| {
                    if matches!(
                        &field.pattern,
                        DestructurePattern::Name(Ident::User(name)) if name == &field.field_name
                    ) {
                        Doc::text(&field.field_name)
                    } else {
                        Doc::concat([
                            Doc::text(&field.field_name),
                            Doc::text(": "),
                            destructure_doc(&field.pattern),
                        ])
                    }
                }),
                "{",
                "}",
                true,
            ),
        ]),
        DestructurePattern::Array(elements) => list(elements.iter().map(destructure_doc), "[", "]"),
    }
}

fn pattern_doc(pattern: &Pattern) -> Doc {
    match pattern {
        Pattern::Variant {
            module_path,
            enum_name,
            type_args,
            variant_name,
            binding,
        } => {
            let mut path = module_path.clone();
            path.push(enum_name.name.clone());
            Doc::concat([
                Doc::text(path.join("::")),
                type_arguments(type_args),
                Doc::text("::"),
                Doc::text(variant_name),
                binding.as_ref().map_or(Doc::Nil, |binding| {
                    delimited("(", ident_doc(binding), ")", false, false)
                }),
            ])
        }
        Pattern::IntegerLiteral(value, ty) => Doc::text(format!("{value}{}", integer_suffix(*ty))),
        Pattern::Wildcard(name) => ident_doc(name),
    }
}

fn reflect_pattern_doc(pattern: &ReflectPattern) -> Doc {
    match pattern {
        ReflectPattern::Kind(kind) => Doc::text(format!("{kind:?}")),
        ReflectPattern::Wildcard => Doc::text("_"),
    }
}

fn ident_doc(identifier: &Ident) -> Doc {
    match identifier {
        Ident::User(name) | Ident::Synthetic(name) => Doc::text(name),
    }
}

fn binary_operator(operator: BinOp) -> &'static str {
    match operator {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Mod => "%",
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
        BinOp::And => "&&",
        BinOp::Or => "||",
        BinOp::BitAnd => "&",
        BinOp::BitOr => "|",
        BinOp::BitXor => "^",
        BinOp::Shl => "<<",
        BinOp::Shr => ">>",
        BinOp::WrapAdd => "++",
        BinOp::WrapSub => "--",
        BinOp::WrapMul => "**",
    }
}

fn integer_suffix(ty: IntegerType) -> &'static str {
    match ty {
        IntegerType::Int8 => "i8",
        IntegerType::Int16 => "i16",
        IntegerType::Int32 => "i32",
        IntegerType::Int64 => "i64",
        IntegerType::Int => "",
        IntegerType::Uint8 => "u8",
        IntegerType::Uint16 => "u16",
        IntegerType::Uint32 => "u32",
        IntegerType::Uint64 => "u64",
        IntegerType::Uint => "u",
    }
}

fn outer_parentheses(expression: &Expr, context: &SourceContext<'_>) -> usize {
    if matches!(expression.kind, ExprKind::TupleLiteral(_)) {
        return 0;
    }
    context
        .parentheses
        .iter()
        .filter(|parentheses| {
            position(parentheses.inner.start) == position(expression.span.start)
                && position(parentheses.inner.end) == position(expression.span.end)
        })
        .count()
}

fn wrap_parentheses(expression: &Expr, mut doc: Doc, context: &SourceContext<'_>) -> Doc {
    for _ in 0..outer_parentheses(expression, context) {
        doc = Doc::concat([Doc::text("("), doc, Doc::text(")")]);
    }
    doc
}

fn literal_doc(
    expression: &Expr,
    context: &SourceContext<'_>,
    fallback: impl FnOnce() -> String,
) -> Doc {
    let text = context.text(expression.span).trim();
    if !text.is_empty() && !text.bytes().any(|byte| byte.is_ascii_whitespace()) {
        Doc::text(text)
    } else {
        Doc::text(fallback())
    }
}

fn render(doc: Doc) -> String {
    render_with_indent(doc, 0)
}

fn render_with_indent(doc: Doc, indent: usize) -> String {
    let mut output = String::new();
    let mut column = 0;
    let mut line_start = true;
    let mut stack = vec![Command {
        indent,
        mode: Mode::Break,
        strict_flat: false,
        doc,
    }];

    while let Some(command) = stack.pop() {
        match command.doc {
            Doc::Nil => {}
            Doc::Text(text) => {
                if line_start {
                    output.extend(std::iter::repeat_n('\t', command.indent));
                    column = command.indent * TAB_WIDTH;
                    line_start = false;
                }
                column += text.chars().count();
                output.push_str(&text);
            }
            Doc::Line(flat) => match command.mode {
                Mode::Flat => {
                    column += flat.chars().count();
                    output.push_str(flat);
                }
                Mode::Break => {
                    output.push('\n');
                    column = 0;
                    line_start = true;
                }
            },
            Doc::HardLine => {
                output.push('\n');
                column = 0;
                line_start = true;
            }
            Doc::BreakParent | Doc::Measure(_) => {}
            Doc::Concat(docs) => {
                stack.extend(docs.into_iter().rev().map(|doc| Command {
                    indent: command.indent,
                    mode: command.mode,
                    strict_flat: command.strict_flat,
                    doc,
                }));
            }
            Doc::Nest(doc) => stack.push(Command {
                indent: command.indent + 1,
                mode: command.mode,
                strict_flat: command.strict_flat,
                doc: *doc,
            }),
            Doc::Group(doc) => {
                let mut trial = stack.clone();
                trial.push(Command {
                    indent: command.indent,
                    mode: Mode::Flat,
                    strict_flat: true,
                    doc: (*doc).clone(),
                });
                let mode = if fits(MAX_WIDTH.saturating_sub(column), trial) {
                    Mode::Flat
                } else {
                    Mode::Break
                };
                stack.push(Command {
                    indent: command.indent,
                    mode,
                    strict_flat: command.strict_flat,
                    doc: *doc,
                });
            }
            Doc::IfBreak(text) => {
                if matches!(command.mode, Mode::Break) {
                    if line_start {
                        output.extend(std::iter::repeat_n('\t', command.indent));
                        column = command.indent * TAB_WIDTH;
                        line_start = false;
                    }
                    output.push_str(text);
                    column += text.chars().count();
                }
            }
        }
    }
    output
}

fn fits(mut remaining: usize, mut stack: Vec<Command>) -> bool {
    while let Some(command) = stack.pop() {
        if matches!(command.mode, Mode::Break) && !command.strict_flat {
            return true;
        }
        match command.doc {
            Doc::Nil | Doc::IfBreak(_) => {}
            Doc::Text(text) => {
                let width = text.chars().count();
                if width > remaining {
                    return false;
                }
                remaining -= width;
            }
            Doc::Line(flat) => {
                let width = flat.chars().count();
                if width > remaining {
                    return false;
                }
                remaining -= width;
            }
            Doc::HardLine => return !command.strict_flat,
            Doc::BreakParent => return !command.strict_flat,
            Doc::Measure(width) => {
                if width > remaining {
                    return false;
                }
                remaining -= width;
            }
            Doc::Concat(docs) => {
                stack.extend(docs.into_iter().rev().map(|doc| Command {
                    indent: command.indent,
                    mode: Mode::Flat,
                    strict_flat: command.strict_flat,
                    doc,
                }));
            }
            Doc::Nest(doc) | Doc::Group(doc) => stack.push(Command {
                indent: command.indent,
                mode: Mode::Flat,
                strict_flat: command.strict_flat,
                doc: *doc,
            }),
        }
    }
    true
}

/// Formats a Solar source file, returning an error when it does not parse.
pub fn format_source(source: &str) -> Result<String, String> {
    let parsed = crate::parser::parse(source).map_err(|errors| {
        errors
            .into_iter()
            .map(|error| error.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    })?;
    let surface = syntax::parse(source);
    let context = SourceContext::new(
        source,
        &surface.trivia,
        &surface.parentheses,
        &surface.blocks,
    );
    let mut formatted = render(source_file_doc(&parsed, &context));
    formatted.truncate(formatted.trim_end_matches('\n').len());
    formatted.push('\n');
    Ok(formatted)
}

#[cfg(test)]
mod tests {
    use super::format_source;

    fn formatted(source: &str) -> String {
        format_source(source).unwrap()
    }

    #[test]
    fn formats_with_semantic_and_surface_trees() {
        assert_eq!(
            formatted("struct Pair { left:Int,right:Int, }\nfn f(a:Int){let x=a+1;x}\n"),
            "struct Pair { left: Int, right: Int }\nfn f(a: Int) { let x = a + 1; x }\n"
        );
    }

    #[test]
    fn preserves_comments_and_canonical_blank_lines() {
        assert_eq!(
            formatted("// docs\n\n\nfn f() {\nlet x=1; // value\n\n// tail\nx\n}\n"),
            "// docs\n\nfn f() {\n\tlet x = 1; // value\n\n\t// tail\n\tx\n}\n"
        );
    }

    #[test]
    fn preserves_multiline_blocks() {
        assert_eq!(
            formatted("fn f() {\nlet x=1;\nx\n}\n"),
            "fn f() {\n\tlet x = 1;\n\tx\n}\n"
        );
        assert_eq!(
            formatted("fn f(){let x=1;x}\n"),
            "fn f() { let x = 1; x }\n"
        );
    }

    #[test]
    fn hugs_sole_nested_blocks_but_not_control_flow() {
        assert_eq!(
            formatted(
                "fn f() {\nthread::spawn(\n\\ {\nlet x=1;\nx\n},\n);\nlet callbacks=[\n\\ {\nlet x=2;\nx\n},\n];\nouter(\ninner(\n\\ {\nlet x=3;\nx\n},\n),\n);\nconsume(\nif true {\n1\n} else {\n2\n},\n);\n}\n"
            ),
            "fn f() {\n\tthread::spawn(\\ {\n\t\tlet x = 1;\n\t\tx\n\t});\n\tlet callbacks = [\\ {\n\t\tlet x = 2;\n\t\tx\n\t}];\n\touter(inner(\\ {\n\t\tlet x = 3;\n\t\tx\n\t}));\n\tconsume(\n\t\tif true {\n\t\t\t1\n\t\t} else {\n\t\t\t2\n\t\t},\n\t);\n}\n"
        );
    }

    #[test]
    fn zero_parameter_closure_calls_are_unambiguous() {
        assert_eq!(
            formatted("fn f(){let x=\\add_one (4);x}\n"),
            "fn f() { let x = \\ add_one(4); x }\n"
        );
    }

    #[test]
    fn ranges_and_bitwise_operators_have_distinct_spacing() {
        assert_eq!(
            formatted("fn f(){for i in 0 .. 2 {println(12&(10|1));}let x=12&;}\n"),
            "fn f() { for i in 0..2 { println(12 & (10 | 1)); } let x = 12&; }\n"
        );
    }

    #[test]
    fn preserves_literal_spelling_and_parentheses() {
        assert_eq!(
            formatted("fn f(){let x=((0xffu));println(\"a\\n\"&);x}\n"),
            "fn f() { let x = ((0xffu)); println(\"a\\n\"&); x }\n"
        );
    }

    #[test]
    fn files_end_with_one_newline() {
        assert_eq!(formatted("fn f() {}\n\n\n"), "fn f() {}\n");
        assert_eq!(formatted("\n\n"), "\n");
    }

    #[test]
    fn rejects_parse_errors() {
        assert!(format_source("fn broken( {").is_err());
    }
}
