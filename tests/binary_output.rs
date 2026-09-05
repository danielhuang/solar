use std::path::{Path, PathBuf};

use solar::pipeline::CompileOptions;

fn check_output(path: PathBuf, options: CompileOptions) {
    if options.optimize {
        test_utils::ensure_release_runtime_built();
    } else {
        test_utils::ensure_runtime_built();
    }
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/runtime/iterator.solar");
    let binary = solar::pipeline::compile(&source)
        .unwrap()
        .to_mangled()
        .to_ir()
        .to_c(&source.display().to_string())
        .to_binary(&path, options);

    assert_eq!(binary.path, path);
    assert!(path.is_file());
    assert_eq!(binary.run("binary_output"), "passed\n");
    std::fs::remove_file(path).unwrap();
}

#[test]
fn debug_binary_uses_nested_relative_output_path() {
    let path = Path::new("target/output path tests")
        .join(format!("{:x}", rand::random::<u64>()))
        .join("nested/program with spaces");
    check_output(path, CompileOptions::DEBUG);
}

#[test]
fn release_binary_uses_absolute_output_path() {
    test_utils::ensure_release_runtime_built();
    let directory = tempdir::TempDir::new("solar-output-test").unwrap();
    let scratch = directory.path().join("scratch");
    std::fs::create_dir(&scratch).unwrap();
    let path = directory.path().join("nested/release program");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/runtime/iterator.solar");
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_compile"))
        .arg(source)
        .arg(&path)
        .env("TMPDIR", &scratch)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 0);
    assert_eq!(
        solar::pipeline::Binary { path }.run("binary_output"),
        "passed\n"
    );
}

#[test]
fn gc_disabled_binary_uses_bare_output_filename() {
    let path = PathBuf::from(format!("solar-output-{:x}", rand::random::<u64>()));
    check_output(
        path,
        CompileOptions {
            enable_gc: false,
            gc_san: false,
            optimize: false,
        },
    );
}

#[cfg(unix)]
#[test]
fn binary_output_accepts_non_utf8_paths() {
    use std::os::unix::ffi::OsStringExt;

    let path = test_utils::binary_output_path("non_utf8")
        .with_file_name(std::ffi::OsString::from_vec(b"program-\xff".to_vec()));
    check_output(path, CompileOptions::DEBUG);
}
