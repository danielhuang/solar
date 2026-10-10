//! Deferred publication of precise GC mark functions.

use std::process::Command;

#[test]
fn publishes_scalar_and_batch_mark_functions_after_initialization() {
    let directory = tempdir::TempDir::new("gc-mark-publication").unwrap();
    let input = directory.path().join("late_publish.ll");
    let output = directory.path().join("late_publish_out.ll");
    std::fs::write(&input, include_str!("gc_mark_publication/late_publish.ll")).unwrap();
    let result = Command::new("opt")
        .arg(format!("-load-pass-plugin={}", env!("SOLAR_WB_PLUGIN")))
        .args(["-passes=solar-publish-gc-mark-fns,verify", "-S"])
        .arg(&input)
        .arg("-o")
        .arg(&output)
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    let ir = std::fs::read_to_string(output).unwrap();
    let body = ir
        .split("define void @solar_publish(")
        .nth(1)
        .unwrap()
        .split("\n}")
        .next()
        .unwrap();
    assert_eq!(
        body.matches("call void @sol_set_mark_fn(").count(),
        7,
        "{ir}"
    );
    assert!(
        ir.contains("@sol_alloc_class_4_impl(i64 128, i64 16, ptr null)"),
        "{ir}"
    );
    assert!(
        body.contains("@sol_alloc(i64 128, i64 16, ptr null)"),
        "{ir}"
    );
    assert!(
        body.contains("@sol_alloc_class_4_batch2(i64 128, i64 16, ptr null)"),
        "{ir}"
    );
    assert!(
        body.contains("@sol_alloc_class_4_batch3_view(i64 128, i64 16, ptr null)"),
        "{ir}"
    );
    let entry = body
        .split("entry:")
        .nth(1)
        .unwrap()
        .split("left:")
        .next()
        .unwrap();
    assert!(
        entry.find("store i64 0, ptr %view2").unwrap()
            < entry.find("call void @sol_set_mark_fn(ptr %view2").unwrap(),
        "{ir}"
    );
    let end = body
        .split("end:")
        .nth(1)
        .unwrap()
        .split("\n}")
        .next()
        .unwrap();
    assert!(
        end.contains("call void @sol_set_mark_fn(ptr %a, ptr @mark)"),
        "{ir}"
    );
    assert!(
        !entry.contains("call void @sol_set_mark_fn(ptr %a,"),
        "{ir}"
    );
    let debug = ir.split("define void @solar_debug(").nth(1).unwrap();
    let zero = debug.find("call void @llvm.memset.p0.i64(").unwrap();
    let publish = debug
        .find("call void @sol_set_mark_fn(ptr %value, ptr @mark)")
        .unwrap();
    let escape = debug.find("store ptr %reload, ptr %out").unwrap();
    assert!(zero < publish && publish < escape, "{ir}");
}

#[test]
fn null_mark_functions_scan_conservatively_until_published() {
    test_utils::ensure_release_runtime_built();
    let directory = tempdir::TempDir::new("gc-null-mark-runtime").unwrap();
    let source = directory.path().join("null_mark.c");
    let binary = directory.path().join("null_mark");
    std::fs::write(&source, include_str!("gc_mark_publication/null_mark.c")).unwrap();
    let result = Command::new("clang")
        .args(["-O2", "-fuse-ld=lld"])
        .arg(&source)
        .arg("target/release/libsolar_system.a")
        .args(["-lm", "-lpthread", "-ldl", "-o"])
        .arg(&binary)
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    for mode in ["0", "1"] {
        let output = Command::new(&binary)
            .env("SOLAR_THREAD_POOL_SIZE", "2")
            .arg(mode)
            .output()
            .unwrap();
        assert!(output.status.success(), "mode {mode}: {output:?}");
    }
}
