use std::fs;
use std::path::Path;

#[test]
fn all_examples_lower_to_ir() {
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

    for path in &files {
        let name = path.file_name().unwrap().to_str().unwrap();
        eprintln!("lowering {name}");
        let typed = solar::pipeline::compile(path).unwrap();
        typed.to_ir();
    }
}
