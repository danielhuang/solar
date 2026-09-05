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
    assert!(binary.artifacts_dir.join("program.c").is_file());
    assert_ne!(binary.artifacts_dir, path.parent().unwrap());
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
    let path = std::env::temp_dir()
        .join(format!("solar output {:x}", rand::random::<u64>()))
        .join("nested/release program");
    check_output(path, CompileOptions::RELEASE);
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
