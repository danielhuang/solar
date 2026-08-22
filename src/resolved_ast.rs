//! Name-resolved AST and compiler-supplied definitions.

use crate::ast;

/// A resolved, untyped Solar program.
#[derive(Debug)]
pub struct SourceFile {
    /// Resolved declarations followed by compiler-supplied definitions.
    pub items: Vec<ast::TopLevelItem>,
}

/// Packages resolver output and adds the numeric constructor overloads once.
pub(crate) fn from_items(mut items: Vec<ast::TopLevelItem>) -> SourceFile {
    generate_numeric_constructors(&mut items);
    SourceFile { items }
}

/// Generates overloaded constructor functions for each numeric type.
fn generate_numeric_constructors(items: &mut Vec<ast::TopLevelItem>) {
    let types: Vec<(ast::NumericType, &str)> = ast::PRIMITIVE_TYPES
        .iter()
        .filter_map(|(primitive, name)| primitive.numeric().map(|numeric| (numeric, *name)))
        .collect();
    let span = ast::SourceSpan {
        file_id: ast::SYNTHETIC_FILE,
        ..ast::SourceSpan::default()
    };

    for &(target, target_name) in &types {
        for &(from, from_name) in &types {
            if target_name == from_name {
                continue;
            }
            let intrinsic = ast::Intrinsic::Cast(from, target);
            items.push(ast::TopLevelItem::Function(ast::FunctionDef {
                name: target_name.to_string(),
                display_name: target_name.to_string(),
                type_params: Vec::new(),
                parameters: vec![ast::Parameter {
                    pattern: ast::DestructurePattern::Name(ast::Ident::user("x")),
                    ty: ast::Type::Named(ast::DefId::new(0, from_name)),
                    default: None,
                    span,
                }],
                return_type: Some(ast::Type::Named(ast::DefId::new(0, target_name))),
                return_type_span: None,
                body: vec![ast::Statement {
                    kind: ast::StatementKind::Expression(ast::Expr {
                        kind: ast::ExprKind::IntrinsicCall {
                            intrinsic,
                            arguments: vec![ast::Expr {
                                kind: ast::ExprKind::Identifier(ast::Ident::user("x")),
                                span,
                            }],
                        },
                        span,
                    }),
                    span,
                }],
                is_pub: false,
                is_unsafe: false,
                inline_hint: false,
                doc: None,
                span,
            }));
        }
    }
}
