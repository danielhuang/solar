//! GC-rooting test for `static` declarations: a heap object whose *only*
//! reference lives in a static must survive collections. Only the release
//! pipeline runs the collector (debug skips the LLVM GC passes), so this is a
//! compiled release test, mirroring `process_args_env.rs`'s GC variant.

use solar::pipeline::CompileMode;
use std::path::PathBuf;
use std::process::Command;

fn build(src: &str, name: &str, mode: CompileMode) -> PathBuf {
    match mode {
        CompileMode::Release => test_utils::ensure_release_runtime_built(),
        CompileMode::Debug => test_utils::ensure_runtime_built(),
    }
    let dir = std::env::temp_dir().join(format!("solar_test_{name}"));
    std::fs::create_dir_all(&dir).unwrap();
    let src_path = dir.join(format!("{name}.solar"));
    std::fs::write(&src_path, src).unwrap();
    let typed = solar::pipeline::compile(&src_path).unwrap();
    typed
        .to_mangled()
        .to_ir()
        .optimized()
        .to_c(&src_path.display().to_string())
        .to_binary(name, mode)
        .path
}

// `setup` populates the statics with heap data and returns, so no stack frame
// keeps the buffers alive; heavy churn then forces collection cycles. If the
// statics table weren't scanned as roots, the buffers would be swept and their
// slots recycled by the churn, corrupting the final checksums.
const GC_SRC: &str = r#"
static KEEP: &?[Uint8] = null#[[Uint8]];
static CHAIN: &?Node = null#[Node];
static SCRATCH: &?[Uint8] = null#[[Uint8]];

pub struct Node {
    pub val: Int,
    pub next: &?Node,
}

fn setup() {
    let buf = [7u8; 4096u];
    buf[0u] = 42u8;
    buf[4095u] = 9u8;
    KEEP = buf[0u..4096u]&;

    // A linked chain reachable only through the static.
    let head = null#[Node];
    for i in 0..100 {
        head = (Node { val: i, next: head })&;
    }
    CHAIN = head;
}

fn churn() {
    // >3 GiB of garbage — well past the collector's 1 GiB trigger floor,
    // forcing multiple cycles. Each buffer is stored into a static so the
    // allocation genuinely escapes (a plain local would be elided entirely
    // by the allocation-promotion passes); every store also exercises the
    // static as a mutating root during concurrent marking.
    for i in 0..800000 {
        SCRATCH = [Uint8(i & 255); 4096u]&;
    }
}

fn main() {
    setup();
    churn();
    println(Int(SCRATCH@[0u])); // (800000-1) & 255 = 255
    println(Int(KEEP@[0u]));
    println(Int(KEEP@[4095u]));
    println(Int(KEEP@[1u]));
    let sum = 0;
    let walk = CHAIN;
    while walk != null#[Node] {
        sum = sum + walk@.val;
        walk = walk@.next;
    }
    println(sum);
}
"#;

#[test]
fn statics_root_heap_objects_across_gc() {
    let bin = build(GC_SRC, "statics_gc", CompileMode::Release);
    let out = Command::new(bin.canonicalize().unwrap())
        .env("SOLAR_PRINT_GC_STATS", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "statics must root their heap objects across GC; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout, "255\n42\n9\n7\n4950\n");
    // Sanity: the churn actually forced collection cycles.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("running gc"),
        "expected at least one GC cycle; stderr: {stderr}"
    );
}
