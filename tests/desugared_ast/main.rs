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

#[test]
fn for_helpers_have_synthetic_function_names() {
    let surface = parse("fn main() { for value in values {} }");
    assert_eq!(function(&surface.items, 0).name, ast::Ident::user("main"));
    let desugared = solar::desugared_ast::lower(&surface);
    let StatementKind::Expression(ast::Expr {
        kind: ExprKind::Block(body),
        ..
    }) = &function(&desugared.items, 0).body[0].kind
    else {
        panic!("expected desugared loop")
    };
    for statement in &body[..2] {
        let StatementKind::NestedFunction(helper) = &statement.kind else {
            panic!("expected helper")
        };
        assert_eq!(helper.name, ast::Ident::synthetic("for_step_0"));
        assert_ne!(helper.name, ast::Ident::user("for_step_0"));
        assert!(helper.is_overlap);
        let StatementKind::NestedFunction(converter) = &helper.body[0].kind else {
            panic!("expected converter")
        };
        assert_eq!(converter.name, ast::Ident::synthetic("for_state_0"));
    }
}

#[test]
fn synthetic_function_names_remain_distinct_through_lowering() {
    let surface = parse(
        "fn helper() -> Int { 1 } fn generated() -> Bool { true } \
         fn main() { let user: Int = helper(); let synthetic: Bool = generated(); }",
    );
    let mut desugared = solar::desugared_ast::lower(&surface);
    let TopLevelItem::Function(generated) = &mut desugared.items[1] else {
        panic!("expected function")
    };
    generated.name = ast::Ident::synthetic("helper");
    let TopLevelItem::Function(main) = &mut desugared.items[2] else {
        panic!("expected function")
    };
    let StatementKind::Let { value, .. } = &mut main.body[1].kind else {
        panic!("expected binding")
    };
    let ExprKind::Call { function, .. } = &mut value.kind else {
        panic!("expected call")
    };
    function.kind = ExprKind::GlobalRef(ast::DefId::with_ident(0, ast::Ident::synthetic("helper")));
    let typed = solar::typed_ast::lower(&solar::resolved_ast::SourceFile {
        items: desugared.items,
        extension_scopes: Default::default(),
    })
    .unwrap();
    for name in [ast::Ident::user("helper"), ast::Ident::synthetic("helper")] {
        assert!(typed.functions.keys().any(|id| id.def.name == name));
    }
}
