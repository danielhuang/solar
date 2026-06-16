//! `#line` directives in the generated C must attribute each piece of code to
//! the file it actually came from — in particular `@std` code should point at
//! the std source files, not at the main program's file.

use std::path::Path;

use solar::pipeline;

#[test]
fn std_code_gets_std_line_directives() {
    let dir = Path::new("target/test-fixtures");
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join("codegen_line.solar");
    // Uses @std's vec, which pulls in std code from src/std/*.solar.
    std::fs::write(
        &path,
        "fn main() {\n  let v = vec::Vec#[Int]([1, 2]&);\n  v&.push(3);\n  println(v&.at(0u)@);\n}\n",
    )
    .unwrap();

    let typed = pipeline::compile(&path).unwrap();
    let c = typed.to_ir().to_c(&path.display().to_string()).c_source;

    // Std code is attributed to the std files...
    assert!(
        c.contains("std/vec.solar\""),
        "expected a #line directive pointing at std/vec.solar"
    );
    assert!(
        c.contains("std/lib.solar\""),
        "expected a #line directive pointing at std/lib.solar"
    );
    // ...and the main program is still attributed to its own file.
    assert!(
        c.contains("codegen_line.solar\""),
        "expected a #line directive pointing at the main file"
    );
}
