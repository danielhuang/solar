use std::path::Path;
use std::process::Command;
use std::sync::Once;

use solar::pipeline::{CompileMode, Ir, Typed};

static BUILD_RUNTIME: Once = Once::new();

/// Ensure the solar-system runtime is built once per test process (for test use
/// only). Built **natively**, dropping the workspace's global
/// `-Clinker-plugin-lto`: that flag emits LLVM bitcode archive members, which the
/// debug link (`CompileMode::Debug`) would otherwise have to LTO-compile on every
/// test — slow. A native `.a` links in milliseconds. (The release runtime, built
/// separately, keeps linker-plugin-lto for cross-language LTO.)
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

pub fn run_ast(typed: &Typed) -> String {
    let mut buf = Vec::new();
    solar::ast_interp::interpret_to(&typed.typed, std::io::empty(), &mut buf);
    String::from_utf8(buf).unwrap()
}

pub fn run_ir(ir: &Ir) -> String {
    let mut buf = Vec::new();
    solar::ir_interp::interpret_to(&ir.ir, std::io::empty(), &mut buf);
    String::from_utf8(buf).unwrap()
}

/// Compile a file and run the AST interpreter.
pub fn run_ast_file(file_path: &Path) -> String {
    let typed = solar::pipeline::compile(file_path).unwrap();
    run_ast(&typed)
}

/// Compile a file and run the IR interpreter.
pub fn run_ir_file(file_path: &Path) -> String {
    let typed = solar::pipeline::compile(file_path).unwrap();
    let ir = typed.to_ir();
    run_ir(&ir)
}

/// Compile a file and run via codegen.
pub fn run_codegen_file(file_path: &Path, test_name: &str) -> String {
    ensure_runtime_built();
    let typed = solar::pipeline::compile(file_path).unwrap();
    typed
        .to_ir()
        .to_c(&file_path.display().to_string())
        .to_binary(test_name, CompileMode::Debug)
        .run(test_name)
}

/// Run all three backends and assert identical output.
pub fn run(file_path: &Path, test_name: &str) -> String {
    ensure_runtime_built();
    let typed = solar::pipeline::compile(file_path).unwrap();
    let ast_out = run_ast(&typed);
    let ir = typed.to_ir();
    let ir_out = run_ir(&ir);
    assert_eq!(
        ast_out, ir_out,
        "ast_interp and ir_interp produced different output"
    );
    let codegen_out = ir
        .to_c(&file_path.display().to_string())
        .to_binary(test_name, CompileMode::Debug)
        .run(test_name);
    assert_eq!(
        ir_out, codegen_out,
        "ir_interp and codegen produced different output"
    );
    ast_out
}
