//! Ensures tail-merged allocations retain their GC metadata.

use solar::pipeline::CompileOptions;
use std::process::Command;

const SRC: &str = r#"
static TESTS: Int = 0;
static FAILS: Int = 0;

fn report(name: &[Uint8], actual: Bool, expected: Bool) {
    TESTS = TESTS + 1;
    if actual == expected {
        println(("PASS " + name@)&);
    } else {
        FAILS = FAILS + 1;
        println(("FAIL " + name@)&);
    }
}

fn main() {
    report("one"&, true, true);
    report("two"&, false, true);
    report("three"&, true, true);
    report("four"&, false, true);
    report("five"&, true, true);
    report("six"&, false, true);
    report("seven"&, true, true);
    report("eight"&, false, true);
    report("nine"&, true, true);
    report("ten"&, false, true);
    report("eleven"&, true, true);
    report("twelve"&, false, true);
    report("thirteen"&, true, true);
    println(TESTS);
    println(FAILS);
}
"#;

#[test]
fn release_branch_merge_preserves_gc_allocation_provenance() {
    test_utils::ensure_release_runtime_built();
    let dir = std::env::temp_dir().join("solar_test_release_alloc_metadata");
    std::fs::create_dir_all(&dir).unwrap();
    let src_path = dir.join("release_alloc_metadata.solar");
    std::fs::write(&src_path, SRC).unwrap();

    let bin = solar::pipeline::compile(&src_path)
        .unwrap()
        .to_mangled()
        .to_ir()
        .optimized()
        .to_c(&src_path.display().to_string())
        .to_binary("release_alloc_metadata", CompileOptions::RELEASE)
        .path;
    let out = Command::new(bin).output().unwrap();
    assert!(
        out.status.success(),
        "release binary failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.starts_with("PASS one\nFAIL two\n"), "{stdout}");
    assert!(stdout.ends_with("PASS thirteen\n13\n6\n"), "{stdout}");
}
