//! Ensures statics retain heap objects across collections.

use solar::pipeline::CompileOptions;
use std::path::PathBuf;
use std::process::Command;

fn build(src: &str, name: &str, options: CompileOptions) -> PathBuf {
    if options.optimize {
        test_utils::ensure_release_runtime_built();
    } else {
        test_utils::ensure_runtime_built();
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
        .to_binary(name, options)
        .path
}

// `setup` returns before collection, leaving the statics as the only roots.
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
    // Store through a static so churn escapes optimization and triggers GC.
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
    let bin = build(GC_SRC, "statics_gc", CompileOptions::RELEASE);
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

const TLS_GC_SRC: &str = r#"
static(thread_local) KEEP: &?[Uint8] = null#[[Uint8]];
static SCRATCH: &?[Uint8] = null#[[Uint8]];

fn worker() {
    let buf = [7u8; 4096u];
    buf[0u] = 42u8;
    buf[4095u] = 9u8;
    KEEP = buf[0u..4096u]&;

    for i in 0..800000 {
        SCRATCH = [Uint8(i & 255); 4096u]&;
    }

    println(Int(KEEP@[0u]));
    println(Int(KEEP@[4095u]));
    println(Int(KEEP@[1u]));
}

fn main() {
    let t = thread::spawn(worker);
    t.join();
}
"#;

#[test]
fn thread_local_statics_root_heap_objects_across_gc() {
    let bin = build(
        TLS_GC_SRC,
        "thread_local_statics_gc",
        CompileOptions::RELEASE,
    );
    let out = Command::new(bin.canonicalize().unwrap())
        .env("SOLAR_PRINT_GC_STATS", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "thread-local statics must root heap objects across GC; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "42\n9\n7\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("running gc"),
        "expected at least one GC cycle; stderr: {stderr}"
    );
}
