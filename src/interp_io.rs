//! Virtual file table shared by both interpreters.
//!
//! The compiled runtime represents a `FileDesc` as an opaque pointer into a
//! GC-traced fd arena and talks to the OS via `libc`. The interpreters have no
//! arena and no collector, so they model a `FileDesc` as a plain integer index
//! into this table of boxed `Read + Write` streams. Index 0 is stdin and index
//! 1 is stdout (matching `file::stdin()` / `file::stdout()`); `file_open` pushes
//! a `File` and returns its index.

use std::fs::File;
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

/// Wraps a writer (process stdout) as a `ReadWrite`; reading panics.
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

/// The interpreters' virtual `FileDesc` table.
pub struct FileTable<'io> {
    files: Vec<Box<dyn ReadWrite + 'io>>,
}

impl<'io> FileTable<'io> {
    /// Build a table seeded with stdin at index 0 and stdout at index 1.
    pub fn new(stdin: impl Read + 'io, stdout: impl Write + 'io) -> Self {
        FileTable {
            files: vec![Box::new(StdinStream(stdin)), Box::new(StdoutStream(stdout))],
        }
    }

    /// Open `path` read-only, push it, and return its `FileDesc` index.
    pub fn open(&mut self, path: &str) -> usize {
        let f =
            File::open(path).unwrap_or_else(|e| panic!("file_open: could not open {path:?}: {e}"));
        self.files.push(Box::new(f));
        self.files.len() - 1
    }

    /// Read into `buf` from the stream at `fd`, returning the byte count.
    pub fn read(&mut self, fd: usize, buf: &mut [u8]) -> usize {
        self.files[fd]
            .read(buf)
            .unwrap_or_else(|e| panic!("file_read failed: {e}"))
    }

    /// Write `buf` to the stream at `fd`, returning the byte count actually
    /// written (a single, possibly partial, write).
    pub fn write_partial(&mut self, fd: usize, buf: &[u8]) -> usize {
        self.files[fd]
            .write(buf)
            .unwrap_or_else(|e| panic!("file_write_partial failed: {e}"))
    }

    /// Write all of `buf` to the stream at `fd` and flush. Used by `write_stdout`.
    pub fn write_all(&mut self, fd: usize, buf: &[u8]) {
        let f = &mut self.files[fd];
        f.write_all(buf).unwrap();
        f.flush().unwrap();
    }
}
