//! Builds native fallbacks for the helpers also inlined into release programs.

fn main() {
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let mut archive = cc::Build::new();
    for name in ["atomic128", "hot", "allocators"] {
        let source = format!("src/{name}.ll");
        let object = format!("{out_dir}/{name}.o");
        let status = std::process::Command::new("clang")
            .args([
                "-c",
                "-O3",
                "-march=native",
                "-fPIC",
                &source,
                "-o",
                &object,
            ])
            .status()
            .unwrap();
        assert!(status.success(), "failed to compile {source}");
        archive.object(&object);
        println!("cargo:rerun-if-changed={source}");
    }
    archive.compile("solar_hot");
}
