use std::path::Path;

use solar::ast::{self, TopLevelItem};

#[test]
fn numeric_constructors_are_added_once_to_resolver_output() {
    let source = "fn main() { Int(1u); }".to_string();
    let (resolved, _) =
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
}
