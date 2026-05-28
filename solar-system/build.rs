fn main() {
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let bc_path = format!("{out_dir}/atomic128.o");

    let status = std::process::Command::new("llvm-as")
        .args(["src/atomic128.ll", "-o", &bc_path])
        .status()
        .unwrap();
    assert!(status.success(), "llvm-as failed");

    cc::Build::new().object(&bc_path).compile("atomic128");
    println!("cargo:rerun-if-changed=src/atomic128.ll");
}
