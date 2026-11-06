//! Coroutine layout and recursive storage regressions.

use solar::typed_ast::{Type, TypeId};

#[test]
fn awaited_coroutines_are_stored_inline_in_the_parent_frame() {
    let directory = tempdir::TempDir::new("solar-inline-await").unwrap();
    let path = directory.path().join("main.solar");
    std::fs::write(
        &path,
        r#"
fn child() { coroutine { yield 1; 2 } }
fn parent() { coroutine { await child() } }
fn outer() { coroutine { await parent() } }
fn main() { let g = outer(); g.resume(); }
"#,
    )
    .unwrap();
    let typed = solar::pipeline::compile(&path).unwrap().typed;
    let frame = |name: &str| {
        let function = typed
            .functions
            .values()
            .find(|f| f.id.def.name == name)
            .unwrap();
        let Type::Struct(id) = &function.return_type else {
            panic!("expected coroutine")
        };
        &typed.structs[id]
    };
    for (parent, child) in [("parent", "child"), ("outer", "parent")] {
        let parent = frame(parent);
        let child = Type::Struct(frame(child).id.clone());
        let fields: Vec<_> = parent
            .fields
            .iter()
            .filter(|f| f.name.starts_with("await_"))
            .collect();
        assert_eq!(fields.len(), 1, "{:?}", parent.fields);
        let Type::Struct(storage) = &fields[0].ty else {
            panic!("expected inline storage")
        };
        assert_eq!(storage.def.name, "MaybeZeroed");
        assert_eq!(storage.args, vec![child.clone()]);
        assert_eq!(typed.structs[storage].fields[0].ty, child);
    }
}

#[test]
fn recursive_awaits_are_rejected_before_frame_layout() {
    let directory = tempdir::TempDir::new("solar-recursive-await").unwrap();
    let path = directory.path().join("main.solar");
    for source in [
        "fn recursive(n: Int) { coroutine { if n == 0 { return 0; } yield n; n + await recursive(n - 1) } } fn main() {}",
        "fn recursive() { coroutine { await recursive() } } fn main() {}",
        "fn first() { coroutine { await second() } } fn second() { coroutine { await first() } } fn main() {}",
        "fn recursive#[T](value: T) { coroutine { yield value; await recursive(value) } } fn main() { recursive(1); }",
        "fn recursive() { coroutine { await coroutine { await recursive() } } } fn main() {}",
    ] {
        std::fs::write(&path, source).unwrap();
        let Err((errors, _)) = solar::pipeline::compile(&path) else {
            panic!("accepted recursive await: {source}");
        };
        fn recursive_await(error: &solar::error::CompileError) -> bool {
            error.message.contains("recursive await is not allowed")
                || error.caused_by.as_deref().is_some_and(recursive_await)
        }
        assert!(errors.iter().any(recursive_await), "{source}: {errors:?}");
    }
}

#[test]
fn coroutine_loop_keeps_short_lived_temporaries_out_of_the_frame() {
    let directory = tempdir::TempDir::new("solar-coroutine-local-temps").unwrap();
    let path = directory.path().join("main.solar");
    std::fs::write(
        &path,
        "fn looper(i: &Int) { coroutine { while i@ < 1000 { if i@ % 10 == 0 { yield i@; } i@ = i@ + 1; } } } fn main() { let i = 0; looper(i&).next(); }",
    )
    .unwrap();
    let typed = solar::pipeline::compile(&path).unwrap();
    let coroutine = typed
        .typed
        .structs
        .values()
        .find(|s| s.id.def.name.starts_with("Coroutine_"))
        .unwrap();
    // Neither loop arithmetic nor the assignment address lives across a yield.
    // The only persistent data is the resume state and the captured binding.
    assert_eq!(coroutine.fields.len(), 2, "{:?}", coroutine.fields);
    assert_eq!(coroutine.fields[0].ty, Type::Int);
    assert_eq!(
        coroutine.fields[1].ty,
        Type::Ref(Box::new(Type::Ref(Box::new(Type::Int))))
    );
}

#[test]
fn coroutine_owns_storage_and_has_concrete_resume_and_next_methods() {
    let directory = tempdir::TempDir::new("solar-coroutine-layout").unwrap();
    let path = directory.path().join("main.solar");
    std::fs::write(&path, "fn main() { let captured = 7; let g = coroutine { let local = captured; yield local; local = local + 1; yield local; }; g.next(); }").unwrap();
    let typed = solar::pipeline::compile(&path).unwrap();
    let coroutines: Vec<_> = typed
        .typed
        .structs
        .values()
        .filter(|s| s.id.def.name.starts_with("Coroutine_"))
        .collect();
    assert_eq!(coroutines.len(), 1);
    let coroutine = coroutines[0];
    assert_eq!(
        coroutine
            .fields
            .iter()
            .find(|f| f.name == "state")
            .unwrap()
            .ty,
        Type::Int
    );
    assert!(
        coroutine
            .fields
            .iter()
            .all(|f| !matches!(f.ty, Type::Function { .. }))
    );
    let local = coroutine
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
        coroutine
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
                && f.parameters[0].ty == Type::Ref(Box::new(Type::Struct(coroutine.id.clone())))
        })
        .unwrap();
    assert_eq!(next.parameters.len(), 1);
    assert!(matches!(&next.return_type, Type::Enum(TypeId { args, .. }) if args == &[Type::Int]));
    assert!(!next.body.is_empty());
    let resume = typed
        .typed
        .functions
        .values()
        .find(|f| {
            f.id.method && f.id.def.name == "resume" && f.parameters[0].ty == next.parameters[0].ty
        })
        .unwrap();
    assert_eq!(resume.parameters.len(), 1);
    assert!(
        matches!(&resume.return_type, Type::Enum(TypeId { def, args }) if def.name == "CoroutineState" && args == &[Type::Int, Type::Unit])
    );
    assert!(!resume.body.is_empty());
}

#[test]
fn recursive_coroutine_storage_must_have_finite_size() {
    let directory = tempdir::TempDir::new("solar-coroutine-recursion").unwrap();
    let path = directory.path().join("main.solar");
    for source in [
        "fn recursive() { coroutine { yield 1; let child = recursive(); } } fn main() {}",
        "fn some#[T](value: T) -> Option#[T] { Option#[T]::Some(value) } fn recursive() { coroutine { yield 1; let child = some(recursive()); } } fn main() {}",
        "fn first() { coroutine { yield 1; let child = second(); } } fn second() { coroutine { yield 2; let child = first(); } } fn main() {}",
        "fn recursive#[T](value: T) { coroutine { yield value; let child = recursive(value); } } fn main() { recursive(1); }",
    ] {
        std::fs::write(&path, source).unwrap();
        let Err((errors, _)) = solar::pipeline::compile(&path) else {
            panic!("accepted infinitely sized coroutine: {source}");
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
                .any(|e| messages(e).contains("recursive coroutine has unbounded size")),
            "{source}: {errors:?}"
        );
    }
}

#[test]
fn coroutine_fields_are_private_and_cannot_be_reflected() {
    let directory = tempdir::TempDir::new("solar-coroutine-private").unwrap();
    let path = directory.path().join("main.solar");
    for (operation, expected) in [
        ("g.state = 0;", "field `state` is private"),
        (
            "let (state, storage) = g;",
            "cannot destructure private coroutine fields",
        ),
        (
            "for.reflect_fields (name, value) in g& {}",
            "cannot reflect fields of opaque struct",
        ),
    ] {
        std::fs::write(
            &path,
            format!("fn main() {{ let g = coroutine {{ yield 1; }}; {operation} }}"),
        )
        .unwrap();
        let Err((errors, _)) = solar::pipeline::compile(&path) else {
            panic!("exposed coroutine internals");
        };
        assert!(
            errors.iter().any(|e| e.message.contains(expected)),
            "{errors:?}"
        );
    }
}
