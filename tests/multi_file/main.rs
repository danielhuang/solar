use std::path::Path;
use test_utils::run;

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/multi_file")
        .join(name)
}

#[test]
fn named_import() {
    let output = run(&fixture("named_import/main.solar"), "named_import");
    assert_eq!(output, "7\n30\n");
}

#[test]
fn wildcard_import() {
    let output = run(&fixture("wildcard_import/main.solar"), "wildcard_import");
    assert_eq!(output, "10\n9\n");
}

#[test]
fn module_import() {
    let output = run(&fixture("module_import/main.solar"), "module_import");
    assert_eq!(output, "0\n0\n");
}

#[test]
fn pub_field_access() {
    let output = run(&fixture("pub_field/main.solar"), "pub_field");
    assert_eq!(output, "10\n20\n");
}

#[test]
fn diamond_dependency() {
    let output = run(&fixture("diamond/main.solar"), "diamond");
    assert_eq!(output, "10\n20\n");
}

#[test]
#[should_panic(expected = "field `hidden` is private")]
fn private_field_access() {
    run(&fixture("private_field/main.solar"), "private_field");
}

#[test]
#[should_panic(expected = "`private_fn` is not exported")]
fn private_fn_import() {
    run(&fixture("private_fn/main.solar"), "private_fn");
}

#[test]
fn many_modules() {
    let output = run(&fixture("many_modules/main.solar"), "many_modules");
    assert_eq!(output, "1\n");
}

#[test]
fn multiple_mains_a() {
    let output = run(&fixture("multiple_mains/a.solar"), "multiple_mains_a");
    assert_eq!(output, "1\n3\n4\n");
}

#[test]
fn multiple_mains_b() {
    let output = run(&fixture("multiple_mains/b.solar"), "multiple_mains_b");
    assert_eq!(output, "2\n3\n4\n");
}

#[test]
#[should_panic(expected = "`bad` is not exported from `a.solar`")]
fn bad_import() {
    run(&fixture("bad_import/main.solar"), "bad_import");
}

#[test]
#[should_panic(expected = "unknown intrinsic: `does_not_exist`")]
fn bad_intrinsic() {
    run(&fixture("bad_intrinsic/main.solar"), "bad_intrinsic");
}

#[test]
fn path_import() {
    let output = run(&fixture("path_import/main.solar"), "path_import");
    assert_eq!(output, "3\n7\n");
}

#[test]
fn path_import_mixed() {
    let output = run(
        &fixture("path_import_mixed/main.solar"),
        "path_import_mixed",
    );
    assert_eq!(output, "10\n");
}

#[test]
fn intrinsic_import() {
    let output = run(&fixture("intrinsic_import/main.solar"), "intrinsic_import");
    assert_eq!(output, "hello");
}

#[test]
fn type_alias_import() {
    let output = run(
        &fixture("type_alias_import/main.solar"),
        "type_alias_import",
    );
    assert_eq!(output, "10\n20\n42\n");
}

#[test]
#[should_panic(expected = "atomic_store: type Color is not atomic-compatible")]
fn bad_atomic_enum() {
    run(&fixture("bad_atomic_enum/main.solar"), "bad_atomic_enum");
}

#[test]
#[should_panic(expected = "atomic_store: type ^Int is not atomic-compatible")]
fn bad_atomic_unique() {
    run(
        &fixture("bad_atomic_unique/main.solar"),
        "bad_atomic_unique",
    );
}

#[test]
#[should_panic(expected = "atomic_store: type Bad is not atomic-compatible")]
fn bad_atomic_struct_with_unique() {
    run(
        &fixture("bad_atomic_struct_with_unique/main.solar"),
        "bad_atomic_struct_with_unique",
    );
}

#[test]
#[should_panic(expected = "atomic_compare_exchange: type Bad is not atomic-compatible")]
fn bad_atomic_size() {
    run(&fixture("bad_atomic_size/main.solar"), "bad_atomic_size");
}
