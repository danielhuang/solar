use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use clap::{Args, Parser, Subcommand, ValueEnum};
use solar::fmt::format_source;
use solar::pipeline::{CompileOptions, Typed};

#[derive(Parser)]
#[command(version, about = "Compile, run, check, and format Solar programs")]
struct Cli {
    #[command(subcommand)]
    command: SolarCommand,
}

#[derive(Subcommand)]
enum SolarCommand {
    /// Compile a program to a native executable.
    Compile {
        #[arg(value_name = "SRC.solar")]
        source: PathBuf,
        #[arg(value_name = "DEST")]
        destination: PathBuf,
        #[command(flatten)]
        build: BuildOptions,
    },
    /// Format one or more source files in place.
    Fmt {
        #[arg(required = true, num_args = 1.., value_name = "FILES")]
        files: Vec<PathBuf>,
    },
    /// Resolve and type-check a program without running it.
    Check {
        #[arg(value_name = "SRC.solar")]
        source: PathBuf,
    },
    /// Compile and run a temporary executable, or use an interpreter.
    Run {
        #[arg(value_name = "SRC.solar")]
        source: PathBuf,
        #[command(flatten)]
        build: BuildOptions,
        /// Run with the AST or IR interpreter instead of native code.
        #[arg(long, value_enum, conflicts_with_all = ["release", "gc_san"])]
        interp: Option<Interpreter>,
    },
}

#[derive(Args)]
struct BuildOptions {
    /// Enable optimized release code generation (default: debug with ASAN).
    #[arg(long)]
    release: bool,
    /// Enable GC-San access checks.
    #[arg(long)]
    gc_san: bool,
}

impl BuildOptions {
    fn compile_options(&self) -> CompileOptions {
        CompileOptions {
            gc_san: self.gc_san,
            ..if self.release {
                CompileOptions::RELEASE
            } else {
                CompileOptions::DEBUG
            }
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum Interpreter {
    Ast,
    Ir,
}

fn main() -> ExitCode {
    match Cli::parse().command {
        SolarCommand::Fmt { files } => format_files(&files),
        SolarCommand::Check { source } => match typecheck(&source) {
            Ok(_) => ExitCode::SUCCESS,
            Err(status) => status,
        },
        SolarCommand::Compile {
            source,
            destination,
            build,
        } => {
            let typed = match typecheck(&source) {
                Ok(typed) => typed,
                Err(status) => return status,
            };
            compile_native(typed, &source, &destination, build.compile_options());
            ExitCode::SUCCESS
        }
        SolarCommand::Run {
            source,
            build,
            interp,
        } => {
            let typed = match typecheck(&source) {
                Ok(typed) => typed,
                Err(status) => return status,
            };
            match interp {
                Some(Interpreter::Ast) => solar::ast_interp::interpret(&typed.to_mangled().mangled),
                Some(Interpreter::Ir) => {
                    solar::ir_interp::interpret(&typed.to_mangled().to_ir().ir)
                }
                None => {
                    let temporary = tempdir::TempDir::new("solar-run").unwrap();
                    let destination = temporary.path().join("program");
                    compile_native(typed, &source, &destination, build.compile_options());
                    let status = Command::new(&destination)
                        .env("ASAN_OPTIONS", "detect_leaks=0")
                        .status()
                        .unwrap();
                    temporary.close().unwrap();
                    return ExitCode::from(status.code().unwrap_or(1) as u8);
                }
            }
            ExitCode::SUCCESS
        }
    }
}

fn typecheck(source: &Path) -> Result<Typed, ExitCode> {
    solar::pipeline::compile(source).map_err(|(errors, source_map)| {
        for error in &errors {
            solar::error::render_error_with_source_map(error, &source_map);
        }
        ExitCode::FAILURE
    })
}

fn compile_native(typed: Typed, source: &Path, destination: &Path, options: CompileOptions) {
    let ir = typed.to_mangled().to_ir();
    let ir = if options.optimize { ir.optimized() } else { ir };
    ir.to_c(&source.to_string_lossy())
        .to_binary(destination, options);
}

fn format_files(paths: &[PathBuf]) -> ExitCode {
    let mut formatted_files = Vec::new();
    let mut failed = false;
    for path in paths {
        let source = match std::fs::read_to_string(path) {
            Ok(source) => source,
            Err(error) => {
                eprintln!("{}: {error}", path.display());
                failed = true;
                continue;
            }
        };
        match format_source(&source) {
            Ok(formatted) => formatted_files.push((path, source, formatted)),
            Err(error) => {
                eprintln!("{}: {error}", path.display());
                failed = true;
            }
        }
    }
    if failed {
        return ExitCode::FAILURE;
    }

    for (path, source, formatted) in formatted_files {
        if source != formatted {
            std::fs::write(path, formatted).unwrap();
        }
    }
    ExitCode::SUCCESS
}
