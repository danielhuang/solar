use std::path::PathBuf;
use std::process::ExitCode;

use solar::fmt::format_source;

fn main() -> ExitCode {
    let paths = std::env::args_os()
        .skip(1)
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    if paths.is_empty() {
        eprintln!("usage: fmt <file.solar>...");
        return ExitCode::from(2);
    }

    let mut formatted_files = Vec::new();
    let mut failed = false;
    for path in &paths {
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
