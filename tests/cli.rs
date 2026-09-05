use std::path::Path;
use std::process::{Command, Output};

fn solar() -> Command {
    Command::new(env!("CARGO_BIN_EXE_solar"))
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn fixture() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/runtime/iterator.solar")
}

#[test]
fn interpreters_run_the_program() {
    for interpreter in ["ast", "ir"] {
        let output = solar()
            .arg("run")
            .arg(fixture())
            .args(["--interp", interpreter])
            .output()
            .unwrap();
        assert_success(&output);
        assert_eq!(output.stdout, b"passed\n");
    }
}

#[test]
fn check_does_not_execute_the_program() {
    let directory = tempdir::TempDir::new("solar-cli-test").unwrap();
    let source = directory.path().join("check.solar");
    std::fs::write(&source, "fn main() { assert(false); }\n").unwrap();
    let output = solar().arg("check").arg(source).output().unwrap();
    assert_success(&output);
    assert!(output.stdout.is_empty());
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn compilation_errors_are_reported_by_each_command() {
    let directory = tempdir::TempDir::new("solar-cli-test").unwrap();
    let source = directory.path().join("invalid.solar");
    std::fs::write(&source, "fn main() { let x: Int = true; }\n").unwrap();
    for command in ["check", "compile", "run"] {
        let mut cli = solar();
        cli.arg(command).arg(&source);
        if command == "compile" {
            cli.arg(directory.path().join("program"));
        }
        let output = cli.output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("invalid.solar"), "{stderr}");
        assert!(stderr.contains("Bool"), "{stderr}");
    }
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn fmt_handles_multiple_files_and_validates_before_writing() {
    let directory = tempdir::TempDir::new("solar-cli-test").unwrap();
    let first = directory.path().join("first.solar");
    let second = directory.path().join("second.solar");
    let original = "fn main(){let x=1;}\n";
    std::fs::write(&first, original).unwrap();
    std::fs::write(&second, "$").unwrap();
    let output = solar().arg("fmt").args([&first, &second]).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(std::fs::read_to_string(&first).unwrap(), original);
    std::fs::write(&second, original).unwrap();
    assert_success(&solar().arg("fmt").args([&first, &second]).output().unwrap());
    let expected = solar::fmt::format_source(original).unwrap();
    assert_ne!(expected, original);
    for path in [&first, &second] {
        assert_eq!(std::fs::read_to_string(path).unwrap(), expected);
    }
}

#[test]
fn cli_rejects_missing_arguments_and_conflicting_modes() {
    for args in [
        vec![],
        vec!["compile", "source.solar"],
        vec!["fmt"],
        vec!["check"],
        vec!["run"],
        vec!["run", "source.solar", "--interp"],
        vec!["run", "source.solar", "--interp", "other"],
        vec!["run", "source.solar", "--interp", "ast", "--release"],
        vec!["run", "source.solar", "--interp", "ir", "--gc-san"],
    ] {
        let output = solar().args(&args).output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?}");
    }
    assert_success(&solar().arg("--help").output().unwrap());
}

#[test]
fn native_run_cleans_up_in_every_build_mode() {
    test_utils::ensure_runtime_built();
    test_utils::ensure_release_runtime_built();
    let directory = tempdir::TempDir::new("solar-cli-test").unwrap();
    for flags in [
        vec![],
        vec!["--release"],
        vec!["--gc-san"],
        vec!["--release", "--gc-san"],
    ] {
        let output = solar()
            .arg("run")
            .arg(fixture())
            .args(flags)
            .env("TMPDIR", directory.path())
            .output()
            .unwrap();
        assert_success(&output);
        assert_eq!(output.stdout, b"passed\n");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }
}

#[test]
fn native_run_preserves_nonzero_exit_status_and_cleans_up() {
    test_utils::ensure_runtime_built();
    let directory = tempdir::TempDir::new("solar-cli-test").unwrap();
    let source = directory.path().join("exit.solar");
    std::fs::write(
        &source,
        "import {exit} from \"@intrinsics\";\nfn main() { exit(23); }\n",
    )
    .unwrap();
    let scratch = directory.path().join("scratch");
    std::fs::create_dir(&scratch).unwrap();
    let output = solar()
        .arg("run")
        .arg(source)
        .env("TMPDIR", &scratch)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(23), "{output:?}");
    assert_eq!(std::fs::read_dir(scratch).unwrap().count(), 0);
}

#[test]
fn compile_defaults_to_debug_and_keeps_the_executable() {
    test_utils::ensure_runtime_built();
    let directory = tempdir::TempDir::new("solar-cli-test").unwrap();
    let destination = directory.path().join("nested/program with spaces");
    assert_success(
        &solar()
            .arg("compile")
            .arg(fixture())
            .arg(&destination)
            .output()
            .unwrap(),
    );
    assert!(destination.is_file());
    assert_eq!(
        solar::pipeline::Binary { path: destination }.run("cli_compile"),
        "passed\n"
    );
}
