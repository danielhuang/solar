use solar::ast::{self, ExprKind, StatementKind, TopLevelItem};

fn parse(source: &str) -> ast::SourceFile {
    solar::parser::parse(source).unwrap()
}

fn function(items: &[TopLevelItem], index: usize) -> &ast::FunctionDef {
    let TopLevelItem::Function(function) = &items[index] else {
        panic!("expected function");
    };
    function
}

#[test]
fn literals_and_bare_return_are_normalized() {
    let surface = parse("fn main() { \"A\"; 'B'; return; }");
    let surface_main = function(&surface.items, 0);
    assert!(matches!(
        &surface_main.body[0].kind,
        StatementKind::Expression(ast::Expr {
            kind: ExprKind::StringLiteral(bytes),
            ..
        }) if bytes == b"A"
    ));
    assert!(matches!(
        &surface_main.body[1].kind,
        StatementKind::Expression(ast::Expr {
            kind: ExprKind::CharLiteral(b'B'),
            ..
        })
    ));
    assert!(matches!(
        surface_main.body[2].kind,
        StatementKind::ReturnVoid
    ));

    let desugared = solar::desugared_ast::lower(&surface);
    let main = function(&desugared.items, 0);
    assert!(matches!(
        &main.body[0].kind,
        StatementKind::Expression(ast::Expr {
            kind: ExprKind::ArrayLiteral(bytes, Some(ast::Type::Named(ty))),
            ..
        }) if ty.name == "Uint8"
            && matches!(&bytes[..], [ast::Expr {
                kind: ExprKind::IntegerLiteral(65, ast::IntegerType::Uint8),
                ..
            }])
    ));
    assert!(matches!(
        &main.body[1].kind,
        StatementKind::Expression(ast::Expr {
            kind: ExprKind::IntegerLiteral(66, ast::IntegerType::Uint8),
            ..
        })
    ));
    assert!(matches!(
        &main.body[2].kind,
        StatementKind::Return(ast::Expr {
            kind: ExprKind::Block(body),
            ..
        }) if body.is_empty()
    ));
}

#[test]
fn try_catch_is_preserved_for_typed_lowering() {
    let surface = parse("fn main() { try { throw(\"x\"&); } catch (e) { println(e); } }");
    assert!(matches!(
        function(&surface.items, 0).body[0].kind,
        StatementKind::Try { .. }
    ));

    let desugared = solar::desugared_ast::lower(&surface);
    let StatementKind::Try {
        body,
        binding,
        binding_type,
        handler,
    } = &function(&desugared.items, 0).body[0].kind
    else {
        panic!("expected try statement");
    };
    assert!(matches!(binding, ast::Ident::User(name) if name == "e"));
    assert!(binding_type.is_none());
    assert_eq!(body.len(), 1);
    assert_eq!(handler.len(), 1);
}

#[test]
fn tuple_struct_fields_and_access_are_normalized() {
    let surface = parse("struct Pair(Int, Int); fn first(p: Pair) -> Int { p.0 }");
    let TopLevelItem::Struct(pair) = &surface.items[0] else {
        panic!("expected struct");
    };
    assert_eq!(pair.fields[0].name, "0");

    let desugared = solar::desugared_ast::lower(&surface);
    let TopLevelItem::Struct(pair) = &desugared.items[0] else {
        panic!("expected struct");
    };
    assert_eq!(pair.fields[0].name, "_0");
    assert!(matches!(
        &function(&desugared.items, 1).body[0].kind,
        StatementKind::Expression(ast::Expr {
            kind: ExprKind::FieldAccess { field, .. },
            ..
        }) if field == "_0"
    ));
}

#[test]
fn numeric_constructor_syntax_remains_a_normal_call() {
    let surface = parse("fn main() { Int(1u); }");
    assert_eq!(surface.items.len(), 1);
    let desugared = solar::desugared_ast::lower(&surface);
    assert_eq!(desugared.items.len(), 1);
    assert!(matches!(
        &function(&desugared.items, 0).body[0].kind,
        StatementKind::Expression(ast::Expr {
            kind: ExprKind::Call { function, .. },
            ..
        }) if matches!(&function.kind, ExprKind::Identifier(ast::Ident::User(name)) if name == "Int")
    ));
}

#[test]
fn for_in_becomes_duck_typed_iterator_loop() {
    let surface =
        parse("fn main() { let values = [1, 2]; for value in values { println(value); } }");
    assert!(matches!(
        function(&surface.items, 0).body[1].kind,
        StatementKind::ForIn { .. }
    ));

    let desugared = solar::desugared_ast::lower(&surface);
    let body = &function(&desugared.items, 0).body;
    assert_eq!(body.len(), 3);
    assert!(matches!(
        &body[1].kind,
        StatementKind::Let {
            pattern: ast::DestructurePattern::Name(name),
            value: ast::Expr {
                kind: ExprKind::MethodCall {
                    method,
                    receiver,
                    ..
                },
                ..
            },
            ..
        } if matches!(name, ast::Ident::Synthetic(name) if name == "__for_iter_0")
            && method == "iter"
            && matches!(receiver.kind, ExprKind::Reference(_))
    ));
    let StatementKind::Expression(ast::Expr {
        kind: ExprKind::Loop(loop_body),
        ..
    }) = &body[2].kind
    else {
        panic!("expected loop expression");
    };
    let StatementKind::Expression(ast::Expr {
        kind: ExprKind::Match { scrutinee, arms },
        ..
    }) = &loop_body[0].kind
    else {
        panic!("expected match expression");
    };
    assert!(matches!(
        &scrutinee.kind,
        ExprKind::MethodCall {
            method,
            receiver,
            ..
        } if method == "next" && matches!(receiver.kind, ExprKind::Reference(_))
    ));
    assert_eq!(arms.len(), 2);
    assert!(matches!(
        &arms[0].pattern,
        ast::Pattern::Variant {
            variant_name,
            binding: Some(ast::Ident::User(name)),
            ..
        } if variant_name == "Some" && name == "value"
    ));
    assert!(matches!(
        &arms[1].pattern,
        ast::Pattern::Variant {
            variant_name,
            binding: None,
            ..
        } if variant_name == "None"
    ));
}
