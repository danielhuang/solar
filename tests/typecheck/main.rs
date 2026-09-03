use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/typecheck")
        .join(name)
}

fn error_chain(error: &solar::error::CompileError) -> String {
    let mut messages = error
        .caused_by
        .as_deref()
        .map(error_chain)
        .unwrap_or_default();
    if !messages.is_empty() {
        messages.push('\n');
    }
    messages.push_str(&error.message);
    messages
}

/// Compile a single file without stdlib (for testing raw type errors).
fn compile(file_path: &Path) {
    let source = std::fs::read_to_string(file_path).unwrap();
    let ast = solar::parser::parse(&source).unwrap();
    let desugared = solar::desugared_ast::lower(&ast);
    let resolved = solar::resolved_ast::SourceFile {
        items: desugared.items,
    };
    match solar::typed_ast::lower(&resolved) {
        Ok(_) => {}
        Err(error) => panic!("{}", error_chain(&error)),
    }
}

/// Compile a file through the full pipeline (with stdlib).
fn compile_with_pipeline(file_path: &Path) {
    match solar::pipeline::compile(file_path) {
        Ok(_) => {}
        Err((errors, _)) => panic!("{}", error_chain(&errors[0])),
    }
}

#[test]
fn example_typechecks() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/example.solar");
    compile_with_pipeline(&path);
}

#[test]
fn monomorphization_error_retains_and_prints_the_call_chain() {
    let path = fixture("monomorphization_error_chain.solar");
    let (errors, _) = match solar::pipeline::compile(&path) {
        Ok(_) => panic!("expected type-check failure"),
        Err(error) => error,
    };
    let outer = &errors[0];
    assert!(outer.message.contains("`outer`"));
    assert_eq!(outer.span.start.line, 4);
    let middle = outer.caused_by.as_ref().expect("outer call cause");
    assert!(middle.message.contains("`inner`"));
    assert_eq!(middle.span.start.line, 2);
    let inner = middle.caused_by.as_ref().expect("inner call cause");
    assert!(inner.message.contains("got Bool"));
    assert_eq!(inner.span.start.line, 0);
    assert!(inner.caused_by.is_none());

    let output = Command::new(env!("CARGO_BIN_EXE_solar"))
        .arg(path)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    let inner_position = stderr.find(&inner.message).expect("inner diagnostic");
    let middle_position = stderr.find(&middle.message).expect("middle diagnostic");
    let outer_position = stderr.find(&outer.message).expect("outer diagnostic");
    assert!(inner_position < middle_position);
    assert!(middle_position < outer_position);
}

#[test]
#[should_panic(expected = "field `kind` has non-C-representable type Kind: enums")]
fn repr_c_rejects_enum_fields() {
    compile(&fixture("repr_c_enum_field.solar"));
}

#[test]
#[should_panic(expected = "field `marker` has non-C-representable type (): zero-sized")]
fn repr_c_rejects_zero_sized_fields() {
    compile(&fixture("repr_c_zst_field.solar"));
}

#[test]
#[should_panic(expected = "zero-sized structs are not representable in C")]
fn repr_c_rejects_empty_structs() {
    compile(&fixture("repr_c_empty.solar"));
}

#[test]
#[should_panic(expected = "nested struct `Inner` must also use repr(C)")]
fn repr_c_rejects_non_c_nested_structs() {
    compile(&fixture("repr_c_nested_non_c.solar"));
}

#[test]
#[should_panic(expected = "function values include a closure environment")]
fn repr_c_rejects_function_values() {
    compile(&fixture("repr_c_function_field.solar"));
}

#[test]
#[should_panic(expected = "type mismatch in let: expected Foo, got Int")]
fn bad_let() {
    compile(&fixture("typecheck_bad_let.solar"));
}

#[test]
#[should_panic(expected = "try: expected fn(&[Uint8]), got fn(Int)")]
fn try_catch_bad_binding_type() {
    compile(&fixture("try_catch_bad_binding_type.solar"));
}

#[test]
#[should_panic(expected = "undefined enum: TryBlockResult_0")]
fn try_result_type_is_inaccessible() {
    compile(&fixture("try_result_type_inaccessible.solar"));
}

#[test]
#[should_panic(expected = "type mismatch in argument `n` of takes_int: expected Int, got Foo")]
fn bad_arg() {
    compile(&fixture("typecheck_bad_arg.solar"));
}

#[test]
#[should_panic(expected = "empty array literal needs an element type annotation: []#[T]")]
fn empty_array_no_annotation() {
    compile(&fixture("empty_array_no_annotation.solar"));
}

#[test]
#[should_panic(expected = "array literal annotated as [Uint] but elements have type Int")]
fn array_annotation_mismatch() {
    compile(&fixture("array_annotation_mismatch.solar"));
}

#[test]
#[should_panic(expected = "type mismatch in assignment: expected Int, got Foo")]
fn bad_assign() {
    compile(&fixture("typecheck_bad_assign.solar"));
}

#[test]
#[should_panic(expected = "binary op type mismatch: left is Uint8, right is Uint16")]
fn bitwise_type_mismatch() {
    compile(&fixture("bitwise_type_mismatch.solar"));
}

#[test]
#[should_panic(expected = "binary op type mismatch: left is Uint8, right is Int8")]
fn bitwise_signedness_mismatch() {
    compile(&fixture("bitwise_signedness_mismatch.solar"));
}

#[test]
#[should_panic(expected = "bitwise operators require integer types, got Bool")]
fn bitwise_non_integer() {
    compile(&fixture("bitwise_non_integer.solar"));
}

#[test]
#[should_panic(expected = "`!` requires a Bool or integer operand, got Foo")]
fn not_non_integer() {
    compile(&fixture("not_non_integer.solar"));
}

#[test]
#[should_panic(expected = "binary op type mismatch: left is Uint8, right is Uint16")]
fn wrap_type_mismatch() {
    compile(&fixture("wrap_type_mismatch.solar"));
}

#[test]
#[should_panic(expected = "wrapping arithmetic operators require integer types, got Bool")]
fn wrap_non_integer() {
    compile(&fixture("wrap_non_integer.solar"));
}

#[test]
#[should_panic(expected = "cannot deref non-reference type Int")]
fn bad_deref() {
    compile(&fixture("typecheck_bad_deref.solar"));
}

#[test]
#[should_panic(expected = "cannot deref non-reference type FileDesc")]
fn filedesc_not_dereferenceable() {
    compile_with_pipeline(&fixture("filedesc_no_deref.solar"));
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
#[should_panic(expected = "arithmetic operators require numeric types, got Bool")]
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
#[should_panic(expected = "arithmetic operators require numeric types, got &Int")]
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
    expected = "inferred type parameter `T` is not used in a parameter of function `foo`"
)]
fn bad_unused_type_param() {
    compile(&fixture("typecheck_bad_unused_type_param.solar"));
}

#[test]
#[should_panic(
    expected = "inferred type parameter `T` is not used in a parameter of function `foo`"
)]
fn inferred_type_param_used_only_in_return() {
    compile(&fixture("inferred_type_param_used_only_in_return.solar"));
}

#[test]
#[should_panic(expected = "requires output type arguments written with `#[...]`")]
fn missing_output_type_args() {
    compile(&fixture("missing_output_type_args.solar"));
}

#[test]
#[should_panic(expected = "inferred type parameters of `identity` cannot be specified explicitly")]
fn inferred_type_args_cannot_be_explicit() {
    compile(&fixture("inferred_type_args_cannot_be_explicit.solar"));
}

#[test]
#[should_panic(expected = "size_of: type [Int] is unsized")]
fn size_of_unsized_type() {
    compile_with_pipeline(&fixture("size_of_unsized_type.solar"));
}

#[test]
#[should_panic(expected = "any_new: expected &T, got &[Int]")]
fn any_rejects_unsized_referents() {
    compile_with_pipeline(&fixture("any_rejects_unsized.solar"));
}

#[test]
#[should_panic(expected = "any_downcast: type [Int] is unsized")]
fn any_downcast_rejects_unsized_types() {
    compile_with_pipeline(&fixture("any_downcast_rejects_unsized.solar"));
}

#[test]
#[should_panic(expected = "field access on non-struct type Any")]
fn any_type_id_is_private() {
    compile_with_pipeline(&fixture("any_type_id_private.solar"));
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
#[should_panic(expected = "type mismatch in let: expected Int, got Uint")]
fn bad_reflect_variant_unit_val() {
    compile(&fixture("typecheck_bad_reflect_variant_unit_val.solar"));
}

// A nullable reference `&?T` does not implicitly coerce to a normal `&T`.
#[test]
#[should_panic(expected = "type mismatch in let: expected &Int, got &?Int")]
fn bad_nullable_coerce() {
    compile(&fixture("typecheck_bad_nullable_coerce.solar"));
}

// A required parameter cannot follow a keyword parameter with a default.
#[test]
#[should_panic(expected = "required parameter cannot follow a keyword parameter with a default")]
fn kwarg_required_after_default() {
    compile(&fixture("kwarg_required_after_default.solar"));
}

// A keyword parameter's default must be a literal.
#[test]
#[should_panic(expected = "default value of a keyword parameter must be a literal")]
fn kwarg_nonliteral_default() {
    compile(&fixture("kwarg_nonliteral_default.solar"));
}

// A keyword argument must name an existing keyword parameter.
#[test]
#[should_panic(expected = "f has no keyword parameter `c`")]
fn kwarg_unknown_name() {
    compile(&fixture("kwarg_unknown_name.solar"));
}

// A const must be assigned a literal value.
#[test]
#[should_panic(expected = "const `BAD` must be assigned a literal value")]
fn const_nonliteral() {
    compile(&fixture("const_nonliteral.solar"));
}

// A const declared in a block is not visible outside it.
#[test]
#[should_panic(expected = "undefined variable: INNER")]
fn const_out_of_scope() {
    compile(&fixture("const_out_of_scope.solar"));
}

// `break` is only valid inside a loop.
#[test]
#[should_panic(expected = "`break` outside of a loop")]
fn break_outside_loop() {
    compile(&fixture("break_outside_loop.solar"));
}

// A closure resets the loop context, so `continue` inside a closure that is
// itself inside a loop is an error.
#[test]
#[should_panic(expected = "`continue` outside of a loop")]
fn continue_outside_loop() {
    compile(&fixture("continue_outside_loop.solar"));
}

// `break <value>` is only allowed in a `loop`, not a `while`/`for`.
#[test]
#[should_panic(expected = "cannot `break` with a value out of a `while`/`for` loop")]
fn break_value_in_while() {
    compile(&fixture("break_value_in_while.solar"));
}

// All `break <value>`s in a `loop` must agree on type.
#[test]
#[should_panic(expected = "`break` value type mismatch")]
fn loop_break_type_mismatch() {
    compile(&fixture("loop_break_type_mismatch.solar"));
}

// Recursive generics require an explicit return type.
#[test]
#[should_panic(expected = "cannot infer return type of recursive generic function `h`")]
fn generic_recursion_inferred_return() {
    compile_with_pipeline(&fixture("generic_recursion_inferred_return.solar"));
}

// Polymorphic recursion must stop at the monomorphization depth limit.
#[test]
#[should_panic(expected = "monomorphization depth limit")]
fn polymorphic_recursion() {
    compile_with_pipeline(&fixture("polymorphic_recursion.solar"));
}

#[test]
#[should_panic(expected = "access to unsafe function `dangerous` requires an unsafe block")]
fn unsafe_call_requires_block() {
    compile(&fixture("unsafe_call_requires_block.solar"));
}

#[test]
#[should_panic(expected = "access to unsafe function `dangerous` requires an unsafe block")]
fn unsafe_reference_requires_block() {
    compile(&fixture("unsafe_reference_requires_block.solar"));
}

#[test]
#[should_panic(expected = "access to unsafe function `touch` requires an unsafe block")]
fn unsafe_method_requires_block() {
    compile(&fixture("unsafe_method_requires_block.solar"));
}

#[test]
#[should_panic(expected = "access to unsafe function `dangerous` requires an unsafe block")]
fn unsafe_function_body_is_not_implicitly_unsafe() {
    compile(&fixture("unsafe_function_body_requires_block.solar"));
}

#[test]
#[should_panic(expected = "access to unsafe function `dangerous` requires an unsafe block")]
fn closure_does_not_inherit_unsafe_context() {
    compile(&fixture("unsafe_closure_requires_own_block.solar"));
}

#[test]
#[should_panic(expected = "access to unsafe intrinsic `syscall` requires an unsafe block")]
fn syscall_requires_unsafe_block() {
    compile_with_pipeline(&fixture("syscall_requires_unsafe_block.solar"));
}

#[test]
#[should_panic(expected = "access to unsafe intrinsic `fd_from_raw` requires an unsafe block")]
fn fd_from_raw_requires_unsafe_block() {
    compile_with_pipeline(&fixture("fd_from_raw_requires_unsafe_block.solar"));
}

#[test]
#[should_panic(expected = "syscall: argument 2 must be Int64, Uint64, or a reference, got Int32")]
fn syscall_rejects_other_argument_types() {
    compile_with_pipeline(&fixture("syscall_bad_argument.solar"));
}

#[test]
#[should_panic(expected = "syscall: expected a syscall number and at most 6 arguments")]
fn syscall_rejects_too_many_arguments() {
    compile_with_pipeline(&fixture("syscall_too_many_arguments.solar"));
}
