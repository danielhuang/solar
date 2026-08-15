use std::path::{Path, PathBuf};

fn collect_solar_files(directory: &Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(directory).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let file_type = entry.file_type().unwrap();
        if file_type.is_dir() {
            if !matches!(
                entry.file_name().to_str(),
                Some(".git" | "node_modules" | "target")
            ) {
                collect_solar_files(&path, files);
            }
        } else if file_type.is_file() && path.extension().is_some_and(|ext| ext == "solar") {
            files.push(path);
        }
    }
}

#[test]
fn all_solar_sources_are_formatted() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    collect_solar_files(root, &mut files);
    files.sort();
    assert!(!files.is_empty(), "found no Solar source files");

    let mut failures = Vec::new();
    for path in files {
        let relative = path.strip_prefix(root).unwrap();
        let source = std::fs::read_to_string(&path).unwrap();
        match solar::fmt::format_source(&source) {
            Ok(formatted) if formatted != source => {
                failures.push(format!("{} is not formatted", relative.display()));
            }
            Err(error) => {
                failures.push(format!("{}: {error}", relative.display()));
            }
            Ok(_) => {}
        }
    }

    assert!(
        failures.is_empty(),
        "Solar formatting failures:\n  {}\n\nrun `cargo run --bin fmt -- $(rg --files -g '*.solar')`",
        failures.join("\n  ")
    );
}
