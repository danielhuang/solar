use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{CompileError, SourceMap};
use crate::{codegen, ir, resolve, typed_ast};

/// Entry point: file path -> resolved + type-checked program.
pub fn compile(file_path: &Path) -> Result<Typed, Vec<CompileError>> {
    let (ast, source_map) = resolve::resolve(file_path)?;
    let typed = typed_ast::lower(&ast).map_err(|e| vec![e])?;
    Ok(Typed { typed, source_map })
}

pub struct Typed {
    pub typed: typed_ast::SourceFile,
    pub source_map: SourceMap,
}

impl Typed {
    pub fn to_ir(self) -> Ir {
        let ir = ir::lower(&self.typed);
        Ir {
            ir,
            source_map: self.source_map,
        }
    }
}

pub struct Ir {
    pub ir: ir::Module,
    pub source_map: SourceMap,
}

impl Ir {
    pub fn to_c(&self, source_file: &str) -> CSource {
        let c_source = codegen::generate(&self.ir, source_file);
        CSource {
            c_source,
            source_map: self.source_map.clone(),
        }
    }
}

pub struct CSource {
    pub c_source: String,
    pub source_map: SourceMap,
}

pub enum CompileMode {
    /// ASAN + simple clang, links target/debug/libsolar_system
    Debug,
    /// LLVM LTO, cross-language optimization, allocator attribute stamping
    Release,
}

impl CSource {
    /// Compile to native binary. Intermediate files go in `target/solar/{name}_{random_hex}/`
    /// and are kept for debugging.
    pub fn to_binary(self, name: &str, mode: CompileMode) -> Binary {
        let unique: u64 = rand::random();
        let slug = format!("{name}_{unique:x}");
        let dir = Path::new("target/solar").join(&slug);
        std::fs::create_dir_all(&dir).unwrap();

        let c_path = dir.join(format!("{name}.c"));
        std::fs::write(&c_path, &self.c_source).unwrap();

        let bin_path = match mode {
            CompileMode::Debug => compile_debug(&c_path, &dir, name),
            CompileMode::Release => compile_release(&c_path, &dir, name),
        };

        Binary { path: bin_path }
    }
}

pub struct Binary {
    pub path: PathBuf,
}

impl Binary {
    /// Execute the binary and return its stdout.
    pub fn run(&self, name: &str) -> String {
        let output = Command::new(self.path.canonicalize().unwrap())
            .env("ASAN_OPTIONS", "detect_leaks=0")
            .output()
            .unwrap();

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "codegen binary failed for {name} (exit {:?}):\nstdout: {stdout}\nstderr: {stderr}",
            output.status.code(),
        );

        stdout.into_owned()
    }
}

// ---------------------------------------------------------------------------
// Debug compilation: clang + ASAN, links target/debug/libsolar_system
// ---------------------------------------------------------------------------

fn compile_debug(c_path: &Path, dir: &Path, name: &str) -> PathBuf {
    let obj_path = dir.join(format!("{name}.o"));
    let bin_path = dir.join(name);

    let compile = Command::new("clang")
        .args([
            "-fsanitize=address",
            "-fno-omit-frame-pointer",
            "-flto",
            "-g",
            "-c",
            c_path.to_str().unwrap(),
            "-o",
            obj_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "C compilation failed for {name}:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let link = Command::new("clang")
        .args([
            "-fsanitize=address",
            "-flto",
            obj_path.to_str().unwrap(),
            "-L",
            "target/debug",
            "-lsolar_system",
            "-o",
            bin_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        link.status.success(),
        "linking failed for {name}:\n{}",
        String::from_utf8_lossy(&link.stderr)
    );

    bin_path
}

// ---------------------------------------------------------------------------
// Release compilation: LLVM LTO with cross-language optimization
// ---------------------------------------------------------------------------

/// Enable aggressive LLVM Attributor pass. Currently disabled due to an LLVM bug
/// where the Attributor miscompiles indirect calls through closure environments
/// when combined with allockind("alloc,zeroed") on sol_alloc.
const ATTRIBUTOR_ENABLE_ALL: bool = false;

fn run_cmd(cmd: &str, args: &[&str]) {
    let status = Command::new(cmd)
        .args(args)
        .status()
        .unwrap_or_else(|e| panic!("failed to run {cmd}: {e}"));
    assert!(status.success(), "{cmd} failed with {status}");
}

fn run_piped(cmd: &str, args: &[&str]) -> String {
    let output = Command::new(cmd)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("failed to run {cmd}: {e}"));
    assert!(
        output.status.success(),
        "{cmd} failed with {}",
        output.status
    );
    String::from_utf8(output.stdout).unwrap()
}

fn force_replace(input: &str, from: &str, to: &str) -> String {
    assert!(from != to);
    let new = input.replace(from, to);
    assert!(new != input, "{new:?}");
    new
}

fn compile_release(c_path: &Path, dir: &Path, name: &str) -> PathBuf {
    let runtime_lib = Path::new("target/release/libsolar_system.a");
    assert!(
        runtime_lib.exists(),
        "libsolar_system.a not found at {}",
        runtime_lib.display()
    );

    // Extract bitcode from runtime archive
    eprintln!("=== Extracting bitcode from runtime archive ===");
    run_cmd(
        "ar",
        &[
            "x",
            runtime_lib.to_str().unwrap(),
            "--output",
            dir.to_str().unwrap(),
        ],
    );

    // Find LLVM IR bitcode .o files
    eprintln!("=== Merging Rust bitcode ===");
    let bc_files: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| {
            let path = e.unwrap().path();
            if path.extension().is_some_and(|e| e == "o") {
                let out = run_piped("file", &[path.to_str().unwrap()]);
                if out.contains("LLVM IR bitcode") {
                    return Some(path.to_str().unwrap().to_string());
                }
            }
            None
        })
        .collect();
    assert!(
        !bc_files.is_empty(),
        "no LLVM IR bitcode files found in archive"
    );

    let merged_rust = dir.join("merged_rust.bc");
    {
        let mut link_args = vec!["-o", merged_rust.to_str().unwrap()];
        for f in &bc_files {
            link_args.push(f.as_str());
        }
        run_cmd("llvm-link", &link_args);
    }

    // Compile generated C to bitcode
    eprintln!("=== Compiling generated C to bitcode ===");
    let c_bc = dir.join(format!("{name}_c.bc"));
    {
        let mut clang_args = vec!["-flto=full", "-c", "-march=native", "-O3", "-g"];
        if ATTRIBUTOR_ENABLE_ALL {
            clang_args.extend(["-mllvm", "-attributor-enable=all"]);
        }
        clang_args.extend([c_path.to_str().unwrap(), "-o", c_bc.to_str().unwrap()]);
        run_cmd("clang", &clang_args);
    }

    // Merge C and Rust bitcode
    eprintln!("=== Merging C and Rust bitcode ===");
    let full_bc = dir.join("full.bc");
    run_cmd(
        "llvm-link",
        &[
            c_bc.to_str().unwrap(),
            merged_rust.to_str().unwrap(),
            "-o",
            full_bc.to_str().unwrap(),
        ],
    );

    // Stamp allocator attributes
    eprintln!("=== Stamping allocator attributes ===");
    let full_ll = dir.join("full.ll");
    run_cmd(
        "llvm-dis",
        &[full_bc.to_str().unwrap(), "-o", full_ll.to_str().unwrap()],
    );
    {
        let ll = std::fs::read_to_string(&full_ll).unwrap();
        let mut patched = String::with_capacity(ll.len());
        let mut matched = 0usize;
        for line in ll.lines() {
            if line.contains("@sol_alloc(") && line.starts_with("define") {
                matched += 1;
                let line = force_replace(
                    line,
                    "personality ptr @rust_eh_personality",
                    "noinline allocsize(0) allockind(\"alloc,aligned,zeroed\") personality ptr @rust_eh_personality",
                );
                let line = force_replace(
                    &line,
                    "@sol_alloc(i64 noundef %0, i64 noundef %1, ptr noundef nonnull %2)",
                    "@sol_alloc(i64 noundef %0, i64 noundef allocalign %1, ptr noundef nonnull %2)",
                );
                let line = force_replace(
                    &line,
                    "define noundef ptr @sol_alloc",
                    "define noundef noalias ptr @sol_alloc",
                );
                patched.push_str(&line);
            } else {
                patched.push_str(line);
            }
            patched.push('\n');
        }
        assert!(
            matched == 1,
            "expected exactly 1 sol_alloc definition, found {matched}"
        );
        std::fs::write(&full_ll, patched).unwrap();
    }
    run_cmd(
        "llvm-as",
        &[full_ll.to_str().unwrap(), "-o", full_bc.to_str().unwrap()],
    );

    // Optimize
    eprintln!("=== Optimizing (cross-language inlining) ===");
    let full_opt_bc = dir.join("full_opt.bc");
    {
        let mut opt_args = vec!["-O3"];
        if ATTRIBUTOR_ENABLE_ALL {
            opt_args.push("-attributor-enable=all");
        }
        opt_args.extend([
            full_bc.to_str().unwrap(),
            "-o",
            full_opt_bc.to_str().unwrap(),
        ]);
        run_cmd("opt", &opt_args);
    }

    // Insert GC write barriers. This runs after `opt -O3` so barrier calls
    // don't block allocation elision/SROA; the final clang -O3 below inlines
    // the barrier fast path into the instrumented stores.
    eprintln!("=== Inserting write barriers ===");
    let full_wb_bc = dir.join("full_wb.bc");
    {
        let full_opt_ll = dir.join("full_opt.ll");
        run_cmd(
            "llvm-dis",
            &[
                full_opt_bc.to_str().unwrap(),
                "-o",
                full_opt_ll.to_str().unwrap(),
            ],
        );
        let ll = std::fs::read_to_string(&full_opt_ll).unwrap();
        let (patched, stats) = crate::write_barriers::insert_write_barriers(&ll);
        eprintln!(
            "write barriers: {} inserted ({} vector-conservative), {} stack stores skipped, {} functions",
            stats.barriers, stats.vector_barriers, stats.stack_skipped, stats.functions
        );
        let full_wb_ll = dir.join("full_wb.ll");
        std::fs::write(&full_wb_ll, patched).unwrap();
        run_cmd(
            "llvm-as",
            &[
                full_wb_ll.to_str().unwrap(),
                "-o",
                full_wb_bc.to_str().unwrap(),
            ],
        );
    }

    // Final link
    eprintln!("=== Final link ===");
    let bin_path = dir.join(name);
    {
        let mut link_args = vec!["-march=native", "-O3", "-g"];
        if ATTRIBUTOR_ENABLE_ALL {
            link_args.extend(["-mllvm", "-attributor-enable=all"]);
        }
        link_args.extend([
            full_wb_bc.to_str().unwrap(),
            runtime_lib.to_str().unwrap(),
            "-lm",
            "-lpthread",
            "-ldl",
            "-o",
            bin_path.to_str().unwrap(),
        ]);
        run_cmd("clang", &link_args);
    }

    eprintln!("=== Built: {} ===", bin_path.display());
    bin_path
}
