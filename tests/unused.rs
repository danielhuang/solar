use std::path::Path;

fn collect_files(dir: &Path, ext: &str) -> Vec<std::path::PathBuf> {
    let mut results = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            results.extend(collect_files(&path, ext));
        } else if path.extension().is_some_and(|e| e == ext) {
            results.push(path);
        }
    }
    results
}

#[test]
fn no_unused_solar_fixtures() {
    let tests_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");

    let solar_files = collect_files(&tests_dir, "solar");
    assert!(!solar_files.is_empty(), "found no .solar files");

    let rs_files = collect_files(&tests_dir, "rs");
    let mut all_contents: String = rs_files
        .iter()
        .map(|p| std::fs::read_to_string(p).unwrap())
        .collect();
    // Also scan .solar files for cross-references (multi-file imports)
    for p in &solar_files {
        all_contents.push_str(&std::fs::read_to_string(p).unwrap());
    }

    let mut unused = Vec::new();
    for path in &solar_files {
        let name = path.file_name().unwrap().to_str().unwrap();
        if !all_contents.contains(name) {
            unused.push(path.strip_prefix(&tests_dir).unwrap().display().to_string());
        }
    }

    assert!(
        unused.is_empty(),
        "unused .solar fixtures:\n  {}",
        unused.join("\n  ")
    );
}
