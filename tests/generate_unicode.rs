//! Checks the Unicode generator's reproducibility and generated predicate behavior.

use std::path::Path;
use std::process::Command;

const CLASSIFY: &str = r#"
import data from "unicode_data.solar";

fn classification(code: Uint32) -> Uint8 {
    (if data::is_alphabetic(code) { 1u8 } else { 0u8 }) |
    (if data::is_numeric(code) { 2u8 } else { 0u8 }) |
    (if data::is_uppercase(code) { 4u8 } else { 0u8 }) |
    (if data::is_whitespace(code) { 8u8 } else { 0u8 })
}

fn main() {
    let bytes = [0u8; 4096u];
    for block in 0u32..272u32 {
        for offset in 0u..4096u {
            bytes[offset] = classification(block * 4096u32 + Uint32(offset));
        }
        write_stdout(bytes&);
    }
    for code in [1114112u32, 1114113u32, 2147483647u32, 2147483648u32, 4294967295u32] {
        assert(classification(code@) == 0u8);
    }
}
"#;

#[test]
fn generated_unicode_is_current_and_matches_rust() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let directory = tempdir::TempDir::new("solar-unicode").unwrap();
    let generator = directory.path().join("generate_unicode");
    let output = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
        .current_dir(root)
        .arg(root.join("tools/generate_unicode.rs"))
        .arg("-o")
        .arg(&generator)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "generator compilation failed: {output:?}"
    );

    let generated = directory.path().join("unicode_data.solar");
    let output = Command::new(generator).arg(&generated).output().unwrap();
    assert!(
        output.status.success(),
        "Unicode generation failed: {output:?}"
    );
    let formatted =
        solar::fmt::format_source(&std::fs::read_to_string(&generated).unwrap()).unwrap();
    let checked_in = std::fs::read_to_string(root.join("src/std/unicode_data.solar")).unwrap();
    assert!(
        formatted == checked_in,
        "Unicode data is stale; regenerate and format it as documented in tools/generate_unicode.rs"
    );
    std::fs::write(&generated, formatted).unwrap();

    let source = directory.path().join("classify.solar");
    std::fs::write(&source, solar::fmt::format_source(CLASSIFY).unwrap()).unwrap();
    let actual = test_utils::run_release_file(&source, "unicode_predicates");
    assert_eq!(
        actual.len(),
        0x110000,
        "one classification byte per code point"
    );
    for (code, actual) in actual.bytes().enumerate() {
        let expected = char::from_u32(code as u32).map_or(0, |ch| {
            u8::from(ch.is_alphabetic())
                | (u8::from(ch.is_numeric()) << 1)
                | (u8::from(ch.is_uppercase()) << 2)
                | (u8::from(ch.is_whitespace()) << 3)
        });
        assert_eq!(
            actual, expected,
            "Unicode predicates disagree at U+{code:04X}"
        );
    }
}
