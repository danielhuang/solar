//! The interpreters emulate stdout writes without issuing arbitrary syscalls.

use std::io::{self, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};

struct PartialOutput {
    calls: usize,
    bytes: Vec<u8>,
}

impl Write for PartialOutput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.calls += 1;
        match self.calls {
            1 => Err(io::ErrorKind::Interrupted.into()),
            2 => Err(io::Error::from_raw_os_error(32)),
            _ => {
                let count = bytes.len().min(2);
                self.bytes.extend_from_slice(&bytes[..count]);
                Ok(count)
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn stdout_syscall_retries_and_reports_errors_through_solar() {
    let directory = tempdir::TempDir::new("solar-test").unwrap();
    let source = directory.path().join("stdout.solar");
    std::fs::write(
        &source,
        r#"fn main() {
            try { println("discard"&); } catch (e) { println(e); }
        }"#,
    )
    .unwrap();
    for ast in [true, false] {
        let mangled = solar::pipeline::compile(&source).unwrap().to_mangled();
        let mut output = PartialOutput {
            calls: 0,
            bytes: Vec::new(),
        };
        if ast {
            solar::ast_interp::interpret_to(&mangled.mangled, io::empty(), &mut output);
        } else {
            solar::ir_interp::interpret_to(&mangled.to_ir().ir, io::empty(), &mut output);
        }
        assert_eq!(
            output.bytes,
            b"file_write_partial failed: Broken pipe (os error 32)\n"
        );
    }
}

#[test]
fn interpreters_reject_other_syscalls_and_descriptors() {
    let directory = tempdir::TempDir::new("solar-test").unwrap();
    for (number, fd) in [(39, 1), (1, 0), (1, 2)] {
        let source = directory.path().join("unsupported.solar");
        std::fs::write(
            &source,
            format!(
                "import intrinsics from \"@intrinsics\";\n\
                 fn main() {{ unsafe {{ intrinsics::syscall({number}i64, {fd}i64, \"x\"&, 1u64); }} }}"
            ),
        )
        .unwrap();
        let mangled = solar::pipeline::compile(&source).unwrap().to_mangled();
        assert!(catch_unwind(AssertUnwindSafe(|| test_utils::run_ast(&mangled))).is_err());
        let ir = mangled.to_ir();
        assert!(catch_unwind(AssertUnwindSafe(|| test_utils::run_ir(&ir))).is_err());
    }
}
