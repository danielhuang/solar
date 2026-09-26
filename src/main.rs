use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use clap::{Args, Parser, Subcommand, ValueEnum};
use solar::fmt::format_source;
use solar::pipeline::{CompileOptions, Typed};

#[derive(Parser)]
#[command(
    version,
    about = "Compile, run, check, format, and dump Solar programs"
)]
struct Cli {
    #[command(subcommand)]
    command: SolarCommand,
}

#[derive(Subcommand)]
enum SolarCommand {
    /// Dump a pipeline stage to standard output without executing the program.
    Dump {
        #[arg(value_name = "SRC.solar")]
        source: PathBuf,
        #[arg(long, value_enum)]
        stage: DumpStage,
        /// Optimize tree IR before dumping tree IR or C; earlier stages are unaffected.
        #[arg(long)]
        release: bool,
    },
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
        /// Run with the AST or tree IR interpreter instead of native code.
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
    #[value(name = "tree_ir")]
    TreeIr,
}

#[derive(Clone, Copy, ValueEnum)]
#[value(rename_all = "snake_case")]
enum DumpStage {
    /// Parsed source file, before resolving imports or type-checking.
    Ast,
    /// Type-checked and monomorphized program.
    TypedAst,
    /// Lowered tree intermediate representation.
    TreeIr,
    /// Generated C source.
    C,
}

fn main() -> ExitCode {
    match Cli::parse().command {
        SolarCommand::Dump {
            source,
            stage,
            release,
        } => dump(&source, stage, release),
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
                Some(Interpreter::TreeIr) => {
                    solar::tree_ir_interp::interpret(&typed.to_mangled().to_tree_ir().tree_ir)
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

fn dump(source: &Path, stage: DumpStage, release: bool) -> ExitCode {
    if matches!(stage, DumpStage::Ast) {
        let text = match std::fs::read_to_string(source) {
            Ok(text) => text,
            Err(error) => {
                eprintln!("{}: {error}", source.display());
                return ExitCode::FAILURE;
            }
        };
        return match solar::parser::parse(&text) {
            Ok(ast) => {
                println!("{ast:#?}");
                ExitCode::SUCCESS
            }
            Err(errors) => {
                for error in errors {
                    eprintln!("{}:{error}", source.display());
                }
                ExitCode::FAILURE
            }
        };
    }

    let typed = match typecheck(source) {
        Ok(typed) => typed,
        Err(status) => return status,
    };
    if matches!(stage, DumpStage::TypedAst) {
        println!("{:#?}", typed.typed);
        return ExitCode::SUCCESS;
    }

    let tree_ir = typed.to_mangled().to_tree_ir();
    let tree_ir = if release {
        tree_ir.optimized()
    } else {
        tree_ir
    };
    match stage {
        DumpStage::TreeIr => println!("{:#?}", tree_ir.tree_ir),
        DumpStage::C => print!("{}", tree_ir.to_c(&source.to_string_lossy()).c_source),
        DumpStage::Ast | DumpStage::TypedAst => unreachable!(),
    }
    ExitCode::SUCCESS
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
    let tree_ir = typed.to_mangled().to_tree_ir();
    let tree_ir = if options.optimize {
        tree_ir.optimized()
    } else {
        tree_ir
    };
    tree_ir
        .to_c(&source.to_string_lossy())
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
