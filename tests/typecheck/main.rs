use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/typecheck")
        .join(name)
}

/// Compile a single file without stdlib (for testing raw type errors).
fn compile(file_path: &Path) {
    let source = std::fs::read_to_string(file_path).unwrap();
    let ast = solar::parser::parse(&source).unwrap();
    match solar::typed_ast::lower(&ast) {
        Ok(_) => {}
        Err(e) => panic!("{}", e.message),
    }
}

/// Compile a file through the full pipeline (with stdlib).
fn compile_with_pipeline(file_path: &Path) {
    match solar::pipeline::compile(file_path) {
        Ok(_) => {}
        Err(errors) => panic!("{}", errors[0].message),
    }
}

#[test]
fn example_typechecks() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/example.solar");
    compile_with_pipeline(&path);
}

#[test]
#[should_panic(expected = "type mismatch in let: expected Foo, got Int")]
fn bad_let() {
    compile(&fixture("typecheck_bad_let.solar"));
}

#[test]
#[should_panic(expected = "type mismatch in argument `n` of takes_int: expected Int, got Foo")]
fn bad_arg() {
    compile(&fixture("typecheck_bad_arg.solar"));
}

#[test]
#[should_panic(expected = "type mismatch in assignment: expected Int, got Foo")]
fn bad_assign() {
    compile(&fixture("typecheck_bad_assign.solar"));
}

#[test]
#[should_panic(expected = "cannot deref non-reference type Int")]
fn bad_deref() {
    compile(&fixture("typecheck_bad_deref.solar"));
}

#[test]
#[should_panic(expected = "field access on non-struct type Int")]
fn bad_field_on_int() {
    compile(&fixture("typecheck_bad_field.solar"));
}

#[test]
#[should_panic(expected = "missing field `y` in Foo literal")]
fn bad_missing_field() {
    compile(&fixture("typecheck_bad_missing_field.solar"));
}

#[test]
#[should_panic(expected = "unknown field `z` in Foo literal")]
fn bad_unknown_field() {
    compile(&fixture("typecheck_bad_unknown_field.solar"));
}

#[test]
#[should_panic(expected = "undefined variable: y")]
fn bad_undefined_var() {
    compile(&fixture("typecheck_bad_undefined_var.solar"));
}

#[test]
#[should_panic(expected = "arithmetic operators require integer types, got Bool")]
fn bad_arith_bool() {
    compile(&fixture("typecheck_bad_arith_bool.solar"));
}

#[test]
#[should_panic(expected = "binary op type mismatch: left is Int, right is Bool")]
fn bad_binop_mismatch() {
    compile(&fixture("typecheck_bad_binop_mismatch.solar"));
}

#[test]
#[should_panic(expected = "logical operators require Bool, got Int")]
fn bad_logic_int() {
    compile(&fixture("typecheck_bad_logic_int.solar"));
}

#[test]
#[should_panic(expected = "equality operators not supported on Foo")]
fn bad_struct_eq() {
    compile(&fixture("typecheck_bad_struct_eq.solar"));
}

#[test]
#[should_panic(expected = "arithmetic operators require integer types, got &Int")]
fn bad_ref_add() {
    compile(&fixture("typecheck_bad_ref_add.solar"));
}

#[test]
#[should_panic(expected = "function `bad` should return Int, but last expression is Bool")]
fn bad_return_type() {
    compile(&fixture("typecheck_bad_return_type.solar"));
}

#[test]
#[should_panic(expected = "return type mismatch: expected Int, got Bool")]
fn bad_return_mismatch() {
    compile(&fixture("typecheck_bad_return_mismatch.solar"));
}

#[test]
#[should_panic(expected = "index on non-array type Int")]
fn bad_index_non_array() {
    compile(&fixture("typecheck_bad_index_non_array.solar"));
}

#[test]
#[should_panic(expected = "array index must be Uint, got Int")]
fn bad_index_type() {
    compile(&fixture("typecheck_bad_index_type.solar"));
}

#[test]
#[should_panic(
    expected = "function `bad` should return Int, but body does not end with an expression"
)]
fn bad_no_return_expr() {
    compile(&fixture("typecheck_bad_no_return_expr.solar"));
}

#[test]
#[should_panic(expected = "function `bad`: parameter has unsized type [Int]")]
fn bad_unsized_param() {
    compile(&fixture("typecheck_bad_unsized_param.solar"));
}

#[test]
#[should_panic(expected = "function `bad`: return type [Int] is unsized")]
fn bad_unsized_return() {
    compile(&fixture("typecheck_bad_unsized_return.solar"));
}

#[test]
#[should_panic(expected = "struct `Bad`: unsized field `xs` must be the last field")]
fn bad_unsized_not_last() {
    compile(&fixture("typecheck_bad_unsized_not_last.solar"));
}

#[test]
#[should_panic(expected = "duplicate struct definition: `Foo`")]
fn bad_duplicate_struct() {
    compile(&fixture("typecheck_bad_duplicate_struct.solar"));
}

#[test]
#[should_panic(expected = "duplicate function definition: `foo`")]
fn bad_duplicate_function() {
    compile(&fixture("typecheck_bad_duplicate_function.solar"));
}

#[test]
#[should_panic(expected = "overloads have equivalent parameter patterns")]
fn bad_overload_generic_conflict() {
    compile(&fixture("typecheck_bad_overload_generic_conflict.solar"));
}

#[test]
#[should_panic(expected = "ambiguous function reference: `foo` has multiple overloads")]
fn bad_overload_ambiguous_ref() {
    compile(&fixture("typecheck_bad_overload_ambiguous_ref.solar"));
}

#[test]
#[should_panic(expected = "cannot call non-function type Int")]
fn bad_call_non_function() {
    compile(&fixture("typecheck_call_non_function.solar"));
}

#[test]
#[should_panic(expected = "cannot assign to non-place expression")]
fn bad_assign_if_nonplace() {
    compile(&fixture("typecheck_bad_assign_if_nonplace.solar"));
}

#[test]
#[should_panic(expected = "cannot assign to non-place expression")]
fn bad_assign_match_nonplace() {
    compile(&fixture("typecheck_bad_assign_match_nonplace.solar"));
}

#[test]
#[should_panic(expected = "for range end must have type Int, got Uint")]
fn bad_for_range_types() {
    compile(&fixture("typecheck_bad_for_range_types.solar"));
}

#[test]
#[should_panic(
    expected = "type parameter `T` is not used in function `foo` parameters or return type"
)]
fn bad_unused_type_param() {
    compile(&fixture("typecheck_bad_unused_type_param.solar"));
}

#[test]
#[should_panic(expected = "cannot infer type of closure parameter `x` without context")]
fn bad_closure_infer_no_context() {
    compile(&fixture("typecheck_bad_closure_infer_no_context.solar"));
}

#[test]
#[should_panic(
    expected = "unknown match.reflect kind \"primitive\" (expected \"struct\" or \"enum\")"
)]
fn bad_reflect_unknown_kind() {
    compile(&fixture("typecheck_bad_reflect_unknown_kind.solar"));
}

#[test]
#[should_panic(expected = "non-exhaustive match.reflect: no `_` arm for type Int")]
fn bad_reflect_no_match() {
    compile(&fixture("typecheck_bad_reflect_no_match.solar"));
}

#[test]
#[should_panic(expected = "undefined type in match.reflect: Missing")]
fn bad_reflect_undefined_type() {
    compile(&fixture("typecheck_bad_reflect_undefined_type.solar"));
}

#[test]
#[should_panic(expected = "duplicate match.reflect arm for \"struct\"")]
fn bad_reflect_duplicate_kind() {
    compile(&fixture("typecheck_bad_reflect_duplicate_kind.solar"));
}

#[test]
#[should_panic(expected = "integer literal out of range for Uint8 (0..=255)")]
fn bad_literal_overflow_u8() {
    compile(&fixture("typecheck_bad_literal_overflow_u8.solar"));
}

#[test]
#[should_panic(
    expected = "integer literal out of range for Int (-9223372036854775808..=9223372036854775807)"
)]
fn bad_literal_overflow_int() {
    compile(&fixture("typecheck_bad_literal_overflow_int.solar"));
}

#[test]
#[should_panic(expected = "integer literal out of range for Uint (0..=18446744073709551615)")]
fn bad_literal_overflow_uint() {
    compile(&fixture("typecheck_bad_literal_overflow_uint.solar"));
}

#[test]
#[should_panic(expected = "for.reflect_fields requires &T where T is a struct, got &Int")]
fn bad_reflect_fields_not_struct() {
    compile(&fixture("typecheck_bad_reflect_fields_not_struct.solar"));
}

#[test]
#[should_panic(expected = "for.reflect_fields requires &T where T is a struct, got P")]
fn bad_reflect_fields_not_ref() {
    compile(&fixture("typecheck_bad_reflect_fields_not_ref.solar"));
}

#[test]
#[should_panic(expected = "match.reflect_variant requires &T where T is an enum, got &Int")]
fn bad_reflect_variant_not_enum() {
    compile(&fixture("typecheck_bad_reflect_variant_not_enum.solar"));
}

#[test]
#[should_panic(expected = "match.reflect_variant requires &T where T is an enum, got E")]
fn bad_reflect_variant_not_ref() {
    compile(&fixture("typecheck_bad_reflect_variant_not_ref.solar"));
}

// `val` is only bound in data-variant arms; using it with a unit variant
// present is a compile error.
#[test]
#[should_panic(expected = "undefined variable: val")]
fn bad_reflect_variant_unit_val() {
    compile(&fixture("typecheck_bad_reflect_variant_unit_val.solar"));
}
