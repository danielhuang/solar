//! Compiled-only regression test for GC-managed `FileDesc` lifetimes.
//!
//! These behaviors are only observable in the native runtime (the interpreters
//! have no fd arena and no collector), so they cannot go through the usual
//! three-backend `run` harness. Instead we compile two programs and run each
//! under a restricted `RLIMIT_NOFILE`.
//!
//! Both build an escaping, atomically-published garbage chain (~32 MiB per
//! generation, so the run's ~3 GiB total churn crosses the collector's 1 GiB
//! trigger floor (`MIN_SIZE_UNTIL_GC`) several times) and the collector
//! actually runs.
//!
//! * `dropped` opens a file every iteration and immediately drops the handle.
//!   The collector closes the unreachable fds, keeping the live count tiny, so
//!   the program survives a small fd ceiling.
//! * `retained` keeps every opened `FileDesc` reachable in a growing chain. The
//!   collector must NOT close them, so the fds accumulate and the program hits
//!   the ceiling (EMFILE) and aborts.

use std::path::{Path, PathBuf};
use std::process::Command;

use solar::pipeline::CompileMode;

const FD_LIMIT: u32 = 64;

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
        .to_ir()
        .optimized()
        .to_c(&path.display().to_string())
        .to_binary(name, mode)
        .path
}

/// Run `bin` with `RLIMIT_NOFILE` lowered to `FD_LIMIT`. Returns whether it
/// exited successfully. Leak detection is disabled: the test programs
/// intentionally orphan GC-managed allocations.
fn run_with_fd_limit(bin: &Path) -> bool {
    Command::new("bash")
        .arg("-c")
        .arg(format!(
            "ulimit -n {FD_LIMIT}; exec '{}'",
            bin.canonicalize().unwrap().display()
        ))
        .env("ASAN_OPTIONS", "detect_leaks=0")
        .output()
        .unwrap()
        .status
        .success()
}

// A spawned thread runs 100 iterations. Each builds a ~32 MiB escaping garbage
// chain (1M nodes × 32-byte size class, published via an atomic root), so the
// 1 GiB GC trigger floor is crossed around every ~32nd iteration — the first
// cycle runs well before the `dropped` case can accumulate 64 open fds.
// `OPEN_STMT` is spliced in per test: drop the handle, or retain it in `kept`.
const TEMPLATE: &str = r#"
enum GOpt {
    Some(&GNode),
    None,
}
struct GNode {
    value: Int,
    next: GOpt,
}
enum FdOpt {
    Some(&FdNode),
    None,
}
struct FdNode {
    fd: FileDesc,
    next: FdOpt,
}

fn main() {
    let g_sentinel = (GNode { value: 0, next: GOpt::None })&;
    let g_root = g_sentinel;
    let fd_sentinel = (FdNode { fd: file::open("Cargo.toml"&), next: FdOpt::None })&;
    let fd_root = fd_sentinel;
    let is_done = false;
    thread::spawn(\ {
        let kept = fd_sentinel;
        for iter in 0..100 {
            OPEN_STMT
            let head = g_sentinel;
            for j in 0..1000000 {
                head = (GNode { value: j, next: GOpt::Some(head) })&;
            }
            g_root&.atomic_store(head);
        }
        is_done&.atomic_store(true);
    });
    while is_done&.atomic_load() == false {}
    println("done"&);
}
"#;

#[test]
fn dropped_file_descriptors_are_closed_by_gc() {
    // The handle is dropped each iteration; the GC closes the unreachable fds,
    // so the live count stays tiny and the program survives the fd ceiling.
    let src = TEMPLATE.replace("OPEN_STMT", r#"let f = file::open("Cargo.toml"&);"#);
    // The collector only runs in the release pipeline (debug skips the GC LLVM
    // passes), and this test depends on it actually closing the dropped fds.
    let bin = build(&src, "fd_gc_dropped", CompileMode::Release);
    assert!(
        run_with_fd_limit(&bin),
        "opening+dropping FileDescs should survive a low fd limit because the \
         GC closes the unreachable ones"
    );
}

#[test]
fn closed_file_descriptors_keep_their_fd_number() {
    // Each handle is `close`d but still retained in the reachable `kept` chain.
    // `close` neuters the file via `dup2(dead_fd, fd)` WITHOUT freeing the fd
    // number — a stale FileDesc must never be able to alias a fd reused by a
    // later open. So the numbers stay occupied and the program exhausts the
    // ceiling exactly as the retain-without-close case does. (A plain `close`
    // would free the numbers, and the program would survive.)
    let src = TEMPLATE.replace(
        "OPEN_STMT",
        r#"let f = file::open("Cargo.toml"&);
            f.close();
            kept = (FdNode { fd: f, next: FdOpt::Some(kept) })&;
            fd_root&.atomic_store(kept);"#,
    );
    let bin = build(&src, "fd_gc_closed_retained", CompileMode::Debug);
    assert!(
        !run_with_fd_limit(&bin),
        "closing a FileDesc must keep its fd number occupied (dup2 over a dead \
         fd, not a real close), so retaining the closed handles still exhausts \
         the fd limit"
    );
}

#[test]
fn retained_file_descriptors_are_not_closed() {
    // Every handle is retained in the reachable `kept` chain; the GC must keep
    // them open, so the fds accumulate and the program exhausts the ceiling.
    let src = TEMPLATE.replace(
        "OPEN_STMT",
        r#"kept = (FdNode { fd: file::open("Cargo.toml"&), next: FdOpt::Some(kept) })&;
            fd_root&.atomic_store(kept);"#,
    );
    let bin = build(&src, "fd_gc_retained", CompileMode::Debug);
    assert!(
        !run_with_fd_limit(&bin),
        "retaining all FileDescs should exhaust the fd limit because the GC \
         must keep reachable fds open"
    );
}
