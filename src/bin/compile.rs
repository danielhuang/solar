use std::path::Path;

use solar::pipeline::CompileOptions;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    assert!(args.len() == 3, "usage: compile <file.solar> <output>");
    let input = &args[1];
    let output_bin = &args[2];

    let file_path = Path::new(input);

    let typed = match solar::pipeline::compile(file_path) {
        Ok(typed) => typed,
        Err((errors, source_map)) => {
            for err in &errors {
                solar::error::render_error_with_source_map(err, &source_map);
            }
            std::process::exit(1);
        }
    };

    typed
        .to_mangled()
        .to_ir()
        .optimized()
        .to_c(input)
        .to_binary(output_bin, CompileOptions::RELEASE);

    eprintln!("=== Output: {output_bin} ===");
}
