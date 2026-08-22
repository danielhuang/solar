//! Virtual file-descriptor table shared by the interpreters.

use std::io::{Read, Write};

/// A stream usable for both reading and writing. Real files implement both; the
/// standard streams use the wrappers below to stub the unsupported half.
pub trait ReadWrite: Read + Write {}
impl<T: Read + Write> ReadWrite for T {}

/// Wraps a reader (process stdin) as a `ReadWrite`; writing panics.
struct StdinStream<R: Read>(R);
impl<R: Read> Read for StdinStream<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}
impl<R: Read> Write for StdinStream<R> {
    fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
        panic!("cannot write to stdin");
    }
    fn flush(&mut self) -> std::io::Result<()> {
        panic!("cannot write to stdin");
    }
}

/// Wraps a writer (process stdout/stderr) as a `ReadWrite`; reading panics.
struct StdoutStream<W: Write>(W);
impl<W: Write> Read for StdoutStream<W> {
    fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
        panic!("cannot read from stdout");
    }
}
impl<W: Write> Write for StdoutStream<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

/// Index of stdin in the table.
pub const STDIN: usize = 0;
/// Index of stdout in the table.
pub const STDOUT: usize = 1;
/// Index of stderr in the table.
pub const STDERR: usize = 2;

/// The interpreters' virtual `FileDesc` table.
pub struct FileTable<'io> {
    files: Vec<Box<dyn ReadWrite + 'io>>,
}

impl<'io> FileTable<'io> {
    /// Build a table seeded with stdin at index 0, stdout at index 1, and the
    /// real process stderr at index 2.
    pub fn new(stdin: impl Read + 'io, stdout: impl Write + 'io) -> Self {
        FileTable {
            files: vec![
                Box::new(StdinStream(stdin)),
                Box::new(StdoutStream(stdout)),
                Box::new(StdoutStream(std::io::stderr())),
            ],
        }
    }

    /// Write `buf` to the stream at `fd`, returning the byte count actually
    /// written (a single, possibly partial, write).
    pub fn write_partial(&mut self, fd: usize, buf: &[u8]) -> std::io::Result<usize> {
        self.files[fd].write(buf)
    }
}
