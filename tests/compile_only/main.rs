use std::path::{Path, PathBuf};
use test_utils::run_codegen_file;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/compile_only")
        .join(name)
}

#[test]
fn atomic_fetch() {
    let output = run_codegen_file(&fixture("atomic_fetch.solar"), "compile_only_atomic_fetch");
    assert_eq!(output, "passed\n");
}

#[test]
fn atomics() {
    let output = run_codegen_file(&fixture("atomics.solar"), "compile_only_atomics");
    assert_eq!(output, "42\n3\n99\n4\n");
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
fn mutex_contended() {
    let output = run_codegen_file(
        &fixture("mutex_contended.solar"),
        "compile_only_mutex_contended",
    );
    assert_eq!(output, "160000\n1\n");
}

#[test]
fn thread_join() {
    let output = run_codegen_file(&fixture("thread_join.solar"), "compile_only_thread_join");
    assert_eq!(output, "42\n");
}

#[test]
fn thread_local_static() {
    let output = run_codegen_file(
        &fixture("thread_local_static.solar"),
        "compile_only_thread_local_static",
    );
    assert_eq!(
        output,
        "7\ninitial\n7\ninitial\n22\nworker\n11\nmain\n40\n41\n50\n51\n"
    );
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

#[test]
fn sleep() {
    let output = run_codegen_file(&fixture("sleep.solar"), "compile_only_sleep");
    assert_eq!(output, "slept\n");
}

#[test]
fn tcp_echo() {
    let output = run_codegen_file(&fixture("tcp_echo.solar"), "compile_only_tcp_echo");
    assert_eq!(output, "hello over tcp\n");
}

#[test]
fn tcp_echo6() {
    let output = run_codegen_file(&fixture("tcp_echo6.solar"), "compile_only_tcp_echo6");
    assert_eq!(output, "hello over tcp6\n");
}

#[test]
fn exit() {
    let output = run_codegen_file(&fixture("exit.solar"), "compile_only_exit");
    assert_eq!(output, "before exit\n");
}

#[test]
fn file_open() {
    let output = run_codegen_file(&fixture("file_open.solar"), "compile_only_file_open");
    assert_eq!(output, "opened\n");
}

#[test]
fn file_io() {
    let output = run_codegen_file(&fixture("file_io.solar"), "compile_only_file_io");
    assert_eq!(output, "5\nhello world\n");
}

#[test]
fn file_open_flags() {
    let output = run_codegen_file(
        &fixture("file_open_flags.solar"),
        "compile_only_file_open_flags",
    );
    assert_eq!(output, "xyz\n");
}

#[test]
fn file_ops() {
    let output = run_codegen_file(&fixture("file_ops.solar"), "compile_only_file_ops");
    assert_eq!(
        output,
        "dir created\n5\nworld\nWORLD\n11\n0\n1\nno phantom\nlocked\n2\na.txt listed\nrenamed\ncleaned\n"
    );
}

#[test]
fn file_open_error() {
    let output = run_codegen_file(
        &fixture("file_open_error.solar"),
        "compile_only_file_open_error",
    );
    assert_eq!(
        output,
        "file_open failed: No such file or directory (os error 2)\ndone\n"
    );
}

#[test]
fn syscall() {
    let output = run_codegen_file(&fixture("syscall.solar"), "compile_only_syscall");
    assert_eq!(output, "syscall\n8\n");
}
