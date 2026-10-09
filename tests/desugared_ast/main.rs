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
