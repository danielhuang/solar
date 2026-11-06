//! Type-independent normalization of the surface AST.

use crate::ast;

// Leaves have the same representation on both sides of desugaring.
pub use crate::ast::{
    BinOp, DefId, DestructureField, DestructurePattern, EnumDef, FieldDef, FloatType, Ident,
    ImportDef, ImportKind, ImportName, IntegerType, NumericType, PRIMITIVE_TYPES, Pattern,
    PrimitiveType, ReflectPattern, STDLIB_FILE, SYNTHETIC_FILE, SourcePos, SourceSpan, StructDef,
    Type, TypeAliasDef, VariantDef,
};
pub use nodes::*;

mod for_loop;
mod nodes;

/// Converts parsed syntax into a separate normalized tree before name resolution.
pub fn lower(source: &ast::SourceFile) -> SourceFile {
    let mut desugarer = Desugarer::default();
    SourceFile {
        items: source
            .items
            .iter()
            .cloned()
            .map(|item| desugarer.item(item))
            .collect(),
    }
}

#[derive(Default)]
struct Desugarer {
    for_counter: usize,
}

impl Desugarer {
    fn item(&mut self, item: ast::TopLevelItem) -> TopLevelItem {
        match item {
            ast::TopLevelItem::Struct(mut def) => {
                if def.is_tuple {
                    for (index, field) in def.fields.iter_mut().enumerate() {
                        field.name = format!("_{index}");
                    }
                }
                TopLevelItem::Struct(def)
            }
            ast::TopLevelItem::Function(def) => TopLevelItem::Function(self.function(def)),
            ast::TopLevelItem::Method(def) => TopLevelItem::Method(self.function(def)),
            ast::TopLevelItem::Enum(def) => TopLevelItem::Enum(def),
            ast::TopLevelItem::Import(def) => TopLevelItem::Import(def),
            ast::TopLevelItem::TypeAlias(def) => TopLevelItem::TypeAlias(def),
            ast::TopLevelItem::Const(def) => TopLevelItem::Const(self.constant(def)),
            ast::TopLevelItem::Static(def) => TopLevelItem::Static(StaticDef {
                name: def.name,
                ty: def.ty,
                value: Box::new(self.expr(*def.value)),
                thread_local: def.thread_local,
                is_pub: def.is_pub,
                doc: def.doc,
                span: def.span,
            }),
        }
    }

    fn constant(&mut self, def: ast::ConstDef) -> ConstDef {
        ConstDef {
            name: def.name,
            ty: def.ty,
            value: Box::new(self.expr(*def.value)),
            is_pub: def.is_pub,
            doc: def.doc,
            span: def.span,
        }
    }

    fn parameter(&mut self, parameter: ast::Parameter) -> Parameter {
        Parameter {
            pattern: parameter.pattern,
            ty: parameter.ty,
            default: parameter.default.map(|value| Box::new(self.expr(*value))),
            span: parameter.span,
        }
    }

    fn function(&mut self, function: ast::FunctionDef) -> FunctionDef {
        FunctionDef {
            associated_type_params: function.associated_type_params,
            associated_type: function.associated_type,
            name: function.name,
            display_name: function.display_name,
            type_params: function.type_params,
            out_type_params: function.out_type_params,
            parameters: function
                .parameters
                .into_iter()
                .map(|p| self.parameter(p))
                .collect(),
            return_type: function.return_type,
            return_type_span: function.return_type_span,
            body: self.statements(function.body),
            is_pub: function.is_pub,
            is_unsafe: function.is_unsafe,
            inline_hint: function.inline_hint,
            is_extension: function.is_extension,
            is_overlap: function.is_overlap,
            doc: function.doc,
            span: function.span,
        }
    }

    fn statements(&mut self, statements: Vec<ast::Statement>) -> Vec<Statement> {
        statements
            .into_iter()
            .map(|statement| self.statement(statement))
            .collect()
    }

    fn statement(&mut self, statement: ast::Statement) -> Statement {
        let kind = match statement.kind {
            ast::StatementKind::Let { pattern, ty, value } => StatementKind::Let {
                pattern,
                ty,
                value: self.expr(value),
            },
            ast::StatementKind::Assignment { target, value } => StatementKind::Assignment {
                target: self.expr(target),
                value: self.expr(value),
            },
            ast::StatementKind::If {
                condition,
                body,
                else_body,
            } => {
                let body = self.statements(body);
                let else_body = self.statements(else_body);
                StatementKind::If {
                    condition: self.expr(condition),
                    body,
                    else_body,
                }
            }
            ast::StatementKind::While { condition, body } => {
                let body = self.statements(body);
                StatementKind::While {
                    condition: self.expr(condition),
                    body,
                }
            }
            ast::StatementKind::Try {
                body,
                binding,
                binding_type,
                handler,
            } => StatementKind::Try {
                body: self.statements(body),
                binding,
                binding_type,
                handler: self.statements(handler),
            },
            ast::StatementKind::ForReflectFields {
                pattern,
                object,
                body,
                paired,
            } => {
                let body = self.statements(body);
                StatementKind::ForReflectFields {
                    pattern,
                    object: self.expr(object),
                    body,
                    paired,
                }
            }
            ast::StatementKind::MatchReflectVariant {
                pattern,
                object,
                body,
                paired,
            } => {
                let body = self.statements(body);
                StatementKind::MatchReflectVariant {
                    pattern,
                    object: self.expr(object),
                    body,
                    paired,
                }
            }
            ast::StatementKind::Expression(expr) => StatementKind::Expression(self.expr(expr)),
            ast::StatementKind::Return(expr) => StatementKind::Return(self.expr(expr)),
            ast::StatementKind::Break(value) => {
                StatementKind::Break(value.map(|value| self.expr(value)))
            }
            ast::StatementKind::Continue => StatementKind::Continue,
            ast::StatementKind::NestedFunction(function) => {
                StatementKind::NestedFunction(self.function(function))
            }
            ast::StatementKind::Const(def) => StatementKind::Const(self.constant(def)),
        };
        Statement {
            kind,
            span: statement.span,
        }
    }

    fn expr(&mut self, expr: ast::Expr) -> Expr {
        let span = expr.span;
        let kind = match expr.kind {
            ast::ExprKind::Identifier(name) => ExprKind::Identifier(name),
            ast::ExprKind::AssociatedFunction { ty, name } => {
                ExprKind::AssociatedFunction { ty, name }
            }
            ast::ExprKind::IntegerLiteral(value, ty) => ExprKind::IntegerLiteral(value, ty),
            ast::ExprKind::FloatLiteral(value, ty) => ExprKind::FloatLiteral(value, ty),
            ast::ExprKind::BooleanLiteral(value) => ExprKind::BooleanLiteral(value),
            ast::ExprKind::StringLiteral(bytes) => ExprKind::ArrayLiteral(
                bytes
                    .into_iter()
                    .map(|byte| Expr {
                        kind: ExprKind::IntegerLiteral(byte as i128, ast::IntegerType::Uint8),
                        span,
                    })
                    .collect(),
                Some(ast::Type::Named(ast::DefId::new(0, "Uint8"))),
            ),
            ast::ExprKind::CharLiteral(byte) => {
                ExprKind::IntegerLiteral(byte as i128, ast::IntegerType::Uint8)
            }
            ast::ExprKind::FieldAccess { object, field } => {
                let field = if !field.is_empty() && field.bytes().all(|byte| byte.is_ascii_digit())
                {
                    format!("_{field}")
                } else {
                    field
                };
                ExprKind::FieldAccess {
                    object: Box::new(self.expr(*object)),
                    field,
                }
            }
            ast::ExprKind::Deref(inner) => ExprKind::Deref(Box::new(self.expr(*inner))),
            ast::ExprKind::Reference(inner) => ExprKind::Reference(Box::new(self.expr(*inner))),
            ast::ExprKind::Unique(inner) => ExprKind::Unique(Box::new(self.expr(*inner))),
            ast::ExprKind::Not(inner) => ExprKind::Not(Box::new(self.expr(*inner))),
            ast::ExprKind::NullLiteral(ty) => ExprKind::NullLiteral(ty),
            ast::ExprKind::Call {
                function,
                type_args,
                arguments,
                kwargs,
            } => ExprKind::Call {
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
            } => ExprKind::StructLiteral {
                module,
                name,
                type_args,
                fields: fields
                    .into_iter()
                    .map(|field| FieldInit {
                        name: field.name,
                        value: self.expr(field.value),
                    })
                    .collect(),
            },
            ast::ExprKind::Index { object, index } => {
                let object = self.expr(*object);
                let receiver_span = object.span;
                ExprKind::Deref(Box::new(Expr {
                    kind: ExprKind::MethodCall {
                        receiver: Box::new(Expr {
                            kind: ExprKind::Reference(Box::new(object)),
                            span: receiver_span,
                        }),
                        method: "operator_index".to_owned(),
                        type_args: vec![],
                        arguments: vec![self.expr(*index)],
                        kwargs: vec![],
                    },
                    span,
                }))
            }
            ast::ExprKind::Slice { object, start, end } => ExprKind::Slice {
                object: Box::new(self.expr(*object)),
                start: Box::new(self.expr(*start)),
                end: Box::new(self.expr(*end)),
            },
            ast::ExprKind::ArrayLiteral(elements, ty) => ExprKind::ArrayLiteral(
                elements
                    .into_iter()
                    .map(|element| self.expr(element))
                    .collect(),
                ty,
            ),
            ast::ExprKind::ArrayRepeat { element, count } => ExprKind::ArrayRepeat {
                element: Box::new(self.expr(*element)),
                count: Box::new(self.expr(*count)),
            },
            ast::ExprKind::ForRange {
                variable,
                start,
                end,
                body,
            } => {
                let body = self.statements(body);
                ExprKind::Block(vec![
                    Statement {
                        kind: StatementKind::ForRange {
                            variable,
                            start: self.expr(*start),
                            end: self.expr(*end),
                            body,
                        },
                        span,
                    },
                    Statement {
                        kind: StatementKind::Expression(Expr {
                            kind: ExprKind::TupleLiteral(Vec::new()),
                            span,
                        }),
                        span,
                    },
                ])
            }
            ast::ExprKind::ForIn {
                variable,
                iterable,
                body,
            } => {
                let body = self.statements(body);
                let iterable = self.expr(*iterable);
                ExprKind::Block(self.for_in(span, variable, iterable, body))
            }
            ast::ExprKind::Loop(body) => {
                let body = self.statements(body);
                ExprKind::Loop(body)
            }
            ast::ExprKind::BinaryOp { op, left, right } => ExprKind::BinaryOp {
                op,
                left: Box::new(self.expr(*left)),
                right: Box::new(self.expr(*right)),
            },
            ast::ExprKind::If {
                condition,
                then_body,
                else_body,
            } => {
                let then_body = self.statements(then_body);
                let else_body = self.statements(else_body);
                ExprKind::If {
                    condition: Box::new(self.expr(*condition)),
                    then_body,
                    else_body,
                }
            }
            ast::ExprKind::Block(body) => {
                let body = self.statements(body);
                ExprKind::Block(body)
            }
            ast::ExprKind::UnsafeBlock(body) => {
                let body = self.statements(body);
                ExprKind::UnsafeBlock(body)
            }
            ast::ExprKind::Coroutine { parameter, body } => {
                let body = self.statements(body);
                ExprKind::Coroutine {
                    parameter: parameter.map(|p| Box::new(self.parameter(*p))),
                    body,
                }
            }
            ast::ExprKind::Await(value) => ExprKind::Await(Box::new(self.expr(*value))),
            ast::ExprKind::Yield(value) => ExprKind::Yield(Box::new(self.expr(*value))),
            ast::ExprKind::Closure {
                parameters,
                return_type,
                body,
            } => {
                let parameters = parameters.into_iter().map(|p| self.parameter(p)).collect();
                let body = self.statements(body);
                ExprKind::Closure {
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
            } => ExprKind::EnumVariant {
                module_path,
                enum_name,
                type_args,
                variant_name,
            },
            ast::ExprKind::Match { scrutinee, arms } => ExprKind::Match {
                scrutinee: Box::new(self.expr(*scrutinee)),
                arms: arms
                    .into_iter()
                    .map(|arm| MatchArm {
                        pattern: arm.pattern,
                        body: self.expr(arm.body),
                    })
                    .collect(),
            },
            ast::ExprKind::MatchReflect { ty, arms } => ExprKind::MatchReflect {
                ty,
                arms: arms
                    .into_iter()
                    .map(|arm| ReflectArm {
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
            } => ExprKind::MethodCall {
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
            ast::ExprKind::TupleLiteral(elements) => ExprKind::TupleLiteral(
                elements
                    .into_iter()
                    .map(|element| self.expr(element))
                    .collect(),
            ),
        };
        Expr { kind, span }
    }
}
