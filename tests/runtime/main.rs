use std::path::{Path, PathBuf};
use test_utils::run;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/runtime")
        .join(name)
}

#[test]
fn assert() {
    let output = run(&fixture("assert.solar"), "assert");
    assert_eq!(output, "passed\nassertion failed\n");
}

#[test]
fn black_box() {
    let output = run(&fixture("black_box.solar"), "black_box");
    assert_eq!(output, "41\n42\n16\n5\n");
}

#[test]
fn any() {
    let path = fixture("any.solar");
    let output = run(&path, "any");
    assert_eq!(output, "42\n1\n16\n1\n77\n");

    let c = solar::pipeline::compile(&path)
        .unwrap()
        .to_mangled()
        .to_ir()
        .to_c(&path.display().to_string())
        .c_source;
    assert!(c.contains("UINT64_C(0x00ff0000000000"));
    assert!(c.contains("sol_store_128_unordered(_"));
    assert!(c.contains("sol_load_128_unordered(_"));
    assert!(c.contains("sol_copy_128_unordered(_"));
}

#[test]
fn gc_keepalive() {
    let output = run(&fixture("gc_keepalive.solar"), "gc_keepalive");
    assert_eq!(output, "42\n9\n");
}

#[test]
fn synthetic_ident_collision() {
    let output = run(
        &fixture("synthetic_ident_collision.solar"),
        "synthetic_ident_collision",
    );
    assert_eq!(
        output,
        "91\n92\n93\n94\n95\n7\n96\n97\n97\n98\n99\n17\n41\n42\n"
    );
}

#[test]
fn try_control_flow() {
    let output = run(&fixture("try_control_flow.solar"), "try_control_flow");
    assert_eq!(
        output,
        "10\n20\n30\n1\n3\n4\n11\n13\n4\n40\n50\n60\n70\n80\n"
    );
}

#[test]
fn hashbrown_tag() {
    let output = run(&fixture("hashbrown_tag.solar"), "hashbrown_tag");
    assert_eq!(output, "255\n128\n127\n0\n1\n0\n1\n0\n1\n0\n");
}

#[test]
fn tuple_struct() {
    let output = run(&fixture("tuple_struct.solar"), "tuple_struct");
    assert_eq!(output, "7\n1\n9\n");
}

#[test]
fn layout_packing() {
    let output = run(&fixture("layout_packing.solar"), "layout_packing");
    assert_eq!(
        output,
        "7\nwide\n300\n9000\n8\n9001\nbyte\nwide\nhalf\nword\n7\n42\n9\n123\n12\n"
    );
}

#[test]
fn repr_c() {
    let output = run(&fixture("repr_c.solar"), "repr_c");
    assert_eq!(output, "24\n24\n7\n9000\n300\n");
}

#[test]
fn out_generics_and_size_of() {
    let path = fixture("out_generics.solar");
    let output = run(&path, "out_generics_and_size_of");
    assert_eq!(output, "1\n2\n8\n16\n6\n16\n32\n4\n3\n42\n17\n3\n");

    let typed = solar::pipeline::compile(&path).unwrap().typed;
    let instances: Vec<_> = typed
        .functions
        .iter()
        .filter(|(id, _)| id.def.name == "size_of")
        .collect();
    assert!(!instances.is_empty());
    assert!(
        instances
            .iter()
            .any(|(id, _)| id.def.file != solar::ast::SYNTHETIC_FILE)
    );
    assert!(
        instances
            .iter()
            .any(|(id, _)| id.def.file == solar::ast::SYNTHETIC_FILE)
    );
    for (id, function) in instances {
        assert_eq!(id.args.len(), 1);
        assert!(function.parameters.is_empty());
        assert!(matches!(
            function.body.as_slice(),
            [solar::typed_ast::Statement {
                kind: solar::typed_ast::StatementKind::Expression(solar::typed_ast::Expr {
                    kind: solar::typed_ast::ExprKind::IntegerLiteral(_),
                    ..
                }),
                ..
            }]
        ));
    }
}

#[test]
fn hashbrown_bitmask() {
    let output = run(&fixture("hashbrown_bitmask.solar"), "hashbrown_bitmask");
    assert_eq!(output, "0\n2\n5\n0\n");
}

#[test]
fn hashbrown_group() {
    let output = run(&fixture("hashbrown_group.solar"), "hashbrown_group");
    assert_eq!(
        output,
        "0\n1\n4\n99\n2\n6\n99\n2\n3\n6\n7\n99\n0\n1\n4\n5\n8\n9\n10\n11\n12\n13\n14\n15\n99\n"
    );
}

#[test]
fn hashbrown_hash() {
    let output = run(&fixture("hashbrown_hash.solar"), "hashbrown_hash");
    assert_eq!(
        output,
        "17103352462266903514\n12646608726195247393\n12646608726195247393\n\
         12056765366625275561\n5134803880729456624\n10895394381029562297\n\
         2784080265892256596\n"
    );
}

#[test]
fn hashbrown_map() {
    let output = run(&fixture("hashbrown_map.solar"), "hashbrown_map");
    assert_eq!(
        output,
        "0\n30\n30\n777\n1\n0\n4350\n50\n-1\n29\n777\n5555\n30\n6666\n30\n11\n777\n0\n1\n"
    );
}

#[test]
fn empty_array() {
    let output = run(&fixture("empty_array.solar"), "empty_array");
    assert_eq!(output, "0\n1\n3\n0\n0\n7\n2\n1\n3\n2\n1\n2\ndone\n");
}

#[test]
fn hoist_capture() {
    let output = run(&fixture("hoist_capture.solar"), "hoist_capture");
    assert_eq!(output, "0\n10\n20\n30\n");
}

#[test]
fn inline_attr() {
    let output = run(&fixture("inline_attr.solar"), "inline_attr");
    assert_eq!(output, "25\n6\n21\n42\n");
}

#[test]
fn match_call() {
    let output = run(&fixture("match_call.solar"), "match_call");
    assert_eq!(output, "150\n1\n0\n");
}

// ASAN checks that values referenced beyond their frame remain heap allocated.
#[test]
fn escape_refs() {
    let output = run(&fixture("escape_refs.solar"), "escape_refs");
    assert_eq!(output, "111\n222\n333\n444\n502\n");
}

#[test]
fn match_call_basic() {
    let output = run(&fixture("match_call_basic.solar"), "match_call_basic");
    assert_eq!(output, "5\n15\n1\n0\n");
}

#[test]
fn reflect_eq() {
    let output = run(&fixture("reflect_eq.solar"), "reflect_eq");
    assert_eq!(output, "1\n0\n1\n1\n0\n1\n0\n1\n0\n1\n0\n1\n0\n");
}

#[test]
fn hashbrown_foldhash() {
    // Values are bit-exact with upstream foldhash's fixed-seed `FoldHasher`.
    let output = run(&fixture("hashbrown_foldhash.solar"), "hashbrown_foldhash");
    assert_eq!(
        output,
        "17103352462266903514\n6908261415171690783\n12646608726195247393\n\
         1980524245637793224\n799436778835710610\n2784080265892256596\n\
         18047866850227357006\n589684135938649225\n"
    );
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
fn if_else_control_flow() {
    let output = run(
        &fixture("if_else_control_flow.solar"),
        "if_else_control_flow",
    );
    assert_eq!(output, "3\n3\n12\n400\n3\n");
}

#[test]
fn binop_arithmetic() {
    let output = run(&fixture("binop_arithmetic.solar"), "binop_arithmetic");
    assert_eq!(output, "5\n6\n21\n5\n2\n14\n20\n");
}

#[test]
fn bitwise() {
    let output = run(&fixture("bitwise.solar"), "bitwise");
    assert_eq!(
        output,
        "8\n14\n6\n16\n16\n0\n0\n8\n18446744073709551615\n3\n24\n5\n\
         1\n1\n1\n1\n1\n1\n1\n1\n1\n1\n1\n"
    );
}

#[test]
fn char_literal() {
    let output = run(&fixture("char_literal.solar"), "char_literal");
    assert_eq!(output, "1\n1\n1\n1\n1\n1\n1\n1\n1\n1\n1\nHi!\n");
}

#[test]
fn bit_count() {
    let output = run(&fixture("bit_count.solar"), "bit_count");
    assert_eq!(output, "3\n64\n0\n63\n64\n64\n3\n3\n8\n7\n8\n8\n3\n8\n31\n");
}

#[test]
fn from_le() {
    let output = run(&fixture("from_le.solar"), "from_le");
    assert_eq!(
        output,
        "650777868590383874\n84148994\n578437695752307201\n67305985\n"
    );
}

#[test]
fn carrying_mul_add() {
    let output = run(&fixture("carrying_mul_add.solar"), "carrying_mul_add");
    assert_eq!(
        output,
        "39\n0\n1\n18446744073709551614\n18446744073709551614\n1\n18446744073709551615\n18446744073709551615\n"
    );
}

#[test]
fn wrapping() {
    let output = run(&fixture("wrapping.solar"), "wrapping");
    assert_eq!(output, "1\n1\n1\n1\n1\n1\n1\n1\n1\n1\n15\n42\n");
}

#[test]
fn binop_comparison() {
    let output = run(&fixture("binop_comparison.solar"), "binop_comparison");
    assert_eq!(output, "1\n0\n1\n0\n1\n0\n1\n0\n1\n0\n1\n0\n");
}

#[test]
fn kwargs() {
    let output = run(&fixture("kwargs.solar"), "kwargs");
    assert_eq!(output, "25\n205\n35\n6\n6\nn=7\nv=9\n101\n105\n");
}

#[test]
fn consts() {
    let output = run(&fixture("consts.solar"), "consts");
    assert_eq!(output, "go\n130\n7\n3\n");
}

#[test]
fn int_radix() {
    let output = run(&fixture("int_radix.solar"), "int_radix");
    assert_eq!(output, "73\n511\n255\n255\n16\n83\n255\n171\n0\n");
}

#[test]
fn break_continue() {
    let output = run(&fixture("break_continue.solar"), "break_continue");
    assert_eq!(
        output,
        "0\n1\n2\n1\n3\n4\n5\n1\n3\n5\n100\n101\n10\n30\n1000\n1001\n1010\n1011\n1020\n1021\n"
    );
}

#[test]
fn loop_expr() {
    let output = run(&fixture("loop_expr.solar"), "loop_expr");
    assert_eq!(output, "5\n50\n1\n3\n8\n");
}

#[test]
fn array_len() {
    let output = run(&fixture("array_len.solar"), "array_len");
    assert_eq!(output, "4\n5\n4\n2\n0\n");
}

#[test]
fn binop_logic() {
    let output = run(&fixture("binop_logic.solar"), "binop_logic");
    assert_eq!(output, "1\n0\n0\n1\n0\n1\n0\n88\n1\n1\n99\n0\n0\n1\n");
}

#[test]
fn binop_array_eq() {
    let output = run(&fixture("binop_array_eq.solar"), "binop_array_eq");
    assert_eq!(output, "1\n0\n0\n1\n0\n");
}

#[test]
fn operator_overload() {
    let output = run(&fixture("operator_overload.solar"), "operator_overload");
    assert_eq!(output, "11\n22\n9\n18\n3\n6\n1\n0\n3\n3\n1\n");
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
    let source = format!("fn main() {{\n  println({open}42{close});\n}}\n");
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
    assert_eq!(output, "0\n5\n16\n11\n1\n200\n99\n0\n1\n2\n7\n42\n77\n");
}

#[test]
fn array_slice() {
    let output = run(&fixture("array_slice.solar"), "array_slice");
    assert_eq!(output, "20\n30\n40\n50\n99\n2\n3\n40\n50\n40\n300\n400\n");
}

#[test]
fn slice_contains() {
    let output = run(&fixture("slice_contains.solar"), "slice_contains");
    assert_eq!(output, "1\n0\n1\n1\n0\n0\n1\n1\n1\n0\n1\n1\n0\n1\n");
}

#[test]
fn slice_find() {
    let output = run(&fixture("slice_find.solar"), "slice_find");
    assert_eq!(output, "1\n99\n1\n3\n99\n0\n2\n");
}

#[test]
fn slice_split() {
    let output = run(&fixture("slice_split.solar"), "slice_split");
    assert_eq!(
        output,
        "1\n1\n2\n2\n3\n1\n4\n99\n0\n0\n1\n7\n0\n99\n2\n8\n9\n99\n0\n99\n1\n5\n1\n6\n1\n5\n"
    );
}

#[test]
fn option_is_some() {
    let output = run(&fixture("option_is_some.solar"), "option_is_some");
    assert_eq!(output, "1\n0\n");
}

#[test]
fn slice_iter() {
    let output = run(&fixture("slice_iter.solar"), "slice_iter");
    assert_eq!(output, "10\n20\n10\n20\n30\n0\n30\n0\n0\n");
}

#[test]
fn slice_eq() {
    let output = run(&fixture("slice_eq.solar"), "slice_eq");
    assert_eq!(output, "1\n0\n0\n1\n0\n0\n");
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
fn for_in_duck_iter() {
    let output = run(&fixture("for_in_duck_iter.solar"), "for_in_duck_iter");
    assert_eq!(output, "90\n3\n5\n");
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
fn unique_ref() {
    let output = run(&fixture("unique_ref.solar"), "unique_ref");
    assert_eq!(
        output,
        "42\n100\n42\n999\n10\n99\n1\n999\n20\n77\n0\n100\n5\n"
    );
}

#[test]
fn generic_overloads() {
    let output = run(&fixture("generic_overloads.solar"), "generic_overloads");
    assert_eq!(output, "50\n1\n1\n0\n11\n22\n");
}

#[test]
fn unique_ref_with_refs() {
    let output = run(
        &fixture("unique_ref_with_refs.solar"),
        "unique_ref_with_refs",
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
fn vec_iter() {
    let output = run(&fixture("vec_iter.solar"), "vec_iter");
    assert_eq!(output, "3\n10\n20\n30\n60\n3\n999\n0\n2\n");
}

#[test]
fn vec_iter_copy() {
    let output = run(&fixture("vec_iter_copy.solar"), "vec_iter_copy");
    assert_eq!(output, "10\n20\n10\n20\n30\n30\n");
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
        "Custom\n2\nRed\n0\n\
         Big\n1\n99\nSmall\n0\n3\n\
         Big\n1\n99\n123\n\
         Custom\n2\nRed\n0\nnot an enum\n\
         Some\n0\nNone\n1\n"
    );
}

#[test]
fn reflect_combined() {
    let output = run(&fixture("reflect_combined.solar"), "reflect_combined");
    assert_eq!(
        output,
        "struct:\nx\n1\ny\n2\n\
         enum:\nSquare\n1\n\
         enum:\nEmpty\n2\n\
         other\n\
         struct:\nvalue\n99\n\
         enum:\nSome\n0\n"
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
fn file_std_streams() {
    let output = run(&fixture("file_std_streams.solar"), "file_std_streams");
    assert_eq!(output, "std streams ok\n");
}

// Every fallible runtime intrinsic (checked arithmetic, bounds/null/length
// checks, file errors) throws a *catchable* Solar exception whose message is
// byte-identical across the three backends.
#[test]
fn catch_runtime_errors() {
    let output = run(
        &fixture("catch_runtime_errors.solar"),
        "catch_runtime_errors",
    );
    assert_eq!(
        output,
        "integer overflow in addition\n\
         integer overflow in subtraction\n\
         integer overflow in multiplication\n\
         integer division by zero\n\
         integer overflow in division\n\
         integer modulo by zero\n\
         index out of bounds: index is 5 but length is 3\n\
         slice end (5) > length (3)\n\
         slice start (2) > end (1)\n\
         null dereference\n\
         array length mismatch: expected 2 elements, got 3\n\
         array length mismatch: expected 2 elements, got 3\n\
         done\n"
    );
}

#[test]
fn throw_try() {
    let output = run(&fixture("throw_try.solar"), "throw_try");
    assert_eq!(
        output,
        "checked ok\nno throw\ncaught:\ntoo big\ncaught nested:\ntoo big\ndone\n"
    );
}

#[test]
fn throw_alias() {
    let output = run(&fixture("throw_alias.solar"), "throw_alias");
    assert_eq!(output, "ABC\nZBC\n");
}

#[test]
fn closure_capture_unsized() {
    let output = run(
        &fixture("closure_capture_unsized.solar"),
        "closure_capture_unsized",
    );
    assert_eq!(output, "ZBX\n");
}

#[test]
fn time() {
    let output = run(&fixture("time.solar"), "time");
    assert_eq!(output, "mono ok\nsys ok\n");
}

#[test]
fn rvalue_field() {
    let output = run(&fixture("rvalue_field.solar"), "rvalue_field");
    assert_eq!(output, "3\n4\n8\n18\n");
}

#[test]
fn blanket_eq_throw() {
    let output = run(&fixture("blanket_eq_throw.solar"), "blanket_eq_throw");
    assert_eq!(
        output,
        "contents equal through refs\nsame ref\ndistinct refs\ncontents equal\nstructs equal\n"
    );
}

#[test]
fn ipv6_parse() {
    let output = run(&fixture("ipv6_parse.solar"), "ipv6_parse");
    assert_eq!(
        output,
        "::1\n0\n0\n0\n0\n0\n0\n0\n0\n0\n0\n0\n0\n0\n0\n0\n1\n\
         1:2:3:4:5:6:7:8\n0\n1\n0\n2\n0\n3\n0\n4\n0\n5\n0\n6\n0\n7\n0\n8\n\
         2001:Db8::8a2e:370:7334\n32\n1\n13\n184\n0\n0\n0\n0\n0\n0\n138\n46\n3\n112\n115\n52\n\
         ::ffff:192.168.1.10\n0\n0\n0\n0\n0\n0\n0\n0\n0\n0\n255\n255\n192\n168\n1\n10\n\
         fe80::\n254\n128\n0\n0\n0\n0\n0\n0\n0\n0\n0\n0\n0\n0\n0\n0\n\
         ::\n0\n0\n0\n0\n0\n0\n0\n0\n0\n0\n0\n0\n0\n0\n0\n0\n\
         203\n0\n113\n7\n"
    );
}

#[test]
fn linux_errors() {
    let output = run(&fixture("linux_errors.solar"), "linux_errors");
    assert_eq!(
        output,
        "Operation not permitted\n\
         Too many levels of symbolic links\n\
         No message of desired type\n\
         Bad message\n\
         Value too large for defined data type\n\
         No buffer space available\n\
         Transport endpoint is already connected\n\
         Memory page has hardware error\n\
         Unknown error\n\
         Unknown error\n\
         operation failed: Permission denied (os error 13)\n\
         done\n"
    );
}

#[test]
fn statics() {
    let output = run(&fixture("statics.solar"), "statics");
    assert_eq!(
        output,
        "17\n18\n3\n9\ninitial\nswapped\nlate is null\n42\nflag set\n13\n"
    );
}

#[test]
fn num_cpus() {
    let output = run(&fixture("num_cpus.solar"), "num_cpus");
    assert_eq!(output, "at least one\nstable\n");
}

#[test]
fn float_math() {
    let output = run(&fixture("float_math.solar"), "float_math");
    assert_eq!(
        output,
        "4000000\n3750000\n1000000\n1666666\n1500000\nlt\nge\nne\nf32 lt\n1\n\
         1414213\n3\n4\n3\n-3\n5\n1024\n1000000\n1\n0\n3000000\n785398\n785398\n1570796\n0\n\
         3\n256\n7\n333333\n"
    );
}

#[test]
fn unit_return() {
    let output = run(&fixture("unit_return.solar"), "unit_return");
    assert_eq!(output, "42\n");
}

#[test]
fn nested_closure_capture() {
    let output = run(
        &fixture("nested_closure_capture.solar"),
        "nested_closure_capture",
    );
    assert_eq!(output, "30\n200\n-10\n");
}

#[test]
fn closure_capture_in_fn() {
    let output = run(
        &fixture("closure_capture_in_fn.solar"),
        "closure_capture_in_fn",
    );
    assert_eq!(output, "7\n");
}

#[test]
fn loop_break_enum() {
    let output = run(&fixture("loop_break_enum.solar"), "loop_break_enum");
    assert_eq!(output, "B 42\n43\n18\n");
}

#[test]
fn loop_break_nested() {
    let output = run(&fixture("loop_break_nested.solar"), "loop_break_nested");
    assert_eq!(output, "300\n1005\n30\n28\n");
}

// Repeat to cover nondeterministic lowering order.
#[test]
fn closure_capture_fn() {
    for i in 0..5 {
        let output = run(
            &fixture("closure_capture_fn.solar"),
            &format!("closure_capture_fn_{i}"),
        );
        assert_eq!(output, "21\n15\n16\n112\n18\n106\n1015\n");
    }
}

// Covers self- and mutually-recursive generic functions and methods.
#[test]
fn generic_recursion() {
    let output = run(&fixture("generic_recursion.solar"), "generic_recursion");
    assert_eq!(output, "4\n2\n110\neven\nodd7\n42\n");
}

// Covers nullable fields whose pointees are generic instantiations.
#[test]
fn nullable_ref_generic_field() {
    let output = run(
        &fixture("nullable_ref_generic_field.solar"),
        "nullable_ref_generic_field",
    );
    assert_eq!(output, "block null\nblock set\n7\n42\n");
}

#[test]
fn tail_if_eof() {
    let output = run(&fixture("tail_if_eof.solar"), "tail_if_eof");
    assert_eq!(output, "10\n20\n");
}

// Escape analysis must retain the owner of an escaping interior reference.
#[test]
fn interior_ref_escape() {
    let output = run(&fixture("interior_ref_escape.solar"), "interior_ref_escape");
    assert_eq!(output, "15\n35\n0\n3\n6\n9\n15\n33\n");
}

// A returned closure must retain the scrutinee aliased by its match binding.
#[test]
fn closure_captures_match_binding() {
    let output = run(
        &fixture("closure_captures_match_binding.solar"),
        "closure_captures_match_binding",
    );
    assert_eq!(output, "735\n124\n731\n");
}

// Fixed-array coercion from a value must use the type's static length.
#[test]
fn fixed_array_from_branch() {
    let output = run(
        &fixture("fixed_array_from_branch.solar"),
        "fixed_array_from_branch",
    );
    assert_eq!(output, "7\n8\n3\n3\n20\n30\n");
}

// Overlapping copies must use memmove semantics on every backend.
#[test]
fn overlap_copy() {
    let output = run(&fixture("overlap_copy.solar"), "overlap_copy");
    assert_eq!(
        output,
        "3\n7\n8\n7\n8\n7\n8\n10\n20\n30\n2\n3\n4\n\
         2\n3\n4\n4\n5\n1\n1\n2\n3\n5\n\
         97\n97\n98\n99\n100\n98\n99\n100\n101\n101\n\
         1\n11\n1\n11\n2\n22\n3\n33\n5\n55\n\
         2\n22\n3\n33\n4\n44\n4\n44\n5\n55\n\
         1\n1\n2\n888\n5\n2\n888\n4\n4\n5\n\
         1\n1\n2\n3\n5\n2\n3\n4\n4\n5\n2\n0\n"
    );
}

#[test]
fn nullable_unsized_deref() {
    let output = run(
        &fixture("nullable_unsized_deref.solar"),
        "nullable_unsized_deref",
    );
    assert_eq!(output, "1\n3\n3\n2\n3\n");
}

#[test]
fn unsized_if_local() {
    let output = run(&fixture("unsized_if_local.solar"), "unsized_if_local");
    assert_eq!(output, "3\n20\n22\n1\nabcdefghijklmnopqrstuvwx\n");
}

#[test]
fn call_result_place() {
    let output = run(&fixture("call_result_place.solar"), "call_result_place");
    assert_eq!(output, "42\n30\nhello\n7\n13\n");
}

#[test]
fn bitwise_parenthesized_rhs_is_not_a_postfix_reference() {
    let output = run(
        &fixture("bitwise_parenthesized_rhs.solar"),
        "bitwise_parenthesized_rhs",
    );
    assert_eq!(output, "8\n7\n");
}

#[test]
fn zero_parameter_closure_call_body_is_not_a_parameter_list() {
    let output = run(
        &fixture("zero_parameter_closure_call_body.solar"),
        "zero_parameter_closure_call_body",
    );
    assert_eq!(output, "5\n13\n");
}

#[test]
fn bare_return_in_unit_function() {
    let output = run(&fixture("bare_return.solar"), "bare_return");
    assert_eq!(output, "99\n");
}

#[test]
fn integer_literal_match_patterns() {
    let output = run(
        &fixture("integer_match_patterns.solar"),
        "integer_match_patterns",
    );
    assert_eq!(output, "10\n20\n37\n80\n7\n64\n9\n102\n");
}

#[test]
fn integer_match_scrutinee_is_evaluated_once() {
    let output = run(
        &fixture("integer_match_scrutinee_once.solar"),
        "integer_match_scrutinee_once",
    );
    assert_eq!(output, "10\n1\n");
}

#[test]
fn nullable_unsized_concat_does_not_require_reborrow() {
    let output = run(
        &fixture("nullable_unsized_concat.solar"),
        "nullable_unsized_concat",
    );
    assert_eq!(output, "abab\n");
}

// Intrinsic calls must be loadable when used directly as values.
#[test]
fn intrinsic_call_value() {
    let output = run(
        &fixture("intrinsic_call_value.solar"),
        "intrinsic_call_value",
    );
    assert_eq!(output, "1\n1\n1\n");
}

// Tail expressions and explicit returns must apply the same coercions.
#[test]
fn tail_expr_coerce() {
    let output = run(&fixture("tail_expr_coerce.solar"), "tail_expr_coerce");
    assert_eq!(output, "1\n1\n3\n3\n");
}

#[test]
fn unsafe_access_and_safe_function_values() {
    let output = run(&fixture("unsafe.solar"), "unsafe");
    assert_eq!(output, "2\n4\n6\n8\n10\n101\n");
}

#[test]
fn transmute_preserves_bytes_across_backends() {
    let output = run(&fixture("transmute.solar"), "transmute");
    assert_eq!(output, "67305985\n1\n2\n3\n4\n1065353216\n1\n1\n4660\n");
}

#[test]
fn transmute_ref_reinterprets_sized_and_unsized_references() {
    let output = run(&fixture("transmute_ref.solar"), "transmute_ref");
    assert_eq!(output, "3\n10\n30\n25\n7\n3\n50\n9\n70\n");
}

#[test]
fn offset_ref_scales_by_sized_pointee() {
    let output = run(&fixture("offset_ref.solar"), "offset_ref");
    assert_eq!(output, "30\n10\n40\n");
}
