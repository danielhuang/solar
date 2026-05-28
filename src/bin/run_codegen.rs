use std::path::Path;
use std::process::Command;

use solar::pipeline::CompileMode;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    assert!(args.len() == 2, "usage: run_codegen <filename>");

    let filename = &args[1];
    let file_path = Path::new(filename);
    let test_name = file_path.file_stem().unwrap().to_str().unwrap();

    let binary = solar::pipeline::compile(file_path)
        .unwrap()
        .to_ir()
        .to_c(filename)
        .to_binary(test_name, CompileMode::Debug);

    let status = Command::new(binary.path.canonicalize().unwrap())
        .env("ASAN_OPTIONS", "detect_leaks=0")
        .status()
        .unwrap();

    std::process::exit(status.code().unwrap_or(1));
}
