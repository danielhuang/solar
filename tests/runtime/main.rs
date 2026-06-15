use std::path::{Path, PathBuf};
use test_utils::run;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/runtime")
        .join(name)
}

#[test]
fn deep_copy_ref_vs_value() {
    let output = run(&fixture("deep_copy.solar"), "deep_copy_ref_vs_value");
    assert_eq!(output, "99\n10\n42\n42\n");
}

#[test]
fn if_while() {
    let output = run(&fixture("if_while.solar"), "if_while");
    assert_eq!(output, "1\n3\n4\n7\n");
}

#[test]
fn binop_arithmetic() {
    let output = run(&fixture("binop_arithmetic.solar"), "binop_arithmetic");
    assert_eq!(output, "5\n6\n21\n5\n2\n14\n20\n");
}

#[test]
fn binop_comparison() {
    let output = run(&fixture("binop_comparison.solar"), "binop_comparison");
    assert_eq!(output, "1\n0\n1\n0\n1\n0\n1\n0\n1\n0\n1\n0\n");
}

#[test]
fn binop_logic() {
    let output = run(&fixture("binop_logic.solar"), "binop_logic");
    assert_eq!(output, "1\n0\n0\n1\n0\n1\n0\n88\n1\n1\n99\n0\n");
}

#[test]
fn binop_array_eq() {
    let output = run(&fixture("binop_array_eq.solar"), "binop_array_eq");
    assert_eq!(output, "1\n0\n0\n1\n0\n");
}

#[test]
fn array_concat() {
    let output = run(&fixture("array_concat.solar"), "array_concat");
    assert_eq!(output, "1\n2\n3\n4\n5\n10\n20\n30\n99\n");
}

#[test]
fn return_values() {
    let output = run(&fixture("return_values.solar"), "return_values");
    assert_eq!(output, "10\n9\n7\n7\n30\n12\n0\n50\n100\n42\n");
}

#[test]
fn array_index() {
    let output = run(&fixture("array_index.solar"), "array_index");
    assert_eq!(output, "10\n20\n30\n99\n30\n100\n300\n999\n");
}

#[test]
fn array_instances() {
    let output = run(&fixture("array_instances.solar"), "array_instances");
    assert_eq!(output, "1\n0\n");
}

#[test]
fn references() {
    let output = run(&fixture("references.solar"), "references");
    assert_eq!(output, "2\n3\n5\n5\n6\n6\n7\n7\n");
}

#[test]
fn string_literal() {
    let output = run(&fixture("string_literal.solar"), "string_literal");
    assert_eq!(output, "1\n0\n1\nHello\n");
}

#[test]
fn ref_literal() {
    let output = run(&fixture("ref_literal.solar"), "ref_literal");
    assert_eq!(output, "5\n10\n20\n");
}

#[test]
fn if_else() {
    let output = run(&fixture("if_else.solar"), "if_else");
    assert_eq!(output, "1\n2\n3\n4\n10\n20\n30\n40\n50\n");
}

#[test]
fn block_expr() {
    let output = run(&fixture("block_expr.solar"), "block_expr");
    assert_eq!(output, "15\n21\n99\n42\n50\n10\n100\n6\n3\n1\n2\n");
}

#[test]
fn shadowing() {
    let output = run(&fixture("shadowing.solar"), "shadowing");
    assert_eq!(output, "2\n20\n10\n99\n5\n1\n30\n12\n42\n100\n");
}

#[test]
fn deeply_nested_parens() {
    let depth = 100;
    let open: String = "(".repeat(depth);
    let close: String = ")".repeat(depth);
    let source = format!("fn main() {{\n  print_int({open}42{close});\n}}\n");
    // Write to a temp file so we can use the file-based pipeline
    let dir = std::path::Path::new("target/test-fixtures");
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join("deeply_nested_parens.solar");
    std::fs::write(&path, &source).unwrap();
    let output = run(&path, "deeply_nested_parens");
    assert_eq!(output, "42\n");
}

#[test]
fn type_inference() {
    let output = run(&fixture("type_inference.solar"), "type_inference");
    assert_eq!(output, "10\n15\n1\n1\n14\n42\n");
}

#[test]
fn array_repeat() {
    let output = run(&fixture("array_repeat.solar"), "array_repeat");
    assert_eq!(output, "42\n42\n42\n0\n0\n99\n0\n7\n");
}

#[test]
fn fixed_array() {
    let output = run(&fixture("fixed_array.solar"), "fixed_array");
    assert_eq!(output, "1\n2\n3\n1\n0\n10\n50\n30\n100\n200\n999\n1\n");
}

#[test]
fn shadow_function() {
    let output = run(&fixture("shadow_function.solar"), "shadow_function");
    assert_eq!(output, "42\n99\n");
}

#[test]
fn first_class_fn() {
    let output = run(&fixture("first_class_fn.solar"), "first_class_fn");
    assert_eq!(output, "10\n14\n21\n8\n12\n6\n9\n30\n200\n");
}

#[test]
fn closures() {
    let output = run(&fixture("closures.solar"), "closures");
    assert_eq!(output, "15\n14\n30\n305\n-42\n20\n3\n11\n12\n");
}

#[test]
fn closure_inference() {
    let output = run(&fixture("closure_inference.solar"), "closure_inference");
    assert_eq!(output, "11\n10\n50\n101\n7\n27\n15\n");
}

#[test]
fn while_fn() {
    let output = run(&fixture("while_fn.solar"), "while_fn");
    assert_eq!(output, "0\n1\n2\n3\n4\n");
}

#[test]
fn write_stdout() {
    let output = run(&fixture("write_stdout.solar"), "write_stdout");
    assert_eq!(output, "test");
}

#[test]
fn enums() {
    let output = run(&fixture("enums.solar"), "enums");
    assert_eq!(output, "0\n5\n16\n11\n1\n200\n99\n7\n42\n77\n");
}

#[test]
fn array_slice() {
    let output = run(&fixture("array_slice.solar"), "array_slice");
    assert_eq!(output, "20\n30\n40\n50\n99\n2\n3\n40\n50\n40\n300\n400\n");
}

#[test]
fn place_if_match() {
    let output = run(&fixture("place_if_match.solar"), "place_if_match");
    assert_eq!(output, "5\n0\n0\n10\n42\n0\n0\n77\n0\n99\n0\n");
}

#[test]
fn enum_refs() {
    let output = run(&fixture("enum_refs.solar"), "enum_refs");
    assert_eq!(output, "0\n20\n42\n");
}

#[test]
fn generics() {
    let output = run(&fixture("generics.solar"), "generics");
    assert_eq!(output, "42\n1\n99\n0\n77\n5\n123\n55\n");
}

#[test]
fn generic_functions() {
    let output = run(&fixture("generic_functions.solar"), "generic_functions");
    assert_eq!(output, "42\n1\n99\n77\n10\n123\n55\n");
}

#[test]
fn generic_fn_inference() {
    let output = run(
        &fixture("generic_fn_inference.solar"),
        "generic_fn_inference",
    );
    assert_eq!(output, "42\n1\n99\n77\n10\n123\n55\n42\n");
}

#[test]
fn methods() {
    let output = run(&fixture("methods.solar"), "methods");
    assert_eq!(output, "10\n10\n42\n99\n77\n-5\n30\n20\n");
}

#[test]
fn tuples() {
    let output = run(&fixture("tuples.solar"), "tuples");
    assert_eq!(
        output,
        "10\n20\n1\n2\n3\n42\n999\n200\n1\n2\n3\n15\n50\n60\n"
    );
}

#[test]
fn destructure() {
    let output = run(&fixture("destructure.solar"), "destructure");
    assert_eq!(
        output,
        "1\n2\n10\n20\n10\n20\n10\n20\n30\n1\n2\n3\n42\n7\n5\n6\n7\n"
    );
}

#[test]
fn overloads() {
    let output = run(&fixture("overloads.solar"), "overloads");
    assert_eq!(output, "1\n2\n3\n");
}

#[test]
fn if_expr_return() {
    let output = run(&fixture("if_expr_return.solar"), "if_expr_return");
    assert_eq!(output, "100\n30\n100\n30\n");
}

#[test]
fn for_loops() {
    let output = run(&fixture("for_loops.solar"), "for_loops");
    assert_eq!(output, "0\n1\n2\n3\n4\n1\n2\n3\n4\n");
}

#[test]
fn array_init() {
    let output = run(&fixture("array_init.solar"), "array_init");
    assert_eq!(output, "0\n1\n4\n0\n1\n4\n9\n10\n11\n12\n0\n2\n");
}

#[test]
fn array_init_infer() {
    let output = run(&fixture("array_init_infer.solar"), "array_init_infer");
    assert_eq!(output, "0\n1\n4\n0\n1\n4\n9\n10\n11\n12\n");
}

#[test]
fn nested_functions() {
    let output = run(&fixture("nested_functions.solar"), "nested_functions");
    assert_eq!(output, "15\n14\n-42\n305\n30\n7\n15\n42\n10\n100\n");
}

#[test]
fn adder() {
    let output = run(&fixture("adder.solar"), "adder");
    assert_eq!(output, "3\n9\n");
}

#[test]
fn make_ref() {
    let output = run(&fixture("make_ref.solar"), "make_ref");
    assert_eq!(output, "3\n5\n");
}

#[test]
fn fn_name_same_as_struct() {
    let output = run(
        &fixture("fn_name_same_as_struct.solar"),
        "fn_name_same_as_struct",
    );
    assert_eq!(output, "42\n");
}

#[test]
fn numeric_constructors() {
    let output = run(
        &fixture("numeric_constructors.solar"),
        "numeric_constructors",
    );
    assert_eq!(output, "42\n42\n42\n10\n10\n7\n7\n7\n5\n");
}

#[test]
fn if_expr_stmt() {
    let output = run(&fixture("if_expr_stmt.solar"), "if_expr_stmt");
    assert_eq!(output, "0\n101\n2\n103\n4\n99\n");
}

#[test]
fn unique_ptr() {
    let output = run(&fixture("unique_ptr.solar"), "unique_ptr");
    assert_eq!(output, "42\n100\n42\n999\n10\n99\n1\n999\n20\n77\n0\n100\n");
}

#[test]
fn generic_overloads() {
    let output = run(&fixture("generic_overloads.solar"), "generic_overloads");
    assert_eq!(output, "50\n1\n1\n0\n11\n22\n");
}

#[test]
fn unique_ptr_with_refs() {
    let output = run(
        &fixture("unique_ptr_with_refs.solar"),
        "unique_ptr_with_refs",
    );
    assert_eq!(output, "6\n6\n7\n");
}

#[test]
fn type_alias() {
    let output = run(&fixture("type_alias.solar"), "type_alias");
    assert_eq!(output, "42\n10\n20\n30\n40\n99\n77\n55\n");
}

#[test]
fn vec() {
    let output = run(&fixture("vec.solar"), "vec");
    assert_eq!(output, "10\n20\n30\n1\n2\n3\n4\n5\n999\n200\n");
}

#[test]
fn print_int_edge() {
    let output = run(&fixture("print_int_edge.solar"), "print_int_edge");
    assert_eq!(
        output,
        "0\n7\n-7\n42\n-42\n1000000\n0\n9\n123456789\n\
         -9223372036854775808\n9223372036854775807\n\
         18446744073709551615\n12345678901234567890\n\
         1844674407370955161\n1\n9223372036854775808\n1\n1\n1\n"
    );
}

#[test]
fn match_reflect() {
    let output = run(&fixture("match_reflect.solar"), "match_reflect");
    assert_eq!(
        output,
        "1\n2\n0\n0\n0\n0\n0\n1\n8\n42\n-1\n10\n20\n30\n100\n"
    );
}

#[test]
fn reflect_fields() {
    let output = run(&fixture("reflect_fields.solar"), "reflect_fields");
    assert_eq!(
        output,
        "x\n10\ny\n20\ncount\n7\n\
         x\n10\ny\n20\ncount\n7\n\
         not a struct\n\
         value\n99\n\
         10\n20\n\
         x\n10\ny\n20\n"
    );
}

#[test]
fn reflect_variant() {
    let output = run(&fixture("reflect_variant.solar"), "reflect_variant");
    assert_eq!(
        output,
        "Custom\nRed\n\
         Big\n99\nSmall\n3\n\
         Big\n99\n123\n\
         Custom\nRed\nnot an enum\n\
         Some\nNone\n"
    );
}

#[test]
fn reflect_combined() {
    let output = run(&fixture("reflect_combined.solar"), "reflect_combined");
    assert_eq!(
        output,
        "struct:\nx\n1\ny\n2\n\
         enum:\nSquare\n\
         enum:\nEmpty\n\
         other\n\
         struct:\nvalue\n99\n\
         enum:\nSome\n"
    );
}

#[test]
fn atomics() {
    let output = run(&fixture("atomics.solar"), "atomics");
    assert_eq!(output, "99\n1\n77\n10\n5\n99\n99\n99\n1\n2\n100\n200\n");
}

#[test]
fn nullable_ref() {
    let output = run(&fixture("nullable_ref.solar"), "nullable_ref");
    assert_eq!(output, "0\n1\n5\n1\n0\n5\n9\n1\n0\n100\n");
}

#[test]
fn file_open() {
    let output = run(&fixture("file_open.solar"), "file_open");
    assert_eq!(output, "opened\n");
}

#[test]
fn file_std_streams() {
    let output = run(&fixture("file_std_streams.solar"), "file_std_streams");
    assert_eq!(output, "std streams ok\n");
}

#[test]
fn file_io() {
    let output = run(&fixture("file_io.solar"), "file_io");
    assert_eq!(output, "5\nhello world\n");
}
