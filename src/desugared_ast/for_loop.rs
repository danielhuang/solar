//! Iteration through ordinary overlapping functions and value-producing loops.

use super::{self as ast, Desugarer};
use ast::{ExprKind as E, StatementKind as S};

impl Desugarer {
    pub(super) fn for_in(
        &mut self,
        span: ast::SourceSpan,
        variable: ast::Ident,
        iterable: ast::Expr,
        body: Vec<ast::Statement>,
    ) -> Vec<ast::Statement> {
        let index = self.for_counter;
        self.for_counter += 1;
        let b = Builder(span);
        let iterator = ast::Ident::synthetic(format!("__for_iter_{index}"));
        let returned = ast::Ident::synthetic(format!("__for_return_{index}"));
        let cursor = ast::Ident::synthetic("cursor");
        let value = ast::Ident::synthetic("value");
        let step = ast::Ident::synthetic(format!("for_step_{index}"));
        let convert = ast::Ident::synthetic(format!("for_state_{index}"));
        let iterator_type = ast::Type::Named(ast::DefId::new(0, "Iterator"));
        let yielded_type = ast::Type::Named(ast::DefId::new(0, "Yield"));
        let return_type = ast::Type::Named(ast::DefId::new(0, "Return"));
        let unit = ast::Type::Tuple(Vec::new());
        let state_args = vec![yielded_type.clone(), unit];

        // The first overload adapts next() to the same state protocol, with ()
        // as its completion value. Its nested converter infers the item type
        // from Option without requiring any annotation on the user's iterator.
        let from_next = b.function(
            &convert,
            &["Yield"],
            value.clone(),
            b.generic("Option", vec![yielded_type.clone()]),
            false,
            vec![b.statement(S::Expression(b.expr(E::Match {
                scrutinee: Box::new(b.ident(&value)),
                arms: vec![
                    ast::MatchArm {
                        pattern: b.pattern(
                            "Option",
                            vec![yielded_type],
                            "Some",
                            Some(value.clone()),
                        ),
                        body: b.variant("Yield", state_args.clone(), b.ident(&value)),
                    },
                    ast::MatchArm {
                        pattern: b.pattern("Option", vec![ast::Type::Infer], "None", None),
                        body: b.variant("Complete", state_args, b.unit()),
                    },
                ],
            })))],
        );
        let next = b.function(
            &step,
            &["Iterator"],
            cursor.clone(),
            ast::Type::Reference(Box::new(iterator_type.clone())),
            true,
            vec![
                from_next,
                b.statement(S::Expression(
                    b.call(&convert, b.method(b.ident(&cursor), "next")),
                )),
            ],
        );

        // The later, equally specific overload wins when resume() typechecks.
        // Check its state shape inside the candidate so an incompatible resume
        // result also permits falling back to next().
        let from_resume = b.function(
            &convert,
            &["Yield", "Return"],
            value.clone(),
            b.generic(
                "CoroutineState",
                vec![ast::Type::Named(ast::DefId::new(0, "Yield")), return_type],
            ),
            false,
            vec![b.statement(S::Expression(b.ident(&value)))],
        );
        let resume = b.function(
            &step,
            &["Iterator"],
            cursor.clone(),
            ast::Type::Reference(Box::new(iterator_type)),
            true,
            vec![
                from_resume,
                b.statement(S::Expression(
                    b.call(&convert, b.method(b.ident(&cursor), "resume")),
                )),
            ],
        );

        // Evaluate the iterable and acquire its cursor once. Passing references
        // to the helper preserves mutation and the existing iter() convention.
        let binding = b.statement(S::Let {
            pattern: ast::DestructurePattern::Name(iterator.clone()),
            ty: None,
            value: b.method(b.reference(iterable), "iter"),
        });
        let ignored_body = b.expr(E::Block(vec![
            b.statement(S::If {
                condition: b.expr(E::BooleanLiteral(true)),
                body,
                else_body: Vec::new(),
            }),
            b.statement(S::Expression(b.unit())),
        ]));
        let state_args = vec![ast::Type::Infer, ast::Type::Infer];
        let advance = b.expr(E::Match {
            scrutinee: Box::new(b.call(&step, b.reference(b.ident(&iterator)))),
            arms: vec![
                ast::MatchArm {
                    pattern: b.pattern(
                        "CoroutineState",
                        state_args.clone(),
                        "Yield",
                        Some(variable),
                    ),
                    body: ignored_body,
                },
                ast::MatchArm {
                    pattern: b.pattern(
                        "CoroutineState",
                        state_args,
                        "Complete",
                        Some(returned.clone()),
                    ),
                    body: b.expr(E::Block(vec![
                        b.statement(S::Break(Some(b.ident(&returned)))),
                    ])),
                },
            ],
        });
        vec![
            next,
            resume,
            binding,
            b.statement(S::Expression(b.expr(E::Loop(vec![
                b.statement(S::Expression(advance)),
                b.statement(S::Expression(b.unit())),
            ])))),
        ]
    }
}

struct Builder(ast::SourceSpan);

impl Builder {
    fn expr(&self, kind: E) -> ast::Expr {
        ast::Expr { kind, span: self.0 }
    }

    fn statement(&self, kind: S) -> ast::Statement {
        ast::Statement { kind, span: self.0 }
    }

    fn ident(&self, name: &ast::Ident) -> ast::Expr {
        self.expr(E::Identifier(name.clone()))
    }

    fn unit(&self) -> ast::Expr {
        self.expr(E::TupleLiteral(Vec::new()))
    }

    fn reference(&self, value: ast::Expr) -> ast::Expr {
        self.expr(E::Reference(Box::new(value)))
    }

    fn method(&self, receiver: ast::Expr, method: &str) -> ast::Expr {
        self.expr(E::MethodCall {
            receiver: Box::new(receiver),
            method: method.to_string(),
            type_args: Vec::new(),
            arguments: Vec::new(),
            kwargs: Vec::new(),
        })
    }

    fn call(&self, name: &ast::Ident, argument: ast::Expr) -> ast::Expr {
        self.expr(E::Call {
            function: Box::new(self.ident(name)),
            type_args: Vec::new(),
            arguments: vec![argument],
            kwargs: Vec::new(),
        })
    }

    fn generic(&self, name: &str, type_args: Vec<ast::Type>) -> ast::Type {
        ast::Type::Generic {
            name: ast::DefId::new(ast::STDLIB_FILE, name),
            type_args,
        }
    }

    fn pattern(
        &self,
        name: &str,
        type_args: Vec<ast::Type>,
        variant: &str,
        binding: Option<ast::Ident>,
    ) -> ast::Pattern {
        ast::Pattern::Variant {
            module_path: Vec::new(),
            enum_name: ast::DefId::new(ast::STDLIB_FILE, name),
            type_args,
            variant_name: variant.to_string(),
            binding,
        }
    }

    fn variant(&self, variant: &str, type_args: Vec<ast::Type>, value: ast::Expr) -> ast::Expr {
        self.expr(E::Call {
            function: Box::new(self.expr(E::EnumVariant {
                module_path: Vec::new(),
                enum_name: ast::DefId::new(ast::STDLIB_FILE, "CoroutineState"),
                type_args,
                variant_name: variant.to_string(),
            })),
            type_args: Vec::new(),
            arguments: vec![value],
            kwargs: Vec::new(),
        })
    }

    fn function(
        &self,
        name: &ast::Ident,
        type_params: &[&str],
        parameter: ast::Ident,
        ty: ast::Type,
        is_overlap: bool,
        body: Vec<ast::Statement>,
    ) -> ast::Statement {
        self.statement(S::NestedFunction(ast::FunctionDef {
            associated_type_params: Vec::new(),
            associated_type: None,
            name: name.clone(),
            display_name: name.to_string(),
            type_params: type_params.iter().map(|s| s.to_string()).collect(),
            out_type_params: Vec::new(),
            parameters: vec![ast::Parameter {
                pattern: ast::DestructurePattern::Name(parameter),
                ty,
                default: None,
                span: self.0,
            }],
            return_type: None,
            return_type_span: None,
            body,
            is_pub: false,
            is_unsafe: false,
            inline_hint: false,
            is_extension: false,
            is_overlap,
            doc: None,
            span: self.0,
        }))
    }
}
