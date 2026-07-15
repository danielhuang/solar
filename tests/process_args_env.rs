//! Compiled-only tests for the `args()` / `env()` intrinsics.
//!
//! These read the real process command line and environment, which the
//! interpreters deliberately expose as empty (they have no process-args source
//! and no collector), so they cannot go through the three-backend `run`
//! harness. Instead we compile a program and run the native binary with a
//! controlled argv and environment.
//!
//! The runtime builds the returned `&[&[Uint8]]` by copying each entry into a
//! fresh GC allocation, so the second test puts the collector under load while
//! holding those copies live to confirm they are traced (via the runtime mark
//! functions) and never prematurely freed or corrupted.

use std::path::{Path, PathBuf};
use std::process::Command;

use solar::pipeline::CompileMode;

fn build(src: &str, name: &str, mode: CompileMode) -> PathBuf {
    match mode {
        CompileMode::Debug => test_utils::ensure_runtime_built(),
        CompileMode::Release => test_utils::ensure_release_runtime_built(),
    }
    let dir = Path::new("target/test-fixtures");
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(format!("{name}.solar"));
    std::fs::write(&path, src).unwrap();
    let typed = solar::pipeline::compile(&path).unwrap();
    typed
        .to_mangled()
        .to_ir()
        .optimized()
        .to_c(&path.display().to_string())
        .to_binary(name, mode)
        .path
}

// Print each arg on its own `arg:` line and the value of the `SOLAR_TEST_VAR`
// environment variable (found by scanning `env()` for the `KEY=` prefix).
const PRINT_SRC: &str = r#"
fn starts_with(s: &[Uint8], prefix: &[Uint8]) -> Bool {
    if s.len() < prefix.len() { return false; }
    for i in 0u..prefix.len() {
        if s@[i] != prefix@[i] { return false; }
    }
    true
}

fn main() {
    let a = process::args();
    for i in 0u..a.len() {
        write_stdout("arg:"&);
        println(a@[i]);
    }
    let e = process::env();
    for i in 0u..e.len() {
        let entry = e@[i];
        if starts_with(entry, "SOLAR_TEST_VAR="&) {
            println(entry);
        }
    }
}
"#;

#[test]
fn args_and_env_are_exposed_to_compiled_programs() {
    let bin = build(PRINT_SRC, "process_print", CompileMode::Release);
    let out = Command::new(bin.canonicalize().unwrap())
        .args(["hello", "wor ld", "42"])
        .env("SOLAR_TEST_VAR", "the-value")
        .env("ASAN_OPTIONS", "detect_leaks=0")
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();

    // argv[0] is included, then the three arguments verbatim (the embedded
    // space in "wor ld" must survive the byte-for-byte copy).
    let arg_lines: Vec<&str> = stdout.lines().filter(|l| l.starts_with("arg:")).collect();
    assert_eq!(arg_lines.len(), 4, "argv[0] + 3 args; got {stdout:?}");
    assert!(
        arg_lines[0].ends_with("process_print"),
        "argv[0]: {stdout:?}"
    );
    assert_eq!(&arg_lines[1..], ["arg:hello", "arg:wor ld", "arg:42"]);

    // env() yields KEY=VALUE strings.
    assert!(
        stdout.lines().any(|l| l == "SOLAR_TEST_VAR=the-value"),
        "env entry missing: {stdout:?}"
    );
}

// Retain every `args()`/`env()` copy in a growing, atomically-published chain
// (>1 MiB of garbage per generation forces collection) and re-checksum a
// retained copy each iteration. If the runtime's allocations were mistraced,
// the retained bytes would be freed/recycled and the checksum would diverge.
const GC_SRC: &str = r#"
enum Opt {
    Some(&Node),
    None,
}
struct Node {
    data: &[&[Uint8]],
    sum: Uint8,
    next: Opt,
}

fn checksum(slices: &[&[Uint8]]) -> Uint8 {
    let acc = 0u8;
    for i in 0u..slices.len() {
        let s = slices@[i];
        for j in 0u..s.len() { acc = acc ++ s@[j]; }
    }
    acc
}

fn main() {
    let env_sum = checksum(process::env());
    let sentinel = (Node { data: process::env(), sum: env_sum, next: Opt::None })&;
    let root = sentinel;
    let head = sentinel;
    for iter in 0..400 {
        // Retain a fresh env() copy reachable through the chain.
        head = (Node { data: process::env(), sum: env_sum, next: Opt::Some(head) })&;
        root&.atomic_store(head);
        if checksum(head@.data) != head@.sum { panic("env copy corrupted!"&); }
        // Build >1 MiB of escaping garbage so the collector runs each iteration.
        let g = sentinel;
        for j in 0..40000 {
            g = (Node { data: head@.data, sum: env_sum, next: Opt::Some(g) })&;
        }
        root&.atomic_store(head);
    }
    // Walk the whole retained chain at the end; every copy must still be intact.
    let walk = head;
    let going = true;
    while going {
        if checksum(walk@.data) != walk@.sum { panic("retained env corrupted!"&); }
        match walk@.next {
            Opt::Some(m) => { walk = m; },
            Opt::None => { going = false; },
        };
    }
    println("gc ok"&);
}
"#;

#[test]
fn retained_env_copies_survive_collection() {
    // Only the release pipeline runs the GC (debug skips the LLVM GC passes), so
    // this is where mistracing of the runtime's allocations would show up.
    let bin = build(GC_SRC, "process_env_gc", CompileMode::Release);
    let out = Command::new(bin.canonicalize().unwrap())
        .env("SOLAR_TEST_VAR", "stress")
        .env("ASAN_OPTIONS", "detect_leaks=0")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "retained env() copies must be traced and survive GC; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8(out.stdout).unwrap().trim(), "gc ok");
}
