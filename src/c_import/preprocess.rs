//! The header preprocessor subset: includes, conditionals, and object/function
//! macros. No compiler subprocess or generated helper header is involved.

use super::lex::{Cursor, Token, ident, lex};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone)]
pub(super) struct Macro {
    pub parameters: Option<Vec<String>>,
    pub body: Vec<Token>,
    variadic: bool,
}

pub(super) struct Preprocessor {
    pub macros: HashMap<String, Macro>,
    pub dependencies: HashSet<PathBuf>,
    paths: Vec<PathBuf>,
    once: HashSet<PathBuf>,
}

impl Preprocessor {
    pub fn new() -> Result<Self, String> {
        // Solar's native runtime and syscall ABI currently target Linux x86-64.
        // These are compiler predefined macros describing that ABI, not copies
        // of declarations from the imported headers.
        if !cfg!(all(
            target_os = "linux",
            target_arch = "x86_64",
            target_pointer_width = "64"
        )) {
            return Err("C imports currently require the Linux x86-64 LP64 ABI".into());
        }
        let mut this = Self {
            macros: HashMap::new(),
            dependencies: HashSet::new(),
            paths: Vec::new(),
            once: HashSet::new(),
        };
        for variable in ["CPATH", "C_INCLUDE_PATH"] {
            if let Some(value) = std::env::var_os(variable) {
                this.paths.extend(std::env::split_paths(&value).map(|path| {
                    if path.as_os_str().is_empty() {
                        PathBuf::from(".")
                    } else {
                        path
                    }
                }));
            }
        }
        this.paths.push("/usr/local/include".into());
        // Compiler-distributed headers (notably stddef.h and limits.h) are
        // ordinary input files. Prefer GCC's portable headers when installed.
        let mut resources = versioned_directories(Path::new("/usr/lib/gcc/x86_64-linux-gnu"));
        resources.extend(versioned_directories(Path::new("/usr/lib/clang")));
        if let Some(path) = resources
            .into_iter()
            .map(|p| p.join("include"))
            .find(|p| p.join("stddef.h").is_file())
        {
            this.paths.push(path);
        }
        this.paths.extend([
            PathBuf::from("/usr/include/x86_64-linux-gnu"),
            PathBuf::from("/usr/include"),
        ]);
        for (name, body) in [
            ("__linux__", "1"),
            ("__linux", "1"),
            ("linux", "1"),
            ("__unix__", "1"),
            ("__unix", "1"),
            ("unix", "1"),
            ("__x86_64__", "1"),
            ("__x86_64", "1"),
            ("__amd64__", "1"),
            ("__LP64__", "1"),
            ("_LP64", "1"),
            ("__ELF__", "1"),
            ("__STDC__", "1"),
            ("__STDC_HOSTED__", "1"),
            ("__STDC_VERSION__", "201112L"),
            ("__GNUC__", "4"),
            ("__GNUC_MINOR__", "2"),
            ("__GNUC_PATCHLEVEL__", "1"),
            ("__CHAR_BIT__", "8"),
            ("__SCHAR_MAX__", "127"),
            ("__SHRT_MAX__", "32767"),
            ("__INT_MAX__", "2147483647"),
            ("__LONG_MAX__", "9223372036854775807L"),
            ("__LONG_LONG_MAX__", "9223372036854775807LL"),
            ("__SIZE_TYPE__", "long unsigned int"),
            ("__PTRDIFF_TYPE__", "long int"),
            ("__WCHAR_TYPE__", "int"),
            ("__WINT_TYPE__", "unsigned int"),
            ("__WCHAR_MAX__", "2147483647"),
            ("__WCHAR_MIN__", "(-2147483647 - 1)"),
            ("__SIZEOF_POINTER__", "8"),
            ("__SIZEOF_LONG__", "8"),
            ("__SIZEOF_INT__", "4"),
            ("__ORDER_LITTLE_ENDIAN__", "1234"),
            ("__ORDER_BIG_ENDIAN__", "4321"),
            ("__BYTE_ORDER__", "__ORDER_LITTLE_ENDIAN__"),
        ] {
            this.macros.insert(
                name.into(),
                Macro {
                    parameters: None,
                    body: lex(body, "<C target>".into())?,
                    variadic: false,
                },
            );
        }
        // Unsupported compiler extensions must select the headers' fallback.
        for name in [
            "__has_attribute",
            "__has_builtin",
            "__has_feature",
            "__has_extension",
        ] {
            this.macros.insert(
                name.into(),
                Macro {
                    parameters: Some(vec!["x".into()]),
                    body: lex("0", "<C target>".into())?,
                    variadic: false,
                },
            );
        }
        Ok(this)
    }

    pub fn header(&mut self, name: &str) -> Result<Vec<Token>, String> {
        if name.contains(['\n', '\r', '"', '<', '>']) {
            return Err("invalid C header path".into());
        }
        let (path, index) = if name.starts_with("./") {
            (
                std::env::current_dir()
                    .map_err(|e| e.to_string())?
                    .join(name),
                None,
            )
        } else {
            self.find(name, None, 0)?
        };
        self.read(&path, index, 0)
    }

    fn find(
        &self,
        name: &str,
        relative: Option<&Path>,
        start: usize,
    ) -> Result<(PathBuf, Option<usize>), String> {
        if let Some(parent) = relative {
            let path = parent.join(name);
            if path.is_file() {
                return Ok((path, None));
            }
        }
        self.paths
            .iter()
            .enumerate()
            .skip(start)
            .find_map(|(i, dir)| {
                let p = dir.join(name);
                p.is_file().then_some((p, Some(i)))
            })
            .ok_or_else(|| format!("C header `{name}` was not found in the system include paths"))
    }

    fn condition(&self, tokens: &[Token]) -> Result<bool, String> {
        let mut cursor = Cursor::new(tokens);
        let mut replaced = Vec::new();
        while cursor.pos < tokens.len() {
            let token = cursor.take()?;
            if token.text == "defined" {
                let parens = cursor.eat("(");
                let name = &cursor.take()?.text;
                if !ident(name) {
                    return Err("expected identifier after defined".into());
                }
                replaced.push(token.with_text(if self.macros.contains_key(name) {
                    "1"
                } else {
                    "0"
                }));
                if parens {
                    cursor.expect(")")?;
                }
            } else {
                replaced.push(token.clone());
            }
        }
        let expanded = self.expand(&replaced)?;
        super::expr::condition(&expanded)
    }

    fn read(
        &mut self,
        path: &Path,
        search_index: Option<usize>,
        depth: usize,
    ) -> Result<Vec<Token>, String> {
        if depth > 100 {
            return Err("C include nesting exceeds 100 headers".into());
        }
        let path = path
            .canonicalize()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if self.once.contains(&path) {
            return Ok(Vec::new());
        }
        self.dependencies.insert(path.clone());
        let source =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let source = source.replace("\\\r\n", "").replace("\\\n", "");
        let tokens = lex(&source, Arc::from(path.to_string_lossy().as_ref()))?;
        let mut output = Vec::new();
        let mut pending = Vec::new();
        // (parent enabled, any arm taken, current arm enabled, saw else)
        let mut conditions: Vec<(bool, bool, bool, bool)> = Vec::new();
        for line in tokens.split(|t| t.text == "\n") {
            let active = conditions.last().is_none_or(|c| c.2);
            if line.first().is_none_or(|t| t.text != "#") {
                if active {
                    pending.extend_from_slice(line);
                }
                continue;
            }
            output.extend(self.expand(&pending)?);
            pending.clear();
            let directive = line.get(1).map_or("", |t| t.text.as_str());
            let args = line.get(2..).unwrap_or_default();
            let result: Result<(), String> = (|| {
                match directive {
                    "if" | "ifdef" | "ifndef" => {
                        let enabled = active
                            && match directive {
                                "ifdef" => self
                                    .macros
                                    .contains_key(&args.first().ok_or("missing #ifdef name")?.text),
                                "ifndef" => !self.macros.contains_key(
                                    &args.first().ok_or("missing #ifndef name")?.text,
                                ),
                                _ => self.condition(args)?,
                            };
                        conditions.push((active, enabled, enabled, false));
                    }
                    "elif" => {
                        let c = conditions.last_mut().ok_or("#elif without #if")?;
                        if c.3 {
                            return Err("#elif after #else".into());
                        }
                        c.2 = c.0 && !c.1 && self.condition(args)?;
                        c.1 |= c.2;
                    }
                    "else" => {
                        let c = conditions.last_mut().ok_or("#else without #if")?;
                        if c.3 {
                            return Err("duplicate #else".into());
                        }
                        c.2 = c.0 && !c.1;
                        c.1 = true;
                        c.3 = true;
                    }
                    "endif" => {
                        conditions.pop().ok_or("#endif without #if")?;
                    }
                    _ if !active => {}
                    "define" => self.define(args)?,
                    "undef" => {
                        self.macros
                            .remove(&args.first().ok_or("missing #undef name")?.text);
                    }
                    "include" | "include_next" => {
                        let expanded = if args
                            .first()
                            .is_some_and(|t| t.text == "<" || t.text.starts_with('"'))
                        {
                            args.to_vec()
                        } else {
                            self.expand(args)?
                        };
                        let mut cursor = Cursor::new(&expanded);
                        let quoted = cursor.peek().starts_with('"');
                        let name = if quoted {
                            cursor.take()?.text.trim_matches('"').to_owned()
                        } else {
                            cursor.expect("<")?;
                            let name = cursor
                                .until(&[">"])?
                                .iter()
                                .map(|t| t.text.as_str())
                                .collect::<String>();
                            cursor.expect(">")?;
                            name
                        };
                        if cursor.pos != expanded.len() {
                            return Err("trailing tokens in #include".into());
                        }
                        let next = directive == "include_next";
                        let start = if next {
                            search_index.ok_or("#include_next outside system header")? + 1
                        } else {
                            0
                        };
                        let relative = if quoted && !next { path.parent() } else { None };
                        let (child, index) = self.find(&name, relative, start)?;
                        output.extend(self.read(&child, index.or(search_index), depth + 1)?);
                    }
                    "pragma" if args.first().is_some_and(|t| t.text == "once") => {
                        self.once.insert(path.clone());
                    }
                    // Layout-affecting pragmas cannot be ignored. Record them
                    // in the token stream so only requested types are rejected.
                    "pragma" if args.first().is_some_and(|t| t.text == "pack") => {
                        output.push(line[0].with_text("__solar_unsupported_pack"));
                        output.push(line[0].with_text(";"));
                    }
                    "pragma" if args.first().is_some_and(|t| t.text == "GCC") => {
                        if args.get(1).is_none_or(|t| {
                            !matches!(t.text.as_str(), "system_header" | "diagnostic")
                        }) {
                            return Err("unsupported C pragma".into());
                        }
                    }
                    "error" => {
                        return Err(args
                            .iter()
                            .map(|t| t.text.as_str())
                            .collect::<Vec<_>>()
                            .join(" "));
                    }
                    "" | "line" | "warning" => {}
                    _ => {
                        return Err(format!(
                            "unsupported C preprocessing directive #{directive}"
                        ));
                    }
                }
                Ok(())
            })();
            result.map_err(|e| format!("{}: {e}", line[0].location()))?;
        }
        if !conditions.is_empty() {
            return Err(format!("{}: unterminated #if", path.display()));
        }
        output.extend(self.expand(&pending)?);
        Ok(output)
    }

    fn define(&mut self, tokens: &[Token]) -> Result<(), String> {
        let mut cursor = Cursor::new(tokens);
        let name = cursor.take()?;
        if !ident(&name.text) {
            return Err("invalid C macro name".into());
        }
        let function = cursor.peek() == "(" && tokens[1].offset == name.offset + name.text.len();
        let mut parameters = None;
        let mut variadic = false;
        if function {
            cursor.expect("(")?;
            let mut names = Vec::new();
            if !cursor.eat(")") {
                loop {
                    let token = cursor.take()?;
                    if token.text == "..." {
                        names.push("__VA_ARGS__".into());
                        variadic = true;
                    } else if ident(&token.text) {
                        names.push(token.text.clone());
                        if cursor.eat("...") {
                            variadic = true;
                        }
                    } else {
                        return Err("unsupported macro parameter".into());
                    }
                    if cursor.eat(")") {
                        break;
                    }
                    if variadic {
                        return Err("variadic parameter must be last".into());
                    }
                    cursor.expect(",")?;
                }
            }
            parameters = Some(names);
        }
        self.macros.insert(
            name.text.clone(),
            Macro {
                parameters,
                body: tokens[cursor.pos..].to_vec(),
                variadic,
            },
        );
        Ok(())
    }

    pub fn expand(&self, tokens: &[Token]) -> Result<Vec<Token>, String> {
        self.expand_inner(tokens, 0)
    }

    fn expand_inner(&self, tokens: &[Token], depth: usize) -> Result<Vec<Token>, String> {
        if depth > 100 {
            return Err("C macro argument nesting exceeds 100".into());
        }
        let mut input: VecDeque<Token> = tokens.iter().cloned().collect();
        let mut output = Vec::new();
        let mut expansions = 0;
        while let Some(token) = input.pop_front() {
            let Some(mac) = self
                .macros
                .get(&token.text)
                .filter(|_| !token.hidden.contains(&token.text))
            else {
                output.push(token);
                continue;
            };
            let mut arguments: HashMap<&str, Vec<Token>> = HashMap::new();
            if let Some(parameters) = &mac.parameters {
                if input.front().is_none_or(|t| t.text != "(") {
                    output.push(token);
                    continue;
                }
                input.pop_front();
                let mut nesting = 0;
                let mut values = vec![Vec::new()];
                loop {
                    let t = input.pop_front().ok_or("unterminated C macro invocation")?;
                    match t.text.as_str() {
                        ")" if nesting == 0 => break,
                        "(" => nesting += 1,
                        ")" => nesting -= 1,
                        "," if nesting == 0
                            && !(mac.variadic && values.len() == parameters.len()) =>
                        {
                            values.push(Vec::new());
                            continue;
                        }
                        _ => {}
                    }
                    values.last_mut().unwrap().push(t);
                }
                if parameters.is_empty() && values.len() == 1 && values[0].is_empty() {
                    values.clear();
                }
                if mac.variadic && values.len() + 1 == parameters.len() {
                    values.push(Vec::new());
                }
                if values.len() != parameters.len() {
                    return Err(format!("wrong argument count for C macro `{}`", token.text));
                }
                arguments.extend(parameters.iter().map(String::as_str).zip(values));
            }
            expansions += 1;
            if expansions > 100_000 {
                return Err("C macro expansion limit exceeded".into());
            }
            let mut replacement: Vec<Token> = Vec::new();
            let mut i = 0;
            while i < mac.body.len() {
                let t = &mac.body[i];
                if t.text == "#" {
                    i += 1;
                    let parameter = mac.body.get(i).ok_or("missing stringification parameter")?;
                    let argument = arguments
                        .get(parameter.text.as_str())
                        .ok_or("invalid stringification parameter")?;
                    let mut text = String::new();
                    for (index, t) in argument.iter().enumerate() {
                        if index > 0 && t.spaced {
                            text.push(' ');
                        }
                        text.push_str(&t.text);
                    }
                    replacement.push(token.with_text(format!(
                        "\"{}\"",
                        text.replace('\\', "\\\\").replace('"', "\\\"")
                    )));
                } else if t.text == "##" {
                    replacement.push(t.clone());
                } else if let Some(value) = arguments.get(t.text.as_str()) {
                    let start = replacement.len();
                    if i > 0 && mac.body[i - 1].text == "##"
                        || mac.body.get(i + 1).is_some_and(|t| t.text == "##")
                    {
                        if value.is_empty() {
                            replacement.push(token.with_text(""));
                        } else {
                            replacement.extend_from_slice(value);
                        }
                    } else {
                        replacement.extend(self.expand_inner(value, depth + 1)?);
                    }
                    if let Some(first) = replacement.get_mut(start) {
                        first.spaced = t.spaced;
                    }
                } else {
                    replacement.push(t.clone());
                }
                i += 1;
            }
            let mut pasted: Vec<Token> = Vec::new();
            let mut iter = replacement.into_iter();
            while let Some(t) = iter.next() {
                if t.text == "##" {
                    let left = pasted.pop().ok_or("missing left token for ##")?;
                    let right = iter.next().ok_or("missing right token for ##")?;
                    let text = format!("{}{}", left.text, right.text);
                    if !text.is_empty() && lex(&text, token.file.clone())?.len() != 1 {
                        return Err("invalid C token paste".into());
                    }
                    pasted.push(Token {
                        spaced: left.spaced,
                        ..token.with_text(text)
                    });
                } else {
                    pasted.push(t);
                }
            }
            if let Some(first) = pasted.first_mut() {
                first.spaced = token.spaced;
            }
            for mut t in pasted.into_iter().rev().filter(|t| !t.text.is_empty()) {
                t.file = token.file.clone();
                t.offset = token.offset;
                t.hidden.extend(token.hidden.iter().cloned());
                t.hidden.push(token.text.clone());
                input.push_front(t);
            }
        }
        Ok(output)
    }
}

fn versioned_directories(path: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<_> = std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .collect();
    dirs.sort_by_cached_key(|path| {
        std::cmp::Reverse(
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .split('.')
                .map(|s| s.parse::<u32>().unwrap_or(0))
                .collect::<Vec<_>>(),
        )
    });
    dirs
}
