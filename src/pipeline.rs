use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{CompileError, SourceMap};
use crate::{codegen, ir, ir_opt, mangled_ast, resolve, typed_ast};

const COMPILE_STACK_SIZE: usize = 64 << 20;

/// Resolves and type-checks a Solar program.
pub fn compile(file_path: &Path) -> Result<Typed, (Vec<CompileError>, SourceMap)> {
    let file_path = file_path.to_owned();
    std::thread::Builder::new()
        .name("solar-compile".to_string())
        .stack_size(COMPILE_STACK_SIZE)
        .spawn(move || compile_inner(&file_path))
        .expect("failed to spawn compiler thread")
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

fn compile_inner(file_path: &Path) -> Result<Typed, (Vec<CompileError>, SourceMap)> {
    let (resolved, source_map) = resolve::resolve(file_path)?;
    let typed = typed_ast::lower(&resolved).map_err(|e| (vec![e], source_map.clone()))?;
    Ok(Typed { typed, source_map })
}

/// A resolved and type-checked program.
pub struct Typed {
    /// Typed AST.
    pub typed: typed_ast::SourceFile,
    /// Sources loaded during compilation.
    pub source_map: SourceMap,
}

impl Typed {
    /// Assigns final symbols to definitions.
    pub fn to_mangled(self) -> Mangled {
        let mangled = mangled_ast::lower(&self.typed, &self.source_map);
        Mangled {
            mangled,
            source_map: self.source_map,
        }
    }
}

/// A program with final definition symbols.
pub struct Mangled {
    /// Mangled AST.
    pub mangled: mangled_ast::SourceFile,
    /// Sources loaded during compilation.
    pub source_map: SourceMap,
}

impl Mangled {
    /// Lowers the program to IR.
    pub fn to_ir(self) -> Ir {
        let ir = ir::lower(&self.mangled);
        Ir {
            ir,
            source_map: self.source_map,
        }
    }
}

/// A lowered IR module.
pub struct Ir {
    /// IR module.
    pub ir: ir::Module,
    /// Sources loaded during compilation.
    pub source_map: SourceMap,
}

impl Ir {
    /// Runs the IR optimization passes.
    pub fn optimized(mut self) -> Ir {
        ir_opt::optimize(&mut self.ir);
        self
    }

    /// Generates C source.
    pub fn to_c(&self, source_file: &str) -> CSource {
        let c_source = codegen::generate(&self.ir, source_file, &self.source_map);
        CSource {
            c_source,
            source_map: self.source_map.clone(),
        }
    }
}

/// Generated C source and its source map.
pub struct CSource {
    /// Generated translation unit.
    pub c_source: String,
    /// Sources loaded during compilation.
    pub source_map: SourceMap,
}

/// Options controlling native compilation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompileOptions {
    /// Enables collection and inserts the GC write-barrier pass.
    pub enable_gc: bool,
    /// Inserts GC-San access checks and disables reuse of swept arena slots.
    /// Requires [`Self::enable_gc`].
    pub gc_san: bool,
    /// Enables `-O3`, cross-language LTO, and allocation specialization.
    /// Requires [`Self::enable_gc`].
    pub optimize: bool,
}

impl CompileOptions {
    /// Unoptimized AddressSanitizer build with collection enabled.
    pub const DEBUG: Self = Self {
        enable_gc: true,
        gc_san: false,
        optimize: false,
    };
    /// Optimized production build with collection enabled.
    pub const RELEASE: Self = Self {
        enable_gc: true,
        gc_san: false,
        optimize: true,
    };
    /// Optimized build with collection and GC-San checks enabled.
    pub const GC_SAN: Self = Self {
        enable_gc: true,
        gc_san: true,
        optimize: true,
    };

    fn validate(self) {
        assert!(!self.gc_san || self.enable_gc, "gc_san requires enable_gc");
        assert!(
            !self.optimize || self.enable_gc,
            "optimize requires enable_gc"
        );
    }
}

impl CSource {
    /// Compiles the generated C to a native binary.
    ///
    /// # Panics
    ///
    /// Panics when GC-San or optimization is requested without GC.
    pub fn to_binary(self, name: &str, options: CompileOptions) -> Binary {
        options.validate();
        let unique: u64 = rand::random();
        let slug = format!("{name}_{unique:x}");
        let dir = Path::new("target/solar").join(&slug);
        std::fs::create_dir_all(&dir).unwrap();

        let c_path = dir.join(format!("{name}.c"));
        std::fs::write(&c_path, &self.c_source).unwrap();

        let bin_path = if options.optimize {
            compile_optimized(&c_path, &dir, name, options.gc_san)
        } else {
            compile_unoptimized(&c_path, &dir, name, options)
        };

        Binary { path: bin_path }
    }
}

/// A compiled native program.
pub struct Binary {
    /// Executable path.
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

/// Path to the GC write-barrier LLVM pass plugin (built by `build.rs`).
fn wb_plugin() -> &'static str {
    match option_env!("SOLAR_WB_PLUGIN") {
        Some(p) => p,
        None => panic!(
            "GC write-barrier pass plugin not built — install an llvm-dev package + clang++ and rebuild"
        ),
    }
}

/// Run a pass from the Solar plugin over `in_bc`, writing `out_bc`.
fn run_solar_pass(pass: &str, in_bc: &Path, out_bc: &Path) {
    let plugin_arg = format!("-load-pass-plugin={}", wb_plugin());
    let passes_arg = format!("-passes={pass}");
    run_cmd(
        "opt",
        &[
            &plugin_arg,
            &passes_arg,
            in_bc.to_str().unwrap(),
            "-o",
            out_bc.to_str().unwrap(),
        ],
    );
}

/// Run the `solar-write-barriers` pass. Debug locations and stack/global
/// provenance are handled structurally by the pass.
fn insert_write_barriers(in_bc: &Path, out_bc: &Path) {
    run_solar_pass("solar-write-barriers", in_bc, out_bc);
}

/// Instrument generated memory operations with arena-allocation checks.
fn insert_gc_san_checks(in_bc: &Path, out_bc: &Path) {
    run_solar_pass("solar-gc-sanitize", in_bc, out_bc);
}

/// Redirect constant-size allocations to fixed-class runtime entry points and
/// lower pointer-free Solar copies to optimizer-visible memmoves.
fn specialize_gc_alloc(in_bc: &Path, out_bc: &Path) {
    run_solar_pass("solar-specialize-gc-alloc", in_bc, out_bc);
}

fn compile_unoptimized(c_path: &Path, dir: &Path, name: &str, options: CompileOptions) -> PathBuf {
    let bin_path = dir.join(name);

    if options.enable_gc {
        let c_bc = dir.join(format!("{name}_c.bc"));
        let mut clang_args = vec![
            "-emit-llvm",
            "-c",
            "-O0",
            "-fno-omit-frame-pointer",
            "-fno-strict-aliasing",
            "-g",
            "-fexceptions",
        ];
        if options.gc_san {
            clang_args.push("-DSOLAR_GC_SAN");
        }
        clang_args.extend([c_path.to_str().unwrap(), "-o", c_bc.to_str().unwrap()]);
        run_cmd("clang", &clang_args);

        let wb_bc = dir.join("debug_wb.bc");
        insert_write_barriers(&c_bc, &wb_bc);
        let gc_san_bc = dir.join("debug_gc_san.bc");
        let final_bc = if options.gc_san {
            insert_gc_san_checks(&wb_bc, &gc_san_bc);
            &gc_san_bc
        } else {
            &wb_bc
        };

        run_cmd(
            "clang",
            &[
                "-O0",
                "-fsanitize=address",
                "-fno-omit-frame-pointer",
                "-fuse-ld=lld",
                final_bc.to_str().unwrap(),
                "target/debug/libsolar_system.a",
                "-lm",
                "-lpthread",
                "-ldl",
                "-o",
                bin_path.to_str().unwrap(),
            ],
        );
        return bin_path;
    }

    // Without write barriers, collection must remain disabled.
    let out = Command::new("clang")
        .args([
            "-O0",
            "-fsanitize=address",
            "-fno-omit-frame-pointer",
            // The generated C accesses the same memory through mixed-typed
            // casts (`uint8_t*` pointer members vs `uint64_t` scalar views),
            // so type-based alias analysis must be off.
            "-fno-strict-aliasing",
            "-g",
            // Emit unwind tables and don't mark functions `nounwind`, so a Solar
            // `throw` (a Rust panic from `sol_throw`) can unwind back through
            // these C frames to the nearest `sol_try` / `catch_unwind`.
            "-fexceptions",
            // No write-barrier pass, so force bump-allocator mode (codegen
            // guards the `sol_disable_gc()` call on this macro).
            "-DSOLAR_DEBUG_DISABLE_GC",
            // lld: some of the runtime archive's dependency-crate members are
            // LLVM bitcode (fat LTO); GNU ld can't read those, lld LTO-compiles
            // them at link time.
            "-fuse-ld=lld",
            c_path.to_str().unwrap(),
            "target/debug/libsolar_system.a",
            "-lm",
            "-lpthread",
            "-ldl",
            "-o",
            bin_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "debug compile/link failed for {name}:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    bin_path
}

// ---------------------------------------------------------------------------
// Optimized compilation: LLVM LTO with cross-language optimization
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

fn compile_optimized(c_path: &Path, dir: &Path, name: &str, gc_san: bool) -> PathBuf {
    let runtime_lib = Path::new("target/release/libsolar_system.a");
    assert!(
        runtime_lib.exists(),
        "libsolar_system.a not found at {}",
        runtime_lib.display()
    );

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

    // Only runtime-owned bitcode participates in cross-language optimization.
    eprintln!("=== Merging Rust bitcode (solar_system only) ===");
    let bc_files: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| {
            let path = e.unwrap().path();
            let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let is_own = file_name.starts_with("solar_system") || file_name == "atomic128.o";
            if is_own && path.extension().is_some_and(|e| e == "o") {
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
        // `-fexceptions`: emit unwind tables and keep functions unwindable (not
        // `nounwind`) so a Solar `throw` can unwind through these C frames to the
        // nearest `sol_try` (`catch_unwind`). Without it C frames abort the unwind.
        let mut clang_args = vec![
            "-flto=full",
            "-fexceptions",
            // The generated C accesses the same memory through mixed-typed
            // casts (`uint8_t*` pointer members vs `uint64_t` scalar views),
            // so type-based alias analysis must be off.
            "-fno-strict-aliasing",
            "-c",
            "-march=native",
            "-O3",
            "-g",
        ];
        if ATTRIBUTOR_ENABLE_ALL {
            clang_args.extend(["-mllvm", "-attributor-enable=all"]);
        }
        if gc_san {
            clang_args.push("-DSOLAR_GC_SAN");
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
                    "noinline allocsize(0) allockind(\"alloc,aligned\") personality ptr @rust_eh_personality",
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

    eprintln!("=== Specializing constant-size GC allocations ===");
    let full_specialized_bc = dir.join("full_specialized.bc");
    specialize_gc_alloc(&full_bc, &full_specialized_bc);

    // Optimize
    eprintln!("=== Optimizing (cross-language inlining) ===");
    let full_opt_bc = dir.join("full_opt.bc");
    {
        let mut opt_args = vec!["-O3"];
        if ATTRIBUTOR_ENABLE_ALL {
            opt_args.push("-attributor-enable=all");
        }
        opt_args.extend([
            full_specialized_bc.to_str().unwrap(),
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
    insert_write_barriers(&full_opt_bc, &full_wb_bc);

    let full_gc_san_bc = dir.join("full_gc_san.bc");
    let final_bc = if gc_san {
        eprintln!("=== Inserting GC sanitizer checks ===");
        insert_gc_san_checks(&full_wb_bc, &full_gc_san_bc);
        &full_gc_san_bc
    } else {
        &full_wb_bc
    };

    // Final link
    eprintln!("=== Final link ===");
    let bin_path = dir.join(name);
    {
        // Use lld: the runtime archive's dependency-crate members (backtrace,
        // gimli, …) are LLVM bitcode (fat LTO) and are now pulled here rather
        // than pre-merged. GNU ld can't read bitcode archive members; lld
        // LTO-compiles them at link time.
        let mut link_args = vec!["-fuse-ld=lld", "-march=native", "-O3", "-g"];
        if ATTRIBUTOR_ENABLE_ALL {
            link_args.extend(["-mllvm", "-attributor-enable=all"]);
        }
        link_args.extend([
            final_bc.to_str().unwrap(),
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

#[cfg(test)]
mod tests {
    use super::CompileOptions;

    #[test]
    fn compile_options_require_gc_for_gc_features() {
        let gc_san_without_gc = CompileOptions {
            enable_gc: false,
            gc_san: true,
            optimize: false,
        };
        assert!(std::panic::catch_unwind(|| gc_san_without_gc.validate()).is_err());

        let optimize_without_gc = CompileOptions {
            enable_gc: false,
            gc_san: false,
            optimize: true,
        };
        assert!(std::panic::catch_unwind(|| optimize_without_gc.validate()).is_err());
    }
}
