//! Name-resolved AST and compiler-supplied definitions.

use crate::ast;
use crate::intrinsics::Intrinsic;
use std::collections::{HashMap, HashSet};

/// A resolved, untyped Solar program.
#[derive(Debug)]
pub struct SourceFile {
    /// Resolved declarations followed by compiler-supplied definitions.
    pub items: Vec<ast::TopLevelItem>,
    /// Imported extension-method definitions visible in each source file.
    pub extension_scopes: HashMap<u32, HashSet<ast::DefId>>,
}

impl SourceFile {
    /// Whether a method is available at a call site in `file`.
    pub fn method_is_visible(&self, method: &ast::FunctionDef, file: u32) -> bool {
        !method.is_extension
            || method.span.file_id == file
            || method.is_pub
                && self.extension_scopes.get(&file).is_some_and(|scope| {
                    scope.contains(&ast::DefId::new(method.span.file_id, &method.name))
                })
    }

    pub(crate) fn methods_share_scope(&self, a: &ast::FunctionDef, b: &ast::FunctionDef) -> bool {
        self.method_is_visible(a, b.span.file_id)
            || self.method_is_visible(b, a.span.file_id)
            || self
                .extension_scopes
                .keys()
                .any(|&file| self.method_is_visible(a, file) && self.method_is_visible(b, file))
    }
}

/// Packages resolver output and adds the numeric constructor overloads once.
pub(crate) fn from_items(mut items: Vec<ast::TopLevelItem>) -> SourceFile {
    generate_numeric_constructors(&mut items);
    SourceFile {
        items,
        extension_scopes: HashMap::new(),
    }
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
            let intrinsic = Intrinsic::Cast(from, target);
            let value = ast::Expr {
                kind: ast::ExprKind::Identifier(ast::Ident::user("x")),
                span,
            };
            items.push(ast::TopLevelItem::Function(ast::FunctionDef {
                associated_type_params: Vec::new(),
                associated_type: Some(ast::Type::Named(ast::DefId::new(0, target_name))),
                name: String::new(),
                display_name: target_name.to_string(),
                type_params: Vec::new(),
                out_type_params: Vec::new(),
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
                        kind: if target == from {
                            // Generic numeric algorithms use T(0), T(1), and
                            // T(value) even when the source already has type T.
                            value.kind
                        } else {
                            ast::ExprKind::IntrinsicCall {
                                intrinsic,
                                type_args: Vec::new(),
                                arguments: vec![value],
                            }
                        },
                        span,
                    }),
                    span,
                }],
                is_pub: false,
                is_unsafe: false,
                inline_hint: false,
                is_extension: false,
                is_overlap: false,
                doc: None,
                span,
            }));
        }
    }
}
