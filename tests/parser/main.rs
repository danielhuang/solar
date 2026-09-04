use solar::ast::*;

fn parse(source: &str) -> SourceFile {
    solar::parser::parse(source).unwrap()
}

#[test]
fn thread_local_static_attribute() {
    let ast = parse("static(thread_local) COUNT: Int = 1;");
    let TopLevelItem::Static(item) = &ast.items[0] else {
        panic!("expected static");
    };
    assert_eq!(item.name, "COUNT");
    assert!(item.thread_local);
}

#[test]
fn ordinary_static_is_not_thread_local() {
    let ast = parse("static COUNT = 1;");
    let TopLevelItem::Static(item) = &ast.items[0] else {
        panic!("expected static");
    };
    assert!(!item.thread_local);
}

#[test]
fn unsafe_declarations_and_blocks_are_preserved() {
    let ast = parse(
        "pub unsafe fn(inline) danger() { unsafe { danger(); } }\n\
         unsafe method touch(value: Int) {}",
    );
    let TopLevelItem::Function(function) = &ast.items[0] else {
        panic!("expected function");
    };
    assert!(function.is_pub);
    assert!(function.is_unsafe);
    assert!(function.inline_hint);
    let StatementKind::Expression(Expr {
        kind: ExprKind::UnsafeBlock(body),
        ..
    }) = &function.body[0].kind
    else {
        panic!("expected unsafe block");
    };
    assert_eq!(body.len(), 1);

    let TopLevelItem::Method(method) = &ast.items[1] else {
        panic!("expected method");
    };
    assert!(method.is_unsafe);
}

#[test]
fn function_output_type_parameters_are_preserved() {
    let ast = parse("fn test#[T, out U, out V](x: T) -> U { x }\n");
    let TopLevelItem::Function(function) = &ast.items[0] else {
        panic!("expected function");
    };
    assert_eq!(function.type_params, ["T"]);
    assert_eq!(function.out_type_params, ["U", "V"]);
}

#[test]
fn output_type_parameters_must_follow_inferred_parameters() {
    assert!(solar::parser::parse("fn bad#[out T, U](x: U) {}\n").is_err());
}

#[test]
fn struct_fields_and_enum_variants_do_not_require_trailing_commas() {
    let ast = parse("struct Pair { left: Int, right: Int }\nenum Maybe { None, Some(Int) }");
    let TopLevelItem::Struct(pair) = &ast.items[0] else {
        panic!("expected struct");
    };
    assert_eq!(pair.fields.len(), 2);
    let TopLevelItem::Enum(maybe) = &ast.items[1] else {
        panic!("expected enum");
    };
    assert_eq!(maybe.variants.len(), 2);
}

#[test]
fn struct_c_repr_attribute_is_preserved() {
    let ast = parse(
        "struct(repr(C)) Header { tag: Uint8, length: Uint64 }\n\
         struct Ordinary { value: Int }",
    );
    let TopLevelItem::Struct(header) = &ast.items[0] else {
        panic!("expected struct");
    };
    assert!(header.repr_c);
    let TopLevelItem::Struct(ordinary) = &ast.items[1] else {
        panic!("expected struct");
    };
    assert!(!ordinary.repr_c);
}

/// Helper to check a span matches expected 0-indexed positions.
fn check(span: SourceSpan, start_line: u32, start_col: u32, end_line: u32, end_col: u32) {
    assert_eq!(
        (span.start.line, span.start.col, span.end.line, span.end.col),
        (start_line, start_col, end_line, end_col),
        "span mismatch"
    );
}

// ---------- Statement spans ----------

#[test]
fn let_statement_span() {
    let ast = parse("fn f() {\n    let x: Int = 42;\n}");
    let func = match &ast.items[0] {
        TopLevelItem::Function(f) => f,
        _ => panic!("expected function"),
    };
    let stmt = &func.body[0];
    // "let x: Int = 42;" = line 1, col 4..20
    check(stmt.span, 1, 4, 1, 20);
}

#[test]
fn assignment_statement_span() {
    let ast = parse("fn f() {\n    let x: Int = 0;\n    x = 42;\n}");
    let func = match &ast.items[0] {
        TopLevelItem::Function(f) => f,
        _ => panic!("expected function"),
    };
    let stmt = &func.body[1]; // x = 42;
    // "x = 42;" = line 2, col 4..11
    check(stmt.span, 2, 4, 2, 11);
}

#[test]
fn return_statement_span() {
    let ast = parse("fn f() -> Int {\n    return 5;\n}");
    let func = match &ast.items[0] {
        TopLevelItem::Function(f) => f,
        _ => panic!("expected function"),
    };
    let stmt = &func.body[0];
    // "return 5;" = line 1, col 4..13
    check(stmt.span, 1, 4, 1, 13);
}

#[test]
fn if_statement_span() {
    let src = "fn f() {\n    if true {\n    } else {\n    }\n}";
    let ast = parse(src);
    let func = match &ast.items[0] {
        TopLevelItem::Function(f) => f,
        _ => panic!("expected function"),
    };
    let stmt = &func.body[0];
    // "if" starts at line 1, col 4; closing "}" of else at line 3, col 5
    check(stmt.span, 1, 4, 3, 5);
}

#[test]
fn while_statement_span() {
    let src = "fn f() {\n    while true {\n    }\n}";
    let ast = parse(src);
    let func = match &ast.items[0] {
        TopLevelItem::Function(f) => f,
        _ => panic!("expected function"),
    };
    let stmt = &func.body[0];
    // "while" at line 1 col 4, closing "}" at line 2 col 5
    check(stmt.span, 1, 4, 2, 5);
}

#[test]
fn expression_statement_span() {
    let ast = parse("fn f() {\n    foo();\n}");
    let func = match &ast.items[0] {
        TopLevelItem::Function(f) => f,
        _ => panic!("expected function"),
    };
    let stmt = &func.body[0];
    // "foo();" = line 1, col 4..10
    check(stmt.span, 1, 4, 1, 10);
}

// ---------- Expression spans ----------

#[test]
fn integer_literal_span() {
    let ast = parse("fn f() {\n    123\n}");
    let func = match &ast.items[0] {
        TopLevelItem::Function(f) => f,
        _ => panic!("expected function"),
    };
    // tail expression becomes expression statement
    let expr = match &func.body[0].kind {
        StatementKind::Expression(e) => e,
        _ => panic!("expected expression"),
    };
    check(expr.span, 1, 4, 1, 7);
}

#[test]
fn identifier_span() {
    let ast = parse("fn f() {\n    xyz\n}");
    let func = match &ast.items[0] {
        TopLevelItem::Function(f) => f,
        _ => panic!("expected function"),
    };
    let expr = match &func.body[0].kind {
        StatementKind::Expression(e) => e,
        _ => panic!("expected expression"),
    };
    check(expr.span, 1, 4, 1, 7);
}

#[test]
fn boolean_literal_span() {
    let ast = parse("fn f() {\n    true\n}");
    let func = match &ast.items[0] {
        TopLevelItem::Function(f) => f,
        _ => panic!("expected function"),
    };
    let expr = match &func.body[0].kind {
        StatementKind::Expression(e) => e,
        _ => panic!("expected expression"),
    };
    check(expr.span, 1, 4, 1, 8);
}

#[test]
fn binary_op_span() {
    let ast = parse("fn f() {\n    1 + 2\n}");
    let func = match &ast.items[0] {
        TopLevelItem::Function(f) => f,
        _ => panic!("expected function"),
    };
    let expr = match &func.body[0].kind {
        StatementKind::Expression(e) => e,
        _ => panic!("expected expression"),
    };
    // "1 + 2" = col 4..9
    check(expr.span, 1, 4, 1, 9);
}

#[test]
fn call_expr_span() {
    let ast = parse("fn f() {\n    foo(1, 2)\n}");
    let func = match &ast.items[0] {
        TopLevelItem::Function(f) => f,
        _ => panic!("expected function"),
    };
    let expr = match &func.body[0].kind {
        StatementKind::Expression(e) => e,
        _ => panic!("expected expression"),
    };
    // "foo(1, 2)" = col 4..13
    check(expr.span, 1, 4, 1, 13);
}

#[test]
fn field_access_span() {
    let ast = parse("fn f() {\n    a.b\n}");
    let func = match &ast.items[0] {
        TopLevelItem::Function(f) => f,
        _ => panic!("expected function"),
    };
    let expr = match &func.body[0].kind {
        StatementKind::Expression(e) => e,
        _ => panic!("expected expression"),
    };
    // "a.b" = col 4..7
    check(expr.span, 1, 4, 1, 7);
}

#[test]
fn reference_expr_span() {
    // Reference is postfix in Solar: "x&"
    let ast = parse("fn f() {\n    x&\n}");
    let func = match &ast.items[0] {
        TopLevelItem::Function(f) => f,
        _ => panic!("expected function"),
    };
    let expr = match &func.body[0].kind {
        StatementKind::Expression(e) => e,
        _ => panic!("expected expression"),
    };
    // "x&" = col 4..6
    check(expr.span, 1, 4, 1, 6);
}

#[test]
fn struct_literal_span() {
    let ast = parse("struct S {\n  x: Int,\n}\nfn f() {\n    let s: S = S { x: 1 };\n}");
    let func = match &ast.items[1] {
        TopLevelItem::Function(f) => f,
        _ => panic!("expected function"),
    };
    let value = match &func.body[0].kind {
        StatementKind::Let { value, .. } => value,
        _ => panic!("expected let"),
    };
    // "S { x: 1 }" = line 4, col 15..25
    check(value.span, 4, 15, 4, 25);
}

#[test]
fn tuple_struct_surface_syntax_preserves_numeric_fields() {
    let ast = parse("struct Pair(Int, Float64);\nfn f() {\n    let p = Pair(1, 1.0f);\n    p.0\n}");
    let pair = match &ast.items[0] {
        TopLevelItem::Struct(pair) => pair,
        _ => panic!("expected struct"),
    };
    assert!(pair.is_tuple);
    assert_eq!(
        pair.fields.iter().map(|f| &f.name).collect::<Vec<_>>(),
        ["0", "1"]
    );
    let func = match &ast.items[1] {
        TopLevelItem::Function(func) => func,
        _ => panic!("expected function"),
    };
    let value = match &func.body[0].kind {
        StatementKind::Let { value, .. } => value,
        _ => panic!("expected let"),
    };
    let ExprKind::Call { arguments, .. } = &value.kind else {
        panic!("expected constructor call");
    };
    assert!(matches!(arguments[1].kind, ExprKind::FloatLiteral(_, _)));
    let StatementKind::Expression(access) = &func.body[1].kind else {
        panic!("expected field access");
    };
    assert!(matches!(&access.kind, ExprKind::FieldAccess { field, .. } if field == "0"));
}

#[test]
fn array_literal_span() {
    let ast = parse("fn f() {\n    [1, 2, 3]\n}");
    let func = match &ast.items[0] {
        TopLevelItem::Function(f) => f,
        _ => panic!("expected function"),
    };
    let expr = match &func.body[0].kind {
        StatementKind::Expression(e) => e,
        _ => panic!("expected expression"),
    };
    // "[1, 2, 3]" = col 4..13
    check(expr.span, 1, 4, 1, 13);
}

#[test]
fn index_expr_span() {
    let ast = parse("fn f() {\n    a[0]\n}");
    let func = match &ast.items[0] {
        TopLevelItem::Function(f) => f,
        _ => panic!("expected function"),
    };
    let expr = match &func.body[0].kind {
        StatementKind::Expression(e) => e,
        _ => panic!("expected expression"),
    };
    // "a[0]" = col 4..8
    check(expr.span, 1, 4, 1, 8);
}

#[test]
fn if_expr_span() {
    // Use if-expression in value position (after =) to ensure it parses as expression
    let ast = parse("fn f() {\n    let x: Int = if true { 1 } else { 2 };\n}");
    let func = match &ast.items[0] {
        TopLevelItem::Function(f) => f,
        _ => panic!("expected function"),
    };
    let expr = match &func.body[0].kind {
        StatementKind::Let { value, .. } => value,
        _ => panic!("expected let"),
    };
    // "if true { 1 } else { 2 }" = line 1, col 17..41
    check(expr.span, 1, 17, 1, 41);
}

#[test]
fn block_expr_span() {
    let ast = parse("fn f() {\n    { 42 }\n}");
    let func = match &ast.items[0] {
        TopLevelItem::Function(f) => f,
        _ => panic!("expected function"),
    };
    let expr = match &func.body[0].kind {
        StatementKind::Expression(e) => e,
        _ => panic!("expected expression"),
    };
    // "{ 42 }" = col 4..10
    check(expr.span, 1, 4, 1, 10);
}

#[test]
fn deref_expr_span() {
    let ast = parse("fn f() {\n    x@\n}");
    let func = match &ast.items[0] {
        TopLevelItem::Function(f) => f,
        _ => panic!("expected function"),
    };
    let expr = match &func.body[0].kind {
        StatementKind::Expression(e) => e,
        _ => panic!("expected expression"),
    };
    // "x@" = col 4..6
    check(expr.span, 1, 4, 1, 6);
}

#[test]
fn parenthesized_expr_span() {
    // Parenthesized expression unwraps to inner expr, so span is the inner expr
    let ast = parse("fn f() {\n    (42)\n}");
    let func = match &ast.items[0] {
        TopLevelItem::Function(f) => f,
        _ => panic!("expected function"),
    };
    let expr = match &func.body[0].kind {
        StatementKind::Expression(e) => e,
        _ => panic!("expected expression"),
    };
    // Parser unwraps parens: inner "42" = col 5..7
    check(expr.span, 1, 5, 1, 7);
}

#[test]
fn string_literal_span() {
    let ast = parse("fn f() {\n    \"hello\"\n}");
    let func = match &ast.items[0] {
        TopLevelItem::Function(f) => f,
        _ => panic!("expected function"),
    };
    let expr = match &func.body[0].kind {
        StatementKind::Expression(e) => e,
        _ => panic!("expected expression"),
    };
    assert!(matches!(&expr.kind, ExprKind::StringLiteral(bytes) if bytes == b"hello"));
    // "\"hello\"" = col 4..11
    check(expr.span, 1, 4, 1, 11);
}

#[test]
fn tuple_literal_span() {
    let ast = parse("fn f() {\n    (1, 2)\n}");
    let func = match &ast.items[0] {
        TopLevelItem::Function(f) => f,
        _ => panic!("expected function"),
    };
    let expr = match &func.body[0].kind {
        StatementKind::Expression(e) => e,
        _ => panic!("expected expression"),
    };
    // "(1, 2)" = col 4..10
    check(expr.span, 1, 4, 1, 10);
}

// ---------- Closure span ----------

#[test]
fn closure_expr_span() {
    // Closures use backslash syntax: \param: Type -> RetType { body }
    let ast = parse("fn f() {\n    \\x: Int -> Int { x }\n}");
    let func = match &ast.items[0] {
        TopLevelItem::Function(f) => f,
        _ => panic!("expected function"),
    };
    let expr = match &func.body[0].kind {
        StatementKind::Expression(e) => e,
        _ => panic!("expected expression"),
    };
    // "\x: Int -> Int { x }" = col 4..24
    check(expr.span, 1, 4, 1, 24);
}

// ---------- Method call span ----------

#[test]
fn method_call_span() {
    let ast = parse("fn f() {\n    a.foo(1)\n}");
    let func = match &ast.items[0] {
        TopLevelItem::Function(f) => f,
        _ => panic!("expected function"),
    };
    let expr = match &func.body[0].kind {
        StatementKind::Expression(e) => e,
        _ => panic!("expected expression"),
    };
    // "a.foo(1)" = col 4..12
    check(expr.span, 1, 4, 1, 12);
}

// ---------- Match expression span ----------

#[test]
fn match_expr_span() {
    let src = "enum E {\n    A,\n    B,\n}\nfn f() {\n    match E::A {\n        E::A => 1,\n        E::B => 2,\n    }\n}";
    let ast = parse(src);
    let func = match &ast.items[1] {
        TopLevelItem::Function(f) => f,
        _ => panic!("expected function"),
    };
    let expr = match &func.body[0].kind {
        StatementKind::Expression(e) => e,
        _ => panic!("expected expression"),
    };
    // "match E::A { ... }" starts at line 5 col 4, closing "}" at line 8 col 5
    check(expr.span, 5, 4, 8, 5);
}

// ---------- Nested expression spans ----------

#[test]
fn nested_binary_op_spans() {
    // "1 + 2 * 3" parses as "1 + (2 * 3)"
    let ast = parse("fn f() {\n    1 + 2 * 3\n}");
    let func = match &ast.items[0] {
        TopLevelItem::Function(f) => f,
        _ => panic!("expected function"),
    };
    let outer = match &func.body[0].kind {
        StatementKind::Expression(e) => e,
        _ => panic!("expected expression"),
    };
    // Whole "1 + 2 * 3" = col 4..13
    check(outer.span, 1, 4, 1, 13);

    // Right operand "2 * 3" = col 8..13
    let rhs = match &outer.kind {
        ExprKind::BinaryOp { right, .. } => right,
        _ => panic!("expected binop"),
    };
    check(rhs.span, 1, 8, 1, 13);

    // Left operand "1" = col 4..5
    let lhs = match &outer.kind {
        ExprKind::BinaryOp { left, .. } => left,
        _ => panic!("expected binop"),
    };
    check(lhs.span, 1, 4, 1, 5);
}

// ---------- Multi-line fixture file ----------

#[test]
fn fixture_file_spans() {
    let source = include_str!("spans.solar");
    let ast = parse(source);

    // First item: fn add(a: Int, b: Int) -> Int { ... }
    let add_fn = match &ast.items[0] {
        TopLevelItem::Function(f) => f,
        _ => panic!("expected function"),
    };
    assert_eq!(add_fn.name, "add");

    // Body: "let x: Int = a + b;" on the second line
    let let_stmt = &add_fn.body[0];
    check(let_stmt.span, 1, 1, 1, 20);

    // The value expr "a + b" inside the let
    let let_value = match &let_stmt.kind {
        StatementKind::Let { value, .. } => value,
        _ => panic!("expected let"),
    };
    check(let_value.span, 1, 14, 1, 19);

    // "return x;" on the third line
    let ret_stmt = &add_fn.body[1];
    check(ret_stmt.span, 2, 1, 2, 10);

    // Second item: fn main() { ... }
    let main_fn = match &ast.items[1] {
        TopLevelItem::Function(f) => f,
        _ => panic!("expected function"),
    };
    assert_eq!(main_fn.name, "main");

    // "let y: Int = add(1, 2);" on the seventh line
    let let_stmt2 = &main_fn.body[0];
    check(let_stmt2.span, 6, 1, 6, 24);

    // The call expr "add(1, 2)"
    let call_expr = match &let_stmt2.kind {
        StatementKind::Let { value, .. } => value,
        _ => panic!("expected let"),
    };
    check(call_expr.span, 6, 14, 6, 23);

    // "print(y);" on the eighth line
    let print_stmt = &main_fn.body[1];
    check(print_stmt.span, 7, 1, 7, 10);
}

// ---------- Doc comments (`///`) ----------

#[test]
fn doc_comment_on_function() {
    let ast = parse("/// Returns x.\nfn f(x: Int) -> Int { x }");
    let func = match &ast.items[0] {
        TopLevelItem::Function(f) => f,
        _ => panic!("expected function"),
    };
    assert_eq!(func.doc.as_deref(), Some("Returns x."));
}

#[test]
fn multi_line_doc_comment_joins() {
    let ast = parse("/// First line.\n/// Second line.\nstruct S { x: Int, }");
    let s = match &ast.items[0] {
        TopLevelItem::Struct(s) => s,
        _ => panic!("expected struct"),
    };
    assert_eq!(s.doc.as_deref(), Some("First line.\nSecond line."));
}

#[test]
fn plain_comment_is_not_a_doc() {
    let ast = parse("// not a doc\nfn f() {}");
    let func = match &ast.items[0] {
        TopLevelItem::Function(f) => f,
        _ => panic!("expected function"),
    };
    assert_eq!(func.doc, None);
}

#[test]
fn doc_comment_on_enum_and_const() {
    let ast = parse("/// An enum.\nenum E { A, }\n/// A const.\nconst K: Int = 1;");
    match &ast.items[0] {
        TopLevelItem::Enum(e) => assert_eq!(e.doc.as_deref(), Some("An enum.")),
        _ => panic!("expected enum"),
    }
    match &ast.items[1] {
        TopLevelItem::Const(c) => assert_eq!(c.doc.as_deref(), Some("A const.")),
        _ => panic!("expected const"),
    }
}

#[test]
fn doc_comment_empty_body() {
    // `///` with nothing after it is a valid, empty doc line.
    let ast = parse("///\nfn f() {}");
    let func = match &ast.items[0] {
        TopLevelItem::Function(f) => f,
        _ => panic!("expected function"),
    };
    assert_eq!(func.doc.as_deref(), Some(""));
}

#[test]
fn associated_function_names_are_preserved() {
    let ast = parse("fn Value::make() {} fn Value::() {}");
    let functions = ast.items.iter().map(|item| match item {
        TopLevelItem::Function(function) => function,
        _ => panic!("expected function"),
    });
    let names = functions
        .map(|function| {
            let Type::Named(owner) = function.associated_type.as_ref().unwrap() else {
                panic!("expected named owner")
            };
            (owner.name.as_str(), function.name.as_str())
        })
        .collect::<Vec<_>>();
    assert_eq!(names, [("Value", "make"), ("Value", "")]);
}

#[test]
fn associated_owner_and_function_generics_are_separate() {
    let ast = parse("fn#[T] Generic#[T]::identity#[U](value: U) -> U { value }");
    let TopLevelItem::Function(function) = &ast.items[0] else {
        panic!("expected function");
    };
    assert_eq!(function.associated_type_params, ["T"]);
    assert_eq!(function.type_params, ["U"]);
    let Type::Generic { name, type_args } = function.associated_type.as_ref().unwrap() else {
        panic!("expected generic owner");
    };
    assert_eq!(name.name, "Generic");
    assert_eq!(type_args.len(), 1);
    assert_eq!(function.name, "identity");
}
