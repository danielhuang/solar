use std::path::{Path, PathBuf};
use test_utils::run_codegen_file;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/compile_only")
        .join(name)
}

#[test]
fn atomics() {
    let output = run_codegen_file(&fixture("atomics.solar"), "compile_only_atomics");
    assert_eq!(output, "42\n");
}

#[test]
fn futex() {
    let output = run_codegen_file(&fixture("futex.solar"), "compile_only_futex");
    assert_eq!(output, "1\n");
}

#[test]
fn mutex() {
    let output = run_codegen_file(&fixture("mutex.solar"), "compile_only_mutex");
    assert_eq!(output, "2\n");
}

#[test]
fn thread_join() {
    let output = run_codegen_file(&fixture("thread_join.solar"), "compile_only_thread_join");
    assert_eq!(output, "42\n");
}

#[test]
fn channel() {
    let output = run_codegen_file(&fixture("channel.solar"), "compile_only_channel");
    assert_eq!(output, "42\n");
}

#[test]
fn channel_multi() {
    let output = run_codegen_file(
        &fixture("channel_multi.solar"),
        "compile_only_channel_multi",
    );
    assert_eq!(output, "60\n");
}

#[test]
fn channel_pingpong() {
    let output = run_codegen_file(
        &fixture("channel_pingpong.solar"),
        "compile_only_channel_pingpong",
    );
    assert_eq!(output, "11\n");
}

#[test]
fn futex_timeout() {
    let output = run_codegen_file(
        &fixture("futex_timeout.solar"),
        "compile_only_futex_timeout",
    );
    assert_eq!(output, "0\n");
}
