fn main() {
    println!("cargo:rerun-if-changed=grammar.js");

    let status = std::process::Command::new("tree-sitter")
        .arg("generate")
        .status()
        .unwrap();
    assert!(status.success(), "tree-sitter generate failed");

    let src_dir = std::path::Path::new("src");

    cc::Build::new()
        .include(src_dir)
        .file(src_dir.join("parser.c"))
        .flag_if_supported("-Wno-unused-but-set-variable")
        .flag_if_supported("-Wno-trigraphs")
        .compile("tree_sitter_solar");
}
