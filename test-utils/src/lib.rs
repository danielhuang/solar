//! Helpers for exercising Solar compiler backends in tests.

use std::path::Path;
use std::process::Command;
use std::sync::Once;

use solar::pipeline::{CompileOptions, Mangled, TreeIr};

static BUILD_RUNTIME: Once = Once::new();

/// Builds the debug Solar runtime once per test process.
pub fn ensure_runtime_built() {
    BUILD_RUNTIME.call_once(|| {
        let status = Command::new("cargo")
            .args(["build", "-p", "solar-system"])
            .env("RUSTFLAGS", "-Ctarget-cpu=native")
            .status()
            .unwrap();
        assert!(status.success(), "failed to build solar-system");
    });
}

static BUILD_RELEASE_RUNTIME: Once = Once::new();

/// Builds the release Solar runtime once per test process.
pub fn ensure_release_runtime_built() {
    BUILD_RELEASE_RUNTIME.call_once(|| {
        let status = Command::new("cargo")
            .args(["build", "--release", "-p", "solar-system"])
            .status()
            .unwrap();
        assert!(status.success(), "failed to build release solar-system");
    });
}

/// Runs a mangled program with the AST interpreter.
pub fn run_ast(mangled: &Mangled) -> String {
    let mut buf = Vec::new();
    solar::ast_interp::interpret_to(&mangled.mangled, std::io::empty(), &mut buf);
    String::from_utf8(buf).unwrap()
}

/// Runs a tree IR program with the tree IR interpreter.
pub fn run_tree_ir(tree_ir: &TreeIr) -> String {
    let mut buf = Vec::new();
    solar::tree_ir_interp::interpret_to(&tree_ir.tree_ir, std::io::empty(), &mut buf);
    String::from_utf8(buf).unwrap()
}

/// Compile a file and run the AST interpreter.
pub fn run_ast_file(file_path: &Path) -> String {
    let mangled = solar::pipeline::compile(file_path).unwrap().to_mangled();
    run_ast(&mangled)
}

/// Compile a file and run the tree IR interpreter.
pub fn run_tree_ir_file(file_path: &Path) -> String {
    let typed = solar::pipeline::compile(file_path).unwrap();
    let tree_ir = typed.to_mangled().to_tree_ir();
    run_tree_ir(&tree_ir)
}

/// Compile a file and run via codegen.
pub fn run_codegen_file(file_path: &Path, test_name: &str) -> String {
    ensure_runtime_built();
    let directory = tempdir::TempDir::new("solar-test").unwrap();
    let typed = solar::pipeline::compile(file_path).unwrap();
    typed
        .to_mangled()
        .to_tree_ir()
        .to_c(&file_path.display().to_string())
        .to_binary(directory.path().join(test_name), CompileOptions::DEBUG)
        .run(test_name)
}

/// Run all three backends and assert identical output.
pub fn run(file_path: &Path, test_name: &str) -> String {
    ensure_runtime_built();
    let directory = tempdir::TempDir::new("solar-test").unwrap();
    let mangled = solar::pipeline::compile(file_path).unwrap().to_mangled();
    let ast_out = run_ast(&mangled);
    // Exercise optimized stack placement under ASAN.
    let tree_ir = mangled.to_tree_ir().optimized();
    let tree_ir_out = run_tree_ir(&tree_ir);
    assert_eq!(
        ast_out, tree_ir_out,
        "ast_interp and tree_ir_interp produced different output"
    );
    let codegen_out = tree_ir
        .to_c(&file_path.display().to_string())
        .to_binary(directory.path().join(test_name), CompileOptions::DEBUG)
        .run(test_name);
    assert_eq!(
        tree_ir_out, codegen_out,
        "tree_ir_interp and codegen produced different output"
    );
    ast_out
}
