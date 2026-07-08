//! Virtual file table shared by both interpreters.
//!
//! The compiled runtime represents a `FileDesc` as an opaque pointer into a
//! GC-traced fd arena and talks to the OS via `libc`. The interpreters have no
//! arena and no collector, so they model a `FileDesc` as a plain integer index
//! into this table. Index 0 is stdin, index 1 is stdout, and index 2 is stderr
//! (matching `file::stdin()` / `file::stdout()` / `file::stderr()`);
//! `file_open` pushes an entry and returns its index.
//!
//! Entries come in three kinds: the standard `Stream`s (in-memory in tests), a
//! real `File` (supports positioned I/O, sync, and locks), and a `Dir` — a
//! directory opened with `O_DIRECTORY`, whose entries are read eagerly at open
//! and drained by `dir_read` (the compiled runtime reads batches lazily with
//! `getdents64(2)`; small directories behave identically). Unsupported
//! combinations return the same `errno`-based errors the compiled runtime's
//! raw syscalls would produce, keeping thrown messages byte-identical.

use std::fs::OpenOptions;
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

// Raw errno values used to mirror the compiled runtime's syscall errors (the
// interpreters' `solar` crate has no libc dependency).
const EISDIR: i32 = 21; // read(2) on a directory fd
const EBADF: i32 = 9; // write(2) on an O_RDONLY directory fd
const ESPIPE: i32 = 29; // pread/pwrite(2) on a non-seekable stream
const ENOTDIR: i32 = 20; // getdents64(2) on a non-directory fd

fn errno(code: i32) -> std::io::Error {
    std::io::Error::from_raw_os_error(code)
}

enum Entry<'io> {
    Stream(Box<dyn ReadWrite + 'io>),
    File(std::fs::File),
    /// Pre-read directory entries (each `[kind byte] ++ name`), drained by
    /// `dir_read`.
    Dir(std::vec::IntoIter<Vec<u8>>),
}

/// The interpreters' virtual `FileDesc` table.
pub struct FileTable<'io> {
    files: Vec<Entry<'io>>,
}

impl<'io> FileTable<'io> {
    /// Build a table seeded with stdin at index 0, stdout at index 1, and the
    /// real process stderr at index 2.
    pub fn new(stdin: impl Read + 'io, stdout: impl Write + 'io) -> Self {
        FileTable {
            files: vec![
                Entry::Stream(Box::new(StdinStream(stdin))),
                Entry::Stream(Box::new(StdoutStream(stdout))),
                Entry::Stream(Box::new(StdoutStream(std::io::stderr()))),
            ],
        }
    }

    /// Open `path` with the given `open(2)` `flags` and creation `mode`, push it,
    /// and return its `FileDesc` index. The flag bits match the Linux values used
    /// by `@std`'s `file::open` (the compiled runtime passes them straight to
    /// `open(2)`); here they're decoded into `OpenOptions`. Errors are returned
    /// so the interpreters can throw them as catchable Solar exceptions.
    pub fn open(&mut self, path: &str, flags: i64, mode: u32) -> std::io::Result<usize> {
        use std::os::unix::fs::OpenOptionsExt;
        // POSIX open(2) flags (Linux values).
        const O_WRONLY: i64 = 1;
        const O_RDWR: i64 = 2;
        const O_ACCMODE: i64 = 3;
        const O_CREAT: i64 = 64;
        const O_EXCL: i64 = 128;
        const O_TRUNC: i64 = 512;
        const O_APPEND: i64 = 1024;
        const O_DIRECTORY: i64 = 0o200000;

        if flags & O_DIRECTORY != 0 {
            // `@std`'s `file::open_dir`: eagerly read the entries `getdents64`
            // would return. `read_dir` omits "." and ".." but the syscall
            // includes them, so they're synthesized (the `@std` wrapper filters
            // them back out).
            let mut entries: Vec<Vec<u8>> = vec![vec![1, b'.'], vec![1, b'.', b'.']];
            for e in std::fs::read_dir(path)? {
                let e = e?;
                let kind: u8 = match e.file_type() {
                    Ok(t) if t.is_file() => 0,
                    Ok(t) if t.is_dir() => 1,
                    _ => 2,
                };
                let mut bytes = vec![kind];
                bytes.extend_from_slice(e.file_name().as_encoded_bytes());
                entries.push(bytes);
            }
            self.files.push(Entry::Dir(entries.into_iter()));
            return Ok(self.files.len() - 1);
        }

        let access = flags & O_ACCMODE;
        let mut o = OpenOptions::new();
        o.read(access != O_WRONLY);
        o.write(access == O_WRONLY || access == O_RDWR);
        o.append(flags & O_APPEND != 0);
        o.truncate(flags & O_TRUNC != 0);
        if flags & O_EXCL != 0 {
            o.create_new(true);
        } else if flags & O_CREAT != 0 {
            o.create(true);
        }
        o.mode(mode);
        let f = o.open(path)?;
        self.files.push(Entry::File(f));
        Ok(self.files.len() - 1)
    }

    /// Read into `buf` from the stream at `fd`, returning the byte count.
    pub fn read(&mut self, fd: usize, buf: &mut [u8]) -> std::io::Result<usize> {
        match &mut self.files[fd] {
            Entry::Stream(s) => s.read(buf),
            Entry::File(f) => f.read(buf),
            Entry::Dir(_) => Err(errno(EISDIR)),
        }
    }

    /// Write `buf` to the stream at `fd`, returning the byte count actually
    /// written (a single, possibly partial, write).
    pub fn write_partial(&mut self, fd: usize, buf: &[u8]) -> std::io::Result<usize> {
        match &mut self.files[fd] {
            Entry::Stream(s) => s.write(buf),
            Entry::File(f) => f.write(buf),
            Entry::Dir(_) => Err(errno(EBADF)),
        }
    }

    /// `pread(2)`: read into `buf` at absolute `offset` without moving the
    /// cursor.
    pub fn read_at(&mut self, fd: usize, buf: &mut [u8], offset: u64) -> std::io::Result<usize> {
        use std::os::unix::fs::FileExt;
        match &mut self.files[fd] {
            Entry::Stream(_) => Err(errno(ESPIPE)),
            Entry::File(f) => f.read_at(buf, offset),
            Entry::Dir(_) => Err(errno(EISDIR)),
        }
    }

    /// `pwrite(2)`: write `buf` at absolute `offset` without moving the cursor
    /// (a single, possibly partial, write).
    pub fn write_at(&mut self, fd: usize, buf: &[u8], offset: u64) -> std::io::Result<usize> {
        use std::os::unix::fs::FileExt;
        match &mut self.files[fd] {
            Entry::Stream(_) => Err(errno(ESPIPE)),
            Entry::File(f) => f.write_at(buf, offset),
            Entry::Dir(_) => Err(errno(EBADF)),
        }
    }

    /// `fsync(2)`. A no-op on the standard streams and virtual dirs.
    pub fn sync(&mut self, fd: usize) -> std::io::Result<()> {
        match &mut self.files[fd] {
            Entry::File(f) => f.sync_all(),
            Entry::Stream(_) | Entry::Dir(_) => Ok(()),
        }
    }

    /// `flock(2)` with the raw `LOCK_*` op word. Returns `Ok(false)` only when
    /// a non-blocking request would have to wait. Locking a non-`File` entry is
    /// a successful no-op (the compiled runtime can flock any fd; the virtual
    /// streams have nothing to lock).
    pub fn lock(&mut self, fd: usize, op: i64) -> std::io::Result<bool> {
        const LOCK_SH: i64 = 1;
        const LOCK_NB: i64 = 4;
        const LOCK_UN: i64 = 8;
        let f = match &mut self.files[fd] {
            Entry::File(f) => f,
            Entry::Stream(_) | Entry::Dir(_) => return Ok(true),
        };
        if op & LOCK_UN != 0 {
            f.unlock()?;
            return Ok(true);
        }
        let shared = op & LOCK_SH != 0;
        if op & LOCK_NB != 0 {
            let r = if shared {
                f.try_lock_shared()
            } else {
                f.try_lock()
            };
            return match r {
                Ok(()) => Ok(true),
                Err(std::fs::TryLockError::WouldBlock) => Ok(false),
                Err(std::fs::TryLockError::Error(e)) => Err(e),
            };
        }
        if shared {
            f.lock_shared()?
        } else {
            f.lock()?
        }
        Ok(true)
    }

    /// One `getdents64(2)` batch: all remaining pre-read entries (empty once
    /// exhausted). Each entry is `[kind byte] ++ name`.
    pub fn dir_read(&mut self, fd: usize) -> std::io::Result<Vec<Vec<u8>>> {
        match &mut self.files[fd] {
            Entry::Dir(entries) => Ok(entries.by_ref().collect()),
            Entry::Stream(_) | Entry::File(_) => Err(errno(ENOTDIR)),
        }
    }
}

/// `stat(2)` for the interpreters: `Ok(Some((size, mtime_nanos, kind)))` on
/// success (kind 0 = file, 1 = dir, 2 = other), `Ok(None)` when the path
/// doesn't exist (`ENOENT`/`ENOTDIR`), `Err` otherwise — mirroring
/// `sol_file_stat`.
pub fn stat_path(path: &str) -> std::io::Result<Option<(u64, u64, u64)>> {
    use std::os::unix::fs::MetadataExt;
    match std::fs::metadata(path) {
        Ok(m) => {
            let kind = if m.file_type().is_file() {
                0
            } else if m.file_type().is_dir() {
                1
            } else {
                2
            };
            let mtime = (m.mtime() as u64)
                .wrapping_mul(1_000_000_000)
                .wrapping_add(m.mtime_nsec() as u64);
            Ok(Some((m.len(), mtime, kind)))
        }
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

/// `mkdir(2)` with explicit permission bits, for the interpreters.
pub fn create_dir(path: &str, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new().mode(mode).create(path)
}
