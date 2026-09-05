//! Finalizer lifetimes and explicit collection across native GC configurations.

use std::path::Path;
use std::process::Command;

use solar::pipeline::CompileOptions;

fn run_fixture(fixture: &str, name: &str, options: CompileOptions, disabled: bool) -> String {
    if options.optimize {
        test_utils::ensure_release_runtime_built();
    } else {
        test_utils::ensure_runtime_built();
    }
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join(fixture);
    let binary = solar::pipeline::compile(&source)
        .map_err(|(errors, _)| errors)
        .unwrap()
        .to_mangled()
        .to_ir()
        .optimized()
        .to_c(&source.display().to_string())
        .to_binary(name, options);
    let result = Command::new("bash")
        .arg("-c")
        .arg("ulimit -c 0; exec timeout 30s \"$1\"")
        .arg("finalizer-test")
        .arg(binary.path.canonicalize().unwrap())
        .env("ASAN_OPTIONS", "detect_leaks=0")
        .env("SOLAR_DISABLE_GC", if disabled { "1" } else { "0" })
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{name}: {}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap()
}

#[test]
fn finalizers_release() {
    assert_eq!(
        run_fixture(
            "tests/compile_only/finalizer_gc.solar",
            "finalizers_release",
            CompileOptions::GC_SAN,
            false
        ),
        "finalizers passed\n"
    );
}

#[test]
fn finalizers_debug() {
    assert_eq!(
        run_fixture(
            "tests/compile_only/finalizer_gc.solar",
            "finalizers_debug",
            CompileOptions {
                gc_san: true,
                ..CompileOptions::DEBUG
            },
            false
        ),
        "finalizers passed\n"
    );
}

#[test]
fn collection_disabled() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/runtime/gc_disabled.solar");
    let expected = "collect_gc: GC is disabled\n";
    assert_eq!(test_utils::run_ast_file(&fixture), expected);
    assert_eq!(test_utils::run_ir_file(&fixture), expected);
    assert_eq!(
        run_fixture(
            "tests/runtime/gc_disabled.solar",
            "gc_disabled_build",
            CompileOptions {
                enable_gc: false,
                ..CompileOptions::DEBUG
            },
            false
        ),
        expected
    );
    assert_eq!(
        run_fixture(
            "tests/runtime/gc_disabled.solar",
            "gc_disabled_env",
            CompileOptions::RELEASE,
            true
        ),
        expected
    );
}
