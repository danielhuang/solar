//! Native-runtime tests for GC-managed file descriptor lifetimes.

use std::path::{Path, PathBuf};
use std::process::Command;

use solar::pipeline::CompileOptions;

const FD_LIMIT: u32 = 64;

fn build(src: &str, name: &str, options: CompileOptions) -> PathBuf {
    if options.optimize {
        test_utils::ensure_release_runtime_built();
    } else {
        test_utils::ensure_runtime_built();
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
        .to_binary(name, options)
        .path
}

// Runs a binary with a low descriptor limit.
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

// `OPEN_STMT` selects whether each descriptor is dropped or retained.
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
    // Unreachable descriptors must be collected before exhausting the limit.
    let src = TEMPLATE.replace("OPEN_STMT", r#"let f = file::open("Cargo.toml"&);"#);
    let bin = build(&src, "fd_gc_dropped", CompileOptions::RELEASE);
    assert!(
        run_with_fd_limit(&bin),
        "opening+dropping FileDescs should survive a low fd limit because the \
         GC closes the unreachable ones"
    );
}

#[test]
fn closed_file_descriptors_keep_their_fd_number() {
    // Closing neuters a descriptor but reserves its number until collection.
    let src = TEMPLATE.replace(
        "OPEN_STMT",
        r#"let f = file::open("Cargo.toml"&);
            f.close();
            kept = (FdNode { fd: f, next: FdOpt::Some(kept) })&;
            fd_root&.atomic_store(kept);"#,
    );
    let bin = build(&src, "fd_gc_closed_retained", CompileOptions::DEBUG);
    assert!(
        !run_with_fd_limit(&bin),
        "closing a FileDesc must keep its fd number occupied (dup2 over a dead \
         fd, not a real close), so retaining the closed handles still exhausts \
         the fd limit"
    );
}

#[test]
fn retained_file_descriptors_are_not_closed() {
    // Reachable descriptors must remain open and exhaust the limit.
    let src = TEMPLATE.replace(
        "OPEN_STMT",
        r#"kept = (FdNode { fd: file::open("Cargo.toml"&), next: FdOpt::Some(kept) })&;
            fd_root&.atomic_store(kept);"#,
    );
    let bin = build(&src, "fd_gc_retained", CompileOptions::DEBUG);
    assert!(
        !run_with_fd_limit(&bin),
        "retaining all FileDescs should exhaust the fd limit because the GC \
         must keep reachable fds open"
    );
}
