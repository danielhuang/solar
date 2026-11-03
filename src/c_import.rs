//! Compile-time C header imports. Clang is used only as a parser and constant
//! evaluator; imported declarations never introduce a C calling convention.

use crate::ast::*;
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

fn children(node: &Value) -> &[Value] {
    node["inner"].as_array().map(Vec::as_slice).unwrap_or(&[])
}

fn string<'a>(node: &'a Value, key: &str) -> &'a str {
    node[key].as_str().unwrap_or("")
}

fn attributes(node: &Value) -> Result<(), String> {
    if let Some(attr) = children(node)
        .iter()
        .find(|child| string(child, "kind").ends_with("Attr"))
    {
        return Err(format!(
            "C attribute `{}` is not representable in Solar source",
            string(attr, "kind")
        ));
    }
    Ok(())
}

struct TranslationUnit {
    declarations: HashMap<String, Value>,
    records: HashMap<String, (Value, String)>,
}

impl TranslationUnit {
    fn new(ast: Value) -> Self {
        fn visit(node: &Value, file: &mut String, unit: &mut TranslationUnit) {
            let location = &node["loc"];
            // Clang elides repeated filenames, including inside macro source
            // locations. Expansion offsets distinguish anonymous records
            // produced by separate invocations of the same macro.
            for location in [
                &location["spellingLoc"],
                location,
                &location["expansionLoc"],
            ] {
                if let Some(path) = location["file"].as_str() {
                    *file = path.to_owned();
                }
            }
            let kind = string(node, "kind");
            let name = string(node, "name");
            if kind == "RecordDecl" {
                let expansion = location.get("expansionLoc").unwrap_or(location);
                let key = format!(
                    "{}:{}:{}:{}",
                    file, expansion["offset"], location["spellingLoc"]["offset"], name
                );
                unit.records
                    .insert(string(node, "id").to_owned(), (node.clone(), key));
            }
            if matches!(
                kind,
                "TypedefDecl" | "RecordDecl" | "EnumConstantDecl" | "VarDecl" | "FunctionDecl"
            ) && !name.is_empty()
            {
                // A struct tag such as `stat` can coexist with a function.
                let previous = unit.declarations.get(name);
                if previous.is_none_or(|previous| {
                    string(previous, "kind") != "TypedefDecl"
                        && (string(previous, "kind") != "RecordDecl" || kind == "RecordDecl")
                }) {
                    unit.declarations.insert(name.to_owned(), node.clone());
                }
            }
            for child in children(node) {
                visit(child, file, unit);
            }
        }
        let mut unit = Self {
            declarations: HashMap::new(),
            records: HashMap::new(),
        };
        visit(&ast, &mut String::new(), &mut unit);
        unit
    }

    fn record(&self, id: &str) -> Result<&(Value, String), String> {
        let record = self.records.get(id).ok_or("missing C record declaration")?;
        if record.0["completeDefinition"] == true {
            return Ok(record);
        }
        self.records
            .values()
            .find(|(candidate, _)| {
                candidate["completeDefinition"] == true
                    && candidate["name"] == record.0["name"]
                    && !string(candidate, "name").is_empty()
            })
            .ok_or_else(|| {
                format!(
                    "incomplete C struct `{}` is not representable in Solar source",
                    string(&record.0, "name")
                )
            })
    }
}

struct Header {
    directory: tempdir::TempDir,
    source: String,
}

impl Header {
    fn new(path: &str) -> Result<Self, String> {
        if path.contains(['\n', '\r', '"', '<', '>']) {
            return Err("invalid C header path".into());
        }
        let include = if path.starts_with("./") {
            let path = std::env::current_dir()
                .map_err(|e| e.to_string())?
                .join(path);
            format!("#include \"{}\"\n", path.display())
        } else {
            format!("#include <{path}>\n")
        };
        Ok(Self {
            directory: tempdir::TempDir::new("solar-c-import").map_err(|e| e.to_string())?,
            source: include
                + "\n_Static_assert(__CHAR_BIT__ == 8 && (char)-1 < 0 && sizeof(short) == 2 && sizeof(int) == 4 && sizeof(long) == 8 && sizeof(long long) == 8 && sizeof(void*) == 8 && sizeof(float) == 4 && sizeof(double) == 8, \"unsupported C scalar layout\");\n",
        })
    }

    fn clang(&self, arguments: &[&str]) -> Result<Vec<u8>, String> {
        let path = self.directory.path().join("import.c");
        std::fs::write(&path, &self.source).map_err(|e| e.to_string())?;
        let output = Command::new("clang")
            .args(["-x", "c", "-std=gnu11", "-Wno-everything"])
            .args(arguments)
            .arg(path)
            .output()
            .map_err(|e| format!("cannot run clang for C import: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "C import failed:\n{}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        Ok(output.stdout)
    }

    fn ast(&self) -> Result<TranslationUnit, String> {
        let dependencies = self.directory.path().join("import.d");
        let output = self.clang(&[
            "-Xclang",
            "-ast-dump=json",
            "-fsyntax-only",
            "-MD",
            "-MF",
            dependencies.to_str().unwrap(),
        ])?;
        serde_json::from_slice(&output)
            .map(TranslationUnit::new)
            .map_err(|e| format!("cannot read clang's C declarations: {e}"))
    }
}

fn named(name: &str) -> Type {
    Type::Named(DefId::new(0, name))
}

fn alias(name: &str, target_type: Type) -> TopLevelItem {
    TopLevelItem::TypeAlias(TypeAliasDef {
        name: name.to_owned(),
        type_params: Vec::new(),
        target_type,
        is_pub: true,
        doc: Some(format!("Imported C type `{name}`.")),
        span: SourceSpan::default(),
    })
}

fn scalar(name: &str) -> Result<(&'static str, u64, u64), String> {
    Ok(match name {
        "_Bool" => ("Bool", 1, 1),
        "char" | "signed char" => ("Int8", 1, 1),
        "unsigned char" => ("Uint8", 1, 1),
        "short" | "short int" => ("Int16", 2, 2),
        "unsigned short" | "unsigned short int" => ("Uint16", 2, 2),
        "int" => ("Int32", 4, 4),
        "unsigned int" => ("Uint32", 4, 4),
        "long" | "long int" => ("Int", 8, 8),
        "unsigned long" | "unsigned long int" => ("Uint", 8, 8),
        "long long" | "long long int" => ("Int64", 8, 8),
        "unsigned long long" | "unsigned long long int" => ("Uint64", 8, 8),
        "float" => ("Float32", 4, 4),
        "double" => ("Float64", 8, 8),
        _ => {
            return Err(format!(
                "C type `{name}` is not representable in Solar source"
            ));
        }
    })
}

fn type_node(declaration: &Value) -> Result<&Value, String> {
    children(declaration)
        .iter()
        .rev()
        .find(|node| string(node, "kind").ends_with("Type"))
        .ok_or_else(|| {
            format!(
                "missing C type information for `{}`",
                string(declaration, "name")
            )
        })
}

fn constant_value(node: &Value) -> Option<&str> {
    if string(node, "kind") == "ConstantExpr" {
        return node["value"].as_str();
    }
    children(node).iter().find_map(constant_value)
}

fn is_enum_type(node: &Value) -> bool {
    match string(node, "kind") {
        "EnumType" => true,
        "TypedefType" | "QualType" | "ElaboratedType" | "ParenType" | "TypeOfExprType"
        | "TypeOfType" => type_node(node).is_ok_and(is_enum_type),
        _ => false,
    }
}

struct Importer {
    header: Header,
    items: Vec<TopLevelItem>,
    records: HashMap<String, String>,
    layouts: HashMap<String, (u64, u64)>,
    next_probe: usize,
}

impl Importer {
    fn probe(&mut self) -> String {
        let name = format!("__solar_c_probe_{}", self.next_probe);
        self.next_probe += 1;
        name
    }

    fn layout(&self, ty: &Type) -> Result<(u64, u64), String> {
        match ty {
            Type::NullableReference(_) => Ok((8, 8)),
            Type::FixedArray(element, count) => {
                let (size, align) = self.layout(element)?;
                Ok((
                    size.checked_mul(*count).ok_or("C array size overflow")?,
                    align,
                ))
            }
            Type::Named(id) => self
                .layouts
                .get(id.name.as_str())
                .copied()
                .ok_or_else(|| format!("unsupported recursive C layout for `{id}`")),
            _ => Err("C type has no representable layout".into()),
        }
    }

    fn ty(&mut self, node: &Value, unit: &TranslationUnit, c_type: &str) -> Result<Type, String> {
        attributes(node)?;
        let kind = string(node, "kind");
        match kind {
            "BuiltinType" => {
                if string(&node["type"], "qualType") == "void" {
                    return Ok(Type::Tuple(Vec::new()));
                }
                let (name, size, align) = scalar(string(&node["type"], "qualType"))?;
                self.layouts.insert(name.to_owned(), (size, align));
                Ok(named(name))
            }
            "PointerType" => {
                let inner = type_node(node)?;
                let ty = if string(&inner["type"], "qualType") == "void" {
                    Type::Tuple(Vec::new())
                } else {
                    self.ty(inner, unit, &format!("__typeof__(*((({c_type})0)))"))?
                };
                Ok(Type::NullableReference(Box::new(ty)))
            }
            "ConstantArrayType" => {
                let size = node["size"]
                    .as_u64()
                    .filter(|size| *size > 0)
                    .ok_or("zero-sized C arrays are not representable in Solar source")?;
                let element = self.ty(
                    type_node(node)?,
                    unit,
                    &format!("__typeof__((({c_type}){{0}})[0])"),
                )?;
                Ok(Type::FixedArray(Box::new(element), size))
            }
            "RecordType" => self.record(node, unit, c_type),
            "TypedefType" => {
                if let Some(declaration) = unit.declarations.get(string(&node["decl"], "name")) {
                    attributes(declaration)?;
                }
                self.ty(type_node(node)?, unit, c_type)
            }
            "QualType" => {
                if string(node, "qualifiers")
                    .split_whitespace()
                    .any(|q| q != "const" && q != "restrict")
                {
                    return Err(format!(
                        "C qualifiers `{}` are not representable in Solar source",
                        string(node, "qualifiers")
                    ));
                }
                self.ty(type_node(node)?, unit, c_type)
            }
            "ElaboratedType" | "ParenType" | "TypeOfExprType" | "TypeOfType" => {
                self.ty(type_node(node)?, unit, c_type)
            }
            _ => Err(format!(
                "C type `{}` ({kind}) is not representable in Solar source",
                string(&node["type"], "qualType")
            )),
        }
    }

    fn evaluate(&mut self, expression: &str, probe: &str, ty: &Type) -> Result<(), String> {
        if let Type::FixedArray(element, count) = ty {
            for index in 0..*count {
                self.evaluate(
                    &format!("({expression})[{index}]"),
                    &format!("{probe}_{index}"),
                    element,
                )?;
            }
            return Ok(());
        }
        let value = match ty {
            Type::Named(id) if id.name == "Float32" => {
                format!("__builtin_bit_cast(unsigned int, (float)({expression}))")
            }
            Type::Named(id) if id.name == "Float64" => {
                format!("__builtin_bit_cast(unsigned long long, (double)({expression}))")
            }
            Type::Named(id)
                if self.layouts.contains_key(id.name.as_str())
                    && !id.name.starts_with("__SolarCRecord") =>
            {
                expression.to_owned()
            }
            Type::NullableReference(_) => format!("({expression}) == (void*)0"),
            _ => {
                return Err(format!(
                    "C constant `{expression}` is not representable as a Solar constant"
                ));
            }
        };
        self.header
            .source
            .push_str(&format!("enum {{ {probe}_value = {value} }};\n"));
        Ok(())
    }

    fn record(
        &mut self,
        node: &Value,
        unit: &TranslationUnit,
        c_type: &str,
    ) -> Result<Type, String> {
        let (record, key) = unit.record(string(&node["decl"], "id"))?;
        if let Some(name) = self.records.get(key) {
            return Ok(named(name));
        }
        if string(record, "tagUsed") != "struct" {
            return Err("C unions are not representable in Solar source".into());
        }
        attributes(record)?;
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut hash);
        let name = format!("__SolarCRecord{:016x}", hash.finish());
        self.records.insert(key.clone(), name.clone());
        let fields: Vec<_> = children(record)
            .iter()
            .filter(|node| string(node, "kind") == "FieldDecl")
            .collect();
        if fields.is_empty() {
            return Err("empty C structs are not representable in Solar source".into());
        }
        let mut probes = Vec::new();
        for field in &fields {
            attributes(field)?;
            if field["isBitfield"] == true {
                return Err("C bitfields are not representable in Solar source".into());
            }
            let field_name = string(field, "name");
            if field_name.is_empty() {
                return Err("anonymous C fields are not representable in Solar source".into());
            }
            let probe = self.probe();
            self.header.source.push_str(&format!(
                "typedef __typeof__((({c_type}*)0)->{field_name}) {probe};\nenum {{ {probe}_offset = __builtin_offsetof({c_type}, {field_name}) }};\n"
            ));
            probes.push(probe);
        }
        let layout_probe = self.probe();
        self.header.source.push_str(&format!("enum {{ {layout_probe}_size = sizeof({c_type}), {layout_probe}_align = _Alignof({c_type}) }};\n"));
        let field_unit = self.header.ast()?;
        let mut result = Vec::new();
        let mut size = 0u64;
        let mut align = 1;
        for (field, probe) in fields.iter().zip(&probes) {
            let ty = self.ty(
                type_node(&field_unit.declarations[probe])?,
                &field_unit,
                probe,
            )?;
            let (field_size, field_align) = self.layout(&ty)?;
            align = align.max(field_align);
            size = size.next_multiple_of(field_align);
            if value_u64(&field_unit, &format!("{probe}_offset"))? != size {
                return Err("C field offsets cannot be represented by struct(repr(C))".into());
            }
            size = size
                .checked_add(field_size)
                .ok_or("C struct size overflow")?;
            result.push(FieldDef {
                name: string(field, "name").to_owned(),
                ty,
                is_pub: true,
                span: SourceSpan::default(),
            });
        }
        size = size.next_multiple_of(align);
        if value_u64(&field_unit, &format!("{layout_probe}_size"))? != size
            || value_u64(&field_unit, &format!("{layout_probe}_align"))? != align
        {
            return Err("C struct layout cannot be represented by struct(repr(C))".into());
        }
        self.layouts.insert(name.clone(), (size, align));
        self.items.push(TopLevelItem::Struct(StructDef {
            name: name.clone(),
            def_id: DefId::new(0, &name),
            type_params: Vec::new(),
            fields: result,
            is_tuple: false,
            repr_c: true,
            opaque: false,
            is_pub: true,
            doc: Some(format!("Imported C struct `{}`.", string(record, "name"))),
            span: SourceSpan::default(),
        }));
        Ok(named(&name))
    }
}

fn literal(
    values: &TranslationUnit,
    name: &str,
    probe: &str,
    ty: &Type,
) -> Result<ExprKind, String> {
    if let Type::FixedArray(element, count) = ty {
        let elements = (0..*count)
            .map(|index| {
                Ok(Expr {
                    kind: literal(values, name, &format!("{probe}_{index}"), element)?,
                    span: SourceSpan::default(),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        return Ok(ExprKind::ArrayLiteral(elements, Some(*element.clone())));
    }
    let value_name = format!("{probe}_value");
    let value = values
        .declarations
        .get(&value_name)
        .and_then(constant_value)
        .ok_or_else(|| format!("C constant `{name}` cannot be evaluated at compile time"))?;
    let kind = match &ty {
        Type::NullableReference(inner) if value == "1" => ExprKind::NullLiteral(*inner.clone()),
        Type::Named(id) => match id.name.as_str() {
            "Float32" => ExprKind::FloatLiteral(
                f32::from_bits(value.parse().map_err(|e| format!("{e}"))?) as f64,
                FloatType::Float32,
            ),
            "Float64" => ExprKind::FloatLiteral(
                f64::from_bits(value.parse().map_err(|e| format!("{e}"))?),
                FloatType::Float64,
            ),
            "Bool" => ExprKind::BooleanLiteral(value != "0"),
            name => {
                let it = match name {
                    "Int8" => IntegerType::Int8,
                    "Uint8" => IntegerType::Uint8,
                    "Int16" => IntegerType::Int16,
                    "Uint16" => IntegerType::Uint16,
                    "Int32" => IntegerType::Int32,
                    "Uint32" => IntegerType::Uint32,
                    "Int64" => IntegerType::Int64,
                    "Uint64" => IntegerType::Uint64,
                    "Int" => IntegerType::Int,
                    "Uint" => IntegerType::Uint,
                    _ => return Err(format!("unsupported C constant type `{name}`")),
                };
                ExprKind::IntegerLiteral(value.parse().map_err(|e| format!("{e}"))?, it)
            }
        },
        _ => {
            return Err(format!(
                "non-null C pointer constant `{name}` is not representable in Solar source"
            ));
        }
    };
    if matches!(&kind, ExprKind::FloatLiteral(value, _) if !value.is_finite()) {
        return Err(format!(
            "non-finite C constant `{name}` is not representable in Solar source"
        ));
    }
    Ok(kind)
}

fn value_u64(unit: &TranslationUnit, name: &str) -> Result<u64, String> {
    unit.declarations
        .get(name)
        .and_then(constant_value)
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| format!("cannot evaluate C constant `{name}`"))
}

/// Reads the requested declarations using the host Clang's system include paths.
fn import_uncached(path: &str, kind: &ImportKind) -> Result<CachedImport, String> {
    let mut importer = Importer {
        header: Header::new(path)?,
        items: Vec::new(),
        records: HashMap::new(),
        layouts: HashMap::new(),
        next_probe: 0,
    };
    let initial = importer.header.ast()?;
    let macros =
        String::from_utf8(importer.header.clang(&["-E", "-dM"])?).map_err(|e| e.to_string())?;
    let macros: HashMap<_, _> = macros
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("#define ")?;
            let end = rest
                .find(|c: char| c.is_whitespace() || c == '(')
                .unwrap_or(rest.len());
            Some((
                rest[..end].to_owned(),
                (
                    rest.as_bytes().get(end) == Some(&b'('),
                    rest[end..].trim().is_empty(),
                ),
            ))
        })
        .collect();
    let names: BTreeSet<String> = match kind {
        ImportKind::Named(names) => {
            if names.iter().any(ImportName::is_path) {
                return Err("C imports require unqualified declaration names".into());
            }
            names
                .iter()
                .map(|name| name.local_name().to_owned())
                .collect()
        }
        ImportKind::Module(_) | ImportKind::Wildcard => initial
            .declarations
            .iter()
            .filter(|(name, decl)| !name.starts_with('_') && string(decl, "kind") != "FunctionDecl")
            .map(|(name, _)| name.clone())
            .chain(
                macros
                    .iter()
                    .filter(|(name, (function, empty))| {
                        !name.starts_with('_') && !function && !empty
                    })
                    .map(|(name, _)| name.clone()),
            )
            .collect(),
    };
    let mut requests = Vec::new();
    for name in names {
        let declaration = initial.declarations.get(&name);
        let probe = importer.probe();
        let is_type = !macros.contains_key(&name)
            && declaration
                .is_some_and(|d| matches!(string(d, "kind"), "TypedefDecl" | "RecordDecl"));
        let expression = if is_type {
            let declaration = declaration.unwrap();
            attributes(declaration)?;
            if string(declaration, "kind") == "RecordDecl" {
                format!("{} {name}", string(declaration, "tagUsed"))
            } else {
                name.clone()
            }
        } else {
            if macros.get(&name).is_some_and(|(function, _)| *function)
                || declaration.is_some_and(|d| string(d, "kind") == "FunctionDecl")
            {
                return Err(format!(
                    "cannot import C function `{name}`: C FFI is not supported"
                ));
            }
            if !macros.contains_key(&name) && declaration.is_none() {
                return Err(format!(
                    "C header `{path}` has no constant or type named `{name}`"
                ));
            }
            if declaration.is_some_and(|d| {
                let ty = string(&d["type"], "qualType");
                string(d, "kind") == "VarDecl"
                    && !(if ty.contains('*') {
                        ty.ends_with("const")
                    } else {
                        ty.starts_with("const ")
                    })
            }) {
                return Err(format!(
                    "C variable `{name}` is not a constant; C FFI is not supported"
                ));
            }
            // C enumerators have integer values, even when typeof names their enum.
            if !macros.contains_key(&name)
                && declaration.is_some_and(|d| string(d, "kind") == "EnumConstantDecl")
            {
                format!("__typeof__(({name}) + 0)")
            } else {
                format!("__typeof__({name})")
            }
        };
        importer
            .header
            .source
            .push_str(&format!("typedef {expression} {probe};\n"));
        requests.push((name, probe, is_type));
    }
    let types = importer.header.ast()?;
    let mut constants = Vec::new();
    for (name, probe, is_type) in requests {
        let promoted;
        let (unit, type_probe) =
            if !is_type && is_enum_type(type_node(&types.declarations[&probe])?) {
                importer.header.source.push_str(&format!(
                    "typedef __typeof__(({name}) + 0) {probe}_integer;\n"
                ));
                promoted = importer.header.ast()?;
                (&promoted, format!("{probe}_integer"))
            } else {
                (&types, probe.clone())
            };
        let ty = importer
            .ty(
                type_node(&unit.declarations[&type_probe])?,
                unit,
                &type_probe,
            )
            .map_err(|error| format!("cannot import C declaration `{name}`: {error}"))?;
        if is_type {
            importer.items.push(alias(&name, ty));
            continue;
        }
        importer.evaluate(&name, &probe, &ty)?;
        constants.push((name, probe, ty));
    }
    if !constants.is_empty() {
        let values = importer.header.ast()?;
        for (name, probe, ty) in constants {
            let kind = literal(&values, &name, &probe, &ty)?;
            importer.items.push(TopLevelItem::Const(ConstDef {
                name: name.clone(),
                ty: Some(ty),
                value: Box::new(Expr {
                    kind,
                    span: SourceSpan::default(),
                }),
                is_pub: true,
                doc: Some(format!("Imported C constant `{name}`.")),
                span: SourceSpan::default(),
            }));
        }
    }
    let dependency_file =
        std::fs::read_to_string(importer.header.directory.path().join("import.d"))
            .map_err(|e| e.to_string())?;
    let dependencies = dependency_paths(&dependency_file)
        .into_iter()
        .filter(|path| !path.starts_with(importer.header.directory.path()))
        .map(|path| {
            let metadata = std::fs::metadata(&path).map_err(|e| e.to_string())?;
            let modified = metadata.modified().map_err(|e| e.to_string())?;
            Ok((path, modified, metadata.len()))
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(CachedImport {
        items: importer.items,
        dependencies,
    })
}

fn dependency_paths(makefile: &str) -> Vec<PathBuf> {
    let makefile = makefile.replace("\\\n", " ").replace("$$", "$");
    let mut words = Vec::new();
    let mut word = String::new();
    let mut escaped = false;
    for ch in makefile.chars() {
        if escaped {
            word.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch.is_whitespace() {
            if !word.is_empty() {
                words.push(PathBuf::from(std::mem::take(&mut word)));
            }
        } else {
            word.push(ch);
        }
    }
    if !word.is_empty() {
        words.push(PathBuf::from(word));
    }
    words.into_iter().skip(1).collect()
}

#[derive(Clone)]
struct CachedImport {
    items: Vec<TopLevelItem>,
    dependencies: Vec<(PathBuf, SystemTime, u64)>,
}

/// Reads requested C declarations. System headers are reused within
/// the compiler process while their transitive dependencies remain unchanged.
pub(crate) fn import(path: &str, kind: &ImportKind) -> Result<Vec<TopLevelItem>, String> {
    static CACHE: OnceLock<Mutex<HashMap<String, CachedImport>>> = OnceLock::new();
    if path.starts_with("./") {
        return Ok(import_uncached(path, kind)?.items);
    }
    let key = format!(
        "{path}:{kind:?}:{:?}",
        ["PATH", "CPATH", "C_INCLUDE_PATH", "SDKROOT"].map(std::env::var_os)
    );
    let cache = CACHE.get_or_init(Mutex::default);
    if let Some(cached) = cache.lock().unwrap().get(&key)
        && cached.dependencies.iter().all(|(path, modified, size)| {
            std::fs::metadata(path).is_ok_and(|metadata| {
                metadata.modified().ok() == Some(*modified) && metadata.len() == *size
            })
        })
    {
        return Ok(cached.items.clone());
    }
    let result = import_uncached(path, kind)?;
    let items = result.items.clone();
    cache.lock().unwrap().insert(key, result);
    Ok(items)
}
