//! Ensures fixed-class allocation specialization preserves LLVM allocation elision.

use solar::pipeline::CompileOptions;
use std::process::Command;

const SRC: &str = r#"
fn while_fn(cond: fn() -> Bool, body: fn()) {
    while cond() {
        body();
    }
}

fn apply#[T](f: fn(T), x: [T; 1]) {
    f(x[0u]);
}

fn add(x: Int) -> fn(Int) -> Int {
    \y: Int x + y
}

fn main() {
    let i: Int = 0;
    while_fn(
        \ i < 1000,
        \ apply(\x: &Int { x@ = add(x@)(1); }, [i&]),
    );
    println(i);
}
"#;

#[test]
fn specialized_allocators_retain_allocation_elision() {
    test_utils::ensure_release_runtime_built();
    let directory = tempdir::TempDir::new("solar-test").unwrap();
    let dir = directory.path();
    let src_path = dir.join("release_alloc_elision.solar");
    std::fs::write(&src_path, SRC).unwrap();

    let bin = solar::pipeline::compile(&src_path)
        .unwrap()
        .to_mangled()
        .to_tree_ir()
        .optimized()
        .to_c(&src_path.display().to_string())
        .to_binary(dir.join("release_alloc_elision"), CompileOptions::RELEASE)
        .path;
    let out = Command::new(bin)
        .env("SOLAR_PRINT_ALLOCS", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "release binary failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "1000\n");

    let stderr = String::from_utf8_lossy(&out.stderr);
    let allocations = stderr.matches("allocating new object:").count();
    assert!(
        (1..=4).contains(&allocations),
        "expected 1 to 4 reported allocations, found {allocations}:\n{stderr}"
    );
}

#[test]
fn println_zero_elides_to_string_buffer() {
    test_utils::ensure_release_runtime_built();
    let directory = tempdir::TempDir::new("solar-test").unwrap();
    let source = directory.path().join("println_zero.solar");
    std::fs::write(&source, "fn main() { println(0); }\n").unwrap();
    let binary = solar::pipeline::compile(&source)
        .unwrap()
        .to_mangled()
        .to_tree_ir()
        .optimized()
        .to_c(&source.display().to_string())
        .to_binary(
            directory.path().join("println_zero"),
            CompileOptions::RELEASE,
        );
    let output = Command::new(binary.path)
        .env("SOLAR_PRINT_ALLOCS", "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"0\n");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let allocations: Vec<_> = stderr
        .lines()
        .filter(|line| line.starts_with("allocating new object:"))
        .collect();
    // The output and file-write error message survive; the 20-byte to_string
    // buffer and the one-byte slice copied out of it must both disappear.
    assert_eq!(
        allocations,
        [
            "allocating new object: 2 bytes (align=1)",
            "allocating new object: 25 bytes (align=1)",
        ],
        "{stderr}"
    );
}
