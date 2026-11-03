use std::path::Path;

#[test]
fn imported_constants_and_structs_across_backends() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/c_import/main.solar");
    assert_eq!(test_utils::run(&path, "c_import"), "passed\n");
    assert_eq!(
        test_utils::run_release_file(&path, "c_import_release"),
        "passed\n"
    );
}

#[test]
fn unsupported_c_declarations_are_diagnosed_at_the_import() {
    let directory = tempdir::TempDir::new("solar-c-errors").unwrap();
    let source = directory.path().join("main.solar");
    for (name, message) in [
        ("BadUnion", "unions"),
        ("BadBits", "bitfields"),
        ("BadPacked", "attribute"),
        ("BadAligned", "attribute"),
        ("BadFieldAlignment", "attribute"),
        ("BadVolatile", "qualifiers"),
        ("BadFlexible", "not representable"),
        ("BadEmpty", "empty C structs"),
        ("BadZero", "zero-sized C arrays"),
        ("Incomplete", "incomplete C struct"),
        ("Callback", "not representable"),
        ("TooWide", "not representable"),
        ("AlignedInt", "attribute"),
        ("BadNested", "unions"),
        ("forbidden_function", "C FFI is not supported"),
        ("mutable_global", "not a constant"),
        ("MISSING", "no constant or type"),
    ] {
        let text =
            format!("import(C) {{{name}}} from \"./tests/c_import/types.h\";\nfn main() {{}}\n");
        std::fs::write(&source, text).unwrap();
        let (errors, map) = match solar::pipeline::compile(&source) {
            Ok(_) => panic!("{name} should not be importable"),
            Err(errors) => errors,
        };
        assert!(
            errors[0].message.contains(message),
            "{name}: {}",
            errors[0].message
        );
        assert_eq!(
            Path::new(map.get(errors[0].span.file_id).unwrap().0),
            source
        );
        assert_eq!(errors[0].span.start.line, 0);
    }
}

#[test]
fn c_import_formatting_preserves_the_language_attribute() {
    let text = "pub import(C){size_t}from\"string.h\";\nimport(C) c from \"limits.h\";\nimport(C)*from\"limits.h\";\n";
    let formatted = solar::fmt::format_source(text).unwrap();
    assert!(formatted.contains("pub import(C) {size_t} from \"string.h\";"));
    assert_eq!(solar::fmt::format_source(&formatted).unwrap(), formatted);
}

#[test]
fn angle_header_lookup_does_not_search_the_solar_file_directory() {
    let directory = tempdir::TempDir::new("solar-c-search").unwrap();
    std::fs::write(
        directory.path().join("limits.h"),
        "#error wrong search directory\n",
    )
    .unwrap();
    let source = directory.path().join("main.solar");
    std::fs::write(
        &source,
        "import(C) {INT_MAX} from \"limits.h\"; fn main() { assert(INT_MAX > 0i32); }",
    )
    .unwrap();
    if let Err((errors, _)) = solar::pipeline::compile(&source) {
        panic!("{}", errors[0].message);
    }
}
