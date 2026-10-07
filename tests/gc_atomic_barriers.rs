//! Atomic heap writes must shade references even when represented as integers
//! or implemented by out-of-line 128-bit helpers.

use std::path::Path;
use std::process::Command;

fn instrument(directory: &Path) -> std::path::PathBuf {
    let input = directory.join("atomic.ll");
    let output = directory.join("atomic-wb.ll");
    std::fs::write(&input, include_str!("gc_atomic_barriers/operations.ll")).unwrap();
    let result = Command::new("opt")
        .arg(format!("-load-pass-plugin={}", env!("SOLAR_WB_PLUGIN")))
        .args([
            "-passes=solar-lower-atomic-memcpy16,solar-write-barriers,verify",
            "-S",
        ])
        .arg(&input)
        .arg("-o")
        .arg(&output)
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    output
}

#[test]
fn instruments_atomic_stores_exchanges_and_wide_helpers() {
    let directory = tempdir::TempDir::new("atomic-barriers").unwrap();
    let output = instrument(directory.path());
    let ir = std::fs::read_to_string(output).unwrap();
    for name in [
        "store",
        "exchange",
        "compare",
        "wide_store",
        "wide_load",
        "wide_copy",
        "wide_compare",
        "unordered_store",
        "unordered_load",
        "direct_wide_exchange",
        "direct_wide_compare",
    ] {
        let body = ir
            .split(&format!("define void @solar_{name}("))
            .nth(1)
            .unwrap()
            .split("\n}")
            .next()
            .unwrap();
        assert!(
            body.contains("call void @sol_write_barrier(")
                || body.contains("call void @sol_gc_memcpy_barrier("),
            "missing barrier for {name}: {body}"
        );
    }
    let stack = ir
        .split("define void @solar_stack_only(")
        .nth(1)
        .unwrap()
        .split("\n}")
        .next()
        .unwrap();
    assert!(!stack.contains("call void @sol_"), "{stack}");
}

#[test]
fn atomic_writes_retain_white_children_of_already_scanned_objects() {
    test_utils::ensure_release_runtime_built();
    let directory = tempdir::TempDir::new("atomic-retention").unwrap();
    let ir = instrument(directory.path());
    let source = directory.path().join("retention.c");
    std::fs::write(&source, include_str!("gc_atomic_barriers/retention.c")).unwrap();
    for optimization in ["-O0", "-O3"] {
        let binary = directory.path().join(format!("retention{optimization}"));
        let result = Command::new("clang")
            .args([optimization, "-fuse-ld=lld"])
            .arg(&source)
            .arg(&ir)
            .arg("target/release/libsolar_system.a")
            .args(["-lm", "-lpthread", "-ldl", "-o"])
            .arg(&binary)
            .output()
            .unwrap();
        assert!(result.status.success(), "{result:?}");
        for operation in 0..11 {
            let result = Command::new(&binary)
                .env("SOLAR_THREAD_POOL_SIZE", "4")
                .arg(operation.to_string())
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{optimization}, operation {operation}: {result:?}"
            );
        }
    }
}
