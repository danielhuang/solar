//! Type-independent normalization of the surface AST.

use crate::ast;

/// An untyped program after type-independent normalization.
#[derive(Debug)]
pub struct SourceFile {
    /// Top-level declarations in source order.
    pub items: Vec<ast::TopLevelItem>,
}

/// Normalizes a surface AST before name resolution.
pub fn lower(source: &ast::SourceFile) -> SourceFile {
    let mut desugarer = Desugarer::default();
    let mut items = source.items.clone();
    for item in &mut items {
        desugarer.item(item);
    }
    SourceFile { items }
}

#[derive(Default)]
struct Desugarer {
    for_counter: usize,
}

impl Desugarer {
    fn item(&mut self, item: &mut ast::TopLevelItem) {
        match item {
            ast::TopLevelItem::Struct(def) => {
                if def.is_tuple {
                    for (index, field) in def.fields.iter_mut().enumerate() {
                        field.name = format!("_{index}");
                    }
                }
            }
            ast::TopLevelItem::Function(def) | ast::TopLevelItem::Method(def) => {
                self.function(def);
            }
            ast::TopLevelItem::Enum(_)
            | ast::TopLevelItem::Import(_)
            | ast::TopLevelItem::TypeAlias(_) => {}
            ast::TopLevelItem::Const(def) => {
                *def.value = self.expr(def.value.as_ref().clone());
            }
            ast::TopLevelItem::Static(def) => {
                *def.value = self.expr(def.value.as_ref().clone());
            }
        }
    }

    fn function(&mut self, function: &mut ast::FunctionDef) {
        for parameter in &mut function.parameters {
            if let Some(default) = &mut parameter.default {
                **default = self.expr((**default).clone());
            }
        }
        self.statements(&mut function.body);
    }

    fn statements(&mut self, statements: &mut Vec<ast::Statement>) {
        let old = std::mem::take(statements);
        *statements = old
            .into_iter()
            .flat_map(|statement| self.statement(statement))
            .collect();
    }

    fn statement(&mut self, statement: ast::Statement) -> Vec<ast::Statement> {
        let span = statement.span;
        let one = |kind| vec![ast::Statement { kind, span }];
        match statement.kind {
            ast::StatementKind::Let { pattern, ty, value } => one(ast::StatementKind::Let {
                pattern,
                ty,
                value: self.expr(value),
            }),
            ast::StatementKind::Assignment { target, value } => {
                one(ast::StatementKind::Assignment {
                    target: self.expr(target),
                    value: self.expr(value),
                })
            }
            ast::StatementKind::If {
                condition,
                mut body,
                mut else_body,
            } => {
                self.statements(&mut body);
                self.statements(&mut else_body);
                one(ast::StatementKind::If {
                    condition: self.expr(condition),
                    body,
                    else_body,
                })
            }
            ast::StatementKind::While {
                condition,
                mut body,
            } => {
                self.statements(&mut body);
                one(ast::StatementKind::While {
                    condition: self.expr(condition),
                    body,
                })
            }
            ast::StatementKind::ForRange {
                variable,
                start,
                end,
                mut body,
            } => {
                self.statements(&mut body);
                one(ast::StatementKind::ForRange {
                    variable,
                    start: self.expr(start),
                    end: self.expr(end),
                    body,
                })
            }
            ast::StatementKind::ForIn {
                variable,
                iterable,
                mut body,
            } => {
                self.statements(&mut body);
                let iterable = self.expr(iterable);
                self.for_in(span, variable, iterable, body)
            }
            ast::StatementKind::Try {
                mut body,
                binding,
                binding_type,
                mut handler,
            } => {
                self.statements(&mut body);
                self.statements(&mut handler);
                one(ast::StatementKind::Try {
                    body,
                    binding,
                    binding_type,
                    handler,
                })
            }
            ast::StatementKind::ForReflectFields {
                pattern,
                object,
                mut body,
                paired,
            } => {
                self.statements(&mut body);
                one(ast::StatementKind::ForReflectFields {
                    pattern,
                    object: self.expr(object),
                    body,
                    paired,
                })
            }
            ast::StatementKind::MatchReflectVariant {
                pattern,
                object,
                mut body,
                paired,
            } => {
                self.statements(&mut body);
                one(ast::StatementKind::MatchReflectVariant {
                    pattern,
                    object: self.expr(object),
                    body,
                    paired,
                })
            }
            ast::StatementKind::Expression(expr) => {
                one(ast::StatementKind::Expression(self.expr(expr)))
            }
            ast::StatementKind::Return(expr) => one(ast::StatementKind::Return(self.expr(expr))),
            ast::StatementKind::ReturnVoid => one(ast::StatementKind::Return(ast::Expr {
                kind: ast::ExprKind::Block(Vec::new()),
                span,
            })),
            ast::StatementKind::Break(value) => one(ast::StatementKind::Break(
                value.map(|value| self.expr(value)),
            )),
            ast::StatementKind::Continue => one(ast::StatementKind::Continue),
            ast::StatementKind::NestedFunction(mut function) => {
                self.function(&mut function);
                one(ast::StatementKind::NestedFunction(function))
            }
            ast::StatementKind::Const(mut def) => {
                def.value = Box::new(self.expr(*def.value));
                one(ast::StatementKind::Const(def))
            }
        }
    }

    fn expr(&mut self, expr: ast::Expr) -> ast::Expr {
        let span = expr.span;
        let kind = match expr.kind {
            ast::ExprKind::Identifier(name) => ast::ExprKind::Identifier(name),
            ast::ExprKind::GlobalRef(def) => ast::ExprKind::GlobalRef(def),
            ast::ExprKind::IntegerLiteral(value, ty) => ast::ExprKind::IntegerLiteral(value, ty),
            ast::ExprKind::FloatLiteral(value, ty) => ast::ExprKind::FloatLiteral(value, ty),
            ast::ExprKind::BooleanLiteral(value) => ast::ExprKind::BooleanLiteral(value),
            ast::ExprKind::StringLiteral(bytes) => ast::ExprKind::ArrayLiteral(
                bytes
                    .into_iter()
                    .map(|byte| ast::Expr {
                        kind: ast::ExprKind::IntegerLiteral(byte as i128, ast::IntegerType::Uint8),
                        span,
                    })
                    .collect(),
                Some(ast::Type::Named(ast::DefId::new(0, "Uint8"))),
            ),
            ast::ExprKind::CharLiteral(byte) => {
                ast::ExprKind::IntegerLiteral(byte as i128, ast::IntegerType::Uint8)
            }
            ast::ExprKind::FieldAccess { object, field } => {
                let field = if !field.is_empty() && field.bytes().all(|byte| byte.is_ascii_digit())
                {
                    format!("_{field}")
                } else {
                    field
                };
                ast::ExprKind::FieldAccess {
                    object: Box::new(self.expr(*object)),
                    field,
                }
            }
            ast::ExprKind::Deref(inner) => ast::ExprKind::Deref(Box::new(self.expr(*inner))),
            ast::ExprKind::Reference(inner) => {
                ast::ExprKind::Reference(Box::new(self.expr(*inner)))
            }
            ast::ExprKind::Unique(inner) => ast::ExprKind::Unique(Box::new(self.expr(*inner))),
            ast::ExprKind::Not(inner) => ast::ExprKind::Not(Box::new(self.expr(*inner))),
            ast::ExprKind::NullLiteral(ty) => ast::ExprKind::NullLiteral(ty),
            ast::ExprKind::Call {
                function,
                type_args,
                arguments,
                kwargs,
            } => ast::ExprKind::Call {
                function: Box::new(self.expr(*function)),
                type_args,
                arguments: arguments
                    .into_iter()
                    .map(|argument| self.expr(argument))
                    .collect(),
                kwargs: kwargs
                    .into_iter()
                    .map(|(name, value)| (name, self.expr(value)))
                    .collect(),
            },
            ast::ExprKind::StructLiteral {
                module,
                name,
                type_args,
                fields,
            } => ast::ExprKind::StructLiteral {
                module,
                name,
                type_args,
                fields: fields
                    .into_iter()
                    .map(|field| ast::FieldInit {
                        name: field.name,
                        value: self.expr(field.value),
                    })
                    .collect(),
            },
            ast::ExprKind::Index { object, index } => ast::ExprKind::Index {
                object: Box::new(self.expr(*object)),
                index: Box::new(self.expr(*index)),
            },
            ast::ExprKind::Slice { object, start, end } => ast::ExprKind::Slice {
                object: Box::new(self.expr(*object)),
                start: Box::new(self.expr(*start)),
                end: Box::new(self.expr(*end)),
            },
            ast::ExprKind::ArrayLiteral(elements, ty) => ast::ExprKind::ArrayLiteral(
                elements
                    .into_iter()
                    .map(|element| self.expr(element))
                    .collect(),
                ty,
            ),
            ast::ExprKind::ArrayRepeat { element, count } => ast::ExprKind::ArrayRepeat {
                element: Box::new(self.expr(*element)),
                count: Box::new(self.expr(*count)),
            },
            ast::ExprKind::Loop(mut body) => {
                self.statements(&mut body);
                ast::ExprKind::Loop(body)
            }
            ast::ExprKind::BinaryOp { op, left, right } => ast::ExprKind::BinaryOp {
                op,
                left: Box::new(self.expr(*left)),
                right: Box::new(self.expr(*right)),
            },
            ast::ExprKind::If {
                condition,
                mut then_body,
                mut else_body,
            } => {
                self.statements(&mut then_body);
                self.statements(&mut else_body);
                ast::ExprKind::If {
                    condition: Box::new(self.expr(*condition)),
                    then_body,
                    else_body,
                }
            }
            ast::ExprKind::Block(mut body) => {
                self.statements(&mut body);
                ast::ExprKind::Block(body)
            }
            ast::ExprKind::UnsafeBlock(mut body) => {
                self.statements(&mut body);
                ast::ExprKind::UnsafeBlock(body)
            }
            ast::ExprKind::Closure {
                mut parameters,
                return_type,
                mut body,
            } => {
                for parameter in &mut parameters {
                    if let Some(default) = &mut parameter.default {
                        **default = self.expr((**default).clone());
                    }
                }
                self.statements(&mut body);
                ast::ExprKind::Closure {
                    parameters,
                    return_type,
                    body,
                }
            }
            ast::ExprKind::EnumVariant {
                module_path,
                enum_name,
                type_args,
                variant_name,
            } => ast::ExprKind::EnumVariant {
                module_path,
                enum_name,
                type_args,
                variant_name,
            },
            ast::ExprKind::Match { scrutinee, arms } => ast::ExprKind::Match {
                scrutinee: Box::new(self.expr(*scrutinee)),
                arms: arms
                    .into_iter()
                    .map(|arm| ast::MatchArm {
                        pattern: arm.pattern,
                        body: self.expr(arm.body),
                    })
                    .collect(),
            },
            ast::ExprKind::MatchReflect { ty, arms } => ast::ExprKind::MatchReflect {
                ty,
                arms: arms
                    .into_iter()
                    .map(|arm| ast::ReflectArm {
                        pattern: arm.pattern,
                        body: self.expr(arm.body),
                    })
                    .collect(),
            },
            ast::ExprKind::MethodCall {
                receiver,
                method,
                type_args,
                arguments,
                kwargs,
            } => ast::ExprKind::MethodCall {
                receiver: Box::new(self.expr(*receiver)),
                method,
                type_args,
                arguments: arguments
                    .into_iter()
                    .map(|argument| self.expr(argument))
                    .collect(),
                kwargs: kwargs
                    .into_iter()
                    .map(|(name, value)| (name, self.expr(value)))
                    .collect(),
            },
            ast::ExprKind::TupleLiteral(elements) => ast::ExprKind::TupleLiteral(
                elements
                    .into_iter()
                    .map(|element| self.expr(element))
                    .collect(),
            ),
            ast::ExprKind::IntrinsicCall {
                intrinsic,
                arguments,
            } => ast::ExprKind::IntrinsicCall {
                intrinsic,
                arguments: arguments
                    .into_iter()
                    .map(|argument| self.expr(argument))
                    .collect(),
            },
        };
        ast::Expr { kind, span }
    }

    fn for_in(
        &mut self,
        span: ast::SourceSpan,
        variable: ast::Ident,
        iterable: ast::Expr,
        body: Vec<ast::Statement>,
    ) -> Vec<ast::Statement> {
        let index = self.for_counter;
        self.for_counter += 1;
        let iterator = ast::Ident::synthetic(format!("__for_iter_{index}"));

        let identifier = |name: &ast::Ident| ast::Expr {
            kind: ast::ExprKind::Identifier(name.clone()),
            span,
        };
        let reference = |value: ast::Expr| ast::Expr {
            kind: ast::ExprKind::Reference(Box::new(value)),
            span,
        };
        let method_call = |receiver: ast::Expr, method: &str| ast::Expr {
            kind: ast::ExprKind::MethodCall {
                receiver: Box::new(receiver),
                method: method.to_string(),
                type_args: Vec::new(),
                arguments: Vec::new(),
                kwargs: Vec::new(),
            },
            span,
        };
        let statement = |kind| ast::Statement { kind, span };

        // Evaluate the iterable once and use ordinary method lookup for both
        // operations. The synthetic references match Solar's mutable iterator
        // convention (`iter(self: &T)`, `next(self: &Iter)`) without imposing a
        // nominal iterator type.
        let iterator_binding = statement(ast::StatementKind::Let {
            pattern: ast::DestructurePattern::Name(iterator.clone()),
            ty: None,
            value: method_call(reference(iterable), "iter"),
        });

        // The payload type is inferred from next()'s concrete Option instance
        // during typed-AST lowering. Keeping this as normal match/loop syntax
        // means break and continue need no special for-in handling downstream.
        let option_type_args = vec![ast::Type::Infer];
        let next = method_call(reference(identifier(&iterator)), "next");
        let match_next = ast::Expr {
            kind: ast::ExprKind::Match {
                scrutinee: Box::new(next),
                arms: vec![
                    ast::MatchArm {
                        pattern: ast::Pattern::Variant {
                            module_path: Vec::new(),
                            enum_name: ast::DefId::new(0, "Option"),
                            type_args: option_type_args.clone(),
                            variant_name: "Some".to_string(),
                            binding: Some(variable),
                        },
                        body: ast::Expr {
                            kind: ast::ExprKind::Block(body),
                            span,
                        },
                    },
                    ast::MatchArm {
                        pattern: ast::Pattern::Variant {
                            module_path: Vec::new(),
                            enum_name: ast::DefId::new(0, "Option"),
                            type_args: option_type_args,
                            variant_name: "None".to_string(),
                            binding: None,
                        },
                        body: ast::Expr {
                            kind: ast::ExprKind::Block(vec![statement(ast::StatementKind::Break(
                                None,
                            ))]),
                            span,
                        },
                    },
                ],
            },
            span,
        };
        let loop_statement = statement(ast::StatementKind::Expression(ast::Expr {
            kind: ast::ExprKind::Loop(vec![statement(ast::StatementKind::Expression(match_next))]),
            span,
        }));

        vec![iterator_binding, loop_statement]
    }
}
