use std::path::Path;

use solar::ast::{self, ExprKind, StatementKind, TopLevelItem};

#[test]
fn numeric_constructors_are_added_once_to_resolver_output() {
    let source = "fn main() { Int(1u); }".to_string();
    let (resolved, source_map) =
        solar::resolve::resolve_source(Path::new("numeric_constructor.solar"), source).unwrap();

    let constructors = resolved
        .items
        .iter()
        .filter(|item| {
            matches!(
                item,
                TopLevelItem::Function(function)
                    if function.span.file_id == ast::SYNTHETIC_FILE
            )
        })
        .count();
    assert_eq!(constructors, 12 * 11);

    let root_file = source_map.root_file_id().unwrap();
    let main = resolved
        .items
        .iter()
        .find_map(|item| match item {
            TopLevelItem::Function(function)
                if function.name == "main" && function.span.file_id == root_file =>
            {
                Some(function)
            }
            _ => None,
        })
        .unwrap();
    assert!(matches!(
        &main.body[0].kind,
        StatementKind::Expression(ast::Expr {
            kind: ExprKind::Call { function, .. },
            ..
        }) if matches!(&function.kind, ExprKind::Identifier(name) if name == "Int")
    ));
}
