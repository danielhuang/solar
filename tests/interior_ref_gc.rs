//! Release-pipeline test for interior references escaping a function: the
//! escape analysis must keep the aggregate heap-boxed (see
//! `tests/runtime/interior_ref_escape.solar` for the ASAN/debug variant), and
//! the collector must keep the aggregate alive through interior pointers under
//! churn. Only the release pipeline runs the GC LLVM passes, mirroring
//! `tests/statics_gc.rs`.

use solar::pipeline::CompileMode;
use std::path::PathBuf;
use std::process::Command;

fn build(src: &str, name: &str) -> PathBuf {
    test_utils::ensure_release_runtime_built();
    let dir = std::env::temp_dir().join(format!("solar_test_{name}"));
    std::fs::create_dir_all(&dir).unwrap();
    let src_path = dir.join(format!("{name}.solar"));
    std::fs::write(&src_path, src).unwrap();
    let typed = solar::pipeline::compile(&src_path).unwrap();
    typed
        .to_ir()
        .optimized()
        .to_c(&src_path.display().to_string())
        .to_binary(name, CompileMode::Release)
        .path
}

const SRC: &str = r#"
pub struct LayerA { pub tag: Int, pub pad: [Int; 6], }
pub struct LayerB { pub x: Int, pub y: Int, }
pub struct Leaf { pub a: LayerA, pub b: LayerB, }

static SCRATCH: &?[Uint8] = null#[[Uint8]];

fn make_leaf(i: Int) -> &LayerB {
    let leaf = Leaf {
        a: LayerA { tag: i, pad: [i; 6u] },
        b: LayerB { x: i * 3, y: i * 7 },
    };
    let leaf_ref = leaf&;
    leaf_ref@.b&
}

fn main() {
    // Retain interior refs only (the containing Leafs have no direct root).
    let keep: [&LayerB] = [make_leaf(0); 64u];
    for i in 0..64 {
        keep[Uint(i)] = make_leaf(i);
    }
    // >1 GiB of escaping garbage to force collection cycles; the Leafs must
    // stay live through the interior pointers alone.
    for i in 0..300000 {
        SCRATCH = [Uint8(i & 255); 4096u]&;
    }
    let sx = 0;
    let sy = 0;
    for i in 0..64 {
        sx = sx + keep[Uint(i)]@.x;
        sy = sy + keep[Uint(i)]@.y;
    }
    println(sx); // 3 * 2016 = 6048
    println(sy); // 7 * 2016 = 14112
}
"#;

#[test]
fn interior_refs_survive_gc() {
    let bin = build(SRC, "interior_ref_gc");
    let out = Command::new(bin.canonicalize().unwrap())
        .env("SOLAR_PRINT_GC_STATS", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "interior refs must keep their aggregates alive; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout, "6048\n14112\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("running gc"),
        "expected at least one GC cycle; stderr: {stderr}"
    );
}
