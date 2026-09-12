//! Builds the LLVM pass plugin used by native code generation.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let hot_sources = [
        "solar-system/src/atomic128.ll",
        "solar-system/src/hot.ll",
        "solar-system/src/allocators.ll",
    ];
    let hot_bc = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("solar_hot.bc");
    for source in hot_sources {
        println!("cargo:rerun-if-changed={source}");
    }
    let status = Command::new("llvm-link")
        .args(hot_sources)
        .arg("-o")
        .arg(&hot_bc)
        .status()
        .unwrap();
    assert!(status.success(), "failed to assemble hot runtime helpers");
    println!("cargo:rustc-env=SOLAR_HOT_BITCODE={}", hot_bc.display());

    println!("cargo:rerun-if-changed=llvm-pass/SolarWriteBarriers.cpp");
    println!("cargo:rerun-if-changed=build.rs");

    let Some(llvm_config) = ["llvm-config", "llvm-config-23"].into_iter().find(|c| {
        Command::new(c)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }) else {
        println!(
            "cargo:warning=llvm-config not found; GC write-barrier plugin not built \
             (native codegen will fail until llvm-dev + clang++ are installed)"
        );
        return;
    };

    let cxxflags = String::from_utf8(
        Command::new(llvm_config)
            .arg("--cxxflags")
            .output()
            .expect("run llvm-config --cxxflags")
            .stdout,
    )
    .expect("llvm-config --cxxflags is not UTF-8");

    let out_dir = std::env::var("OUT_DIR").unwrap();
    let so = PathBuf::from(&out_dir).join("SolarWriteBarriers.so");

    let status = Command::new("clang++")
        .args(cxxflags.split_whitespace())
        .args(["-fPIC", "-shared", "llvm-pass/SolarWriteBarriers.cpp", "-o"])
        .arg(&so)
        .status()
        .expect("run clang++ to build the write-barrier plugin");
    assert!(
        status.success(),
        "GC write-barrier plugin failed to compile"
    );

    println!("cargo:rustc-env=SOLAR_WB_PLUGIN={}", so.display());
}
