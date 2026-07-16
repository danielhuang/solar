use crate::ast::*;
use crate::error::{CompileError, SourceMap};
use crate::parser;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

const STDLIB_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/std");

type FileId = u32;
type ModuleAliasMap = HashMap<String, FileId>;
type AllModuleAliases = HashMap<FileId, ModuleAliasMap>;

struct ParsedFile {
    file_id: FileId,
    path: PathBuf,
    ast: SourceFile,
}

fn is_method_export(kind: &ExportKind) -> bool {
    matches!(kind, ExportKind::Method)
}

#[derive(Debug, Clone)]
enum ExportKind {
    Struct,
    Enum,
    Function,
    Method,
    TypeAlias,
    Const,
    Static,
}

struct Resolver {
    files: Vec<ParsedFile>,
    path_to_id: HashMap<PathBuf, FileId>,
    source_map: SourceMap,
    /// FileId of the stdlib root (lib.solar), if parsed.
    std_root_id: Option<FileId>,
}

impl Resolver {
    fn new() -> Self {
        Self {
            files: Vec::new(),
            path_to_id: HashMap::new(),
            source_map: SourceMap::new(),
            std_root_id: None,
        }
    }

    /// Parse a file and assign it a FileId. Returns the FileId.
    /// If already parsed (by canonical path), returns the existing FileId.
    fn parse_file(&mut self, path: &Path) -> Result<FileId, Vec<CompileError>> {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if let Some(&id) = self.path_to_id.get(&canonical) {
            return Ok(id);
        }

        let source = std::fs::read_to_string(path).map_err(|e| {
            vec![CompileError::new(
                format!("cannot read file `{}`: {e}", path.display()),
                SourceSpan::default(),
            )]
        })?;

        let file_id = self.files.len() as FileId;
        self.path_to_id.insert(canonical.clone(), file_id);

        let filename = path.display().to_string();
        self.source_map.add_file(file_id, filename, source.clone());

        let ast = parser::parse(&source).map_err(|errors| {
            errors
                .into_iter()
                .map(|e| {
                    CompileError::new(
                        e.message,
                        SourceSpan {
                            start: SourcePos {
                                line: e.line as u32,
                                col: e.column as u32,
                            },
                            end: SourcePos {
                                line: e.line as u32,
                                col: e.column as u32 + 1,
                            },
                            file_id,
                        },
                    )
                })
                .collect::<Vec<_>>()
        })?;

        self.files.push(ParsedFile {
            file_id,
            path: canonical,
            ast,
        });

        Ok(file_id)
    }

    /// Recursively parse all imported files starting from root.
    fn parse_imports(&mut self, file_id: FileId) -> Result<(), Vec<CompileError>> {
        // Collect import paths from this file
        let imports: Vec<(String, SourceSpan)> = self.files[file_id as usize]
            .ast
            .items
            .iter()
            .filter_map(|item| {
                if let TopLevelItem::Import(imp) = item {
                    Some((imp.path.clone(), imp.span))
                } else {
                    None
                }
            })
            .collect();

        let base_dir = self.files[file_id as usize]
            .path
            .parent()
            .unwrap_or(Path::new("."))
            .to_path_buf();

        for (import_path, span) in imports {
            if import_path == "@intrinsics" || import_path == "@std" {
                continue;
            }
            let resolved_path = base_dir.join(&import_path);
            if !resolved_path.exists() {
                return Err(vec![CompileError::new(
                    format!("cannot find imported file: `{import_path}`"),
                    span,
                )]);
            }
            let canonical = resolved_path
                .canonicalize()
                .unwrap_or_else(|_| resolved_path.clone());
            let already_seen = self.path_to_id.contains_key(&canonical);
            let imported_id = self.parse_file(&resolved_path)?;
            if !already_seen {
                self.parse_imports(imported_id)?;
            }
        }

        Ok(())
    }

    /// Collect exports from a file (pub items).
    fn collect_exports(&self, file_id: FileId) -> HashMap<String, Vec<ExportKind>> {
        let mut exports: HashMap<String, Vec<ExportKind>> = HashMap::new();
        for item in &self.files[file_id as usize].ast.items {
            match item {
                TopLevelItem::Struct(s) if s.is_pub => {
                    exports
                        .entry(s.name.clone())
                        .or_default()
                        .push(ExportKind::Struct);
                }
                TopLevelItem::Enum(e) if e.is_pub => {
                    exports
                        .entry(e.name.clone())
                        .or_default()
                        .push(ExportKind::Enum);
                }
                TopLevelItem::Function(f) if f.is_pub => {
                    exports
                        .entry(f.name.clone())
                        .or_default()
                        .push(ExportKind::Function);
                }
                TopLevelItem::Method(m) if m.is_pub => {
                    exports
                        .entry(m.name.clone())
                        .or_default()
                        .push(ExportKind::Method);
                }
                TopLevelItem::TypeAlias(ta) if ta.is_pub => {
                    exports
                        .entry(ta.name.clone())
                        .or_default()
                        .push(ExportKind::TypeAlias);
                }
                TopLevelItem::Const(c) if c.is_pub => {
                    exports
                        .entry(c.name.clone())
                        .or_default()
                        .push(ExportKind::Const);
                }
                TopLevelItem::Static(st) if st.is_pub => {
                    exports
                        .entry(st.name.clone())
                        .or_default()
                        .push(ExportKind::Static);
                }
                _ => {}
            }
        }
        exports
    }

    /// Compute module aliases for a file (module imports only): alias → the
    /// aliased module's defining file id.
    fn compute_module_aliases(&self, file_id: FileId, _root_id: FileId) -> ModuleAliasMap {
        let file = &self.files[file_id as usize];
        let base_dir = file.path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let mut aliases = HashMap::new();
        for item in &file.ast.items {
            if let TopLevelItem::Import(imp) = item
                && let ImportKind::Module(alias) = &imp.kind
                && imp.path != "@intrinsics"
            {
                if imp.path == "@std" {
                    if let Some(std_id) = self.std_root_id {
                        aliases.insert(alias.clone(), std_id);
                    }
                } else {
                    let resolved_path = base_dir.join(&imp.path);
                    let canonical = resolved_path
                        .canonicalize()
                        .unwrap_or_else(|_| resolved_path.clone());
                    if let Some(&source_file_id) = self.path_to_id.get(&canonical) {
                        aliases.insert(alias.clone(), source_file_id);
                    }
                }
            }
        }
        aliases
    }

    /// Build rename map for a file and rewrite its AST.
    fn resolve_file(
        &self,
        file_id: FileId,
        _root_id: FileId,
        all_module_aliases: &AllModuleAliases,
    ) -> Result<Vec<TopLevelItem>, Vec<CompileError>> {
        let file = &self.files[file_id as usize];

        // Build rename map: original source name -> resolved provenance `DefId`.
        // References (types, struct/enum construction, function/const/static
        // identifiers) are resolved to these `DefId`s directly — the final
        // module-mangling is deferred to `mangled_ast`.
        let mut rename_map: HashMap<String, DefId> = HashMap::new();
        // Module aliases: alias -> defining file id.
        let mut module_aliases: ModuleAliasMap = HashMap::new();

        // Collect this file's own definitions
        let mut local_defs: HashSet<String> = HashSet::new();
        // Methods are resolved globally by bare name as overload sets (their
        // definitions are never renamed), so a method import sharing a name with
        // a local method is not a real clash — the two simply overload.
        let mut local_method_defs: HashSet<String> = HashSet::new();
        for item in &file.ast.items {
            match item {
                TopLevelItem::Struct(s) => {
                    local_defs.insert(s.name.clone());
                    rename_map.insert(s.name.clone(), DefId::new(file_id, s.name.clone()));
                }
                TopLevelItem::Enum(e) => {
                    local_defs.insert(e.name.clone());
                    rename_map.insert(e.name.clone(), DefId::new(file_id, e.name.clone()));
                }
                TopLevelItem::Function(f) => {
                    local_defs.insert(f.name.clone());
                    rename_map.insert(f.name.clone(), DefId::new(file_id, f.name.clone()));
                }
                TopLevelItem::Method(m) => {
                    local_defs.insert(m.name.clone());
                    local_method_defs.insert(m.name.clone());
                    // Methods get renamed via self-type mangling in typed_ast,
                    // but the name itself needs prefixing for the resolve stage
                    rename_map.insert(m.name.clone(), DefId::new(file_id, m.name.clone()));
                }
                TopLevelItem::TypeAlias(ta) => {
                    local_defs.insert(ta.name.clone());
                    rename_map.insert(ta.name.clone(), DefId::new(file_id, ta.name.clone()));
                }
                TopLevelItem::Const(c) => {
                    local_defs.insert(c.name.clone());
                    rename_map.insert(c.name.clone(), DefId::new(file_id, c.name.clone()));
                }
                TopLevelItem::Static(st) => {
                    local_defs.insert(st.name.clone());
                    rename_map.insert(st.name.clone(), DefId::new(file_id, st.name.clone()));
                }
                TopLevelItem::Import(_) => {}
            }
        }

        // Process imports
        let base_dir = file.path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let mut intrinsic_names: HashSet<String> = HashSet::new();
        let mut intrinsic_modules: HashSet<String> = HashSet::new();

        for item in &file.ast.items {
            if let TopLevelItem::Import(imp) = item {
                if imp.path == "@intrinsics" {
                    // Handle intrinsic imports
                    match &imp.kind {
                        ImportKind::Named(names) => {
                            for name in names {
                                if name.is_path() {
                                    return Err(vec![CompileError::new(
                                        "path imports from \"@intrinsics\" are not supported"
                                            .to_string(),
                                        imp.span,
                                    )]);
                                }
                                let local = name.local_name();
                                if Intrinsic::from_name(local).is_none() {
                                    return Err(vec![CompileError::new(
                                        format!("unknown intrinsic: `{local}`"),
                                        imp.span,
                                    )]);
                                }
                                intrinsic_names.insert(local.to_string());
                            }
                        }
                        ImportKind::Module(alias) => {
                            intrinsic_modules.insert(alias.clone());
                        }
                        ImportKind::Wildcard => {
                            return Err(vec![CompileError::new(
                                "wildcard import from \"@intrinsics\" is not allowed".to_string(),
                                imp.span,
                            )]);
                        }
                    }
                    continue;
                }

                let source_file_id = if imp.path == "@std" {
                    match self.std_root_id {
                        Some(id) => id,
                        None => {
                            return Err(vec![CompileError::new(
                                "stdlib not available".to_string(),
                                imp.span,
                            )]);
                        }
                    }
                } else {
                    let resolved_path = base_dir.join(&imp.path);
                    let canonical = resolved_path
                        .canonicalize()
                        .unwrap_or_else(|_| resolved_path.clone());
                    self.path_to_id[&canonical]
                };
                let exports = self.collect_exports(source_file_id);

                match &imp.kind {
                    ImportKind::Named(names) => {
                        for name in names {
                            let local = name.local_name().to_string();
                            if name.is_path() {
                                // Path import: resolve module chain starting from source file
                                let mod_segs = name.module_segments();
                                let source_aliases = all_module_aliases.get(&source_file_id);
                                let empty_aliases: ModuleAliasMap = HashMap::new();
                                let aliases = source_aliases.unwrap_or(&empty_aliases);

                                if let Some(final_fid) =
                                    resolve_module_chain_full(mod_segs, aliases, all_module_aliases)
                                {
                                    let final_exports = self.collect_exports(final_fid);
                                    if !final_exports.contains_key(&local) {
                                        return Err(vec![CompileError::new(
                                            format!(
                                                "`{local}` is not exported from the resolved module in `{}`",
                                                imp.path
                                            ),
                                            imp.span,
                                        )]);
                                    }
                                    if rename_map.contains_key(&local) {
                                        return Err(vec![CompileError::new(
                                            format!(
                                                "import `{local}` conflicts with an existing import or definition"
                                            ),
                                            imp.span,
                                        )]);
                                    }
                                    rename_map
                                        .insert(local, DefId::new(final_fid, name.local_name()));
                                } else {
                                    return Err(vec![CompileError::new(
                                        format!(
                                            "could not resolve path `{}` in `{}`",
                                            name.segments.join("::"),
                                            imp.path,
                                        ),
                                        imp.span,
                                    )]);
                                }
                            } else {
                                // Plain import
                                if !exports.contains_key(&local) {
                                    return Err(vec![CompileError::new(
                                        format!("`{local}` is not exported from `{}`", imp.path),
                                        imp.span,
                                    )]);
                                }
                                let method_overload = local_method_defs.contains(&local)
                                    && exports
                                        .get(&local)
                                        .is_some_and(|ks| ks.iter().any(is_method_export));
                                if local_defs.contains(&local)
                                    && !rename_map.contains_key(&local)
                                    && !method_overload
                                {
                                    return Err(vec![CompileError::new(
                                        format!("import `{local}` conflicts with local definition"),
                                        imp.span,
                                    )]);
                                }
                                if method_overload {
                                    continue;
                                }
                                rename_map
                                    .insert(local.clone(), DefId::new(source_file_id, &local));
                            }
                        }
                    }
                    ImportKind::Module(alias) => {
                        module_aliases.insert(alias.clone(), source_file_id);
                    }
                    ImportKind::Wildcard => {
                        for (name, kinds) in &exports {
                            // A method name shared with a local method just adds
                            // overloads (methods resolve globally), so it is not a
                            // conflict and needs no rename entry.
                            if local_method_defs.contains(name)
                                && kinds.iter().any(is_method_export)
                            {
                                continue;
                            }
                            if local_defs.contains(name) && !rename_map.contains_key(name) {
                                return Err(vec![CompileError::new(
                                    format!(
                                        "wildcard import of `{name}` from `{}` conflicts with local definition",
                                        imp.path
                                    ),
                                    imp.span,
                                )]);
                            }
                            rename_map.insert(name.clone(), DefId::new(source_file_id, name));
                        }
                        // Propagate pub module re-exports
                        if let Some(source_aliases) = all_module_aliases.get(&source_file_id) {
                            for src_item in &self.files[source_file_id as usize].ast.items {
                                if let TopLevelItem::Import(src_imp) = src_item
                                    && src_imp.is_pub
                                    && src_imp.path != "@intrinsics"
                                    && let ImportKind::Module(alias) = &src_imp.kind
                                    && let Some(fid) = source_aliases.get(alias)
                                {
                                    module_aliases.insert(alias.clone(), *fid);
                                }
                            }
                        }
                    }
                }
            }
        }

        // Rewrite AST items
        let mut rewritten = Vec::new();
        for item in &file.ast.items {
            match item {
                TopLevelItem::Struct(s) => {
                    let mut s = s.clone();
                    // Record provenance (defining file + original name) before the
                    // name is rewritten to its module-mangled form. The mangling
                    // itself is deferred to `mangled_ast`.
                    s.def_id = DefId::new(file_id, s.name.clone());
                    // Rewrite field types and set file_id on field spans
                    for field in &mut s.fields {
                        field.ty =
                            rewrite_type(&field.ty, &rename_map, &module_aliases, &s.type_params);
                        set_file_id_span(&mut field.span, file_id);
                    }
                    set_file_id_span(&mut s.span, file_id);
                    rewritten.push(TopLevelItem::Struct(s));
                }
                TopLevelItem::Enum(e) => {
                    let mut e = e.clone();
                    e.def_id = DefId::new(file_id, e.name.clone());
                    // Rewrite variant inner types
                    for variant in &mut e.variants {
                        if let Some(ty) = &mut variant.inner_type {
                            *ty = rewrite_type(ty, &rename_map, &module_aliases, &e.type_params);
                        }
                    }
                    set_file_id_span(&mut e.span, file_id);
                    rewritten.push(TopLevelItem::Enum(e));
                }
                TopLevelItem::Function(f) => {
                    let mut f = f.clone();
                    rewrite_function_body(
                        &mut f,
                        &rename_map,
                        &module_aliases,
                        all_module_aliases,
                        &intrinsic_names,
                        &intrinsic_modules,
                        file_id,
                    );
                    set_file_id_span(&mut f.span, file_id);
                    rewritten.push(TopLevelItem::Function(f));
                }
                TopLevelItem::Method(m) => {
                    let mut m = m.clone();
                    // Don't rename method name — it stays as-is for typed_ast method mangling
                    // But DO rewrite types in parameters and body
                    rewrite_function_body(
                        &mut m,
                        &rename_map,
                        &module_aliases,
                        all_module_aliases,
                        &intrinsic_names,
                        &intrinsic_modules,
                        file_id,
                    );
                    set_file_id_span(&mut m.span, file_id);
                    rewritten.push(TopLevelItem::Method(m));
                }
                TopLevelItem::TypeAlias(ta) => {
                    let mut ta = ta.clone();
                    ta.target_type = rewrite_type(
                        &ta.target_type,
                        &rename_map,
                        &module_aliases,
                        &ta.type_params,
                    );
                    set_file_id_span(&mut ta.span, file_id);
                    rewritten.push(TopLevelItem::TypeAlias(ta));
                }
                TopLevelItem::Const(c) => {
                    let mut c = c.clone();
                    // The value is a literal (no name references), but an explicit
                    // type may reference a renamed/imported type.
                    if let Some(ty) = &mut c.ty {
                        *ty = rewrite_type(ty, &rename_map, &module_aliases, &[]);
                    }
                    set_file_id_span(&mut c.span, file_id);
                    rewritten.push(TopLevelItem::Const(c));
                }
                TopLevelItem::Static(st) => {
                    let mut st = st.clone();
                    // An explicit type may reference a renamed/imported type.
                    if let Some(ty) = &mut st.ty {
                        *ty = rewrite_type(ty, &rename_map, &module_aliases, &[]);
                    }
                    // The value is a literal, but `null#[T]` carries a type arg
                    // that may name an imported module type — rewrite it like a
                    // function-body expression.
                    let ctx = RewriteCtx {
                        rename_map: &rename_map,
                        module_aliases: &module_aliases,
                        all_module_aliases,
                        intrinsic_names: &intrinsic_names,
                        intrinsic_modules: &intrinsic_modules,
                        type_params: &[],
                        file_id,
                    };
                    rewrite_expr(&mut st.value, &ctx, &HashSet::new());
                    set_file_id_span(&mut st.span, file_id);
                    rewritten.push(TopLevelItem::Static(st));
                }
                TopLevelItem::Import(_) => {
                    // Strip imports from output
                }
            }
        }

        Ok(rewritten)
    }
}

fn set_file_id_span(span: &mut SourceSpan, file_id: FileId) {
    span.file_id = file_id;
}

/// Resolve a `Type::Named`/`Generic` reference `DefId` to its real provenance
/// `DefId` (defining file + name). Builtins and type parameters are left with
/// file `0` — `typed_ast` dispatches on the name for those. This is what lets a
/// **struct/enum type reference carry the real `DefId`** instead of a stringified
/// `__def{file}_…` name that a later stage would parse back.
fn resolve_type_ref(
    name: &DefId,
    rename_map: &HashMap<String, DefId>,
    module_aliases: &ModuleAliasMap,
    type_params: &[String],
) -> DefId {
    // `module::Local` module-qualified reference.
    if let Some((module, local)) = name.name.split_once("::") {
        return match module_aliases.get(module) {
            Some(fid) => DefId::new(*fid, local),
            None => name.clone(),
        };
    }
    if type_params.contains(&name.name) {
        return name.clone();
    }
    rename_map
        .get(&name.name)
        .cloned()
        .unwrap_or_else(|| name.clone())
}

/// Resolve a struct/enum reference (`name`) with an optional single-segment
/// `module` qualifier (struct literals, struct destructure patterns) to its real
/// provenance `DefId` in place.
fn resolve_qualified_def(
    name: &mut DefId,
    module: Option<String>,
    rename_map: &HashMap<String, DefId>,
    module_aliases: &ModuleAliasMap,
) {
    match module {
        Some(m) => {
            if let Some(fid) = module_aliases.get(&m) {
                *name = DefId::new(*fid, &name.name);
            }
        }
        None => {
            if let Some(defid) = rename_map.get(&name.name) {
                *name = defid.clone();
            }
        }
    }
}

/// Rewrite a type, replacing names via the rename map and resolving module-qualified types.
fn rewrite_type(
    ty: &Type,
    rename_map: &HashMap<String, DefId>,
    module_aliases: &ModuleAliasMap,
    type_params: &[String],
) -> Type {
    match ty {
        Type::Named(name) => Type::Named(resolve_type_ref(
            name,
            rename_map,
            module_aliases,
            type_params,
        )),
        Type::Generic { name, type_args } => {
            let rewritten_args: Vec<Type> = type_args
                .iter()
                .map(|t| rewrite_type(t, rename_map, module_aliases, type_params))
                .collect();
            Type::Generic {
                name: resolve_type_ref(name, rename_map, module_aliases, type_params),
                type_args: rewritten_args,
            }
        }
        Type::Reference(inner) => Type::Reference(Box::new(rewrite_type(
            inner,
            rename_map,
            module_aliases,
            type_params,
        ))),
        Type::NullableReference(inner) => Type::NullableReference(Box::new(rewrite_type(
            inner,
            rename_map,
            module_aliases,
            type_params,
        ))),
        Type::Unique(inner) => Type::Unique(Box::new(rewrite_type(
            inner,
            rename_map,
            module_aliases,
            type_params,
        ))),
        Type::Slice(inner) => Type::Slice(Box::new(rewrite_type(
            inner,
            rename_map,
            module_aliases,
            type_params,
        ))),
        Type::FixedArray(inner, size) => Type::FixedArray(
            Box::new(rewrite_type(inner, rename_map, module_aliases, type_params)),
            *size,
        ),
        Type::Function {
            params,
            return_type,
        } => Type::Function {
            params: params
                .iter()
                .map(|(name, ty)| {
                    (
                        name.clone(),
                        rewrite_type(ty, rename_map, module_aliases, type_params),
                    )
                })
                .collect(),
            return_type: return_type
                .as_ref()
                .map(|t| Box::new(rewrite_type(t, rename_map, module_aliases, type_params))),
        },
        Type::Tuple(types) => Type::Tuple(
            types
                .iter()
                .map(|t| rewrite_type(t, rename_map, module_aliases, type_params))
                .collect(),
        ),
        Type::Infer => Type::Infer,
    }
}

/// Context for rewriting AST names during module resolution.
struct RewriteCtx<'a> {
    rename_map: &'a HashMap<String, DefId>,
    module_aliases: &'a ModuleAliasMap,
    all_module_aliases: &'a AllModuleAliases,
    intrinsic_names: &'a HashSet<String>,
    intrinsic_modules: &'a HashSet<String>,
    type_params: &'a [String],
    file_id: FileId,
}

/// Rewrite all names in a function's parameters, return type, and body.
fn rewrite_function_body(
    f: &mut FunctionDef,
    rename_map: &HashMap<String, DefId>,
    module_aliases: &ModuleAliasMap,
    all_module_aliases: &AllModuleAliases,
    intrinsic_names: &HashSet<String>,
    intrinsic_modules: &HashSet<String>,
    file_id: FileId,
) {
    let type_params = &f.type_params;

    // Collect locally-bound names to avoid renaming them
    let mut locals: HashSet<String> = HashSet::new();
    for p in &f.parameters {
        collect_pattern_names(&p.pattern, &mut locals);
    }

    // Rewrite parameter types
    for p in &mut f.parameters {
        p.ty = rewrite_type(&p.ty, rename_map, module_aliases, type_params);
        rewrite_destructure_pattern(&mut p.pattern, rename_map, module_aliases);
    }

    // Rewrite return type
    if let Some(rt) = &mut f.return_type {
        *rt = rewrite_type(rt, rename_map, module_aliases, type_params);
    }

    let ctx = RewriteCtx {
        rename_map,
        module_aliases,
        all_module_aliases,
        intrinsic_names,
        intrinsic_modules,
        type_params,
        file_id,
    };

    // Rewrite body
    rewrite_statements(&mut f.body, &ctx, &mut locals);
}

fn collect_pattern_names(pat: &DestructurePattern, names: &mut HashSet<String>) {
    match pat {
        DestructurePattern::Name(n) => {
            names.insert(n.clone());
        }
        DestructurePattern::Tuple(pats) => {
            for p in pats {
                collect_pattern_names(p, names);
            }
        }
        DestructurePattern::Struct { fields, .. } => {
            for f in fields {
                collect_pattern_names(&f.pattern, names);
            }
        }
        DestructurePattern::Array(pats) => {
            for p in pats {
                collect_pattern_names(p, names);
            }
        }
    }
}

fn rewrite_destructure_pattern(
    pat: &mut DestructurePattern,
    rename_map: &HashMap<String, DefId>,
    module_aliases: &ModuleAliasMap,
) {
    match pat {
        DestructurePattern::Name(_) => {}
        DestructurePattern::Tuple(pats) => {
            for p in pats {
                rewrite_destructure_pattern(p, rename_map, module_aliases);
            }
        }
        DestructurePattern::Struct {
            module,
            name,
            fields,
        } => {
            resolve_qualified_def(name, module.take(), rename_map, module_aliases);
            for f in fields {
                rewrite_destructure_pattern(&mut f.pattern, rename_map, module_aliases);
            }
        }
        DestructurePattern::Array(pats) => {
            for p in pats {
                rewrite_destructure_pattern(p, rename_map, module_aliases);
            }
        }
    }
}

fn rewrite_statements(stmts: &mut [Statement], ctx: &RewriteCtx, locals: &mut HashSet<String>) {
    for stmt in stmts.iter_mut() {
        rewrite_statement(stmt, ctx, locals);
    }
}

fn rewrite_statement(stmt: &mut Statement, ctx: &RewriteCtx, locals: &mut HashSet<String>) {
    stmt.span.file_id = ctx.file_id;
    match &mut stmt.kind {
        StatementKind::Let { pattern, ty, value } => {
            collect_pattern_names(pattern, locals);
            rewrite_destructure_pattern(pattern, ctx.rename_map, ctx.module_aliases);
            if let Some(t) = ty {
                *t = rewrite_type(t, ctx.rename_map, ctx.module_aliases, ctx.type_params);
            }
            rewrite_expr(value, ctx, locals);
        }
        StatementKind::Assignment { target, value } => {
            rewrite_expr(target, ctx, locals);
            rewrite_expr(value, ctx, locals);
        }
        StatementKind::If {
            condition,
            body,
            else_body,
        } => {
            rewrite_expr(condition, ctx, locals);
            rewrite_statements(body, ctx, locals);
            rewrite_statements(else_body, ctx, locals);
        }
        StatementKind::While { condition, body } => {
            rewrite_expr(condition, ctx, locals);
            rewrite_statements(body, ctx, locals);
        }
        StatementKind::ForRange {
            variable,
            start,
            end,
            body,
        } => {
            locals.insert(variable.clone());
            rewrite_expr(start, ctx, locals);
            rewrite_expr(end, ctx, locals);
            rewrite_statements(body, ctx, locals);
        }
        StatementKind::ForIn {
            variable,
            iterable,
            body,
        } => {
            locals.insert(variable.clone());
            rewrite_expr(iterable, ctx, locals);
            rewrite_statements(body, ctx, locals);
        }
        StatementKind::ForReflectFields {
            pattern,
            object,
            body,
            paired: _,
        }
        | StatementKind::MatchReflectVariant {
            pattern,
            object,
            body,
            paired: _,
        } => {
            collect_pattern_names(pattern, locals);
            rewrite_destructure_pattern(pattern, ctx.rename_map, ctx.module_aliases);
            rewrite_expr(object, ctx, locals);
            rewrite_statements(body, ctx, locals);
        }
        StatementKind::Expression(expr) => {
            rewrite_expr(expr, ctx, locals);
        }
        StatementKind::Return(expr) => {
            rewrite_expr(expr, ctx, locals);
        }
        StatementKind::NestedFunction(fdef) => {
            locals.insert(fdef.name.clone());
            rewrite_function_body(
                fdef,
                ctx.rename_map,
                ctx.module_aliases,
                ctx.all_module_aliases,
                ctx.intrinsic_names,
                ctx.intrinsic_modules,
                ctx.file_id,
            );
        }
        StatementKind::Const(c) => {
            // A local const is block-scoped; treat its name as a local so later
            // references aren't rewritten to a module-mangled name.
            locals.insert(c.name.clone());
            if let Some(t) = &mut c.ty {
                *t = rewrite_type(t, ctx.rename_map, ctx.module_aliases, ctx.type_params);
            }
            rewrite_expr(&mut c.value, ctx, locals);
            c.span.file_id = ctx.file_id;
        }
        StatementKind::Break(value) => {
            if let Some(v) = value {
                rewrite_expr(v, ctx, locals);
            }
        }
        StatementKind::Continue => {}
    }
}

fn rewrite_expr(expr: &mut Expr, ctx: &RewriteCtx, locals: &HashSet<String>) {
    expr.span.file_id = ctx.file_id;
    match &mut expr.kind {
        ExprKind::Identifier(name) => {
            // A non-local name is a reference to a top-level function / const /
            // static — resolve it to its provenance `DefId`.
            if !locals.contains(name.as_str())
                && let Some(defid) = ctx.rename_map.get(name.as_str())
            {
                expr.kind = ExprKind::GlobalRef(defid.clone());
            }
        }
        ExprKind::GlobalRef(_) => {}
        ExprKind::IntegerLiteral(_, _)
        | ExprKind::FloatLiteral(_, _)
        | ExprKind::BooleanLiteral(_) => {}
        ExprKind::FieldAccess { object, .. } => {
            rewrite_expr(object, ctx, locals);
        }
        ExprKind::Deref(inner)
        | ExprKind::Reference(inner)
        | ExprKind::Unique(inner)
        | ExprKind::Not(inner) => {
            rewrite_expr(inner, ctx, locals);
        }
        ExprKind::NullLiteral(ty) => {
            *ty = rewrite_type(ty, ctx.rename_map, ctx.module_aliases, ctx.type_params);
        }
        ExprKind::Call {
            function,
            type_args,
            arguments,
            kwargs,
        } => {
            // Case 1: direct name import — func(x) where func is in intrinsic_names
            if let ExprKind::Identifier(name) = &function.kind
                && ctx.intrinsic_names.contains(name.as_str())
            {
                let intrinsic = Intrinsic::from_name(name).unwrap();
                let mut arguments = std::mem::take(arguments);
                for arg in &mut arguments {
                    rewrite_expr(arg, ctx, locals);
                }
                expr.kind = ExprKind::IntrinsicCall {
                    intrinsic,
                    arguments,
                };
                return;
            }
            // Case 2: module-qualified — intrinsics::func(x)
            if let ExprKind::EnumVariant {
                module_path,
                enum_name,
                variant_name,
                ..
            } = &function.kind
                && module_path.is_empty()
                && ctx.intrinsic_modules.contains(enum_name.name.as_str())
            {
                let intrinsic = Intrinsic::from_name(variant_name)
                    .unwrap_or_else(|| panic!("unknown intrinsic: {variant_name}"));
                let mut arguments = std::mem::take(arguments);
                for arg in &mut arguments {
                    rewrite_expr(arg, ctx, locals);
                }
                expr.kind = ExprKind::IntrinsicCall {
                    intrinsic,
                    arguments,
                };
                return;
            }

            rewrite_expr(function, ctx, locals);
            for ta in type_args {
                *ta = rewrite_type(ta, ctx.rename_map, ctx.module_aliases, ctx.type_params);
            }
            for arg in arguments {
                rewrite_expr(arg, ctx, locals);
            }
            for (_, value) in kwargs {
                rewrite_expr(value, ctx, locals);
            }
        }
        ExprKind::StructLiteral {
            module,
            name,
            type_args,
            fields,
        } => {
            resolve_qualified_def(name, module.take(), ctx.rename_map, ctx.module_aliases);
            for ta in type_args {
                *ta = rewrite_type(ta, ctx.rename_map, ctx.module_aliases, ctx.type_params);
            }
            for field in fields {
                rewrite_expr(&mut field.value, ctx, locals);
            }
        }
        ExprKind::Index { object, index } => {
            rewrite_expr(object, ctx, locals);
            rewrite_expr(index, ctx, locals);
        }
        ExprKind::Slice { object, start, end } => {
            rewrite_expr(object, ctx, locals);
            rewrite_expr(start, ctx, locals);
            rewrite_expr(end, ctx, locals);
        }
        ExprKind::ArrayLiteral(elements, elem_ty) => {
            for e in elements {
                rewrite_expr(e, ctx, locals);
            }
            if let Some(ty) = elem_ty {
                *ty = rewrite_type(ty, ctx.rename_map, ctx.module_aliases, ctx.type_params);
            }
        }
        ExprKind::TupleLiteral(elements) => {
            for e in elements {
                rewrite_expr(e, ctx, locals);
            }
        }
        ExprKind::ArrayRepeat { element, count } => {
            rewrite_expr(element, ctx, locals);
            rewrite_expr(count, ctx, locals);
        }
        ExprKind::BinaryOp { left, right, .. } => {
            rewrite_expr(left, ctx, locals);
            rewrite_expr(right, ctx, locals);
        }
        ExprKind::If {
            condition,
            then_body,
            else_body,
        } => {
            rewrite_expr(condition, ctx, locals);
            let mut locals_copy = locals.clone();
            rewrite_statements(then_body, ctx, &mut locals_copy);
            let mut locals_copy = locals.clone();
            rewrite_statements(else_body, ctx, &mut locals_copy);
        }
        ExprKind::Block(stmts) => {
            let mut locals_copy = locals.clone();
            rewrite_statements(stmts, ctx, &mut locals_copy);
        }
        ExprKind::Loop(stmts) => {
            let mut locals_copy = locals.clone();
            rewrite_statements(stmts, ctx, &mut locals_copy);
        }
        ExprKind::Closure {
            parameters,
            return_type,
            body,
        } => {
            let mut closure_locals = locals.clone();
            for p in parameters.iter_mut() {
                collect_pattern_names(&p.pattern, &mut closure_locals);
                p.ty = rewrite_type(&p.ty, ctx.rename_map, ctx.module_aliases, ctx.type_params);
            }
            if let Some(rt) = return_type {
                *rt = rewrite_type(rt, ctx.rename_map, ctx.module_aliases, ctx.type_params);
            }
            rewrite_statements(body, ctx, &mut closure_locals);
        }
        ExprKind::EnumVariant {
            module_path,
            enum_name,
            type_args,
            variant_name,
        } => {
            if !module_path.is_empty() {
                if let Some(fid) = resolve_module_chain_full(
                    module_path,
                    ctx.module_aliases,
                    ctx.all_module_aliases,
                ) {
                    *enum_name = DefId::new(fid, &enum_name.name);
                }
                module_path.clear();
            } else if ctx.intrinsic_modules.contains(enum_name.name.as_str()) {
                return;
            } else if let Some(fid) = ctx.module_aliases.get(enum_name.name.as_str()).cloned() {
                // `alias::Item` where `alias` is a module — a reference to a
                // top-level function / const in that module.
                expr.kind = ExprKind::GlobalRef(DefId::new(fid, variant_name.clone()));
                return;
            } else if let Some(defid) = ctx.rename_map.get(enum_name.name.as_str()) {
                *enum_name = defid.clone();
            }
            for ta in type_args {
                *ta = rewrite_type(ta, ctx.rename_map, ctx.module_aliases, ctx.type_params);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            rewrite_expr(scrutinee, ctx, locals);
            for arm in arms {
                rewrite_pattern(
                    &mut arm.pattern,
                    ctx.rename_map,
                    ctx.module_aliases,
                    ctx.all_module_aliases,
                    ctx.type_params,
                );
                // The arm's pattern binding is a local in the arm body — it must
                // shadow any same-named top-level def so its references aren't
                // rewritten to that def's mangled name.
                let mut arm_locals = locals.clone();
                match &arm.pattern {
                    Pattern::Variant {
                        binding: Some(b), ..
                    } => {
                        arm_locals.insert(b.clone());
                    }
                    Pattern::Wildcard(name) => {
                        arm_locals.insert(name.clone());
                    }
                    _ => {}
                }
                rewrite_expr(&mut arm.body, ctx, &arm_locals);
            }
        }
        ExprKind::MatchReflect { ty, arms } => {
            *ty = rewrite_type(ty, ctx.rename_map, ctx.module_aliases, ctx.type_params);
            for arm in arms {
                rewrite_expr(&mut arm.body, ctx, locals);
            }
        }
        ExprKind::MethodCall {
            receiver,
            type_args,
            arguments,
            kwargs,
            ..
        } => {
            rewrite_expr(receiver, ctx, locals);
            for ta in type_args {
                *ta = rewrite_type(ta, ctx.rename_map, ctx.module_aliases, ctx.type_params);
            }
            for arg in arguments {
                rewrite_expr(arg, ctx, locals);
            }
            for (_, value) in kwargs {
                rewrite_expr(value, ctx, locals);
            }
        }
        ExprKind::IntrinsicCall { arguments, .. } => {
            for arg in arguments {
                rewrite_expr(arg, ctx, locals);
            }
        }
    }
}

/// Follow a module-path chain (`a::b::c`) through the alias maps to the final
/// module's defining file id.
fn resolve_module_chain_full(
    segments: &[String],
    current_aliases: &ModuleAliasMap,
    all_module_aliases: &AllModuleAliases,
) -> Option<FileId> {
    if segments.is_empty() {
        return None;
    }
    let mut current_fid = *current_aliases.get(&segments[0])?;
    for seg in &segments[1..] {
        current_fid = *all_module_aliases.get(&current_fid)?.get(seg)?;
    }
    Some(current_fid)
}

/// Walk a chain of module aliases to resolve a multi-segment module path.
fn rewrite_pattern(
    pat: &mut Pattern,
    rename_map: &HashMap<String, DefId>,
    module_aliases: &ModuleAliasMap,
    all_module_aliases: &AllModuleAliases,
    type_params: &[String],
) {
    match pat {
        Pattern::Variant {
            module_path,
            enum_name,
            type_args,
            ..
        } => {
            if !module_path.is_empty() {
                if let Some(fid) =
                    resolve_module_chain_full(module_path, module_aliases, all_module_aliases)
                {
                    *enum_name = DefId::new(fid, &enum_name.name);
                }
                module_path.clear();
            } else if let Some(defid) = rename_map.get(enum_name.name.as_str()) {
                *enum_name = defid.clone();
            }
            for ta in type_args {
                *ta = rewrite_type(ta, rename_map, module_aliases, type_params);
            }
        }
        Pattern::Wildcard(_) => {}
    }
}

/// Resolve a Solar program starting from the given file path.
/// Returns a unified AST (with stdlib and numeric constructors) and a SourceMap.
pub fn resolve(
    file_path: &Path,
) -> Result<(SourceFile, SourceMap), (Vec<CompileError>, SourceMap)> {
    let mut resolver = Resolver::new();
    // Run resolution to completion, then hand back the source map regardless of
    // outcome so errors (including those whose spans point into stdlib files)
    // can be rendered against the correct file via the SourceMap.
    let result = resolve_impl(&mut resolver, file_path);
    let source_map = resolver.source_map;
    match result {
        Ok(all_items) => Ok((SourceFile { items: all_items }, source_map)),
        Err(errors) => Err((errors, source_map)),
    }
}

fn resolve_impl(
    resolver: &mut Resolver,
    file_path: &Path,
) -> Result<Vec<TopLevelItem>, Vec<CompileError>> {
    // Parse stdlib first (all files get implicit `import * from "@std"`)
    let std_lib = Path::new(STDLIB_DIR).join("lib.solar");
    let std_root_id = resolver.parse_file(&std_lib)?;
    resolver.parse_imports(std_root_id)?;
    resolver.std_root_id = Some(std_root_id);
    let std_file_count = resolver.files.len();

    // Parse root file
    let root_id = resolver.parse_file(file_path)?;
    // Record the root: its definitions render to bare (un-module-mangled) names
    // in `mangled_ast` (so `main` stays `main`).
    resolver.source_map.set_root_file_id(root_id);

    // Recursively parse imported files
    resolver.parse_imports(root_id)?;

    // Inject synthetic `import * from "@std"` into all non-stdlib files
    for i in std_file_count..resolver.files.len() {
        resolver.files[i].ast.items.insert(
            0,
            TopLevelItem::Import(ImportDef {
                kind: ImportKind::Wildcard,
                path: "@std".to_string(),
                is_pub: false,
                span: SourceSpan::default(),
            }),
        );
    }

    // Check for circular pub import re-export chains
    check_circular_reexports(resolver)?;

    // Build global module alias map (needed for multi-segment module paths)
    let file_count = resolver.files.len();
    let all_module_aliases: AllModuleAliases = (0..file_count)
        .map(|i| {
            let fid = i as FileId;
            (fid, resolver.compute_module_aliases(fid, root_id))
        })
        .collect();

    // Resolve and rewrite each file
    let mut all_items = Vec::new();

    // Process each file (root first, then imported files in order)
    let mut processed: HashSet<FileId> = HashSet::new();

    for i in 0..file_count {
        let fid = i as FileId;
        if processed.contains(&fid) {
            continue;
        }
        processed.insert(fid);
        let items = resolver.resolve_file(fid, root_id, &all_module_aliases)?;
        all_items.extend(items);
    }

    // Generate numeric constructors
    parser::generate_numeric_constructors(&mut all_items);

    Ok(all_items)
}

/// Check for circular pub import re-export chains.
fn check_circular_reexports(resolver: &Resolver) -> Result<(), Vec<CompileError>> {
    // Build a directed graph of pub import edges
    let mut pub_import_edges: HashMap<FileId, Vec<(FileId, SourceSpan)>> = HashMap::new();

    for file in &resolver.files {
        let base_dir = file.path.parent().unwrap_or(Path::new(".")).to_path_buf();

        for item in &file.ast.items {
            if let TopLevelItem::Import(imp) = item
                && imp.is_pub
                && imp.path != "@intrinsics"
            {
                let resolved_path = base_dir.join(&imp.path);
                let canonical = resolved_path
                    .canonicalize()
                    .unwrap_or_else(|_| resolved_path.clone());
                if let Some(&target_id) = resolver.path_to_id.get(&canonical) {
                    pub_import_edges
                        .entry(file.file_id)
                        .or_default()
                        .push((target_id, imp.span));
                }
            }
        }
    }

    // DFS cycle detection
    let mut visited: HashSet<FileId> = HashSet::new();
    let mut in_stack: HashSet<FileId> = HashSet::new();

    for &fid in resolver.path_to_id.values() {
        if !visited.contains(&fid)
            && let Some(span) = dfs_cycle(&pub_import_edges, fid, &mut visited, &mut in_stack)
        {
            return Err(vec![CompileError::new(
                "circular pub import re-export chain detected".to_string(),
                span,
            )]);
        }
    }

    Ok(())
}

fn dfs_cycle(
    edges: &HashMap<FileId, Vec<(FileId, SourceSpan)>>,
    node: FileId,
    visited: &mut HashSet<FileId>,
    in_stack: &mut HashSet<FileId>,
) -> Option<SourceSpan> {
    visited.insert(node);
    in_stack.insert(node);

    if let Some(neighbors) = edges.get(&node) {
        for &(target, span) in neighbors {
            if in_stack.contains(&target) {
                return Some(span);
            }
            if !visited.contains(&target)
                && let Some(span) = dfs_cycle(edges, target, visited, in_stack)
            {
                return Some(span);
            }
        }
    }

    in_stack.remove(&node);
    None
}
