//! Verifies the LLVM GC sanitizer instruments every supported memory operation.

use std::process::Command;

#[test]
fn instruments_generated_memory_operations() {
    let dir = std::env::temp_dir().join(format!("solar_gc_san_pass_{:x}", rand::random::<u64>()));
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("input.ll");
    let output = dir.join("output.ll");
    std::fs::write(
        &input,
        r#"
declare void @llvm.memmove.p0.p0.i64(ptr, ptr, i64, i1 immarg)
declare void @llvm.memset.p0.i64(ptr, i8, i64, i1 immarg)

define void @solar_test(ptr %a, ptr %b, i64 %n) {
entry:
  %v = load i64, ptr %a
  store i64 %v, ptr %b
  %old = atomicrmw add ptr %a, i64 1 seq_cst
  %pair = cmpxchg ptr %b, i64 0, i64 1 seq_cst seq_cst
  call void @llvm.memmove.p0.p0.i64(ptr %a, ptr %b, i64 %n, i1 false)
  call void @llvm.memset.p0.i64(ptr %a, i8 0, i64 %n, i1 false)
  call void @llvm.memset.p0.i64(ptr %a, i8 0, i64 0, i1 false)
  ret void
}

define i64 @runtime_helper(ptr %p) {
entry:
  %v = load i64, ptr %p
  ret i64 %v
}
"#,
    )
    .unwrap();

    let plugin_arg = format!("-load-pass-plugin={}", env!("SOLAR_WB_PLUGIN"));
    let result = Command::new("opt")
        .args([
            plugin_arg.as_str(),
            "-passes=solar-gc-sanitize",
            input.to_str().unwrap(),
            "-S",
            "-o",
            output.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "opt failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );

    let ir = std::fs::read_to_string(output).unwrap();
    assert_eq!(ir.matches("call void @sol_gc_san_check").count(), 8, "{ir}");
    let runtime = ir.split("define i64 @runtime_helper").nth(1).unwrap();
    let runtime = runtime.split('}').next().unwrap();
    assert!(!runtime.contains("sol_gc_san_check"), "{runtime}");
}
