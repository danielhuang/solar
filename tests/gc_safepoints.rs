//! Cooperative polls, syscall suspension, and unrelated memory faults.

use solar::pipeline::CompileOptions;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

fn output_with_deadline(command: &mut Command) -> Output {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!("safepoint test timed out: {output:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn polls_and_syscall_signals_cover_gc_and_thread_lifecycle_races() {
    test_utils::ensure_runtime_built();
    test_utils::ensure_release_runtime_built();
    let directory = tempdir::TempDir::new("solar-test").unwrap();
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/compile_only/gc_safepoints.solar");
    for (i, options) in [
        CompileOptions::DEBUG,
        CompileOptions::RELEASE,
        CompileOptions::GC_SAN,
    ]
    .into_iter()
    .enumerate()
    {
        let binary = solar::pipeline::compile(&source)
            .unwrap()
            .to_mangled()
            .to_tree_ir()
            .optimized()
            .to_c(&source.display().to_string())
            .to_binary(directory.path().join(format!("safepoints-{i}")), options);
        let output = output_with_deadline(
            Command::new(binary.path)
                .env("ASAN_OPTIONS", "detect_leaks=0")
                .env("SOLAR_THREAD_POOL_SIZE", "2"),
        );
        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, b"safepoints passed\n");
    }
}

#[test]
fn unrelated_segv_remains_fatal() {
    test_utils::ensure_release_runtime_built();
    let directory = tempdir::TempDir::new("solar-test").unwrap();
    let source = directory.path().join("fault.c");
    let binary = directory.path().join("fault");
    // Use a real protection fault, without C null-dereference UB or Solar's
    // checked nullable-reference access. The runtime must not swallow it.
    std::fs::write(
        &source,
        r#"
#include <sys/mman.h>
#include <sys/resource.h>
#include <stddef.h>
#include <stdint.h>
extern void sol_start(void (*)(void*), void*, size_t, void (*)(void));
const char *sol_payload_type_name(uint64_t tag) { return "()"; }
static void body(void *env) {
    void *page = mmap(0, 4096, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    __asm__ volatile("testb $0, (%0)" : : "r"(page) : "memory", "cc");
}
int main(void) { struct rlimit limit = {0, 0}; setrlimit(RLIMIT_CORE, &limit); sol_start(body, 0, 0, 0); return 0; }
"#,
    )
    .unwrap();
    let output = Command::new("clang")
        .args(["-O3", "-fuse-ld=lld"])
        .arg(source)
        .arg("target/release/libsolar_system.a")
        .args(["-lm", "-lpthread", "-ldl", "-o"])
        .arg(&binary)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let output = output_with_deadline(Command::new(binary).env("SOLAR_THREAD_POOL_SIZE", "2"));
    assert_eq!(output.status.signal(), Some(11), "{output:?}");
}

#[test]
fn llvm_pass_polls_optimized_loops_and_excludes_runtime_and_mark_functions() {
    let directory = tempdir::TempDir::new("solar-test").unwrap();
    let input = directory.path().join("input.ll");
    let output = directory.path().join("output.ll");
    std::fs::write(
        &input,
        r#"
define void @solar_loop(ptr %stop) {
entry:
  br label %loop
loop:
  %v = load atomic i32, ptr %stop monotonic, align 4
  %done = icmp ne i32 %v, 0
  br i1 %done, label %exit, label %loop
exit:
  ret void
}
define void @solar_leaf() { ret void }
define void @runtime_helper() { ret void }
define void @_mark_value() { ret void }
"#,
    )
    .unwrap();
    let result = Command::new("opt")
        .arg(format!("-load-pass-plugin={}", env!("SOLAR_WB_PLUGIN")))
        .args(["-passes=default<O3>,solar-safepoints,verify", "-S"])
        .arg(input)
        .arg("-o")
        .arg(&output)
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    let ir = std::fs::read_to_string(output).unwrap();
    assert_eq!(
        ir.matches("asm sideeffect \"testb $$0, SOL_SAFEPOINT_PAGE(%rip)\"")
            .count(),
        3,
        "{ir}"
    );
    for name in ["runtime_helper", "_mark_value"] {
        let body = ir
            .split(&format!("@{name}("))
            .nth(1)
            .unwrap()
            .split('}')
            .next()
            .unwrap();
        assert!(!body.contains("SOL_SAFEPOINT_PAGE"), "{body}");
    }
}
