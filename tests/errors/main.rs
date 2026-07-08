use std::path::{Path, PathBuf};
use test_utils::{run_ast_file, run_codegen_file, run_ir_file};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/errors")
        .join(name)
}

#[test]
#[should_panic(expected = "index out of bounds: index is 5 but length is 3")]
fn oob_index_ast() {
    run_ast_file(&fixture("oob_index.solar"));
}

#[test]
#[should_panic(expected = "index out of bounds: index is 5 but length is 3")]
fn oob_index_ir() {
    run_ir_file(&fixture("oob_index.solar"));
}

#[test]
#[should_panic(expected = "index out of bounds: index is 5 but length is 3")]
fn oob_index_codegen() {
    run_codegen_file(&fixture("oob_index.solar"), "oob_index");
}

#[test]
#[should_panic(expected = "null reference dereference")]
fn null_deref_ast() {
    run_ast_file(&fixture("null_deref.solar"));
}

#[test]
#[should_panic(expected = "null reference dereference")]
fn null_deref_ir() {
    run_ir_file(&fixture("null_deref.solar"));
}

#[test]
#[should_panic(expected = "null reference dereference")]
fn null_deref_codegen() {
    run_codegen_file(&fixture("null_deref.solar"), "null_deref");
}

#[test]
#[should_panic(expected = "slice end (5) > length (3)")]
fn oob_slice_ast() {
    run_ast_file(&fixture("oob_slice.solar"));
}

#[test]
#[should_panic(expected = "slice end (5) > length (3)")]
fn oob_slice_ir() {
    run_ir_file(&fixture("oob_slice.solar"));
}

#[test]
#[should_panic(expected = "slice end (5) > length (3)")]
fn oob_slice_codegen() {
    run_codegen_file(&fixture("oob_slice.solar"), "oob_slice");
}

#[test]
#[should_panic(expected = "array length mismatch: expected 2 elements, got 3")]
fn destructure_bad_len_ast() {
    run_ast_file(&fixture("destructure_bad_len.solar"));
}

#[test]
#[should_panic(expected = "array length mismatch: expected 2 elements, got 3")]
fn destructure_bad_len_ir() {
    run_ir_file(&fixture("destructure_bad_len.solar"));
}

#[test]
#[should_panic(expected = "array length mismatch: expected 2 elements, got 3")]
fn destructure_bad_len_codegen() {
    run_codegen_file(&fixture("destructure_bad_len.solar"), "destructure_bad_len");
}

#[test]
#[should_panic(expected = "something went wrong")]
fn panic_ast() {
    run_ast_file(&fixture("panic.solar"));
}

#[test]
#[should_panic(expected = "something went wrong")]
fn panic_ir() {
    run_ir_file(&fixture("panic.solar"));
}

#[test]
#[should_panic(expected = "something went wrong")]
fn panic_codegen() {
    run_codegen_file(&fixture("panic.solar"), "panic");
}
