use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let emit_c = args.iter().any(|a| a == "--emit-c");
    let file = if emit_c {
        assert_eq!(args.len(), 3);
        &args[2]
    } else {
        assert_eq!(args.len(), 2);
        &args[1]
    };

    let file_path = Path::new(file);

    let typed = match solar::pipeline::compile(file_path) {
        Ok(typed) => typed,
        Err(errors) => {
            let source = std::fs::read_to_string(file_path).unwrap();
            for err in &errors {
                solar::error::render_error(err, &source, file);
            }
            std::process::exit(1);
        }
    };

    let ir = typed.to_ir();

    if emit_c {
        print!("{}", solar::codegen::generate(&ir.ir, file, &ir.source_map));
    } else {
        solar::ir_interp::interpret(&ir.ir);
    }
}
