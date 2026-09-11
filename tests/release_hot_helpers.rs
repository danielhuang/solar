//! Checks the small LLVM runtime module against the diagnostic backends.

use solar::pipeline::CompileOptions;
use std::path::Path;

#[test]
fn hot_helpers_preserve_runtime_results_and_exceptions() {
    test_utils::ensure_release_runtime_built();
    let directory = tempdir::TempDir::new("solar-test").unwrap();
    for name in [
        "catch_runtime_errors",
        "binop_arithmetic",
        "carrying_mul_add",
        "array_slice",
        "nullable_ref",
        "closure_capture_fn",
        "atomics",
    ] {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/runtime")
            .join(format!("{name}.solar"));
        let expected = test_utils::run(&source, name);
        let actual = solar::pipeline::compile(&source)
            .unwrap()
            .to_mangled()
            .to_ir()
            .optimized()
            .to_c(&source.display().to_string())
            .to_binary(directory.path().join(name), CompileOptions::RELEASE)
            .run(name);
        assert_eq!(actual, expected, "release helper mismatch in {name}");
    }
}
