//! TEMPORARY probe harness — delete before finishing.
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

fn show(label: &str, r: Result<String, Box<dyn std::any::Any + Send>>) {
    match r {
        Ok(s) => eprintln!("  {label}: OK {:?}", s),
        Err(e) => {
            let msg = e
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "<non-string panic>".to_string());
            eprintln!("  {label}: PANIC {:?}", msg);
        }
    }
}

#[test]
fn probe_all() {
    let dir = Path::new(
        "/tmp/claude-1000/-workspaces-solar/bc6acd8e-60e7-410c-ba71-fffb073aa6cd/scratchpad/probes",
    );
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "solar"))
        .collect();
    files.sort();
    for f in &files {
        let name = f.file_stem().unwrap().to_str().unwrap().to_string();
        eprintln!("=== {name}");
        show(
            "ast",
            catch_unwind(AssertUnwindSafe(|| test_utils::run_ast_file(f))),
        );
        show(
            "ir ",
            catch_unwind(AssertUnwindSafe(|| test_utils::run_ir_file(f))),
        );
        show(
            "cg ",
            catch_unwind(AssertUnwindSafe(|| {
                test_utils::run_codegen_file(f, &format!("probe_{name}"))
            })),
        );
    }
}
