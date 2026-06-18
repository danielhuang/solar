use std::io::Write;

/// Demangle a Solar function name from its C symbol.
/// Returns `None` if it's not a Solar function.
///
/// This is the exact inverse of the mangling in `src/typed_ast.rs` (see the
/// grammar note there). It is a defensive recursive-descent parser: it runs
/// inside the panic path, so it never panics — on any malformed input it falls
/// back to the raw (post-`solar_`) symbol.
fn demangle_solar(symbol: &str) -> Option<String> {
    let name = symbol.strip_prefix("solar_")?;

    if name == "main" {
        return Some("main".into());
    }
    // __closure_N → <closure N>
    if let Some(n) = name.strip_prefix("__closure_") {
        return Some(format!("<closure {n}>"));
    }
    // __method_<name> → Receiver.method(args). The first type arg is the receiver.
    if let Some(rest) = name.strip_prefix("__method_") {
        return Some(demangle_method(rest).unwrap_or_else(|| rest.to_string()));
    }
    // Otherwise a free function: name(args).
    Some(demangle_function(name).unwrap_or_else(|| name.to_string()))
}

/// A free function symbol `name(args)` (the args are the param/generic types used
/// to key the symbol).
fn demangle_function(s: &str) -> Option<String> {
    let mut p = Parser::new(s);
    let (base, args) = p.parse_name()?;
    Some(render_call(&pretty_base(&base), &args))
}

/// A method symbol: the first type arg is the receiver, rendered `Recv.method(..)`.
fn demangle_method(s: &str) -> Option<String> {
    let mut p = Parser::new(s);
    let (base, mut args) = p.parse_name()?;
    if args.is_empty() {
        return Some(base_item(&base).to_string());
    }
    let recv = args.remove(0);
    Some(format!("{recv}.{}", render_call(base_item(&base), &args)))
}

fn render_call(name: &str, args: &[String]) -> String {
    format!("{name}({})", args.join(", "))
}

/// Pretty-print an entity base: `__mod<len>_<mod><item>` → `mod::item`.
fn pretty_base(base: &str) -> String {
    if let Some(rest) = base.strip_prefix("__mod") {
        let mut p = Parser::new(rest);
        if let Some(module) = p.parse_enc_id() {
            return format!("{module}::{}", p.rest());
        }
    }
    base.to_string()
}

/// The bare item name of a (possibly module-qualified) base: `__mod3_libfoo` → `foo`.
fn base_item(base: &str) -> &str {
    if let Some(rest) = base.strip_prefix("__mod") {
        let mut p = Parser::new(rest);
        if p.parse_enc_id().is_some() {
            return &rest[p.pos..];
        }
    }
    base
}

struct Parser<'a> {
    s: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(s: &'a str) -> Self {
        Parser {
            s: s.as_bytes(),
            pos: 0,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.pos).copied()
    }

    fn rest(&self) -> &'a str {
        std::str::from_utf8(&self.s[self.pos..]).unwrap_or("")
    }

    fn eat(&mut self, b: u8) -> bool {
        if self.peek() == Some(b) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    /// Read a decimal run (no leading-zero handling needed; lengths/counts are
    /// written plainly).
    fn num(&mut self) -> Option<usize> {
        let start = self.pos;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
        if self.pos == start {
            return None;
        }
        std::str::from_utf8(&self.s[start..self.pos])
            .ok()?
            .parse()
            .ok()
    }

    /// `enc_id` = `<len> '_' <len bytes>`.
    fn parse_enc_id(&mut self) -> Option<String> {
        let len = self.num()?;
        if !self.eat(b'_') {
            return None;
        }
        let end = self.pos.checked_add(len)?;
        if end > self.s.len() {
            return None;
        }
        let out = String::from_utf8_lossy(&self.s[self.pos..end]).into_owned();
        self.pos = end;
        Some(out)
    }

    /// A type fragment → readable Solar type.
    fn parse_type(&mut self) -> Option<String> {
        match self.peek()? {
            b'0'..=b'9' => {
                // Leaf: a length-prefixed (possibly itself mangled) entity name.
                let inner = self.parse_enc_id()?;
                Some(pretty_name(&inner))
            }
            b'R' => {
                self.pos += 1;
                Some(format!("&{}", self.parse_type()?))
            }
            b'Q' => {
                self.pos += 1;
                Some(format!("&?{}", self.parse_type()?))
            }
            b'U' => {
                self.pos += 1;
                Some(format!("^{}", self.parse_type()?))
            }
            b'S' => {
                self.pos += 1;
                Some(format!("[{}]", self.parse_type()?))
            }
            b'A' => {
                self.pos += 1;
                let n = self.num()?;
                self.eat(b'_');
                let inner = self.parse_type()?;
                Some(format!("[{inner}; {n}]"))
            }
            b'F' => {
                self.pos += 1;
                let n = self.num()?;
                self.eat(b'_');
                let mut params = Vec::with_capacity(n);
                for _ in 0..n {
                    params.push(self.parse_type()?);
                }
                let ret = self.parse_type()?;
                let p = params.join(", ");
                if ret == "Unit" || ret == "()" {
                    Some(format!("fn({p})"))
                } else {
                    Some(format!("fn({p}) -> {ret}"))
                }
            }
            _ => None,
        }
    }

    /// An entity name: `base` or `enc_id(base) 'G' <n> '_' type{n}`.
    fn parse_name(&mut self) -> Option<(String, Vec<String>)> {
        // `0T<n>_...` tuples don't appear as top-level symbols, but parse_type
        // handles them via pretty_name; here a leading digit means `enc_id(base)`.
        if matches!(self.peek(), Some(b'0'..=b'9')) {
            let base = self.parse_enc_id()?;
            let mut args = Vec::new();
            if self.eat(b'G') {
                let n = self.num()?;
                self.eat(b'_');
                for _ in 0..n {
                    args.push(self.parse_type()?);
                }
            }
            Some((base, args))
        } else {
            // Bare base (no type args), e.g. a zero-parameter function.
            Some((self.rest().to_string(), Vec::new()))
        }
    }
}

/// Render an entity-identity string (a `name` or `0T..` tuple) as a readable type,
/// e.g. `3_BoxG1_3_Int` → `Box#[Int]`, `0T2_3_Int4_Bool` → `(Int, Bool)`,
/// `__mod3_libPoint` → `lib::Point`, `Int` → `Int`.
fn pretty_name(inner: &str) -> String {
    if let Some(rest) = inner.strip_prefix("0T") {
        // Tuple.
        let mut p = Parser::new(rest);
        if let Some(n) = p.num() {
            p.eat(b'_');
            let mut elems = Vec::with_capacity(n);
            let mut ok = true;
            for _ in 0..n {
                match p.parse_type() {
                    Some(t) => elems.push(t),
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                return format!("({})", elems.join(", "));
            }
        }
        return inner.to_string();
    }
    let mut p = Parser::new(inner);
    if let Some((base, args)) = p.parse_name() {
        let pretty = pretty_base(&base);
        if args.is_empty() {
            return pretty;
        }
        return format!("{pretty}#[{}]", args.join(", "));
    }
    inner.to_string()
}

/// Print an error message and a demangled Solar stack trace, then abort.
///
/// Frames are resolved with the `backtrace` crate (the symbolizer std and samply
/// use), which reads the executable's `.symtab` + DWARF. That's what lets it name
/// the generated `solar_*` functions even though codegen emits them `static`
/// (local) — `dladdr`, which only consults the dynamic symbol table, can't. It
/// also recovers inlined frames, so optimized builds still show the call chain.
pub fn sol_panic_internal(msg: &str) -> ! {
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "{msg}");
    let _ = writeln!(err, "\nStack trace:");

    let mut frame_num = 0;
    backtrace::trace(|frame| {
        backtrace::resolve_frame(frame, |sym| {
            // A frame can resolve to several symbols (inlined call chain); show each.
            if let Some(name) = sym.name().and_then(|n| n.as_str())
                && let Some(display_name) = demangle_solar(name)
            {
                let _ = writeln!(err, "  {frame_num}: {display_name}");
                frame_num += 1;
            }
        });
        true
    });

    if frame_num == 0 {
        let _ = writeln!(err, "  (no Solar frames found)");
    }

    let _ = err.flush();
    std::process::abort();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sol_panic(msg: *const u8, len: usize) -> ! {
    let slice = unsafe { std::slice::from_raw_parts(msg, len) };
    let text = std::str::from_utf8(slice).unwrap_or("<invalid utf8>");
    sol_panic_internal(text)
}

/// Install a panic hook that uses `sol_panic_internal` for nice Solar stack traces.
/// Called from `sol_start`.
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let msg = if let Some(s) = info.payload().downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "panic".into()
        };
        sol_panic_internal(&msg);
    }));
}

#[cfg(test)]
mod tests {
    use super::demangle_solar;

    #[track_caller]
    fn check(sym: &str, expect: &str) {
        assert_eq!(
            demangle_solar(sym).as_deref(),
            Some(expect),
            "symbol: {sym}"
        );
    }

    #[test]
    fn demangles_real_symbols() {
        check("solar_main", "main");
        check("solar___closure_0", "<closure 0>");
        check("solar_3_addG1_3_Int", "add(Int)");
        check("solar___mod4_filestdout", "file::stdout()");
        check(
            "solar_22___mod3_libwrite_stdoutG1_RS5_Uint8",
            "lib::write_stdout(&[Uint8])",
        );
        check(
            "solar_8_while_fnG2_F0_4_BoolF0_4_Unit",
            "while_fn(fn() -> Bool, fn())",
        );
        check("solar___method_9_to_stringG1_3_Int", "Int.to_string()");
        check(
            "solar___method_5_writeG2_8_FileDescRS5_Uint8",
            "FileDesc.write(&[Uint8])",
        );
    }

    #[test]
    fn renders_composite_and_generic_types() {
        // generic struct leaf: Box#[Int]
        check("solar_3_fooG1_13_3_BoxG1_3_Int", "foo(Box#[Int])");
        // tuple leaf: (Int, Bool)
        check("solar_3_barG1_15_0T2_3_Int4_Bool", "bar((Int, Bool))");
        // nullable ref, unique, fixed array
        check(
            "solar_1_fG3_Q3_IntU4_BoolA3_5_Uint8",
            "f(&?Int, ^Bool, [Uint8; 3])",
        );
    }

    #[test]
    fn non_solar_symbols_are_ignored() {
        assert_eq!(demangle_solar("_RNvCs_foo"), None);
        assert_eq!(demangle_solar("malloc"), None);
    }
}
