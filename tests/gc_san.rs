//! End-to-end coverage for optimized and unoptimized GC configurations.

use solar::pipeline::CompileOptions;
use std::process::Command;

const SRC: &str = r#"
static KEEP: &?[Uint8] = null#[[Uint8]];

fn main() {
    KEEP = [7u8; 4096u]&;
    KEEP@[0u] = 42u8;
    gc::collect_gc();
    println(Int(KEEP@[0u]));
    println(Int(KEEP@[4095u]));
}
"#;

const UNOPTIMIZED_SRC: &str = r#"
fn main() {
    let data = [7u8; 4096u];
    data[0u] = 42u8;
    gc::collect_gc();
    println(Int(data[0u]));
    println(Int(data[4095u]));
}
"#;

const OFFSET_REF_OUT_OF_BOUNDS_SRC: &str = r#"
fn main() {
    let values = [10, 20, 30]&;
    let second = unsafe { mem::offset_ref(values@[0u]&, 1) };
    println(second@);
    unsafe { mem::offset_ref(values@[0u]&, 4) };
    println(1);
}
"#;

#[test]
fn gc_san_runs_collections_without_rejecting_live_objects() {
    test_utils::ensure_release_runtime_built();
    let dir = std::env::temp_dir().join(format!("solar_gc_san_{:x}", rand::random::<u64>()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("gc_san.solar");
    std::fs::write(&source, SRC).unwrap();

    let binary = solar::pipeline::compile(&source)
        .unwrap()
        .to_mangled()
        .to_ir()
        .optimized()
        .to_c(&source.display().to_string())
        .to_binary("gc_san", CompileOptions::GC_SAN);
    let result = Command::new(binary.path.canonicalize().unwrap())
        .env("SOLAR_PRINT_GC_STATS", "1")
        .output()
        .unwrap();

    assert!(
        result.status.success(),
        "GC-San binary failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&result.stdout), "42\n7\n");
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("running gc"),
        "expected GC-San to run a collection: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn gc_san_runs_without_lto_or_optimization() {
    test_utils::ensure_runtime_built();
    let dir = std::env::temp_dir().join(format!(
        "solar_gc_san_unoptimized_{:x}",
        rand::random::<u64>()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("gc_san_unoptimized.solar");
    std::fs::write(&source, UNOPTIMIZED_SRC).unwrap();

    let options = CompileOptions {
        enable_gc: true,
        gc_san: true,
        optimize: false,
    };
    let binary = solar::pipeline::compile(&source)
        .unwrap()
        .to_mangled()
        .to_ir()
        .to_c(&source.display().to_string())
        .to_binary("gc_san_unoptimized", options);
    let result = Command::new(binary.path.canonicalize().unwrap())
        .env("ASAN_OPTIONS", "detect_leaks=0")
        .env("SOLAR_PRINT_GC_STATS", "1")
        .output()
        .unwrap();

    assert!(
        result.status.success(),
        "unoptimized GC-San binary failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&result.stdout), "42\n7\n");
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("memory used:") && !stderr.contains("gc was disabled"),
        "expected collection to be enabled in unoptimized GC-San: {stderr}"
    );
    let artifacts = binary.path.parent().unwrap();
    assert!(artifacts.join("debug_wb.bc").exists());
    assert!(artifacts.join("debug_gc_san.bc").exists());
}

#[test]
fn gc_san_rejects_offset_ref_outside_its_source_allocation() {
    test_utils::ensure_runtime_built();
    let dir = std::env::temp_dir().join(format!(
        "solar_gc_san_offset_ref_{:x}",
        rand::random::<u64>()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("gc_san_offset_ref.solar");
    std::fs::write(&source, OFFSET_REF_OUT_OF_BOUNDS_SRC).unwrap();

    let c_source = solar::pipeline::compile(&source)
        .unwrap()
        .to_mangled()
        .to_ir()
        .to_c(&source.display().to_string());
    assert!(
        c_source.c_source.matches("sol_offset_ref(").count() >= 2,
        "generated C must declare and call sol_offset_ref: {}",
        c_source.c_source
    );

    let options = CompileOptions {
        enable_gc: true,
        gc_san: true,
        optimize: false,
    };
    let binary = c_source.to_binary("gc_san_offset_ref", options);
    let result = Command::new(binary.path.canonicalize().unwrap())
        .env("ASAN_OPTIONS", "detect_leaks=0")
        .output()
        .unwrap();

    assert!(
        !result.status.success(),
        "out-of-allocation offset succeeded"
    );
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("offset_ref result"),
        "expected offset_ref GC-San failure: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&result.stdout), "20\n");
}

#[test]
fn gc_runs_without_lto_gc_san_or_optimization() {
    test_utils::ensure_runtime_built();
    let dir =
        std::env::temp_dir().join(format!("solar_gc_unoptimized_{:x}", rand::random::<u64>()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("gc_unoptimized.solar");
    std::fs::write(&source, UNOPTIMIZED_SRC).unwrap();

    let options = CompileOptions {
        enable_gc: true,
        gc_san: false,
        optimize: false,
    };
    let binary = solar::pipeline::compile(&source)
        .unwrap()
        .to_mangled()
        .to_ir()
        .to_c(&source.display().to_string())
        .to_binary("gc_unoptimized", options);
    let result = Command::new(binary.path.canonicalize().unwrap())
        .env("ASAN_OPTIONS", "detect_leaks=0")
        .env("SOLAR_PRINT_GC_STATS", "1")
        .output()
        .unwrap();

    assert!(
        result.status.success(),
        "unoptimized GC binary failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&result.stdout), "42\n7\n");
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("memory used:") && !stderr.contains("gc was disabled"),
        "expected collection to be enabled in unoptimized build: {stderr}"
    );
    let artifacts = binary.path.parent().unwrap();
    assert!(artifacts.join("debug_wb.bc").exists());
    assert!(!artifacts.join("debug_gc_san.bc").exists());
}
