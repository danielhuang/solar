use crate::ast::{SourcePos, SourceSpan};

pub(super) struct Surface {
    pub(super) trivia: Vec<Trivia>,
    pub(super) parentheses: Vec<Parentheses>,
    pub(super) blocks: Vec<CodeBlock>,
}

pub(super) enum Trivia {
    Comment(Comment),
    BlankLine(SourceSpan),
}

pub(super) struct Comment {
    pub(super) text: String,
    pub(super) span: SourceSpan,
}

pub(super) struct Parentheses {
    pub(super) inner: SourceSpan,
}

pub(super) struct CodeBlock {
    pub(super) span: SourceSpan,
}

pub(super) fn parse(source: &str) -> Surface {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_solar::LANGUAGE.into())
        .expect("failed to set tree-sitter language");
    let tree = parser
        .parse(source, None)
        .expect("tree-sitter parse failed");
    let root = tree.root_node();
    Surface {
        trivia: collect_trivia(root, source),
        parentheses: collect_parentheses(root),
        blocks: collect_blocks(root),
    }
}

fn source_span(node: tree_sitter::Node<'_>) -> SourceSpan {
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

fn code_children(node: tree_sitter::Node<'_>) -> Vec<tree_sitter::Node<'_>> {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .filter(|child| !matches!(child.kind(), "comment" | "doc_comment"))
        .filter(|child| child.is_named())
        .collect()
}

fn named_child_by_kind<'tree>(
    node: tree_sitter::Node<'tree>,
    kind: &str,
) -> Option<tree_sitter::Node<'tree>> {
    code_children(node)
        .into_iter()
        .find(|child| child.kind() == kind)
}

fn ambiguous_zero_parameter_closure_call(node: tree_sitter::Node<'_>) -> bool {
    if node.kind() != "closure_expr" || node.child_by_field_name("return_type").is_some() {
        return false;
    }
    let Some(parameters) = named_child_by_kind(node, "closure_param_list") else {
        return false;
    };
    let parameters = code_children(parameters)
        .into_iter()
        .filter(|child| child.kind() == "closure_param")
        .collect::<Vec<_>>();
    parameters.len() == 1
        && parameters[0].child_by_field_name("type").is_none()
        && node
            .child_by_field_name("body")
            .is_some_and(|body| matches!(body.kind(), "parenthesized_expression" | "tuple_literal"))
}

fn ambiguous_bitwise_call(node: tree_sitter::Node<'_>) -> bool {
    if node.kind() != "call_expr"
        || node
            .parent()
            .is_some_and(|parent| parent.kind() == "binary_expression")
    {
        return false;
    }
    let Some(function) = node.child_by_field_name("function") else {
        return false;
    };
    if !matches!(function.kind(), "reference_expr" | "unique_expr") {
        return false;
    }
    let Some(arguments) = named_child_by_kind(node, "argument_list") else {
        return false;
    };
    let arguments = code_children(arguments)
        .into_iter()
        .filter(|child| child.kind() == "argument")
        .collect::<Vec<_>>();
    arguments.len() == 1 && arguments[0].child_by_field_name("name").is_none()
}

fn collect_trivia(root: tree_sitter::Node<'_>, source: &str) -> Vec<Trivia> {
    fn comments(node: tree_sitter::Node<'_>, source: &str, output: &mut Vec<Trivia>) {
        if matches!(node.kind(), "comment" | "doc_comment") {
            output.push(Trivia::Comment(Comment {
                text: source[node.byte_range()].to_owned(),
                span: source_span(node),
            }));
            return;
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            comments(child, source, output);
        }
    }

    let mut trivia = Vec::new();
    comments(root, source, &mut trivia);
    for (line, text) in source.split('\n').enumerate() {
        if text.trim().is_empty() {
            let line = line as u32;
            trivia.push(Trivia::BlankLine(SourceSpan {
                start: SourcePos { line, col: 0 },
                end: SourcePos {
                    line,
                    col: text.len() as u32,
                },
                file_id: 0,
            }));
        }
    }
    trivia.sort_by_key(|trivia| {
        let span = match trivia {
            Trivia::Comment(comment) => comment.span,
            Trivia::BlankLine(span) => *span,
        };
        (span.start.line, span.start.col)
    });
    trivia
}

fn collect_parentheses(root: tree_sitter::Node<'_>) -> Vec<Parentheses> {
    fn semantic_inner(mut node: tree_sitter::Node<'_>) -> tree_sitter::Node<'_> {
        while node.kind() == "parenthesized_expression" {
            node = code_children(node).into_iter().next().unwrap();
        }
        node
    }

    fn collect(node: tree_sitter::Node<'_>, output: &mut Vec<Parentheses>) {
        if node.kind() == "parenthesized_expression"
            && !node.parent().is_some_and(|parent| {
                parent.kind() == "closure_expr" && ambiguous_zero_parameter_closure_call(parent)
            })
        {
            output.push(Parentheses {
                inner: source_span(semantic_inner(node)),
            });
        } else if ambiguous_bitwise_call(node) {
            let arguments = named_child_by_kind(node, "argument_list").unwrap();
            let argument = code_children(arguments)
                .into_iter()
                .find(|child| child.kind() == "argument")
                .unwrap();
            output.push(Parentheses {
                inner: source_span(semantic_inner(
                    argument.child_by_field_name("value").unwrap(),
                )),
            });
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            collect(child, output);
        }
    }

    let mut parentheses = Vec::new();
    collect(root, &mut parentheses);
    parentheses
}

fn collect_blocks(root: tree_sitter::Node<'_>) -> Vec<CodeBlock> {
    fn collect(node: tree_sitter::Node<'_>, output: &mut Vec<CodeBlock>) {
        if node.kind() == "block" {
            output.push(CodeBlock {
                span: source_span(node),
            });
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            collect(child, output);
        }
    }

    let mut blocks = Vec::new();
    collect(root, &mut blocks);
    blocks
}
