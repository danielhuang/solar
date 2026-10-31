//! Generator layout and recursive storage regressions.

use solar::typed_ast::{Type, TypeId};

#[test]
fn generator_owns_storage_and_has_a_concrete_next_method() {
    let directory = tempdir::TempDir::new("solar-generator-layout").unwrap();
    let path = directory.path().join("main.solar");
    std::fs::write(&path, "fn main() { let captured = 7; let g = generator { let local = captured; yield local; local = local + 1; yield local; }; g.next(); }").unwrap();
    let typed = solar::pipeline::compile(&path).unwrap();
    let generators: Vec<_> = typed
        .typed
        .structs
        .values()
        .filter(|s| s.id.def.name.starts_with("Generator_"))
        .collect();
    assert_eq!(generators.len(), 1);
    let generator = generators[0];
    assert_eq!(
        generator
            .fields
            .iter()
            .find(|f| f.name == "state")
            .unwrap()
            .ty,
        Type::Int
    );
    assert!(
        generator
            .fields
            .iter()
            .all(|f| !matches!(f.ty, Type::Function { .. }))
    );
    let local = generator
        .fields
        .iter()
        .find(|f| f.name.starts_with("local_local_"))
        .unwrap();
    let Type::Struct(storage) = &local.ty else {
        panic!("local storage must be MaybeZeroed");
    };
    assert_eq!(storage.def.name.as_str(), "MaybeZeroed");
    assert_eq!(storage.args, vec![Type::Int]);
    assert_eq!(
        generator
            .fields
            .iter()
            .find(|f| f.name.starts_with("capture_captured_"))
            .unwrap()
            .ty,
        Type::Ref(Box::new(Type::Int))
    );
    let next = typed
        .typed
        .functions
        .values()
        .find(|f| {
            f.id.method
                && f.id.def.name == "next"
                && f.parameters[0].ty == Type::Ref(Box::new(Type::Struct(generator.id.clone())))
        })
        .unwrap();
    assert_eq!(next.parameters.len(), 1);
    assert!(matches!(&next.return_type, Type::Enum(TypeId { args, .. }) if args == &[Type::Int]));
    assert!(!next.body.is_empty());
}

#[test]
fn recursive_generator_storage_must_have_finite_size() {
    let directory = tempdir::TempDir::new("solar-generator-recursion").unwrap();
    let path = directory.path().join("main.solar");
    for source in [
        "fn recursive() { generator { yield 1; let child = recursive(); } } fn main() {}",
        "fn some#[T](value: T) -> Option#[T] { Option#[T]::Some(value) } fn recursive() { generator { yield 1; let child = some(recursive()); } } fn main() {}",
        "fn first() { generator { yield 1; let child = second(); } } fn second() { generator { yield 2; let child = first(); } } fn main() {}",
        "fn recursive#[T](value: T) { generator { yield value; let child = recursive(value); } } fn main() { recursive(1); }",
    ] {
        std::fs::write(&path, source).unwrap();
        let Err((errors, _)) = solar::pipeline::compile(&path) else {
            panic!("accepted infinitely sized generator: {source}");
        };
        fn messages(error: &solar::error::CompileError) -> String {
            format!(
                "{} {}",
                error.message,
                error.caused_by.as_deref().map(messages).unwrap_or_default()
            )
        }
        assert!(
            errors
                .iter()
                .any(|e| messages(e).contains("recursive generator has unbounded size")),
            "{source}: {errors:?}"
        );
    }
}

#[test]
fn generator_fields_are_private_and_cannot_be_reflected() {
    let directory = tempdir::TempDir::new("solar-generator-private").unwrap();
    let path = directory.path().join("main.solar");
    for (operation, expected) in [
        ("g.state = 0;", "field `state` is private"),
        (
            "let (state, storage) = g;",
            "cannot destructure private generator fields",
        ),
        (
            "for.reflect_fields (name, value) in g& {}",
            "cannot reflect fields of opaque struct",
        ),
    ] {
        std::fs::write(
            &path,
            format!("fn main() {{ let g = generator {{ yield 1; }}; {operation} }}"),
        )
        .unwrap();
        let Err((errors, _)) = solar::pipeline::compile(&path) else {
            panic!("exposed generator internals");
        };
        assert!(
            errors.iter().any(|e| e.message.contains(expected)),
            "{errors:?}"
        );
    }
}
