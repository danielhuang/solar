//! Temporary GC buffers write directly into their sole copy destination.

use std::process::Command;

fn optimize(source: &str) -> String {
    let directory = tempdir::TempDir::new("solar-alloc-forward").unwrap();
    let input = directory.path().join("input.ll");
    let output = directory.path().join("output.ll");
    std::fs::write(&input, source).unwrap();
    let result = Command::new("opt")
        .arg(format!("-load-pass-plugin={}", env!("SOLAR_WB_PLUGIN")))
        .args(["-passes=solar-forward-gc-alloc,verify", "-S"])
        .arg(input)
        .arg("-o")
        .arg(&output)
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    std::fs::read_to_string(output).unwrap()
}

fn body<'a>(ir: &'a str, name: &str) -> &'a str {
    ir.split(&format!("define ptr @{name}("))
        .nth(1)
        .unwrap()
        .split("\n}")
        .next()
        .unwrap()
}

#[test]
fn forwards_stores_slices_memsets_and_specialized_allocations() {
    let ir = optimize(include_str!("release_alloc_forward/forward.ll"));
    for name in [
        "solar_stores",
        "solar_slice",
        "solar_memset",
        "solar_specialized",
    ] {
        let function = body(&ir, name);
        assert_eq!(
            function.matches("call ptr @sol_alloc").count(),
            1,
            "{function}"
        );
        assert!(!function.contains("%temporary"), "{function}");
        assert!(!function.contains("call void @llvm.memcpy"), "{function}");
        assert!(!function.contains("call void @llvm.memmove"), "{function}");
    }
    let stores = body(&ir, "solar_stores");
    let observe = stores.find("call void @observe(ptr %destination)").unwrap();
    let first = stores.find("store i64 %x").unwrap();
    let overwrite = stores.find("store i64 %y").unwrap();
    assert!(observe < first && first < overwrite, "{stores}");
    assert!(!stores.contains("align 16"), "{stores}");
    assert!(!stores.contains("!alias.scope"), "{stores}");
    assert!(
        stores.contains("getelementptr i8, ptr %destination, i64 1"),
        "{stores}"
    );
    assert!(
        stores.contains("getelementptr i8, ptr %offset, i64 8"),
        "{stores}"
    );

    let slice = body(&ir, "solar_slice");
    assert!(
        slice.contains("store i8 48, ptr %destination, align 1"),
        "{slice}"
    );
    assert!(
        slice.contains("store i8 10, ptr %newline, align 1"),
        "{slice}"
    );
    assert!(!slice.contains("i64 19"), "{slice}");
    let set = body(&ir, "solar_memset");
    assert!(set.contains("ptr align 1 %offset, i8 7, i64 16"), "{set}");
}

#[test]
fn forwarded_buffers_preserve_contents_and_adjacent_bytes() {
    let directory = tempdir::TempDir::new("solar-alloc-forward").unwrap();
    let input = directory.path().join("forward.ll");
    let harness = directory.path().join("forward.c");
    let binary = directory.path().join("forward");
    std::fs::write(
        &input,
        optimize(include_str!("release_alloc_forward/forward.ll")),
    )
    .unwrap();
    std::fs::write(&harness, include_str!("release_alloc_forward/forward.c")).unwrap();
    let result = Command::new("clang")
        .args(["-O0", "-fsanitize=address"])
        .arg(input)
        .arg(harness)
        .arg("-o")
        .arg(&binary)
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    let result = Command::new(binary).output().unwrap();
    assert!(result.status.success(), "{result:?}");
}

const BASE: &str = r#"
target datalayout = "e-p:64:64-i64:64"
declare ptr @sol_alloc(i64, i64, ptr)
declare void @observe(ptr)
declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1 immarg)
define ptr @solar_example(i64 %n) {
entry:
  %temporary = call ptr @sol_alloc(i64 8, i64 8, ptr null)
  store i64 42, ptr %temporary, align 8
  %destination = call ptr @sol_alloc(i64 16, i64 1, ptr null)
  call void @llvm.memcpy.p0.p0.i64(ptr %destination, ptr %temporary, i64 8, i1 false)
  ret ptr %destination
}
"#;

#[test]
fn preserves_buffers_when_forwarding_is_not_proven_safe() {
    let cases = [
        (
            "escaping",
            "store i64 42, ptr %temporary, align 8",
            "call void @observe(ptr %temporary)",
        ),
        (
            "read",
            "store i64 42, ptr %temporary, align 8",
            "%read = load i64, ptr %temporary",
        ),
        (
            "self_reference",
            "store i64 42, ptr %temporary, align 8",
            "store ptr %temporary, ptr %temporary",
        ),
        ("volatile_store", "store i64 42", "store volatile i64 42"),
        (
            "atomic_store",
            "store i64 42, ptr %temporary, align 8",
            "store atomic i64 42, ptr %temporary unordered, align 8",
        ),
        ("volatile_copy", "i1 false", "i1 true"),
        (
            "copy_bundle",
            "i64 8, i1 false)",
            "i64 8, i1 false) [\"deopt\"(ptr %temporary)]",
        ),
        (
            "dynamic_copy",
            "ptr %temporary, i64 8, i1",
            "ptr %temporary, i64 %n, i1",
        ),
        ("short_destination", "@sol_alloc(i64 16", "@sol_alloc(i64 4"),
        ("dynamic_source", "@sol_alloc(i64 8", "@sol_alloc(i64 %n"),
        (
            "dynamic_destination",
            "@sol_alloc(i64 16",
            "@sol_alloc(i64 %n",
        ),
        (
            "partial_store",
            "ptr %temporary, i64 8, i1",
            "ptr %temporary, i64 4, i1",
        ),
        (
            "source_overrun",
            "ptr %temporary, i64 8, i1",
            "ptr %temporary, i64 16, i1",
        ),
        (
            "late_write",
            "ret ptr %destination",
            "store i64 13, ptr %temporary\nret ptr %destination",
        ),
        (
            "late_read",
            "ret ptr %destination",
            "%read = load i64, ptr %temporary\nret ptr %destination",
        ),
        (
            "second_copy",
            "ret ptr %destination",
            "call void @llvm.memcpy.p0.p0.i64(ptr %destination, ptr %temporary, i64 8, i1 false)\nret ptr %destination",
        ),
        (
            "control_flow",
            "%destination = call",
            "br label %next\nnext:\n%destination = call",
        ),
        (
            "dynamic_offset",
            "store i64 42, ptr %temporary, align 8",
            "%offset = getelementptr i8, ptr %temporary, i64 %n\nstore i8 42, ptr %offset",
        ),
        (
            "negative_offset",
            "store i64 42, ptr %temporary, align 8",
            "%offset = getelementptr i8, ptr %temporary, i64 -1\nstore i8 42, ptr %offset",
        ),
        (
            "pointer_identity",
            "store i64 42, ptr %temporary, align 8",
            "%address = ptrtoint ptr %temporary to i64\nstore i64 %address, ptr %temporary",
        ),
    ];
    for (name, from, to) in cases {
        let ir = optimize(&BASE.replace(from, to));
        let function = body(&ir, "solar_example");
        assert_eq!(
            function.matches("call ptr @sol_alloc").count(),
            2,
            "{name}: {function}"
        );
        assert!(
            function.contains("call void @llvm.memcpy"),
            "{name}: {function}"
        );
    }
}
