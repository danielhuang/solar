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
fn for_in_becomes_indexed_while_loop() {
    let surface =
        parse("fn main() { let values = [1, 2]; for value in values { println(value); } }");
    assert!(matches!(
        function(&surface.items, 0).body[1].kind,
        StatementKind::ForIn { .. }
    ));

    let desugared = solar::desugared_ast::lower(&surface);
    let body = &function(&desugared.items, 0).body;
    assert_eq!(body.len(), 5);
    assert!(matches!(
        &body[1].kind,
        StatementKind::Let {
            pattern: ast::DestructurePattern::Name(name),
            ..
        } if matches!(name, ast::Ident::Synthetic(name) if name == "__for_arr_0")
    ));
    assert!(matches!(
        &body[2].kind,
        StatementKind::Let {
            value: ast::Expr {
                kind: ExprKind::IntrinsicCall {
                    intrinsic: ast::Intrinsic::ArrayLen,
                    ..
                },
                ..
            },
            ..
        }
    ));
    let StatementKind::While {
        body: loop_body, ..
    } = &body[4].kind
    else {
        panic!("expected while loop");
    };
    assert!(matches!(
        &loop_body[0].kind,
        StatementKind::Let {
            pattern: ast::DestructurePattern::Name(name),
            value: ast::Expr {
                kind: ExprKind::Index { .. },
                ..
            },
            ..
        } if matches!(name, ast::Ident::User(name) if name == "value")
    ));
    assert!(matches!(
        loop_body[1].kind,
        StatementKind::Assignment { .. }
    ));
}
