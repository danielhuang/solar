use crate::ast::*;
use crate::error::{CompileError, SourceMap};
use crate::parser;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

const STDLIB_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/std");

type FileId = u32;
type ModuleAliasMap = HashMap<String, (FileId, String)>;
type AllModuleAliases = HashMap<FileId, ModuleAliasMap>;

struct ParsedFile {
    file_id: FileId,
    path: PathBuf,
    ast: SourceFile,
}

#[derive(Debug, Clone)]
enum ExportKind {
    Struct,
    Enum,
    Function,
    Method,
    TypeAlias,
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
                _ => {}
            }
        }
        exports
    }

    /// Derive a module prefix from a file path stem.
    fn module_prefix(path: &Path) -> String {
        let stem = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let sanitized: String = stem
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        format!("__mod_{sanitized}__")
    }

    /// Compute module aliases for a file (module imports only).
    fn compute_module_aliases(
        &self,
        file_id: FileId,
        root_id: FileId,
    ) -> HashMap<String, (FileId, String)> {
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
                        let source_prefix = self.file_prefix(std_id, root_id);
                        aliases.insert(alias.clone(), (std_id, source_prefix));
                    }
                } else {
                    let resolved_path = base_dir.join(&imp.path);
                    let canonical = resolved_path
                        .canonicalize()
                        .unwrap_or_else(|_| resolved_path.clone());
                    if let Some(&source_file_id) = self.path_to_id.get(&canonical) {
                        let source_prefix = self.file_prefix(source_file_id, root_id);
                        aliases.insert(alias.clone(), (source_file_id, source_prefix));
                    }
                }
            }
        }
        aliases
    }

    fn file_prefix(&self, file_id: FileId, root_id: FileId) -> String {
        if file_id == root_id {
            String::new()
        } else {
            Self::module_prefix(&self.files[file_id as usize].path)
        }
    }

    /// Build rename map for a file and rewrite its AST.
    fn resolve_file(
        &self,
        file_id: FileId,
        root_id: FileId,
        all_module_aliases: &HashMap<FileId, HashMap<String, (FileId, String)>>,
    ) -> Result<Vec<TopLevelItem>, Vec<CompileError>> {
        let file = &self.files[file_id as usize];
        let prefix = self.file_prefix(file_id, root_id);

        // Build rename map: original_name -> mangled_name
        let mut rename_map: HashMap<String, String> = HashMap::new();
        // Module aliases: alias -> (file_id, prefix)
        let mut module_aliases: HashMap<String, (FileId, String)> = HashMap::new();

        // Collect this file's own definitions
        let mut local_defs: HashSet<String> = HashSet::new();
        for item in &file.ast.items {
            match item {
                TopLevelItem::Struct(s) => {
                    local_defs.insert(s.name.clone());
                    if !prefix.is_empty() {
                        rename_map.insert(s.name.clone(), format!("{prefix}{}", s.name));
                    }
                }
                TopLevelItem::Enum(e) => {
                    local_defs.insert(e.name.clone());
                    if !prefix.is_empty() {
                        rename_map.insert(e.name.clone(), format!("{prefix}{}", e.name));
                    }
                }
                TopLevelItem::Function(f) => {
                    local_defs.insert(f.name.clone());
                    if !prefix.is_empty() {
                        rename_map.insert(f.name.clone(), format!("{prefix}{}", f.name));
                    }
                }
                TopLevelItem::Method(m) => {
                    local_defs.insert(m.name.clone());
                    // Methods get renamed via self-type mangling in typed_ast,
                    // but the name itself needs prefixing for the resolve stage
                    if !prefix.is_empty() {
                        rename_map.insert(m.name.clone(), format!("{prefix}{}", m.name));
                    }
                }
                TopLevelItem::TypeAlias(ta) => {
                    local_defs.insert(ta.name.clone());
                    if !prefix.is_empty() {
                        rename_map.insert(ta.name.clone(), format!("{prefix}{}", ta.name));
                    }
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
                let source_prefix = self.file_prefix(source_file_id, root_id);
                let exports = self.collect_exports(source_file_id);

                match &imp.kind {
                    ImportKind::Named(names) => {
                        for name in names {
                            let local = name.local_name().to_string();
                            if name.is_path() {
                                // Path import: resolve module chain starting from source file
                                let mod_segs = name.module_segments();
                                let source_aliases = all_module_aliases.get(&source_file_id);
                                let empty_aliases: HashMap<String, (FileId, String)> =
                                    HashMap::new();
                                let aliases = source_aliases.unwrap_or(&empty_aliases);

                                if let Some((final_fid, final_prefix)) =
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
                                    rename_map.insert(
                                        local,
                                        format!("{final_prefix}{}", name.local_name()),
                                    );
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
                                if local_defs.contains(&local) && !rename_map.contains_key(&local) {
                                    return Err(vec![CompileError::new(
                                        format!("import `{local}` conflicts with local definition"),
                                        imp.span,
                                    )]);
                                }
                                rename_map.insert(local.clone(), format!("{source_prefix}{local}"));
                            }
                        }
                    }
                    ImportKind::Module(alias) => {
                        module_aliases
                            .insert(alias.clone(), (source_file_id, source_prefix.clone()));
                    }
                    ImportKind::Wildcard => {
                        for name in exports.keys() {
                            if local_defs.contains(name) && !rename_map.contains_key(name) {
                                return Err(vec![CompileError::new(
                                    format!(
                                        "wildcard import of `{name}` from `{}` conflicts with local definition",
                                        imp.path
                                    ),
                                    imp.span,
                                )]);
                            }
                            rename_map.insert(name.clone(), format!("{source_prefix}{name}"));
                        }
                        // Propagate pub module re-exports
                        if let Some(source_aliases) = all_module_aliases.get(&source_file_id) {
                            for src_item in &self.files[source_file_id as usize].ast.items {
                                if let TopLevelItem::Import(src_imp) = src_item
                                    && src_imp.is_pub
                                    && src_imp.path != "@intrinsics"
                                    && let ImportKind::Module(alias) = &src_imp.kind
                                    && let Some((fid, pfx)) = source_aliases.get(alias)
                                {
                                    module_aliases.insert(alias.clone(), (*fid, pfx.clone()));
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
                    s.name = rename_map.get(&s.name).cloned().unwrap_or(s.name.clone());
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
                    e.name = rename_map.get(&e.name).cloned().unwrap_or(e.name.clone());
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
                    f.name = rename_map.get(&f.name).cloned().unwrap_or(f.name.clone());
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
                    ta.name = rename_map.get(&ta.name).cloned().unwrap_or(ta.name.clone());
                    ta.target_type = rewrite_type(
                        &ta.target_type,
                        &rename_map,
                        &module_aliases,
                        &ta.type_params,
                    );
                    set_file_id_span(&mut ta.span, file_id);
                    rewritten.push(TopLevelItem::TypeAlias(ta));
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

/// Rewrite a type, replacing names via the rename map and resolving module-qualified types.
fn rewrite_type(
    ty: &Type,
    rename_map: &HashMap<String, String>,
    module_aliases: &ModuleAliasMap,
    type_params: &[String],
) -> Type {
    match ty {
        Type::Named(name) => {
            // Check for module-qualified type: "module::Name"
            if let Some((module, local_name)) = name.split_once("::") {
                if let Some((_fid, prefix)) = module_aliases.get(module) {
                    Type::Named(format!("{prefix}{local_name}"))
                } else {
                    // Not a known module — leave as-is (might be an error caught later)
                    ty.clone()
                }
            } else if type_params.contains(name) {
                // Don't rename type parameters
                ty.clone()
            } else if let Some(mangled) = rename_map.get(name) {
                Type::Named(mangled.clone())
            } else {
                ty.clone()
            }
        }
        Type::Generic { name, type_args } => {
            let rewritten_args: Vec<Type> = type_args
                .iter()
                .map(|t| rewrite_type(t, rename_map, module_aliases, type_params))
                .collect();
            // Check for module-qualified generic: "module::Name"
            if let Some((module, local_name)) = name.split_once("::") {
                if let Some((_fid, prefix)) = module_aliases.get(module) {
                    Type::Generic {
                        name: format!("{prefix}{local_name}"),
                        type_args: rewritten_args,
                    }
                } else {
                    Type::Generic {
                        name: name.clone(),
                        type_args: rewritten_args,
                    }
                }
            } else if type_params.contains(name) {
                Type::Generic {
                    name: name.clone(),
                    type_args: rewritten_args,
                }
            } else {
                let new_name = rename_map.get(name).cloned().unwrap_or(name.clone());
                Type::Generic {
                    name: new_name,
                    type_args: rewritten_args,
                }
            }
        }
        Type::Reference(inner) => Type::Reference(Box::new(rewrite_type(
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
    rename_map: &'a HashMap<String, String>,
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
    rename_map: &HashMap<String, String>,
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
    rename_map: &HashMap<String, String>,
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
            if let Some(m) = module.take() {
                if let Some((_fid, prefix)) = module_aliases.get(&m) {
                    *name = format!("{prefix}{name}");
                }
            } else if let Some(mangled) = rename_map.get(name.as_str()) {
                *name = mangled.clone();
            }
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
    }
}

fn rewrite_expr(expr: &mut Expr, ctx: &RewriteCtx, locals: &HashSet<String>) {
    expr.span.file_id = ctx.file_id;
    match &mut expr.kind {
        ExprKind::Identifier(name) => {
            if !locals.contains(name.as_str())
                && let Some(mangled) = ctx.rename_map.get(name.as_str())
            {
                *name = mangled.clone();
            }
        }
        ExprKind::IntegerLiteral(_, _) | ExprKind::BooleanLiteral(_) => {}
        ExprKind::FieldAccess { object, .. } => {
            rewrite_expr(object, ctx, locals);
        }
        ExprKind::Deref(inner) | ExprKind::Reference(inner) | ExprKind::Unique(inner) => {
            rewrite_expr(inner, ctx, locals);
        }
        ExprKind::Call {
            function,
            type_args,
            arguments,
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
                && ctx.intrinsic_modules.contains(enum_name.as_str())
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
        }
        ExprKind::StructLiteral {
            module,
            name,
            type_args,
            fields,
        } => {
            if let Some(m) = module.take() {
                if let Some((_fid, prefix)) = ctx.module_aliases.get(&m) {
                    *name = format!("{prefix}{name}");
                }
            } else if let Some(mangled) = ctx.rename_map.get(name.as_str()) {
                *name = mangled.clone();
            }
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
        ExprKind::ArrayLiteral(elements) | ExprKind::TupleLiteral(elements) => {
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
                if let Some(prefix) =
                    resolve_module_chain(module_path, ctx.module_aliases, ctx.all_module_aliases)
                {
                    *enum_name = format!("{prefix}{enum_name}");
                }
                module_path.clear();
            } else if ctx.intrinsic_modules.contains(enum_name.as_str()) {
                return;
            } else if ctx.module_aliases.contains_key(enum_name.as_str()) {
                let (_fid, prefix) = &ctx.module_aliases[enum_name.as_str()];
                let mangled = format!("{prefix}{variant_name}");
                expr.kind = ExprKind::Identifier(mangled);
                return;
            } else if let Some(mangled) = ctx.rename_map.get(enum_name.as_str()) {
                *enum_name = mangled.clone();
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
                rewrite_expr(&mut arm.body, ctx, locals);
            }
        }
        ExprKind::MethodCall {
            receiver,
            type_args,
            arguments,
            ..
        } => {
            rewrite_expr(receiver, ctx, locals);
            for ta in type_args {
                *ta = rewrite_type(ta, ctx.rename_map, ctx.module_aliases, ctx.type_params);
            }
            for arg in arguments {
                rewrite_expr(arg, ctx, locals);
            }
        }
        ExprKind::IntrinsicCall { arguments, .. } => {
            for arg in arguments {
                rewrite_expr(arg, ctx, locals);
            }
        }
    }
}

/// Like `resolve_module_chain` but returns `(FileId, prefix)` instead of just the prefix.
fn resolve_module_chain_full(
    segments: &[String],
    current_aliases: &ModuleAliasMap,
    all_module_aliases: &AllModuleAliases,
) -> Option<(FileId, String)> {
    if segments.is_empty() {
        return None;
    }
    let (mut current_fid, mut current_prefix) = current_aliases.get(&segments[0])?.clone();
    for seg in &segments[1..] {
        let aliases = all_module_aliases.get(&current_fid)?;
        let (fid, prefix) = aliases.get(seg)?.clone();
        current_fid = fid;
        current_prefix = prefix;
    }
    Some((current_fid, current_prefix))
}

/// Walk a chain of module aliases to resolve a multi-segment module path.
/// E.g., for `d::c::b::a::Enum::Variant`, module_path = ["d", "c", "b", "a"].
/// Returns the prefix for the final module in the chain.
fn resolve_module_chain(
    segments: &[String],
    current_aliases: &ModuleAliasMap,
    all_module_aliases: &AllModuleAliases,
) -> Option<String> {
    if segments.is_empty() {
        return None;
    }
    // Look up the first segment in the current file's module aliases
    let (mut current_fid, mut current_prefix) = current_aliases.get(&segments[0])?.clone();

    // Follow the chain through each subsequent segment
    for seg in &segments[1..] {
        let aliases = all_module_aliases.get(&current_fid)?;
        let (fid, prefix) = aliases.get(seg)?.clone();
        current_fid = fid;
        current_prefix = prefix;
    }

    Some(current_prefix)
}

fn rewrite_pattern(
    pat: &mut Pattern,
    rename_map: &HashMap<String, String>,
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
                if let Some(prefix) =
                    resolve_module_chain(module_path, module_aliases, all_module_aliases)
                {
                    *enum_name = format!("{prefix}{enum_name}");
                }
                module_path.clear();
            } else if let Some(mangled) = rename_map.get(enum_name.as_str()) {
                *enum_name = mangled.clone();
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
pub fn resolve(file_path: &Path) -> Result<(SourceFile, SourceMap), Vec<CompileError>> {
    let mut resolver = Resolver::new();

    // Parse stdlib first (all files get implicit `import * from "@std"`)
    let std_lib = Path::new(STDLIB_DIR).join("lib.solar");
    let std_root_id = resolver.parse_file(&std_lib)?;
    resolver.parse_imports(std_root_id)?;
    resolver.std_root_id = Some(std_root_id);
    let std_file_count = resolver.files.len();

    // Parse root file
    let root_id = resolver.parse_file(file_path)?;

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
    check_circular_reexports(&resolver)?;

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

    let source_map = resolver.source_map;

    Ok((SourceFile { items: all_items }, source_map))
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
