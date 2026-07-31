use std::fs;
use std::path::{Path, PathBuf};

use solar::pipeline::CompileMode;

fn example_files() -> Vec<PathBuf> {
    let examples_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    let mut files: Vec<_> = fs::read_dir(&examples_dir)
        .unwrap()
        .filter_map(|e| {
            let path = e.unwrap().path();
            if path.extension().is_some_and(|e| e == "solar") {
                Some(path)
            } else {
                None
            }
        })
        .collect();
    files.sort();
    assert!(!files.is_empty(), "found no .solar files in examples/");
    files
}

#[test]
fn all_examples_lower_to_ir() {
    for path in &example_files() {
        let name = path.file_name().unwrap().to_str().unwrap();
        eprintln!("lowering {name}");
        let typed = solar::pipeline::compile(path).unwrap();
        typed.to_mangled().to_ir();
    }
}

// Exercises native code generation without running the examples.
#[test]
fn all_examples_compile_debug() {
    test_utils::ensure_runtime_built();
    for path in &example_files() {
        let name = path.file_name().unwrap().to_str().unwrap();
        eprintln!("compiling {name}");
        let test_name = format!("example_compile_{}", name.replace(".solar", ""));
        solar::pipeline::compile(path)
            .unwrap()
            .to_mangled()
            .to_ir()
            .optimized()
            .to_c(&path.display().to_string())
            .to_binary(&test_name, CompileMode::Debug);
    }
}
