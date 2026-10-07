//! Retained objects and concurrently allocated replacements across sweeps.

use solar::pipeline::CompileOptions;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn live_nodes_remain_distinct_during_concurrent_reuse() {
    test_utils::ensure_runtime_built();
    test_utils::ensure_release_runtime_built();
    let directory = tempdir::TempDir::new("solar-test").unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/gc_sweep_reuse/retention.solar");
    for (i, options) in [
        CompileOptions::RELEASE,
        CompileOptions::GC_SAN,
        CompileOptions {
            gc_san: true,
            ..CompileOptions::DEBUG
        },
    ]
    .into_iter()
    .enumerate()
    {
        let binary = solar::pipeline::compile(&source)
            .unwrap()
            .to_mangled()
            .to_tree_ir()
            .optimized()
            .to_c(&source.display().to_string())
            .to_binary(directory.path().join(format!("reuse-{i}")), options);
        let mut child = Command::new(binary.path)
            .env("SOLAR_THREAD_POOL_SIZE", "4")
            .env("ASAN_OPTIONS", "detect_leaks=0")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                child.kill().unwrap();
                panic!(
                    "sweep reuse timed out: {:?}",
                    child.wait_with_output().unwrap()
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, b"sweep reuse passed\n");
    }
}
