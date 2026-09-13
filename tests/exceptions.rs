//! Exception details and lifetime checks for the production runtime.
use solar::pipeline::CompileOptions;
use std::{path::Path, process::Command};

#[test]
fn native_exception_payloads_survive_collection() {
    test_utils::ensure_runtime_built();
    test_utils::ensure_release_runtime_built();
    let directory = tempdir::TempDir::new("solar-exceptions").unwrap();
    let source = directory.path().join("lifetime.solar");
    std::fs::write(
        &source,
        r#"
import intrinsics from "@intrinsics";
struct Details { code: Int, text: &[Uint8] }
struct Holder { exception: Exception }
fn failure(n: Int) {
    let text = ("payload " + n.to_string()@)&;
    let payload = Details { code: n, text: text };
    throw(("failure " + n.to_string()@)&, Any(payload&));
}
fn main() {
    let done = false;
    let finished = false;
    thread::spawn(\ {
        while !done&.atomic_load() { gc::collect_gc(); }
        finished&.atomic_store(true);
    });
    let resolved = intrinsics::resolve_address(0u);
    gc::collect_gc();
    assert(resolved@ == "0x0 <unknown>");
    resolved@[0u] = 'X';
    assert(resolved@ == "Xx0 <unknown>");
    let holder = (Holder { exception: Exception("initial"&) })&;
    for n in 0..20 {
        try { failure(n); } catch (e) {
            holder@.exception = e;
        }
        gc::collect_gc();
        let e = holder@.exception;
        assert(e.payload.downcast#[Details]()@.code == n);
        assert(e.message@ == "failure " + n.to_string()@);
        assert(e.payload.downcast#[Details]()@.text@ == "payload " + n.to_string()@);
        assert(e.to_string().contains("Payload type: Details"&));
        assert(e.trace.len() > 0u);
        try { throw(e); } catch (again) {
            gc::collect_gc();
            assert(ref_eq(e.trace, again.trace));
            assert(again.payload.downcast#[Details]()@.code == n);
        }
    }
    done&.atomic_store(true);
    while !finished&.atomic_load() {}
    println("passed"&);
}
"#,
    )
    .unwrap();
    let ir = solar::pipeline::compile(&source)
        .unwrap()
        .to_mangled()
        .to_ir()
        .optimized();
    for (index, options) in [
        CompileOptions::RELEASE,
        CompileOptions::GC_SAN,
        CompileOptions {
            optimize: false,
            ..CompileOptions::GC_SAN
        },
    ]
    .into_iter()
    .enumerate()
    {
        let binary = ir
            .to_c(&source.display().to_string())
            .to_binary(directory.path().join(format!("lifetime-{index}")), options);
        assert_eq!(binary.run("exception lifetime"), "passed\n");
    }
}

#[test]
fn uncaught_exceptions_print_payload_type_and_original_demangled_trace() {
    test_utils::ensure_release_runtime_built();
    let directory = tempdir::TempDir::new("solar-exceptions").unwrap();
    let source = directory.path().join("uncaught.solar");
    std::fs::write(
        &source,
        r#"
fn original_failure() {
    let value = 42;
    throw("uncaught details"&, Any(value&));
}
fn main() {
    try { original_failure(); } catch (e) { throw(e); }
}
"#,
    )
    .unwrap();
    let binary = solar::pipeline::compile(&source)
        .unwrap()
        .to_mangled()
        .to_ir()
        .optimized()
        .to_c(&source.display().to_string())
        .to_binary(directory.path().join("uncaught"), CompileOptions::RELEASE);
    let result = Command::new("sh")
        .args(["-c", "ulimit -c 0; exec \"$1\"", "solar-exception"])
        .arg(&binary.path)
        .output()
        .unwrap();
    assert!(!result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("uncaught exception: uncaught details\nPayload type: Int\nStack trace:"),
        "{stderr}"
    );
    assert!(stderr.contains("original_failure()"), "{stderr}");
    assert!(stderr.contains("uncaught.solar:"), "{stderr}");
}

#[test]
fn production_exception_behavior_matches_interpreters() {
    test_utils::ensure_release_runtime_built();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/runtime/exception.solar");
    let directory = tempdir::TempDir::new("solar-exceptions").unwrap();
    let binary = solar::pipeline::compile(&source)
        .unwrap()
        .to_mangled()
        .to_ir()
        .optimized()
        .to_c(&source.display().to_string())
        .to_binary(directory.path().join("exception"), CompileOptions::RELEASE);
    assert_eq!(binary.run("exceptions"), "passed\n");
}

#[test]
fn exception_is_a_standard_library_type() {
    let directory = tempdir::TempDir::new("solar-exceptions").unwrap();
    let source = directory.path().join("standard_type.solar");
    std::fs::write(
        &source,
        r#"
import std from "@std";
type Failure = std::Exception;
fn main() {
    let original: Failure = std::Exception("standard exception"&);
    try { throw(original); } catch (e: Failure) {
        let standard: std::Exception = e;
        assert(standard.message@ == "standard exception");
        assert(standard.payload.downcast#[Unit]() != null#[Unit]);
        assert(ref_eq(standard.trace, original.trace));
    }
    println("passed"&);
}
"#,
    )
    .unwrap();
    assert_eq!(test_utils::run(&source, "standard_exception"), "passed\n");
}
