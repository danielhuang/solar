//! Dynamic array byte counts must be checked before allocating or initializing.

use solar::pipeline::CompileOptions;
use std::process::Command;

const SRC: &str = r#"
fn main() {
    let count = 2305843009213693953u;
    try {
        let values: [Int] = [count; \i: Uint {
            if i == 2u { throw("initializer ran"&); }
            42
        }];
        println(values[0u]);
    } catch (error) {
        println(error.message);
    }
}
"#;

#[test]
fn release_array_size_overflow_throws_before_initialization() {
    test_utils::ensure_release_runtime_built();
    let directory = tempdir::TempDir::new("solar-test").unwrap();
    let source = directory.path().join("array_size_overflow.solar");
    std::fs::write(&source, SRC).unwrap();
    let expected = "integer overflow in multiplication\n";

    assert_eq!(test_utils::run_ast_file(&source), expected);
    assert_eq!(test_utils::run_tree_ir_file(&source), expected);

    let binary = solar::pipeline::compile(&source)
        .unwrap()
        .to_mangled()
        .to_tree_ir()
        .optimized()
        .to_c(&source.display().to_string())
        .to_binary(
            directory.path().join("array_size_overflow"),
            CompileOptions::RELEASE,
        )
        .path;
    let output = Command::new(binary).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, expected.as_bytes());
}
