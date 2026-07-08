use crate::ast::BinOp;
use crate::ast::Intrinsic;
use crate::error::SourceMap;
use crate::ir::*;
use std::collections::{HashMap, HashSet};

/// Upper size bound for placing a non-escaping local/param on the C stack. Above
/// this it stays a heap box, so a large non-escaping value in deep recursion
/// can't overflow the stack where a heap allocation would not.
const STACK_ALLOC_MAX: usize = 4096;

/// A by-value C typedef spec: (size, align, pointer-word runs).
type ValTypeSpec = (usize, usize, Vec<(usize, usize)>);

pub fn generate(module: &Module, source_file: &str, source_map: &SourceMap) -> String {
    let mut cg = Codegen {
        module,
        out: String::new(),
        indent: 0,
        tmp_counter: 0,
        source_file: source_file.to_string(),
        source_map,
        emitted_mark_fns: HashSet::new(),
        static_root_count: 0,
        loop_dst: Vec::new(),
        cur_loc: None,
    };
    cg.emit_module();
    cg.out
}

struct Codegen<'a> {
    module: &'a Module,
    out: String,
    indent: usize,
    tmp_counter: usize,
    /// Fallback path for `#line` directives when a span's file isn't in the map.
    source_file: String,
    /// Maps a span's `file_id` to its source path, so `#line` points at the
    /// actual file (e.g. a `@std` file) rather than always the main file.
    source_map: &'a SourceMap,
    emitted_mark_fns: HashSet<String>,
    /// Number of GC-relevant static slots in the emitted `_sol_statics` root
    /// table (0 = no table emitted; `main` passes null to `sol_start`).
    static_root_count: usize,
    /// C lvalue strings for the enclosing loop expressions' result destinations;
    /// `break <v>` assigns into the innermost one.
    loop_dst: Vec<String>,
    /// Source location (`#line` value, file) to stamp on every emitted code line.
    /// A statement expands to many C lines, but a `#line N` directive only sets the
    /// *next* line — the C preprocessor auto-increments after that, so without
    /// re-asserting it each line a statement's instructions drift onto later source
    /// lines (closing braces, blanks, the next function). Re-stamping every line via
    /// `line()` pins them all to the statement's own line. `None` = synthetic glue,
    /// emitted with no directive.
    cur_loc: Option<(usize, String)>,
}

impl<'a> Codegen<'a> {
    /// Emit a physical line verbatim (indented), with no `#line` directive.
    fn raw_line(&mut self, s: &str) {
        for _ in 0..self.indent {
            self.out.push_str("    ");
        }
        self.out.push_str(s);
        self.out.push('\n');
    }

    /// Emit a code line, re-asserting the current `#line` location before it so the
    /// preprocessor's per-line auto-increment can't drift the attribution off the
    /// statement's own source line (see `cur_loc`).
    fn line(&mut self, s: &str) {
        if let Some((n, f)) = self.cur_loc.clone() {
            self.raw_line(&format!("#line {n} \"{f}\""));
        }
        self.raw_line(s);
    }

    fn linef(&mut self, s: String) {
        self.line(&s);
    }

    /// Emit a heap allocation: a `sol_alloc` call followed by an explicit
    /// `memset` that zeroes it, as two separate statements. `sol_alloc` no longer
    /// zeroes; emitting the zeroing as its own store lets LLVM dead-store-
    /// eliminate it wherever the caller fully overwrites the object before it
    /// escapes, while preserving it for any field left unwritten (so the GC never
    /// traces an uninitialized pointer field).
    fn emit_alloc(
        &mut self,
        var: impl std::fmt::Display,
        size: impl std::fmt::Display,
        align: impl std::fmt::Display,
        mark_fn: impl std::fmt::Display,
    ) {
        let (var, size) = (var.to_string(), size.to_string());
        self.linef(format!(
            "uint8_t* {var} = sol_alloc({size}, {align}, {mark_fn});"
        ));
        self.linef(format!("memset({var}, 0, {size});"));
    }

    fn fresh_tmp(&mut self) -> String {
        let n = self.tmp_counter;
        self.tmp_counter += 1;
        format!("_t{n}")
    }

    fn type_size(&self, ty: &Type) -> usize {
        type_size(ty, &self.module.datatypes)
    }

    fn type_align(&self, ty: &Type) -> usize {
        type_align(ty, &self.module.datatypes)
    }

    fn type_contains_unique(&self, ty: &Type) -> bool {
        type_contains_unique(ty, &self.module.datatypes)
    }

    fn is_sized(&self, ty: &Type) -> bool {
        is_sized(ty, &self.module.datatypes)
    }

    fn type_contains_enum(&self, ty: &Type) -> bool {
        type_contains_enum(ty, &self.module.datatypes)
    }

    fn type_contains_gc_ptr(&self, ty: &Type) -> bool {
        type_contains_gc_ptr(ty, &self.module.datatypes)
    }

    /// Returns the C expression for the mark_fn to pass to sol_alloc for a given
    /// allocation content type.
    fn mark_fn_expr(&self, ty: &Type) -> String {
        if !self.type_contains_gc_ptr(ty) {
            return "_mark_noop".to_string();
        }
        match ty {
            // A `FileDesc` is a single pointer (into the fd arena); the marker
            // enqueues its value and `drain` routes it to the fd mark bitmap.
            Type::Ref(_) | Type::NullableRef(_) | Type::Unique(_) | Type::FileDesc => {
                "_mark_single_ptr".to_string()
            }
            Type::RefUnsized(_) | Type::NullableRefUnsized(_) | Type::UniqueUnsized(_) => {
                "_mark_wide_ptr".to_string()
            }
            Type::Function { .. } => "_mark_fn_value".to_string(),
            Type::Struct(name) | Type::Enum(name) => {
                format!("_mark_{}", Self::sanitize_type_name(name))
            }
            Type::FixedArray(inner, _) | Type::Array(inner) => {
                if !self.type_contains_gc_ptr(inner) {
                    "_mark_noop".to_string()
                } else {
                    "_mark_ptr_array".to_string()
                }
            }
            _ => "_mark_noop".to_string(),
        }
    }

    fn sanitize_type_name(name: &str) -> String {
        name.replace("::", "_").replace('#', "_")
    }

    fn emit_mark_functions(&mut self) {
        self.line("// GC mark functions");

        // Built-in mark functions
        self.line("static void _mark_noop(void* ctx, uint8_t* obj, uint64_t size) { (void)ctx; (void)obj; (void)size; }");
        self.line("static void _mark_single_ptr(void* ctx, uint8_t* obj, uint64_t size) { (void)size; sol_gc_mark(ctx, *(uint8_t**)obj); }");
        self.line("static void _mark_wide_ptr(void* ctx, uint8_t* obj, uint64_t size) { (void)size; sol_gc_mark(ctx, *(uint8_t**)obj); }");
        self.line("static void _mark_fn_value(void* ctx, uint8_t* obj, uint64_t size) { (void)size; sol_gc_mark(ctx, *(uint8_t**)(obj + 8)); }");
        self.line("static void _mark_ptr_array(void* ctx, uint8_t* obj, uint64_t size) { for (uint64_t _i = 0; _i < size; _i += 8) sol_gc_mark(ctx, *(uint8_t**)(obj + _i)); }");
        self.line("");

        // Collect datatype names to process (avoid borrowing issues)
        let dt_names: Vec<String> = self.module.datatypes.keys().cloned().collect();

        for name in &dt_names {
            let dt = &self.module.datatypes[name.as_str()];
            // Collect field info to avoid borrow conflicts
            let fields: Vec<(String, Type, usize, usize)> = dt
                .fields
                .iter()
                .map(|f| (f.name.clone(), f.ty.clone(), f.offset, f.size))
                .collect();
            let has_gc_ptr = fields
                .iter()
                .any(|(_, ty, _, _)| type_contains_gc_ptr(ty, &self.module.datatypes));

            if !has_gc_ptr {
                continue;
            }

            let mark_name = format!("_mark_{}", Self::sanitize_type_name(name));
            if self.emitted_mark_fns.contains(&mark_name) {
                continue;
            }
            self.emitted_mark_fns.insert(mark_name.clone());

            // Forward-declare for recursive types
            self.linef(format!(
                "static void {mark_name}(void* ctx, uint8_t* obj, uint64_t size);"
            ));
        }
        self.line("");

        for name in &dt_names {
            let dt = &self.module.datatypes[name.as_str()];
            let fields: Vec<(String, Type, usize, usize)> = dt
                .fields
                .iter()
                .map(|f| (f.name.clone(), f.ty.clone(), f.offset, f.size))
                .collect();
            let variant_map = dt.variant_map.clone();
            let has_gc_ptr = fields
                .iter()
                .any(|(_, ty, _, _)| type_contains_gc_ptr(ty, &self.module.datatypes));

            if !has_gc_ptr {
                continue;
            }

            let mark_name = format!("_mark_{}", Self::sanitize_type_name(name));

            self.linef(format!(
                "static void {mark_name}(void* ctx, uint8_t* obj, uint64_t size) {{"
            ));
            self.indent += 1;
            self.line("(void)size;");

            if let Some(ref vm) = variant_map {
                // Enum: switch on discriminant
                self.line("uint64_t _disc = *(uint64_t*)obj;");
                let mut first = true;
                for (i, vm_entry) in vm.iter().enumerate() {
                    if let Some(field_name) = vm_entry {
                        let field = fields.iter().find(|f| f.0 == *field_name).unwrap();
                        let (_, ref field_ty, offset, field_size) = *field;
                        if !self.type_contains_gc_ptr(field_ty) {
                            continue;
                        }
                        let keyword = if first { "if" } else { "else if" };
                        first = false;
                        self.linef(format!("{keyword} (_disc == {i}) {{"));
                        self.indent += 1;
                        self.emit_mark_fields_for_type(field_ty, offset, field_size);
                        self.indent -= 1;
                        self.line("}");
                    }
                }
            } else {
                // Struct: mark all fields with GC pointers
                for (_, field_ty, offset, field_size) in &fields {
                    if !self.type_contains_gc_ptr(field_ty) {
                        continue;
                    }
                    self.emit_mark_fields_for_type(field_ty, *offset, *field_size);
                }
            }

            self.indent -= 1;
            self.line("}");
            self.line("");
        }
    }

    fn emit_mark_fields_for_type(&mut self, ty: &Type, offset: usize, size: usize) {
        let base = if offset == 0 {
            "obj".to_string()
        } else {
            format!("(obj + {offset})")
        };
        self.emit_mark_at(&base, ty, size);
    }

    fn emit_mark_at(&mut self, base: &str, ty: &Type, size: usize) {
        match ty {
            Type::Ref(_) | Type::NullableRef(_) | Type::Unique(_) | Type::FileDesc => {
                self.linef(format!("sol_gc_mark(ctx, *(uint8_t**){base});"));
            }
            Type::RefUnsized(_) | Type::NullableRefUnsized(_) | Type::UniqueUnsized(_) => {
                self.linef(format!("sol_gc_mark(ctx, *(uint8_t**){base});"));
            }
            Type::Function { .. } => {
                self.linef(format!("sol_gc_mark(ctx, *(uint8_t**)({base} + 8));"));
            }
            Type::Struct(name) | Type::Enum(name) => {
                let mark_name = format!("_mark_{}", Self::sanitize_type_name(name));
                self.linef(format!("{mark_name}(ctx, {base}, {size});"));
            }
            Type::FixedArray(inner, count) if self.type_contains_gc_ptr(inner) => {
                let es = self.type_size(inner);
                let count = *count;
                let inner = (**inner).clone();
                self.linef(format!("for (uint64_t _fi = 0; _fi < {count}; _fi++) {{"));
                self.indent += 1;
                let elem_base = format!("({base} + _fi * {es})");
                self.emit_mark_at(&elem_base, &inner, es);
                self.indent -= 1;
                self.line("}");
            }
            Type::Array(inner) if self.type_contains_gc_ptr(inner) => {
                let es = self.type_size(inner);
                let inner = (**inner).clone();
                self.linef(format!(
                    "for (uint64_t _ai = 0; _ai < size / {es}; _ai++) {{"
                ));
                self.indent += 1;
                let elem_base = format!("({base} + _ai * {es})");
                self.emit_mark_at(&elem_base, &inner, es);
                self.indent -= 1;
                self.line("}");
            }
            _ => {}
        }
    }

    fn c_int_type(&self, ty: &Type) -> &'static str {
        match ty {
            Type::Int8 => "int8_t",
            Type::Int16 => "int16_t",
            Type::Int32 => "int32_t",
            Type::Int64 | Type::Int => "int64_t",
            Type::Uint8 => "uint8_t",
            Type::Uint16 => "uint16_t",
            Type::Uint32 => "uint32_t",
            Type::Uint64 | Type::Uint => "uint64_t",
            Type::Float32 => "float",
            Type::Float64 => "double",
            Type::Bool => "uint8_t",
            Type::Ref(_) | Type::NullableRef(_) | Type::Unique(_) | Type::FileDesc => "uint8_t*",
            _ => unreachable!("c_int_type on non-scalar type: {ty}"),
        }
    }

    fn c_atomic_type(&self, size: usize) -> &'static str {
        match size {
            1 => "uint8_t",
            2 => "uint16_t",
            4 => "uint32_t",
            8 => "uint64_t",
            _ => unreachable!("c_atomic_type: unsupported size {size}"),
        }
    }

    /// Byte offsets of the GC-pointer words inside the *value representation*
    /// of `ty` (fat pointers contribute their data-pointer word, not the
    /// meta/len word; function values contribute both the code and env words;
    /// enums contribute the union over all variants' payloads). These offsets
    /// drive both the pointer-typed members of the value typedefs and the
    /// typed member-wise copies in `emit_copy` — every GC-pointer store must
    /// reach LLVM as a `store ptr` so the write-barrier pass can instrument
    /// pointer stores precisely instead of shading every 8-byte store.
    fn collect_ptr_words(
        ty: &Type,
        dt: &std::collections::HashMap<String, DataType>,
        base: usize,
        out: &mut std::collections::BTreeSet<usize>,
    ) {
        match ty {
            Type::Ref(_)
            | Type::NullableRef(_)
            | Type::Unique(_)
            | Type::FileDesc
            | Type::RefUnsized(_)
            | Type::NullableRefUnsized(_)
            | Type::UniqueUnsized(_) => {
                out.insert(base);
            }
            // Owned slice value: (data ptr, len) — only the data word is a pointer.
            Type::Array(_) => {
                out.insert(base);
            }
            // Function value: (code ptr, env ptr). Only env is a GC pointer, but
            // the code word is a real pointer too — typing both keeps provenance.
            Type::Function { .. } => {
                out.insert(base);
                out.insert(base + 8);
            }
            Type::Struct(name) | Type::Enum(name) => {
                let fields: Vec<(Type, usize)> = dt[name.as_str()]
                    .fields
                    .iter()
                    .map(|f| (f.ty.clone(), f.offset))
                    .collect();
                for (fty, off) in &fields {
                    Self::collect_ptr_words(fty, dt, base + off, out);
                }
            }
            Type::FixedArray(inner, count) => {
                let es = type_size(inner, dt);
                let mut elem = std::collections::BTreeSet::new();
                Self::collect_ptr_words(inner, dt, 0, &mut elem);
                if !elem.is_empty() {
                    for i in 0..*count as usize {
                        for &o in &elem {
                            out.insert(base + i * es + o);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Maximal runs of consecutive pointer words: `(start_word, len_words)`.
    fn ptr_runs(&self, ty: &Type) -> Vec<(usize, usize)> {
        let mut set = std::collections::BTreeSet::new();
        Self::collect_ptr_words(ty, &self.module.datatypes, 0, &mut set);
        let mut runs: Vec<(usize, usize)> = Vec::new();
        for off in set {
            debug_assert!(off % 8 == 0, "pointer word at unaligned offset {off}");
            let w = off / 8;
            match runs.last_mut() {
                Some((start, len)) if *start + *len == w => *len += 1,
                _ => runs.push((w, 1)),
            }
        }
        runs
    }

    /// Name for a value-type struct: `_v{size}_{align}` for pointer-free blobs,
    /// with `_p{start}x{len}` per pointer-word run when the type holds GC
    /// pointers (e.g. a splay `Node {key, value, left, right}` is `_v32_8_p1x3`).
    fn val_type_name(size: usize, align: usize, ptr_runs: &[(usize, usize)]) -> String {
        let mut name = format!("_v{size}_{align}");
        for (start, len) in ptr_runs {
            name.push_str(&format!("_p{start}x{len}"));
        }
        name
    }

    /// C value type for `ty`: computes size/align and the pointer-word runs.
    fn val_type(&self, ty: &Type) -> String {
        let s = self.type_size(ty);
        let a = self.type_align(ty);
        Self::val_type_name(s, a, &self.ptr_runs(ty))
    }

    /// Collect all (size, align, ptr-runs) triples used for by-value params and
    /// returns.
    fn collect_val_types_from_type(&self, ty: &Type, set: &mut HashSet<ValTypeSpec>) {
        if let Type::Function {
            params,
            return_type,
        } = ty
        {
            let dt = &self.module.datatypes;
            for p in params {
                set.insert((type_size(p, dt), type_align(p, dt), self.ptr_runs(p)));
                self.collect_val_types_from_type(p, set);
            }
            if !matches!(**return_type, Type::Unit | Type::Never) {
                set.insert((
                    type_size(return_type, dt),
                    type_align(return_type, dt),
                    self.ptr_runs(return_type),
                ));
                self.collect_val_types_from_type(return_type, set);
            }
        }
    }

    fn collect_val_types(&self) -> Vec<ValTypeSpec> {
        let mut set = HashSet::new();
        for func in &self.module.functions {
            if !matches!(func.return_type, Type::Unit | Type::Never) {
                let s = self.type_size(&func.return_type);
                let a = self.type_align(&func.return_type);
                set.insert((s, a, self.ptr_runs(&func.return_type)));
            }
            for param in &func.params {
                let s = self.type_size(&param.ty);
                let a = self.type_align(&param.ty);
                set.insert((s, a, self.ptr_runs(&param.ty)));
                self.collect_val_types_from_type(&param.ty, &mut set);
            }
            self.collect_val_types_from_type(&func.return_type, &mut set);
        }
        let mut v: Vec<_> = set.into_iter().collect();
        v.sort();
        v
    }

    /// Collect all function names referenced by a function's nodes.
    fn function_callees(func: &Function) -> HashSet<String> {
        let mut callees = HashSet::new();
        for node in &func.nodes {
            match &node.kind {
                NodeKind::Call { function, .. }
                | NodeKind::FunctionRef(function)
                | NodeKind::MakeClosure { function, .. } => {
                    callees.insert(function.clone());
                }
                _ => {}
            }
        }
        callees
    }

    /// Return function indices in topological order (callees before callers),
    /// with "main" forced to be last. Uses Tarjan's SCC algorithm so that
    /// mutually recursive functions are grouped together.
    fn topological_function_order(&self) -> Vec<usize> {
        let funcs = &self.module.functions;
        let name_to_idx: HashMap<&str, usize> = funcs
            .iter()
            .enumerate()
            .map(|(i, f)| (f.name.as_str(), i))
            .collect();

        // Build adjacency list: caller -> callees (by index)
        let adj: Vec<Vec<usize>> = funcs
            .iter()
            .map(|f| {
                Self::function_callees(f)
                    .into_iter()
                    .filter_map(|name| name_to_idx.get(name.as_str()).copied())
                    .collect()
            })
            .collect();

        // Tarjan's SCC – produces SCCs in reverse topological order
        let n = funcs.len();
        struct TarjanState {
            index_counter: u32,
            stack: Vec<usize>,
            on_stack: Vec<bool>,
            indices: Vec<u32>,
            lowlinks: Vec<u32>,
            sccs: Vec<Vec<usize>>,
        }

        fn strongconnect(v: usize, adj: &[Vec<usize>], state: &mut TarjanState) {
            state.indices[v] = state.index_counter;
            state.lowlinks[v] = state.index_counter;
            state.index_counter += 1;
            state.stack.push(v);
            state.on_stack[v] = true;

            for &w in &adj[v] {
                if state.indices[w] == u32::MAX {
                    strongconnect(w, adj, state);
                    state.lowlinks[v] = state.lowlinks[v].min(state.lowlinks[w]);
                } else if state.on_stack[w] {
                    state.lowlinks[v] = state.lowlinks[v].min(state.indices[w]);
                }
            }

            if state.lowlinks[v] == state.indices[v] {
                let mut scc = Vec::new();
                loop {
                    let w = state.stack.pop().unwrap();
                    state.on_stack[w] = false;
                    scc.push(w);
                    if w == v {
                        break;
                    }
                }
                state.sccs.push(scc);
            }
        }

        let mut state = TarjanState {
            index_counter: 0,
            stack: Vec::new(),
            on_stack: vec![false; n],
            indices: vec![u32::MAX; n],
            lowlinks: vec![0u32; n],
            sccs: Vec::new(),
        };

        // Start from "main" so it finishes last in the DFS and naturally
        // ends up at the end of the topological order.
        if let Some(main_idx) = name_to_idx.get("main").copied() {
            strongconnect(main_idx, &adj, &mut state);
        }
        for i in 0..n {
            if state.indices[i] == u32::MAX {
                strongconnect(i, &adj, &mut state);
            }
        }

        // Tarjan's produces SCCs in reverse topological order of the call graph,
        // meaning callees come first — exactly what we want.
        state.sccs.into_iter().flatten().collect()
    }

    fn emit_module(&mut self) {
        self.emit_prelude();

        // Emit value-type typedefs. Pointer-free types stay opaque byte blobs;
        // a type with GC pointers gets real `uint8_t*` members at its pointer
        // words (with `char` filler between) so that LLVM sees genuinely
        // pointer-typed values flow through params/returns/copies — the write
        // barrier instruments `store ptr` precisely, instead of conservatively
        // shading every 8-byte store.
        let val_types = self.collect_val_types();
        for (size, align, runs) in &val_types {
            let name = Self::val_type_name(*size, *align, runs);
            if runs.is_empty() {
                self.linef(format!(
                    "typedef struct {{ _Alignas({align}) char _d[{size}]; }} {name};"
                ));
            } else {
                let mut members = String::new();
                let mut off = 0usize;
                let mut gap_n = 0usize;
                for (i, (start, len)) in runs.iter().enumerate() {
                    let run_off = start * 8;
                    if run_off > off {
                        members.push_str(&format!("char _g{gap_n}[{}]; ", run_off - off));
                        gap_n += 1;
                    }
                    members.push_str(&format!("uint8_t* _p{i}[{len}]; "));
                    off = run_off + len * 8;
                }
                if *size > off {
                    members.push_str(&format!("char _g{gap_n}[{}]; ", *size - off));
                }
                self.linef(format!(
                    "typedef struct {{ _Alignas({align}) {members}}} {name};"
                ));
            }
        }
        if !val_types.is_empty() {
            self.line("");
        }

        // Emit GC mark functions
        self.emit_mark_functions();

        // Emit the `static` globals as raw aligned byte slots (zero-initialized;
        // their literal initial values are stored by the assignments IR lowering
        // prepended to `main`). Copies in/out go through the same `emit_copy`
        // machinery as any place, so 16-byte pointer-carrying values (fat
        // pointers, function values) use `sol_copy_128_unordered` — reads and
        // writes of a wide static cannot tear (the slot's `_Alignas` satisfies
        // the i128 atomics' 16-byte alignment requirement, matching the type's
        // Solar alignment). The GC-relevant slots are collected into a root
        // table handed to `sol_start`: the collector runs each entry's mark_fn
        // over the slot at both stop-the-world root scans (statics need no
        // write barrier for the same reason stacks don't — roots are re-scanned
        // at pause 2).
        let statics_meta: Vec<(usize, String)> = self
            .module
            .statics
            .iter()
            .enumerate()
            .map(|(i, st)| (i, self.mark_fn_expr(&st.ty)))
            .collect();
        for (i, st) in self.module.statics.iter().enumerate() {
            let size = self.type_size(&st.ty).max(1);
            let align = self.type_align(&st.ty);
            self.linef(format!(
                "static _Alignas({align}) uint8_t _gs{i}[{size}]; // static {}",
                st.name
            ));
        }
        let gc_statics: Vec<&(usize, String)> = statics_meta
            .iter()
            .filter(|(_, mark)| mark != "_mark_noop")
            .collect();
        if !gc_statics.is_empty() {
            let entries: Vec<String> = gc_statics
                .iter()
                .map(|(i, mark)| {
                    let size = self.type_size(&self.module.statics[*i].ty).max(1);
                    format!("{{ _gs{i}, {size}, {mark} }}")
                })
                .collect();
            self.linef(format!(
                "static const sol_static_entry _sol_statics[] = {{ {} }};",
                entries.join(", ")
            ));
        }
        self.static_root_count = gc_statics.len();
        if !self.module.statics.is_empty() {
            self.line("");
        }

        // Forward-declare user functions
        for func in &self.module.functions {
            let sig = self.func_signature(func);
            let q = Self::func_qualifiers(func);
            self.linef(format!("{q} {sig};"));
        }
        self.line("");

        // Emit user functions in topological order (callees before callers),
        // with solar_main last. Uses SCCs to handle recursion.
        let order = self.topological_function_order();
        for idx in &order {
            self.emit_function(&self.module.functions[*idx]);
        }

        // Emit main
        self.line("int main(void) {");
        self.indent += 1;
        // The debug compile pipeline (single clang, no write-barrier pass) emits
        // no GC barriers, so a real collection could free live objects. Force
        // bump-allocator mode there; `-DSOLAR_DEBUG_DISABLE_GC` is passed only by
        // the debug build (release runs the collector normally).
        self.raw_line("#ifdef SOLAR_DEBUG_DISABLE_GC");
        self.raw_line("sol_disable_gc();");
        self.raw_line("#endif");
        if self.static_root_count > 0 {
            self.linef(format!(
                "sol_start(solar_main, _sol_statics, {});",
                self.static_root_count
            ));
        } else {
            self.line("sol_start(solar_main, 0, 0);");
        }
        self.line("return 0;");
        self.indent -= 1;
        self.line("}");
    }

    fn emit_prelude(&mut self) {
        self.line("#include <stdint.h>");
        self.line("#include <string.h>");
        self.line("");
        self.line("// Runtime externs");
        self.line("typedef void (*sol_mark_fn_t)(void*, uint8_t*, uint64_t);");
        self.line("extern uint8_t* sol_alloc(size_t size, size_t align, sol_mark_fn_t mark_fn);");
        self.line("extern void sol_gc_mark(void* ctx, uint8_t* ptr);");
        self.line("extern void sol_memcpy(uint8_t* dst, const uint8_t* src, size_t size);");
        self.line("extern void sol_panic(const uint8_t* ptr, size_t len);");
        self.line("extern uint8_t* sol_file_open(const uint8_t* ptr, size_t len, int64_t flags, uint64_t mode);");
        self.line("extern void sol_file_close(uint8_t* fd);");
        self.line("extern uint8_t* sol_file_stdin(void);");
        self.line("extern uint8_t* sol_file_stdout(void);");
        self.line("extern uint8_t* sol_file_stderr(void);");
        self.line("extern size_t sol_file_read(uint8_t* fd, uint8_t* ptr, size_t len);");
        self.line(
            "extern size_t sol_file_write_partial(uint8_t* fd, const uint8_t* ptr, size_t len);",
        );
        self.line(
            "extern size_t sol_file_read_at(uint8_t* fd, uint8_t* ptr, size_t len, uint64_t offset);",
        );
        self.line(
            "extern size_t sol_file_write_at(uint8_t* fd, const uint8_t* ptr, size_t len, uint64_t offset);",
        );
        self.line("extern void sol_file_sync(uint8_t* fd);");
        self.line("extern uint8_t sol_file_lock(uint8_t* fd, int64_t op);");
        self.line("extern void sol_file_remove(const uint8_t* ptr, size_t len);");
        self.line("extern void sol_dir_remove(const uint8_t* ptr, size_t len);");
        self.line(
            "extern void sol_file_rename(const uint8_t* old_ptr, size_t old_len, const uint8_t* new_ptr, size_t new_len);",
        );
        self.line("extern void sol_dir_create(const uint8_t* ptr, size_t len, uint64_t mode);");
        self.line(
            "extern uint8_t sol_file_stat(const uint8_t* ptr, size_t len, uint64_t* size, uint64_t* mtime, uint64_t* kind);",
        );
        self.line("extern void sol_dir_read(uint8_t* fd, uint8_t* out);");
        self.line(
            "extern uint8_t* sol_socket_create(int64_t domain, int64_t type, int64_t protocol);",
        );
        self.line(
            "extern void sol_socket_bind(uint8_t* fd, const uint8_t* addr, size_t addr_len);",
        );
        self.line("extern void sol_socket_listen(uint8_t* fd, int64_t backlog);");
        self.line("extern uint8_t* sol_socket_accept(uint8_t* fd);");
        self.line(
            "extern void sol_socket_connect(uint8_t* fd, const uint8_t* addr, size_t addr_len);",
        );
        self.line(
            "extern void sol_socket_set_option(uint8_t* fd, int64_t level, int64_t name, int64_t value);",
        );
        self.line(
            "extern size_t sol_socket_local_addr(uint8_t* fd, uint8_t* dst, size_t dst_len);",
        );
        self.line("extern void sol_socket_shutdown(uint8_t* fd, int64_t how);");
        self.line("extern void sol_args(uint8_t* out);");
        self.line("extern void sol_env(uint8_t* out);");
        self.line("extern int64_t sol_checked_add_int(int64_t a, int64_t b);");
        self.line("extern int64_t sol_checked_sub_int(int64_t a, int64_t b);");
        self.line("extern int64_t sol_checked_mul_int(int64_t a, int64_t b);");
        self.line("extern int64_t sol_checked_div_int(int64_t a, int64_t b);");
        self.line("extern int64_t sol_checked_mod_int(int64_t a, int64_t b);");
        self.line("extern uint64_t sol_checked_add_uint(uint64_t a, uint64_t b);");
        self.line("extern uint64_t sol_checked_sub_uint(uint64_t a, uint64_t b);");
        self.line("extern uint64_t sol_checked_mul_uint(uint64_t a, uint64_t b);");
        self.line("extern uint64_t sol_checked_div_uint(uint64_t a, uint64_t b);");
        self.line("extern uint64_t sol_checked_mod_uint(uint64_t a, uint64_t b);");
        self.line(
            "extern void sol_carrying_mul_add(uint64_t a, uint64_t b, uint64_t carry, uint64_t add, uint64_t* out_lo, uint64_t* out_hi);",
        );
        self.line("extern uint8_t* sol_slice_index(uint8_t* base, uint64_t index, uint64_t len, uint64_t elem_size);");
        self.line("extern uint8_t* sol_slice_range(uint8_t* base, uint64_t start, uint64_t end, uint64_t len, uint64_t elem_size);");
        self.line("extern uint8_t* sol_null_check(uint8_t* ptr);");
        self.line("extern void sol_assert_array_len(uint64_t actual, uint64_t expected);");
        self.line(
            "typedef struct { uint8_t* addr; uint64_t size; sol_mark_fn_t mark_fn; } sol_static_entry;",
        );
        self.line(
            "extern void sol_start(void (*solar_main)(void*), const sol_static_entry* statics, size_t statics_len);",
        );
        self.line("extern void sol_disable_gc(void);");
        self.line("extern void sol_thread_spawn(void* fn_ptr, void* env);");
        self.line("extern void sol_throw(const uint8_t* ptr, size_t len);");
        self.line(
            "extern void sol_try(void* body_fn, void* body_env, void* handler_fn, void* handler_env);",
        );
        // Tear-free but UNORDERED 16-byte moves: give a concurrent reader / the GC
        // marker a non-torn `{ptr,len}`, with no inter-thread ordering. Used only for
        // plain value copies (`a = b`) of fat pointers / function values. Real atomics
        // use the `_acq`/`_rel`/cmpxchg variants below.
        self.line("extern void sol_store_128_unordered(uint8_t* dst, const uint8_t* src);");
        self.line("extern void sol_load_128_unordered(uint8_t* dst, const uint8_t* src);");
        self.line("extern void sol_copy_128_unordered(uint8_t* dst, const uint8_t* src);");
        self.line("extern void sol_atomic_load_128_acq(uint8_t* dst, const uint8_t* src);");
        self.line("extern void sol_atomic_store_128_rel(uint8_t* dst, const uint8_t* src);");
        self.line("extern void sol_atomic_compare_exchange_128_acq_rel(uint8_t* dst, uint8_t* ref, const uint8_t* expected, const uint8_t* new_val);");
        self.line(
            "extern void sol_futex_wait(uint32_t* ptr, uint32_t expected, uint64_t timeout_ns);",
        );
        self.line("extern void sol_futex_wake(uint32_t* ptr, uint32_t count);");
        self.line("extern uint64_t sol_monotonic_time(void);");
        self.line("extern uint64_t sol_system_time(void);");
        self.line("extern uint64_t sol_num_cpus(void);");
        self.line("extern void sol_exit(int64_t code);");
        self.line("");
        // SIMD group-scan helpers (the SwissTable hot path). Written with vector
        // extensions + an explicit move-mask so they stay vectorized into SSE2
        // (vpbroadcastb/vpcmpeqb/vpmovmskb) regardless of caller context — unlike
        // an auto-vectorized scalar loop, which scalarizes inside a complex probe.
        self.line("typedef unsigned char _sol_v16qi __attribute__((vector_size(16)));");
        self.line(
            "static inline uint64_t _sol_simd_match_byte_x16(const uint8_t* p, uint8_t tag) {",
        );
        self.line("  _sol_v16qi g; __builtin_memcpy(&g, p, 16);");
        self.line("  _sol_v16qi t = tag - (_sol_v16qi){0};");
        self.line("  _sol_v16qi eq = (g == t);");
        self.line("  return (uint64_t)(uint32_t)__builtin_ia32_pmovmskb128((char __attribute__((vector_size(16)))) eq);");
        self.line("}");
        self.line("static inline uint64_t _sol_simd_match_high_bit_x16(const uint8_t* p) {");
        self.line("  _sol_v16qi g; __builtin_memcpy(&g, p, 16);");
        self.line("  return (uint64_t)(uint32_t)__builtin_ia32_pmovmskb128((char __attribute__((vector_size(16)))) g);");
        self.line("}");
        self.line("");
    }

    fn func_name(&self, name: &str) -> String {
        if name == "main" {
            "solar_main".into()
        } else {
            format!("solar_{name}")
        }
    }

    /// Storage/inline qualifiers for a generated function. A `fn(inline)` hint
    /// becomes `static inline __attribute__((always_inline))`, which clang lowers
    /// to the LLVM `alwaysinline` attribute — forcing the function to be inlined
    /// at every call site regardless of the inliner's cost model (not merely
    /// raising its threshold, as `inlinehint` would).
    fn func_qualifiers(func: &Function) -> &'static str {
        if func.inline_hint {
            "static inline __attribute__((always_inline))"
        } else {
            "static"
        }
    }

    fn func_signature(&self, func: &Function) -> String {
        let name = self.func_name(&func.name);
        let mut params: Vec<String> = vec!["void* __env".to_string()];
        for (i, p) in func.params.iter().enumerate() {
            let vt = self.val_type(&p.ty);
            params.push(format!("{vt} _p{i}"));
        }
        let params_str = params.join(", ");
        if matches!(func.return_type, Type::Unit | Type::Never) {
            format!("void {name}({params_str})")
        } else {
            let vt = self.val_type(&func.return_type);
            format!("{vt} {name}({params_str})")
        }
    }

    /// Whether a non-escaping value of type `ty` may be placed on the C stack
    /// (a sized, non-`FixedArray` value within the size cap). The escape proof is
    /// the caller's responsibility (`noescape` flag / `param_noescape`).
    fn stack_eligible(&self, ty: &Type) -> bool {
        self.is_sized(ty)
            && !matches!(ty, Type::FixedArray(_, _))
            && (1..=STACK_ALLOC_MAX).contains(&self.type_size(ty))
    }

    fn emit_function(&mut self, func: &Function) {
        self.tmp_counter = 0;
        let sig = self.func_signature(func);
        let q = Self::func_qualifiers(func);
        self.linef(format!("{q} {sig} {{"));
        self.indent += 1;

        // Bind captured variables from env. Each capture occupies a 16-byte
        // slot: a thin pointer (sized) or a fat pointer `(ptr, meta)` (unsized,
        // e.g. a captured `[Uint8]` — its `meta` is the length).
        for cap in &func.env_captures {
            let off = cap.index * 16;
            self.linef(format!(
                "uint8_t* _v{} = *(uint8_t**)((uint8_t*)__env + {off});",
                cap.var.0,
            ));
            if cap.is_unsized {
                self.linef(format!(
                    "uint64_t _vm{} = *(uint64_t*)((uint8_t*)__env + {});",
                    cap.var.0,
                    off + 8
                ));
            }
        }

        // Bind params: each gets a slot so a `&`/`^` of it is a valid pointer.
        // A param the escape analysis proved non-escaping (`param_noescape`) gets
        // a C stack buffer — its address can't outlive the call, so it needn't be
        // a GC heap box (embedded pointers stay reachable via the conservative
        // stack scan). Otherwise it's a `sol_alloc`; the GC-alloc lowering turns a
        // non-escaping one into a `calloc` that `opt -O3` may SROA into registers.
        for (i, param) in func.params.iter().enumerate() {
            let s = self.type_size(&param.ty);
            let a = self.type_align(&param.ty);
            let var_id = param.var.0;
            let ty = param.ty.clone();
            if func.param_noescape[i] && self.stack_eligible(&ty) {
                self.linef(format!(
                    "uint8_t _v{var_id}_stk[{s}] __attribute__((aligned({a})));"
                ));
                self.linef(format!("uint8_t* _v{var_id} = _v{var_id}_stk;"));
            } else {
                let mf = self.mark_fn_expr(&ty);
                self.emit_alloc(format!("_v{var_id}"), s, a, &mf);
            }
            let dst_str = format!("_v{var_id}");
            let src_str = format!("(uint8_t*)&_p{i}");
            self.emit_copy(&dst_str, &src_str, &ty, &s.to_string());
        }

        let nodes = &func.nodes;
        let body = &func.body;

        // Detect tail expression
        let has_tail = !matches!(func.return_type, Type::Unit | Type::Never)
            && body
                .last()
                .is_some_and(|&id| matches!(nodes[id.0].kind, NodeKind::Expr(_)));

        // Declare return variable for non-unit functions
        if !matches!(func.return_type, Type::Unit | Type::Never) {
            let vt = self.val_type(&func.return_type);
            self.linef(format!("{vt} _ret;"));
        }

        let (init, tail) = if has_tail {
            let (init, tail) = body.split_at(body.len() - 1);
            (init, Some(tail[0]))
        } else {
            (body.as_slice(), None)
        };

        for &id in init {
            self.emit_stmt(nodes, id);
        }

        if let Some(tid) = tail
            && let NodeKind::Expr(inner) = nodes[tid.0].kind
        {
            self.emit_into(nodes, inner, "(uint8_t*)&_ret");
            self.line("return _ret;");
        }

        self.indent -= 1;
        self.line("}");
        self.line("");
    }

    /// Returns (ptr_expr, Option<meta_expr>) — C expressions for address and optional metadata.
    fn emit_place(&mut self, nodes: &[Node], id: NodeId) -> (String, Option<String>) {
        match &nodes[id.0].kind {
            NodeKind::Local(var) => {
                let ptr = format!("_v{}", var.0);
                let ty = &nodes[id.0].ty;
                if !self.is_sized(ty) {
                    let meta_var = format!("_vm{}", var.0);
                    (ptr, Some(meta_var))
                } else {
                    (ptr, None)
                }
            }
            // Statics are sized global slots.
            NodeKind::Global(idx) => (format!("((uint8_t*)_gs{idx})"), None),
            NodeKind::FieldAccess { object, field } => {
                let object = *object;
                let field = field.clone();
                let (base, base_meta) = self.emit_place(nodes, object);
                let struct_name = match &nodes[object.0].ty {
                    Type::Struct(n) => n.clone(),
                    _ => unreachable!(),
                };
                let dt = &self.module.datatypes[struct_name.as_str()];
                let fl = dt.fields.iter().find(|f| f.name == field).unwrap();
                let is_last = dt.fields.last().unwrap().name == field;
                let offset = fl.offset;
                let ptr = if offset == 0 {
                    base
                } else {
                    let tmp = self.fresh_tmp();
                    self.linef(format!("uint8_t* {tmp} = {base} + {offset};"));
                    tmp
                };
                if is_last && !self.is_sized(&fl.ty) {
                    (ptr, base_meta)
                } else {
                    (ptr, None)
                }
            }
            NodeKind::Deref(inner) => {
                let inner = *inner;
                let place = if is_place(nodes, inner) {
                    let (p, _) = self.emit_place(nodes, inner);
                    p
                } else {
                    let ty = nodes[inner.0].ty.clone();
                    let s = self.type_size(&ty);
                    let a = self.type_align(&ty);
                    let mf = self.mark_fn_expr(&ty);
                    let tmp = self.fresh_tmp();
                    self.emit_alloc(&tmp, s, a, &mf);
                    self.emit_into(nodes, inner, &tmp);
                    tmp
                };
                match &nodes[inner.0].ty {
                    Type::Ref(_) | Type::Unique(_) => {
                        let tmp = self.fresh_tmp();
                        self.linef(format!("uint8_t* {tmp} = *(uint8_t**){place};"));
                        (tmp, None)
                    }
                    // `&?T` deref: null-check the loaded pointer before use.
                    Type::NullableRef(_) => {
                        let tmp = self.fresh_tmp();
                        self.linef(format!(
                            "uint8_t* {tmp} = sol_null_check(*(uint8_t**){place});"
                        ));
                        (tmp, None)
                    }
                    Type::RefUnsized(_) | Type::UniqueUnsized(_) => {
                        let wide_tmp = self.fresh_tmp();
                        self.linef(format!(
                            "uint8_t {wide_tmp}[16] __attribute__((aligned(16)));"
                        ));
                        self.linef(format!("sol_load_128_unordered({wide_tmp}, {place});"));
                        let ptr_tmp = self.fresh_tmp();
                        let meta_tmp = self.fresh_tmp();
                        self.linef(format!("uint8_t* {ptr_tmp} = *(uint8_t**){wide_tmp};"));
                        self.linef(format!(
                            "uint64_t {meta_tmp} = *(uint64_t*)({wide_tmp} + 8);"
                        ));
                        (ptr_tmp, Some(meta_tmp))
                    }
                    // `&?[T]` deref: null-check the pointer half of the fat pointer.
                    Type::NullableRefUnsized(_) => {
                        let wide_tmp = self.fresh_tmp();
                        self.linef(format!(
                            "uint8_t {wide_tmp}[16] __attribute__((aligned(16)));"
                        ));
                        self.linef(format!("sol_load_128_unordered({wide_tmp}, {place});"));
                        let ptr_tmp = self.fresh_tmp();
                        let meta_tmp = self.fresh_tmp();
                        self.linef(format!(
                            "uint8_t* {ptr_tmp} = sol_null_check(*(uint8_t**){wide_tmp});"
                        ));
                        self.linef(format!(
                            "uint64_t {meta_tmp} = *(uint64_t*)({wide_tmp} + 8);"
                        ));
                        (ptr_tmp, Some(meta_tmp))
                    }
                    _ => unreachable!(),
                }
            }
            NodeKind::Index { object, index } => {
                let object = *object;
                let index = *index;
                let (base, meta) = self.emit_place(nodes, object);
                let len = meta
                    .or_else(|| {
                        if let Type::FixedArray(_, n) = &nodes[object.0].ty {
                            Some(format!("{n}"))
                        } else {
                            None
                        }
                    })
                    .unwrap_or_else(|| self.emit_meta(nodes, object).unwrap());
                let idx_expr = self.emit_load(nodes, index);
                let elem_ty = &nodes[id.0].ty;
                let es = self.type_size(elem_ty);
                let tmp = self.fresh_tmp();
                self.linef(format!(
                    "uint8_t* {tmp} = sol_slice_index({base}, (uint64_t){idx_expr}, {len}, {es});"
                ));
                (tmp, None)
            }
            NodeKind::Slice { object, start, end } => {
                let object = *object;
                let start = *start;
                let end = *end;
                let (base, meta) = self.emit_place(nodes, object);
                let len = meta
                    .or_else(|| {
                        if let Type::FixedArray(_, n) = &nodes[object.0].ty {
                            Some(format!("{n}"))
                        } else {
                            None
                        }
                    })
                    .unwrap_or_else(|| self.emit_meta(nodes, object).unwrap());
                let start_expr = self.emit_load(nodes, start);
                let end_expr = self.emit_load(nodes, end);
                let elem_ty = match &nodes[id.0].ty {
                    Type::Array(inner) | Type::FixedArray(inner, _) => inner,
                    _ => unreachable!(),
                };
                let es = self.type_size(elem_ty);
                let ptr_tmp = self.fresh_tmp();
                self.linef(format!(
                    "uint8_t* {ptr_tmp} = sol_slice_range({base}, (uint64_t){start_expr}, (uint64_t){end_expr}, {len}, {es});"
                ));
                let meta_tmp = self.fresh_tmp();
                self.linef(format!(
                    "uint64_t {meta_tmp} = (uint64_t){end_expr} - (uint64_t){start_expr};"
                ));
                (ptr_tmp, Some(meta_tmp))
            }
            NodeKind::IfExpr {
                condition,
                then_body,
                else_body,
            } => {
                let condition = *condition;
                let then_body = then_body.clone();
                let else_body = else_body.clone();
                let ptr_tmp = self.fresh_tmp();
                let has_meta = !self.is_sized(&nodes[id.0].ty);
                let meta_tmp = if has_meta {
                    let m = self.fresh_tmp();
                    self.linef(format!("uint64_t {m};"));
                    m
                } else {
                    String::new()
                };
                self.linef(format!("uint8_t* {ptr_tmp};"));
                let cond = self.emit_load(nodes, condition);
                self.linef(format!("if ((uint8_t){cond}) {{"));
                self.indent += 1;
                self.emit_branch_place(
                    nodes,
                    &then_body,
                    &ptr_tmp,
                    if has_meta { Some(&meta_tmp) } else { None },
                );
                self.indent -= 1;
                self.line("} else {");
                self.indent += 1;
                self.emit_branch_place(
                    nodes,
                    &else_body,
                    &ptr_tmp,
                    if has_meta { Some(&meta_tmp) } else { None },
                );
                self.indent -= 1;
                self.line("}");
                (ptr_tmp, if has_meta { Some(meta_tmp) } else { None })
            }
            NodeKind::Match { scrutinee, arms } => {
                let scrutinee = *scrutinee;
                let arms = arms.clone();
                let enum_ty = nodes[scrutinee.0].ty.clone();
                let enum_name = match &enum_ty {
                    Type::Enum(name) => name.clone(),
                    _ => unreachable!(),
                };
                let enum_base = if is_place(nodes, scrutinee) {
                    let (place, _) = self.emit_place(nodes, scrutinee);
                    place
                } else {
                    let s = self.type_size(&enum_ty);
                    let a = self.type_align(&enum_ty);
                    let mf = self.mark_fn_expr(&enum_ty);
                    let tmp = self.fresh_tmp();
                    self.emit_alloc(&tmp, s, a, &mf);
                    self.emit_into(nodes, scrutinee, &tmp);
                    tmp
                };
                // Acquire pairs with the release store of the discriminant in
                // variant construction/copy: observing the tag also observes
                // the payload it describes.
                let disc = self.fresh_tmp();
                self.linef(format!(
                    "uint64_t {disc} = __atomic_load_n((uint64_t*){enum_base}, __ATOMIC_ACQUIRE);"
                ));
                let ptr_tmp = self.fresh_tmp();
                let has_meta = !self.is_sized(&nodes[id.0].ty);
                let meta_tmp = if has_meta {
                    let m = self.fresh_tmp();
                    self.linef(format!("uint64_t {m};"));
                    m
                } else {
                    String::new()
                };
                self.linef(format!("uint8_t* {ptr_tmp};"));
                for (i, arm) in arms.iter().enumerate() {
                    let is_wildcard = matches!(arm.pattern, MatchPattern::Wildcard(_, _));
                    if i == 0 {
                        if is_wildcard {
                            self.line("{");
                        } else {
                            let idx = match &arm.pattern {
                                MatchPattern::Variant { variant_index, .. } => *variant_index,
                                _ => unreachable!(),
                            };
                            self.linef(format!("if ({disc} == {idx}u) {{"));
                        }
                    } else if is_wildcard {
                        self.line("} else {");
                    } else {
                        let idx = match &arm.pattern {
                            MatchPattern::Variant { variant_index, .. } => *variant_index,
                            _ => unreachable!(),
                        };
                        self.linef(format!("}} else if ({disc} == {idx}u) {{"));
                    }
                    self.indent += 1;
                    match &arm.pattern {
                        MatchPattern::Variant {
                            variant_name,
                            binding: Some((var, _)),
                            ..
                        } => {
                            let dt = &self.module.datatypes[enum_name.as_str()];
                            let fl = dt.fields.iter().find(|f| f.name == *variant_name).unwrap();
                            let offset = fl.offset;
                            self.linef(format!("uint8_t* _v{} = {enum_base} + {offset};", var.0));
                        }
                        MatchPattern::Wildcard(var, _) => {
                            self.linef(format!("uint8_t* _v{} = {enum_base};", var.0));
                        }
                        _ => {}
                    }
                    self.emit_branch_place(
                        nodes,
                        &arm.body,
                        &ptr_tmp,
                        if has_meta { Some(&meta_tmp) } else { None },
                    );
                    self.indent -= 1;
                }
                self.line("}");
                (ptr_tmp, if has_meta { Some(meta_tmp) } else { None })
            }
            _ => {
                // Non-place node: materialize into a temporary
                let ty = &nodes[id.0].ty;
                let s = self.type_size(ty);
                let a = self.type_align(ty);
                let mf = self.mark_fn_expr(ty);
                let tmp = self.fresh_tmp();
                self.emit_alloc(&tmp, s, a, &mf);
                self.emit_into(nodes, id, &tmp);
                (tmp, None)
            }
        }
    }

    fn emit_branch_place(
        &mut self,
        nodes: &[Node],
        body: &[NodeId],
        ptr_dst: &str,
        meta_dst: Option<&str>,
    ) {
        let (init, tail) = body.split_at(body.len() - 1);
        for &id in init {
            self.emit_stmt(nodes, id);
        }
        match nodes[tail[0].0].kind {
            NodeKind::Expr(inner) => {
                let (place, meta) = self.emit_place(nodes, inner);
                self.linef(format!("{ptr_dst} = {place};"));
                if let Some(md) = meta_dst {
                    self.linef(format!("{md} = {};", meta.unwrap()));
                }
            }
            _ => unreachable!(),
        }
    }

    /// Emit a C expression computing full_size of a type given a metadata variable name.
    fn emit_full_size_expr(&self, ty: &Type, meta: &str) -> String {
        match ty {
            Type::Array(inner) => {
                let es = self.type_size(inner);
                format!("({meta} * {es})")
            }
            Type::Struct(name) => {
                let dt = &self.module.datatypes[name.as_str()];
                if dt.is_sized {
                    format!("{}", dt.size)
                } else {
                    let last = dt.fields.last().unwrap();
                    let tail_expr = self.emit_full_size_expr(&last.ty, meta);
                    let base = last.offset;
                    let al = dt.align;
                    format!("(({base} + {tail_expr} + {al} - 1) & ~(uint64_t)({al} - 1))")
                }
            }
            _ => format!("{}", self.type_size(ty)),
        }
    }

    /// Returns a C expression for metadata, or None for sized types (except FixedArray).
    fn emit_meta(&mut self, nodes: &[Node], id: NodeId) -> Option<String> {
        let ty = &nodes[id.0].ty;
        // FixedArray is sized but still has a known meta (element count)
        if let Type::FixedArray(_, n) = ty {
            return Some(format!("{n}"));
        }
        if self.is_sized(ty) {
            return None;
        }
        match &nodes[id.0].kind {
            NodeKind::ArrayLiteral(elems) => Some(format!("{}", elems.len())),
            NodeKind::ArrayRepeat { count, .. } | NodeKind::ArrayInit { count, .. } => {
                let count = *count;
                Some(self.emit_load(nodes, count))
            }
            NodeKind::ArraySizeCoerce { size, .. } => Some(format!("{size}")),
            NodeKind::StructLiteral { name, fields } => {
                let dt = &self.module.datatypes[name.as_str()];
                let last_field_name = dt.fields.last().unwrap().name.clone();
                let last_init = fields.iter().find(|(n, _)| *n == last_field_name).unwrap();
                self.emit_meta(nodes, last_init.1)
            }
            NodeKind::Local(var) => Some(format!("_vm{}", var.0)),
            NodeKind::FieldAccess { object, .. } => self.emit_meta(nodes, *object),
            NodeKind::Deref(inner) => {
                let inner = *inner;
                match &nodes[inner.0].ty {
                    Type::RefUnsized(_) | Type::UniqueUnsized(_) => {
                        let (place, _) = self.emit_place(nodes, inner);
                        let meta_tmp = self.fresh_tmp();
                        self.linef(format!("uint64_t {meta_tmp} = *(uint64_t*)({place} + 8);"));
                        Some(meta_tmp)
                    }
                    _ => None,
                }
            }
            NodeKind::Slice { start, end, .. } => {
                let start = *start;
                let end = *end;
                let start_expr = self.emit_load(nodes, start);
                let end_expr = self.emit_load(nodes, end);
                let tmp = self.fresh_tmp();
                self.linef(format!(
                    "uint64_t {tmp} = (uint64_t){end_expr} - (uint64_t){start_expr};"
                ));
                Some(tmp)
            }
            NodeKind::BinaryOp { op, left, right } if *op == BinOp::Add => {
                let left = *left;
                let right = *right;
                let lm = self.emit_meta(nodes, left).unwrap();
                let rm = self.emit_meta(nodes, right).unwrap();
                let tmp = self.fresh_tmp();
                self.linef(format!("uint64_t {tmp} = {lm} + {rm};"));
                Some(tmp)
            }
            _ => None,
        }
    }

    /// Returns a C expression for a scalar value — no sol_alloc.
    fn emit_load(&mut self, nodes: &[Node], id: NodeId) -> String {
        match &nodes[id.0].kind {
            NodeKind::IntegerLiteral(n) => {
                let n = *n;
                let ty = &nodes[id.0].ty;
                if ty.is_unsigned() {
                    let c_ty = self.c_int_type(ty);
                    format!("({c_ty}){}u", n as u64)
                } else {
                    format!("{n}")
                }
            }
            NodeKind::BooleanLiteral(b) => {
                if *b {
                    "1".to_string()
                } else {
                    "0".to_string()
                }
            }
            // A sized `null#[T]` is the null pointer.
            NodeKind::Null => "(uint8_t*)0".to_string(),
            NodeKind::Local(var) => {
                let ty = &nodes[id.0].ty;
                let c_ty = self.c_int_type(ty);
                format!("*({c_ty}*)_v{}", var.0)
            }
            NodeKind::Global(_)
            | NodeKind::FieldAccess { .. }
            | NodeKind::Deref(_)
            | NodeKind::Index { .. }
            | NodeKind::Slice { .. } => {
                let (place, _) = self.emit_place(nodes, id);
                let ty = &nodes[id.0].ty;
                let c_ty = self.c_int_type(ty);
                format!("*({c_ty}*){place}")
            }
            NodeKind::BinaryOp { op, left, right } => {
                let op = *op;
                let left = *left;
                let right = *right;
                let result_ty = nodes[id.0].ty.clone();
                let left_ty = nodes[left.0].ty.clone();
                self.emit_load_binop(nodes, op, left, right, &result_ty, &left_ty)
            }
            NodeKind::Not(inner) => {
                let inner = *inner;
                let ty = nodes[inner.0].ty.clone();
                let val = self.emit_load(nodes, inner);
                if ty.is_integer() {
                    // Bitwise complement; the cast truncates to the type's width.
                    let c_ty = self.c_int_type(&ty);
                    format!("({c_ty})(~({c_ty}){val})")
                } else {
                    format!("!(uint8_t){val}")
                }
            }
            NodeKind::IfExpr { .. } | NodeKind::Match { .. } => {
                let ty = nodes[id.0].ty.clone();
                let s = self.type_size(&ty);
                let a = self.type_align(&ty);
                let mf = self.mark_fn_expr(&ty);
                let tmp = self.fresh_tmp();
                self.emit_alloc(&tmp, s, a, &mf);
                self.emit_into(nodes, id, &tmp);
                let c_ty = self.c_int_type(&ty);
                format!("*({c_ty}*){tmp}")
            }
            NodeKind::Ref(_)
            | NodeKind::Unique(_)
            | NodeKind::FunctionRef(_)
            | NodeKind::MakeClosure { .. } => {
                let ty = nodes[id.0].ty.clone();
                let s = self.type_size(&ty);
                let a = self.type_align(&ty);
                let mf = self.mark_fn_expr(&ty);
                let tmp = self.fresh_tmp();
                self.emit_alloc(&tmp, s, a, &mf);
                self.emit_into(nodes, id, &tmp);
                let c_ty = self.c_int_type(&ty);
                format!("*({c_ty}*){tmp}")
            }
            NodeKind::Call { function, args } => {
                let function = function.clone();
                let args: Vec<NodeId> = args.clone();
                let result_ty = nodes[id.0].ty.clone();
                let vt = self.val_type(&result_ty);
                let call_expr = self.emit_call_expr(nodes, &function, &args);
                let tmp = self.fresh_tmp();
                self.linef(format!("{vt} {tmp} = {call_expr};"));
                let c_ty = self.c_int_type(&result_ty);
                format!("*({c_ty}*)(uint8_t*)&{tmp}")
            }
            NodeKind::IntrinsicCall { intrinsic, args } => {
                let intrinsic = intrinsic.clone();
                let args: Vec<NodeId> = args.clone();
                let result_ty = nodes[id.0].ty.clone();
                let s = self.type_size(&result_ty);
                let a = self.type_align(&result_ty);
                let mf = self.mark_fn_expr(&result_ty);
                let tmp = self.fresh_tmp();
                self.emit_alloc(&tmp, s, a, &mf);
                self.emit_intrinsic(nodes, &intrinsic, &args, &result_ty, &tmp);
                let c_ty = self.c_int_type(&result_ty);
                format!("*({c_ty}*){tmp}")
            }
            NodeKind::CallIndirect { .. } => {
                let ty = nodes[id.0].ty.clone();
                let s = self.type_size(&ty);
                let a = self.type_align(&ty);
                let mf = self.mark_fn_expr(&ty);
                let tmp = self.fresh_tmp();
                self.emit_alloc(&tmp, s, a, &mf);
                self.emit_into(nodes, id, &tmp);
                let c_ty = self.c_int_type(&ty);
                format!("*({c_ty}*){tmp}")
            }
            _ => unreachable!("emit_load on non-scalar: {:?}", nodes[id.0].kind),
        }
    }

    fn emit_load_binop(
        &mut self,
        nodes: &[Node],
        op: BinOp,
        left: NodeId,
        right: NodeId,
        result_ty: &Type,
        left_ty: &Type,
    ) -> String {
        let result_c_ty = self.c_int_type(result_ty);
        match op {
            BinOp::And => {
                let la = self.emit_load(nodes, left);
                let result = self.fresh_tmp();
                self.linef(format!("{result_c_ty} {result};"));
                self.linef(format!("if ((uint8_t){la} == 0) {{"));
                self.indent += 1;
                self.linef(format!("{result} = 0;"));
                self.indent -= 1;
                self.line("} else {");
                self.indent += 1;
                let ra = self.emit_load(nodes, right);
                self.linef(format!("{result} = {ra};"));
                self.indent -= 1;
                self.line("}");
                result
            }
            BinOp::Or => {
                let la = self.emit_load(nodes, left);
                let result = self.fresh_tmp();
                self.linef(format!("{result_c_ty} {result};"));
                self.linef(format!("if ((uint8_t){la} != 0) {{"));
                self.indent += 1;
                self.linef(format!("{result} = {la};"));
                self.indent -= 1;
                self.line("} else {");
                self.indent += 1;
                let ra = self.emit_load(nodes, right);
                self.linef(format!("{result} = {ra};"));
                self.indent -= 1;
                self.line("}");
                result
            }
            _ if left_ty.is_integer() || *left_ty == Type::Bool => {
                let load_ty = self.c_int_type(left_ty);
                let la = self.emit_load(nodes, left);
                let ra = self.emit_load(nodes, right);
                let result = self.fresh_tmp();
                match op {
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                        let (func, wide_ty) = if left_ty.is_unsigned() {
                            let func = match op {
                                BinOp::Add => "sol_checked_add_uint",
                                BinOp::Sub => "sol_checked_sub_uint",
                                BinOp::Mul => "sol_checked_mul_uint",
                                BinOp::Div => "sol_checked_div_uint",
                                BinOp::Mod => "sol_checked_mod_uint",
                                _ => unreachable!(),
                            };
                            (func, "uint64_t")
                        } else {
                            let func = match op {
                                BinOp::Add => "sol_checked_add_int",
                                BinOp::Sub => "sol_checked_sub_int",
                                BinOp::Mul => "sol_checked_mul_int",
                                BinOp::Div => "sol_checked_div_int",
                                BinOp::Mod => "sol_checked_mod_int",
                                _ => unreachable!(),
                            };
                            (func, "int64_t")
                        };
                        self.linef(format!(
                            "{result_c_ty} {result} = ({result_c_ty}){func}(({wide_ty})({load_ty}){la}, ({wide_ty})({load_ty}){ra});"
                        ));
                    }
                    BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                        let c_op = match op {
                            BinOp::Eq => "==",
                            BinOp::Ne => "!=",
                            BinOp::Lt => "<",
                            BinOp::Le => "<=",
                            BinOp::Gt => ">",
                            BinOp::Ge => ">=",
                            _ => unreachable!(),
                        };
                        self.linef(format!(
                            "{result_c_ty} {result} = (({load_ty}){la} {c_op} ({load_ty}){ra}) ? 1 : 0;"
                        ));
                    }
                    BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => {
                        let c_op = match op {
                            BinOp::BitAnd => "&",
                            BinOp::BitOr => "|",
                            BinOp::BitXor => "^",
                            _ => unreachable!(),
                        };
                        self.linef(format!(
                            "{result_c_ty} {result} = ({result_c_ty})(({load_ty}){la} {c_op} ({load_ty}){ra});"
                        ));
                    }
                    BinOp::WrapAdd | BinOp::WrapSub | BinOp::WrapMul => {
                        // Compute in the unsigned 64-bit domain (defined wraparound,
                        // unlike signed overflow which is UB in C); the cast back to
                        // {result_c_ty} truncates to the operand's width.
                        let c_op = match op {
                            BinOp::WrapAdd => "+",
                            BinOp::WrapSub => "-",
                            BinOp::WrapMul => "*",
                            _ => unreachable!(),
                        };
                        self.linef(format!(
                            "{result_c_ty} {result} = ({result_c_ty})((uint64_t)({load_ty}){la} {c_op} (uint64_t)({load_ty}){ra});"
                        ));
                    }
                    BinOp::Shl => {
                        // Shift in the unsigned 64-bit domain to avoid C's UB on
                        // signed/overflowing shifts; a count reaching the value's
                        // width (or negative, which casts to a huge unsigned)
                        // overflows to 0. The cast back to {result_c_ty} truncates.
                        // The count is cast through its *own* type (not the value's)
                        // so a wider count isn't truncated before the width check.
                        let width = self.type_size(left_ty) * 8;
                        let count_c_ty = self.c_int_type(&nodes[right.0].ty);
                        self.linef(format!(
                            "{result_c_ty} {result} = ((uint64_t)({count_c_ty}){ra} >= {width}) ? 0 : ({result_c_ty})((uint64_t)({load_ty}){la} << ((uint64_t)({count_c_ty}){ra}));"
                        ));
                    }
                    BinOp::Shr => {
                        let width = self.type_size(left_ty) * 8;
                        let count_c_ty = self.c_int_type(&nodes[right.0].ty);
                        if left_ty.is_unsigned() {
                            self.linef(format!(
                                "{result_c_ty} {result} = ((uint64_t)({count_c_ty}){ra} >= {width}) ? 0 : ({result_c_ty})((uint64_t)({load_ty}){la} >> ((uint64_t)({count_c_ty}){ra}));"
                            ));
                        } else {
                            // Arithmetic shift; a count reaching the width caps at
                            // width-1 so the result fills with the sign bit.
                            self.linef(format!(
                                "{result_c_ty} {result} = ({result_c_ty})((int64_t)({load_ty}){la} >> (((uint64_t)({count_c_ty}){ra} >= {width}) ? {} : (uint64_t)({count_c_ty}){ra}));",
                                width - 1
                            ));
                        }
                    }
                    _ => unreachable!(),
                }
                result
            }
            _ if left_ty.is_nullable_ref() => {
                // Nullable-reference equality: compare the pointer (first 8 bytes).
                // Handles `ref == null#[T]` and pointer identity; for fat refs the
                // length half is ignored.
                let c_op = match op {
                    BinOp::Eq => "==",
                    BinOp::Ne => "!=",
                    _ => unreachable!("only ==/!= allowed on nullable references"),
                };
                let vt = self.val_type(left_ty);
                let lt = self.fresh_tmp();
                self.linef(format!("{vt} {lt};"));
                self.emit_into(nodes, left, &format!("(uint8_t*)&{lt}"));
                let rt = self.fresh_tmp();
                self.linef(format!("{vt} {rt};"));
                self.emit_into(nodes, right, &format!("(uint8_t*)&{rt}"));
                let result = self.fresh_tmp();
                self.linef(format!(
                    "{result_c_ty} {result} = (*(uint8_t**)&{lt} {c_op} *(uint8_t**)&{rt}) ? 1 : 0;"
                ));
                result
            }
            _ if matches!(left_ty, Type::Array(_) | Type::FixedArray(_, _)) => {
                // Array equality: emit both into temp storage, compare
                let inner = match left_ty {
                    Type::Array(inner) | Type::FixedArray(inner, _) => inner,
                    _ => unreachable!(),
                };
                let es = self.type_size(inner);
                let ea = self.type_align(inner);
                let mf = if self.type_contains_gc_ptr(inner) {
                    "_mark_ptr_array"
                } else {
                    "_mark_noop"
                };
                let la_meta = self.emit_meta(nodes, left).unwrap();
                let la_tmp = self.fresh_tmp();
                self.emit_alloc(&la_tmp, format!("{la_meta} * {es}"), ea, mf);
                self.emit_into(nodes, left, &la_tmp);
                let ra_meta = self.emit_meta(nodes, right).unwrap();
                let ra_tmp = self.fresh_tmp();
                self.emit_alloc(&ra_tmp, format!("{ra_meta} * {es}"), ea, mf);
                self.emit_into(nodes, right, &ra_tmp);
                let eq_var = self.fresh_tmp();
                self.linef(format!(
                    "uint8_t {eq_var} = ({la_meta} == {ra_meta}) ? 1 : 0;"
                ));
                self.linef(format!("if ({eq_var}) {{"));
                self.indent += 1;
                let idx_var = self.fresh_tmp();
                self.linef(format!(
                    "for (uint64_t {idx_var} = 0; {idx_var} < {la_meta}; {idx_var}++) {{"
                ));
                self.indent += 1;
                self.linef(format!(
                    "if (memcmp({la_tmp} + {idx_var} * {es}, {ra_tmp} + {idx_var} * {es}, {es}) != 0) {{ {eq_var} = 0; break; }}"
                ));
                self.indent -= 1;
                self.line("}");
                self.indent -= 1;
                self.line("}");
                let result = self.fresh_tmp();
                let is_eq = match op {
                    BinOp::Eq => eq_var.to_string(),
                    BinOp::Ne => format!("{eq_var} ? 0 : 1"),
                    _ => unreachable!(),
                };
                self.linef(format!("{result_c_ty} {result} = {is_eq};"));
                result
            }
            _ => unreachable!("binop on type {:?}", left_ty),
        }
    }

    /// Build a call expression string (without assigning result).
    fn emit_call_expr(&mut self, nodes: &[Node], function: &str, args: &[NodeId]) -> String {
        let func = self
            .module
            .functions
            .iter()
            .find(|f| f.name == function)
            .unwrap();
        let cname = self.func_name(function);
        let mut arg_exprs: Vec<String> = vec!["NULL".to_string()];
        for (param, &arg) in func.params.iter().zip(args.iter()) {
            let vt = self.val_type(&param.ty);
            // Create a local val-type, emit_into it, pass by value
            let ptmp = self.fresh_tmp();
            self.linef(format!("{vt} {ptmp};"));
            self.emit_into(nodes, arg, &format!("(uint8_t*)&{ptmp}"));
            arg_exprs.push(ptmp);
        }
        let args_str = arg_exprs.join(", ");
        format!("{cname}({args_str})")
    }

    /// Emit a plain (no deep-copy, no variant dispatch) copy of a *value* of
    /// `ty`. Pointer-free types are one `sol_memcpy`; a type with GC pointers
    /// is copied member-wise — `uint8_t*`-typed assignments at its pointer
    /// words, `sol_memcpy` for the pointer-free gaps — so every GC-pointer
    /// store reaches LLVM as `store ptr` and the write-barrier pass can
    /// instrument pointer stores precisely. (A flat memcpy of a small value
    /// gets shrunk by `opt` into an *integer* store, which the barrier can't
    /// tell from data; that was a real missed-mark bug on splay.)
    fn emit_plain_copy(&mut self, dst: &str, src: &str, ty: &Type, size_expr: &str) {
        let runs = self.ptr_runs(ty);
        if runs.is_empty() {
            self.linef(format!("sol_memcpy({dst}, {src}, {size_expr});"));
            return;
        }
        // Owned-slice *value*: a (data ptr, len) fat pointer. `type_size` below
        // refuses unsized types, so lay it out explicitly.
        if let Type::Array(_) = ty {
            self.linef(format!("*(uint8_t**)({dst}) = *(uint8_t**)({src});"));
            self.linef(format!(
                "*(uint64_t*)(({dst}) + 8) = *(uint64_t*)(({src}) + 8);"
            ));
            return;
        }
        let size = self.type_size(ty);
        let mut off = 0usize;
        let copy_gap = |this: &mut Self, from: usize, to: usize| {
            if to > from {
                this.linef(format!(
                    "sol_memcpy(({dst}) + {from}, ({src}) + {from}, {});",
                    to - from
                ));
            }
        };
        for (start, len) in &runs {
            let run_off = start * 8;
            copy_gap(self, off, run_off);
            for w in 0..*len {
                let o = run_off + w * 8;
                self.linef(format!(
                    "*(uint8_t**)(({dst}) + {o}) = *(uint8_t**)(({src}) + {o});"
                ));
            }
            off = run_off + len * 8;
        }
        copy_gap(self, off, size);
    }

    /// Emit a type-aware copy from `src` to `dst`. If the type contains unique
    /// pointers, recursively deep-copies the pointees. Otherwise copies the
    /// value with pointer-typed member stores (see `emit_plain_copy`).
    fn emit_copy(&mut self, dst: &str, src: &str, ty: &Type, size_expr: &str) {
        self.emit_copy_ctx(dst, src, ty, size_expr, false)
    }

    /// Copy the *pointee data* of an unsized `ty` (element data for `[T]`,
    /// not its 16-byte fat-pointer value). `size_expr` is the total byte size.
    fn emit_copy_contents(&mut self, dst: &str, src: &str, ty: &Type, size_expr: &str) {
        self.emit_copy_ctx(dst, src, ty, size_expr, true)
    }

    /// `contents == true` means `dst`/`src` point at the *pointee data* of an
    /// unsized `ty` (from a `^T`-of-unsized deep copy) rather than at a value
    /// of `ty` — for `[T]` that's the element data, not the 16-byte fat pointer.
    fn emit_copy_ctx(&mut self, dst: &str, src: &str, ty: &Type, size_expr: &str, contents: bool) {
        // UniqueUnsized needs deep-copy, so it goes through the match below
        if matches!(
            ty,
            Type::Function { .. } | Type::RefUnsized(_) | Type::NullableRefUnsized(_)
        ) {
            self.linef(format!("sol_copy_128_unordered({dst}, {src});"));
            return;
        }
        // Unsized-struct contents: copy the sized field prefix field-by-field
        // (each field via `emit_copy`, so pointer runs / deep copies apply),
        // then recurse on the unsized tail with the remaining bytes.
        // `emit_plain_copy` can't handle this — it needs a static `type_size`.
        if contents
            && let Type::Struct(name) = ty
            && !self.module.datatypes[name.as_str()].is_sized
        {
            let fields: Vec<_> = self.module.datatypes[name.as_str()]
                .fields
                .iter()
                .map(|f| (f.offset, f.ty.clone(), f.size))
                .collect();
            let (prefix, tail) = fields.split_at(fields.len() - 1);
            for (offset, field_ty, field_size) in prefix {
                let fdst = format!("(({dst}) + {offset})");
                let fsrc = format!("(({src}) + {offset})");
                self.emit_copy(&fdst, &fsrc, field_ty, &field_size.to_string());
            }
            let (tail_off, tail_ty, _) = &tail[0];
            let tdst = format!("(({dst}) + {tail_off})");
            let tsrc = format!("(({src}) + {tail_off})");
            // Includes the struct's trailing alignment padding, like the
            // whole-value memcpy this replaces; both allocations are
            // `full_size` so the extra bytes are in bounds.
            let tail_size = format!("(({size_expr}) - {tail_off})");
            self.emit_copy_ctx(&tdst, &tsrc, tail_ty, &tail_size, true);
            return;
        }
        if !self.type_contains_unique(ty) && !self.type_contains_enum(ty) {
            if contents && let Type::Array(inner) = ty {
                // Unsized array contents: copy per element so pointer-carrying
                // elements still get typed pointer stores.
                if self.ptr_runs(inner).is_empty() {
                    self.linef(format!("sol_memcpy({dst}, {src}, {size_expr});"));
                } else {
                    let es = self.type_size(inner);
                    let inner = (**inner).clone();
                    let count_tmp = self.fresh_tmp();
                    self.linef(format!("size_t {count_tmp} = ({size_expr}) / {es};"));
                    let idx = self.fresh_tmp();
                    self.linef(format!(
                        "for (size_t {idx} = 0; {idx} < {count_tmp}; {idx}++) {{"
                    ));
                    self.indent += 1;
                    let edst = format!("(({dst}) + {idx} * {es})");
                    let esrc = format!("(({src}) + {idx} * {es})");
                    self.emit_plain_copy(&edst, &esrc, &inner, &es.to_string());
                    self.indent -= 1;
                    self.line("}");
                }
            } else {
                self.emit_plain_copy(dst, src, ty, size_expr);
            }
            return;
        }
        match ty {
            Type::Unique(inner) => {
                let size = self.type_size(inner);
                let align = self.type_align(inner);
                let mf = self.mark_fn_expr(inner);
                let new_ptr = self.fresh_tmp();
                self.emit_alloc(&new_ptr, size, align, &mf);
                let src_ptr = self.fresh_tmp();
                self.linef(format!("uint8_t* {src_ptr} = *(uint8_t**){src};"));
                self.emit_copy(&new_ptr, &src_ptr, inner, &size.to_string());
                self.linef(format!("*(uint8_t**){dst} = {new_ptr};"));
            }
            Type::UniqueUnsized(inner) => {
                // Atomic load from src (it's a wide ptr)
                let src_wide = self.fresh_tmp();
                self.linef(format!(
                    "uint8_t {src_wide}[16] __attribute__((aligned(16)));"
                ));
                self.linef(format!("sol_load_128_unordered({src_wide}, {src});"));
                let align = self.type_align(inner);
                let src_ptr = self.fresh_tmp();
                self.linef(format!("uint8_t* {src_ptr} = *(uint8_t**){src_wide};"));
                let src_meta = self.fresh_tmp();
                self.linef(format!(
                    "uint64_t {src_meta} = *(uint64_t*)({src_wide} + 8);"
                ));
                let inner_size = self.emit_full_size_expr(inner, &src_meta);
                let mf = self.mark_fn_expr(inner);
                let new_ptr = self.fresh_tmp();
                self.emit_alloc(&new_ptr, &inner_size, align, &mf);
                // `inner` is unsized: new_ptr/src_ptr are its pointee data.
                self.emit_copy_ctx(&new_ptr, &src_ptr, inner, &inner_size, true);
                let wide_tmp = self.fresh_tmp();
                self.linef(format!(
                    "uint8_t {wide_tmp}[16] __attribute__((aligned(16)));"
                ));
                self.linef(format!("*(uint8_t**){wide_tmp} = {new_ptr};"));
                self.linef(format!("*(uint64_t*)({wide_tmp} + 8) = {src_meta};"));
                self.linef(format!("sol_store_128_unordered({dst}, {wide_tmp});"));
            }
            Type::Enum(name) => {
                let dt = &self.module.datatypes[name.as_str()];
                let variant_map: Vec<_> = dt.variant_map.as_ref().unwrap().clone();
                let disc_tmp = self.fresh_tmp();
                self.linef(format!("uint64_t {disc_tmp} = *(uint64_t*){src};"));
                let mut first = true;
                for (i, vm_entry) in variant_map.iter().enumerate() {
                    if let Some(field_name) = vm_entry {
                        let field = dt.fields.iter().find(|f| f.name == *field_name).unwrap();
                        let offset = field.offset;
                        let field_ty = field.ty.clone();
                        let field_size = field.size;
                        let keyword = if first { "if" } else { "else if" };
                        first = false;
                        self.linef(format!("{keyword} ({disc_tmp} == {i}) {{"));
                        self.indent += 1;
                        let fdst = format!("({dst} + {offset})");
                        let fsrc = format!("({src} + {offset})");
                        self.emit_copy(&fdst, &fsrc, &field_ty, &field_size.to_string());
                        self.indent -= 1;
                        self.line("}");
                    }
                    // Unit variants: no data to copy
                }
                // Store the destination's discriminant last, with release
                // ordering, so a concurrent reader that observes the tag also
                // observes the payload it describes (same as variant
                // construction).
                self.linef(format!(
                    "__atomic_store_n((uint64_t*){dst}, {disc_tmp}, __ATOMIC_RELEASE);"
                ));
            }
            Type::Struct(name) => {
                let fields: Vec<_> = self.module.datatypes[name.as_str()]
                    .fields
                    .iter()
                    .map(|f| (f.offset, f.ty.clone(), f.size))
                    .collect();
                for (offset, field_ty, field_size) in &fields {
                    let fdst = if *offset == 0 {
                        dst.to_string()
                    } else {
                        format!("({dst} + {offset})")
                    };
                    let fsrc = if *offset == 0 {
                        src.to_string()
                    } else {
                        format!("({src} + {offset})")
                    };
                    self.emit_copy(&fdst, &fsrc, field_ty, &field_size.to_string());
                }
            }
            Type::FixedArray(inner, count) => {
                let es = self.type_size(inner);
                let inner = (**inner).clone();
                let count = *count as usize;
                let idx = self.fresh_tmp();
                self.linef(format!(
                    "for (size_t {idx} = 0; {idx} < {count}; {idx}++) {{"
                ));
                self.indent += 1;
                let edst = format!("({dst} + {idx} * {es})");
                let esrc = format!("({src} + {idx} * {es})");
                self.emit_copy(&edst, &esrc, &inner, &es.to_string());
                self.indent -= 1;
                self.line("}");
            }
            Type::Array(inner) => {
                // Unsized array — size_expr is the total byte size
                let es = self.type_size(inner);
                let inner = (**inner).clone();
                let count_tmp = self.fresh_tmp();
                self.linef(format!("size_t {count_tmp} = {size_expr} / {es};"));
                let idx = self.fresh_tmp();
                self.linef(format!(
                    "for (size_t {idx} = 0; {idx} < {count_tmp}; {idx}++) {{"
                ));
                self.indent += 1;
                let edst = format!("({dst} + {idx} * {es})");
                let esrc = format!("({src} + {idx} * {es})");
                self.emit_copy(&edst, &esrc, &inner, &es.to_string());
                self.indent -= 1;
                self.line("}");
            }
            _ => {
                self.emit_plain_copy(dst, src, ty, size_expr);
            }
        }
    }

    /// Emit C code writing value directly into `dst`.
    fn emit_into(&mut self, nodes: &[Node], id: NodeId, dst: &str) {
        // Unit values are zero-sized and have no destination to write — skip.
        // (Unit-typed blocks produce a dummy literal node as their value.)
        if matches!(nodes[id.0].ty, Type::Unit | Type::Never)
            && matches!(
                nodes[id.0].kind,
                NodeKind::BooleanLiteral(_) | NodeKind::IntegerLiteral(_)
            )
        {
            return;
        }
        match &nodes[id.0].kind {
            NodeKind::Local(_)
            | NodeKind::Global(_)
            | NodeKind::FieldAccess { .. }
            | NodeKind::Deref(_)
            | NodeKind::Index { .. }
            | NodeKind::Slice { .. } => {
                let ty = nodes[id.0].ty.clone();
                let (src, src_meta) = self.emit_place(nodes, id);
                if self.is_sized(&ty) {
                    let size = self.type_size(&ty);
                    self.emit_copy(dst, &src, &ty, &size.to_string());
                } else {
                    // Unsized place: `src`/`dst` are the pointee data.
                    let meta = src_meta.unwrap();
                    let size_expr = self.emit_full_size_expr(&ty, &meta);
                    self.emit_copy_contents(dst, &src, &ty, &size_expr);
                }
            }
            NodeKind::IntegerLiteral(n) => {
                let n = *n;
                let ty = &nodes[id.0].ty;
                let c_ty = self.c_int_type(ty);
                let literal = if ty.is_unsigned() {
                    format!("({c_ty}){}u", n as u64)
                } else {
                    format!("{n}")
                };
                self.linef(format!("*({c_ty}*){dst} = {literal};"));
            }
            NodeKind::BooleanLiteral(b) => {
                let v = if *b { 1 } else { 0 };
                self.linef(format!("*(uint8_t*){dst} = {v};"));
            }
            NodeKind::Null => {
                // null#[T]: zero pointer (plus zero meta for the fat-pointer case).
                self.linef(format!("*(uint8_t**){dst} = (uint8_t*)0;"));
                if matches!(nodes[id.0].ty, Type::NullableRefUnsized(_)) {
                    self.linef(format!("*(uint64_t*)({dst} + 8) = 0;"));
                }
            }
            NodeKind::Ref(inner) => {
                let inner = *inner;
                let inner_ty = &nodes[inner.0].ty;
                if is_place(nodes, inner) {
                    let (place, place_meta) = self.emit_place(nodes, inner);
                    if self.is_sized(inner_ty) {
                        self.linef(format!("*(uint8_t**){dst} = {place};"));
                    } else {
                        let meta = place_meta.unwrap();
                        let wide_tmp = self.fresh_tmp();
                        self.linef(format!(
                            "uint8_t {wide_tmp}[16] __attribute__((aligned(16)));"
                        ));
                        self.linef(format!("*(uint8_t**){wide_tmp} = {place};"));
                        self.linef(format!("*(uint64_t*)({wide_tmp} + 8) = {meta};"));
                        self.linef(format!("sol_store_128_unordered({dst}, {wide_tmp});"));
                    }
                } else {
                    // &expr where expr is not a place — alloc temp
                    let inner_ty_clone = inner_ty.clone();
                    if self.is_sized(&inner_ty_clone) {
                        let size = self.type_size(&inner_ty_clone);
                        let align = self.type_align(&inner_ty_clone);
                        let mf = self.mark_fn_expr(&inner_ty_clone);
                        let tmp = self.fresh_tmp();
                        self.emit_alloc(&tmp, size, align, &mf);
                        self.emit_into(nodes, inner, &tmp);
                        self.linef(format!("*(uint8_t**){dst} = {tmp};"));
                    } else {
                        let meta = self.emit_meta(nodes, inner).unwrap();
                        let align = self.type_align(&inner_ty_clone);
                        let mf = self.mark_fn_expr(&inner_ty_clone);
                        let size_expr = self.emit_full_size_expr(&inner_ty_clone, &meta);
                        let tmp = self.fresh_tmp();
                        self.emit_alloc(&tmp, &size_expr, align, &mf);
                        self.emit_into(nodes, inner, &tmp);
                        let wide_tmp = self.fresh_tmp();
                        self.linef(format!(
                            "uint8_t {wide_tmp}[16] __attribute__((aligned(16)));"
                        ));
                        self.linef(format!("*(uint8_t**){wide_tmp} = {tmp};"));
                        self.linef(format!("*(uint64_t*)({wide_tmp} + 8) = {meta};"));
                        self.linef(format!("sol_store_128_unordered({dst}, {wide_tmp});"));
                    }
                }
            }
            NodeKind::Unique(inner) => {
                // Unique reference creation: always allocates fresh memory
                let inner = *inner;
                let inner_ty_clone = nodes[inner.0].ty.clone();
                if self.is_sized(&inner_ty_clone) {
                    let size = self.type_size(&inner_ty_clone);
                    let align = self.type_align(&inner_ty_clone);
                    let mf = self.mark_fn_expr(&inner_ty_clone);
                    let tmp = self.fresh_tmp();
                    self.emit_alloc(&tmp, size, align, &mf);
                    self.emit_into(nodes, inner, &tmp);
                    self.linef(format!("*(uint8_t**){dst} = {tmp};"));
                } else {
                    let meta = self.emit_meta(nodes, inner).unwrap();
                    let align = self.type_align(&inner_ty_clone);
                    let mf = self.mark_fn_expr(&inner_ty_clone);
                    let size_expr = self.emit_full_size_expr(&inner_ty_clone, &meta);
                    let tmp = self.fresh_tmp();
                    self.emit_alloc(&tmp, &size_expr, align, &mf);
                    self.emit_into(nodes, inner, &tmp);
                    let wide_tmp = self.fresh_tmp();
                    self.linef(format!(
                        "uint8_t {wide_tmp}[16] __attribute__((aligned(16)));"
                    ));
                    self.linef(format!("*(uint8_t**){wide_tmp} = {tmp};"));
                    self.linef(format!("*(uint64_t*)({wide_tmp} + 8) = {meta};"));
                    self.linef(format!("sol_store_128_unordered({dst}, {wide_tmp});"));
                }
            }
            NodeKind::StructLiteral { name, fields } => {
                let name = name.clone();
                let field_inits: Vec<(String, NodeId)> = fields.clone();
                for (fname, fnode) in &field_inits {
                    let fl = self.module.datatypes[name.as_str()]
                        .fields
                        .iter()
                        .find(|f| f.name == *fname)
                        .unwrap();
                    let offset = fl.offset;
                    if offset == 0 {
                        self.emit_into(nodes, *fnode, dst);
                    } else {
                        self.emit_into(nodes, *fnode, &format!("({dst} + {offset})"));
                    }
                }
            }
            NodeKind::ArrayLiteral(elements) => {
                let elem_ids: Vec<NodeId> = elements.clone();
                let elem_ty = match &nodes[id.0].ty {
                    Type::Array(inner) | Type::FixedArray(inner, _) => (**inner).clone(),
                    _ => unreachable!(),
                };
                let es = self.type_size(&elem_ty);
                for (i, eid) in elem_ids.iter().enumerate() {
                    if i == 0 {
                        self.emit_into(nodes, *eid, dst);
                    } else {
                        self.emit_into(nodes, *eid, &format!("({dst} + {})", i * es));
                    }
                }
            }
            NodeKind::ArrayRepeat { element, count } => {
                let element = *element;
                let count = *count;
                let elem_ty = match &nodes[id.0].ty {
                    Type::Array(inner) | Type::FixedArray(inner, _) => (**inner).clone(),
                    _ => unreachable!(),
                };
                let es = self.type_size(&elem_ty);
                let cnt_expr = self.emit_load(nodes, count);
                let cnt_tmp = self.fresh_tmp();
                self.linef(format!("uint64_t {cnt_tmp} = (uint64_t){cnt_expr};"));
                // Evaluate element into first slot
                self.linef(format!("if ({cnt_tmp} > 0) {{"));
                self.indent += 1;
                self.emit_into(nodes, element, dst);
                let idx = self.fresh_tmp();
                self.linef(format!(
                    "for (uint64_t {idx} = 1; {idx} < {cnt_tmp}; {idx}++) {{"
                ));
                self.indent += 1;
                let edst = format!("{dst} + {idx} * {es}");
                self.emit_copy(&edst, dst, &elem_ty, &es.to_string());
                self.indent -= 1;
                self.line("}");
                self.indent -= 1;
                self.line("}");
            }
            NodeKind::ArraySizeCoerce { value, size } => {
                let value = *value;
                let size = *size;
                // Runtime check: meta == size. Throws a catchable Solar
                // exception (same message as the interpreters); the call is
                // kept off the happy path behind the compare. The check runs
                // BEFORE the copy — `dst` is sized for `size` elements, so a
                // longer source would write past it.
                let meta = self.emit_meta(nodes, value).unwrap();
                self.linef(format!(
                    "if ((uint64_t){meta} != {size}u) {{ sol_assert_array_len((uint64_t){meta}, {size}u); }}"
                ));
                self.emit_into(nodes, value, dst);
            }
            NodeKind::BinaryOp { op, left, right } => {
                let op = *op;
                let left = *left;
                let right = *right;
                let result_ty = nodes[id.0].ty.clone();
                let left_ty = nodes[left.0].ty.clone();
                let left_inner = match &left_ty {
                    Type::Array(inner) | Type::FixedArray(inner, _) => Some((**inner).clone()),
                    _ => None,
                };
                if let Some(inner) = left_inner.filter(|_| op == BinOp::Add) {
                    let es = self.type_size(&inner);
                    let ea = self.type_align(&inner);
                    let mf = if self.type_contains_gc_ptr(&inner) {
                        "_mark_ptr_array"
                    } else {
                        "_mark_noop"
                    };
                    let lm = self.emit_meta(nodes, left).unwrap();
                    let la_tmp = self.fresh_tmp();
                    self.emit_alloc(&la_tmp, format!("{lm} * {es}"), ea, mf);
                    self.emit_into(nodes, left, &la_tmp);
                    let rm = self.emit_meta(nodes, right).unwrap();
                    let ra_tmp = self.fresh_tmp();
                    self.emit_alloc(&ra_tmp, format!("{rm} * {es}"), ea, mf);
                    self.emit_into(nodes, right, &ra_tmp);
                    // Concatenation copies array *data* halves, not fat values.
                    let left_size = format!("{lm} * {es}");
                    self.emit_copy_contents(
                        dst,
                        &la_tmp,
                        &Type::Array(Box::new(inner.clone())),
                        &left_size,
                    );
                    let right_dst = format!("({dst} + {lm} * {es})");
                    let right_size = format!("{rm} * {es}");
                    self.emit_copy_contents(
                        &right_dst,
                        &ra_tmp,
                        &Type::Array(Box::new(inner)),
                        &right_size,
                    );
                } else {
                    let val = self.emit_load_binop(nodes, op, left, right, &result_ty, &left_ty);
                    let c_ty = self.c_int_type(&result_ty);
                    self.linef(format!("*({c_ty}*){dst} = {val};"));
                }
            }
            NodeKind::Not(_) => {
                let result_ty = nodes[id.0].ty.clone();
                let val = self.emit_load(nodes, id);
                let c_ty = self.c_int_type(&result_ty);
                self.linef(format!("*({c_ty}*){dst} = {val};"));
            }
            NodeKind::IfExpr {
                condition,
                then_body,
                else_body,
            } => {
                let condition = *condition;
                let then_body = then_body.clone();
                let else_body = else_body.clone();
                let cond = self.emit_load(nodes, condition);
                self.linef(format!("if ((uint8_t){cond}) {{"));
                self.indent += 1;
                self.emit_branch_into(nodes, &then_body, dst);
                self.indent -= 1;
                self.line("} else {");
                self.indent += 1;
                self.emit_branch_into(nodes, &else_body, dst);
                self.indent -= 1;
                self.line("}");
            }
            NodeKind::Loop { body } => {
                // Loop expression: `break <v>` assigns its value into `dst`.
                let body = body.clone();
                self.loop_dst.push(dst.to_string());
                self.line("while (1) {");
                self.indent += 1;
                for &stmt_id in &body {
                    self.emit_stmt(nodes, stmt_id);
                }
                self.indent -= 1;
                self.line("}");
                self.loop_dst.pop();
            }
            NodeKind::EnumVariant {
                enum_name,
                variant_name,
                variant_index,
                value,
            } => {
                let enum_name = enum_name.clone();
                let variant_name = variant_name.clone();
                let variant_index = *variant_index;
                let value = *value;
                // Write the payload first, if present
                if let Some(val_id) = value {
                    let dt = &self.module.datatypes[enum_name.as_str()];
                    let fl = dt.fields.iter().find(|f| f.name == variant_name).unwrap();
                    let offset = fl.offset;
                    if offset == 0 {
                        self.emit_into(nodes, val_id, dst);
                    } else {
                        self.emit_into(nodes, val_id, &format!("({dst} + {offset})"));
                    }
                }
                // Write the discriminant last, with release ordering, so a
                // concurrent reader (e.g. the GC marker) that observes the
                // tag also observes the payload it describes.
                self.linef(format!(
                    "__atomic_store_n((uint64_t*){dst}, {variant_index}u, __ATOMIC_RELEASE);"
                ));
            }
            NodeKind::Match { scrutinee, arms } => {
                let scrutinee = *scrutinee;
                let arms = arms.clone();
                let enum_ty = nodes[scrutinee.0].ty.clone();
                let enum_name = match &enum_ty {
                    Type::Enum(name) => name.clone(),
                    _ => unreachable!(),
                };
                // Get enum base address
                let enum_base = if is_place(nodes, scrutinee) {
                    let (place, _) = self.emit_place(nodes, scrutinee);
                    place
                } else {
                    let s = self.type_size(&enum_ty);
                    let a = self.type_align(&enum_ty);
                    let mf = self.mark_fn_expr(&enum_ty);
                    let tmp = self.fresh_tmp();
                    self.emit_alloc(&tmp, s, a, &mf);
                    self.emit_into(nodes, scrutinee, &tmp);
                    tmp
                };
                // Load discriminant with acquire ordering (pairs with the
                // release store in variant construction/copy)
                let disc = self.fresh_tmp();
                self.linef(format!(
                    "uint64_t {disc} = __atomic_load_n((uint64_t*){enum_base}, __ATOMIC_ACQUIRE);"
                ));
                // Emit if-else chain
                for (i, arm) in arms.iter().enumerate() {
                    let is_wildcard = matches!(arm.pattern, MatchPattern::Wildcard(_, _));
                    if i == 0 {
                        if is_wildcard {
                            self.line("{");
                        } else {
                            let idx = match &arm.pattern {
                                MatchPattern::Variant { variant_index, .. } => *variant_index,
                                _ => unreachable!(),
                            };
                            self.linef(format!("if ({disc} == {idx}u) {{"));
                        }
                    } else if is_wildcard {
                        self.line("} else {");
                    } else {
                        let idx = match &arm.pattern {
                            MatchPattern::Variant { variant_index, .. } => *variant_index,
                            _ => unreachable!(),
                        };
                        self.linef(format!("}} else if ({disc} == {idx}u) {{"));
                    }
                    self.indent += 1;
                    // Bind pattern variable
                    match &arm.pattern {
                        MatchPattern::Variant {
                            variant_name,
                            binding: Some((var, _)),
                            ..
                        } => {
                            let dt = &self.module.datatypes[enum_name.as_str()];
                            let fl = dt.fields.iter().find(|f| f.name == *variant_name).unwrap();
                            let offset = fl.offset;
                            self.linef(format!("uint8_t* _v{} = {enum_base} + {offset};", var.0));
                        }
                        MatchPattern::Wildcard(var, _) => {
                            self.linef(format!("uint8_t* _v{} = {enum_base};", var.0));
                        }
                        _ => {}
                    }
                    self.emit_branch_into(nodes, &arm.body, dst);
                    self.indent -= 1;
                }
                self.line("}");
            }
            NodeKind::FunctionRef(name) => {
                let cname = self.func_name(name);
                let wide_tmp = self.fresh_tmp();
                self.linef(format!(
                    "uint8_t {wide_tmp}[16] __attribute__((aligned(16)));"
                ));
                self.linef(format!("*(void(**)()){wide_tmp} = (void(*)())&{cname};"));
                self.linef(format!("*(void**)({wide_tmp} + 8) = NULL;"));
                self.linef(format!("sol_store_128_unordered({dst}, {wide_tmp});"));
            }
            NodeKind::MakeClosure { function, captures } => {
                let cname = self.func_name(function);
                let capture_ids: Vec<NodeId> = captures.clone();
                let n = capture_ids.len();
                let wide_tmp = self.fresh_tmp();
                self.linef(format!(
                    "uint8_t {wide_tmp}[16] __attribute__((aligned(16)));"
                ));
                if n > 0 {
                    let env_tmp = self.fresh_tmp();
                    // 16-byte slots: a sized capture writes a thin pointer; an
                    // unsized one (e.g. a `[Uint8]`) writes a fat pointer (ptr+meta).
                    self.emit_alloc(&env_tmp, n * 16, 8, "_mark_ptr_array");
                    for (i, &cap_id) in capture_ids.iter().enumerate() {
                        let slot = format!("({env_tmp} + {})", i * 16);
                        self.emit_into(nodes, cap_id, &slot);
                    }
                    self.linef(format!("*(void(**)()){wide_tmp} = (void(*)())&{cname};"));
                    self.linef(format!("*(void**)({wide_tmp} + 8) = (void*){env_tmp};"));
                } else {
                    self.linef(format!("*(void(**)()){wide_tmp} = (void(*)())&{cname};"));
                    self.linef(format!("*(void**)({wide_tmp} + 8) = NULL;"));
                }
                self.linef(format!("sol_store_128_unordered({dst}, {wide_tmp});"));
            }
            NodeKind::IntrinsicCall { intrinsic, args } => {
                let intrinsic = intrinsic.clone();
                let args: Vec<NodeId> = args.clone();
                let result_ty = nodes[id.0].ty.clone();
                self.emit_intrinsic(nodes, &intrinsic, &args, &result_ty, dst);
            }
            NodeKind::Call { function, args } => {
                let function = function.clone();
                let args: Vec<NodeId> = args.clone();
                let result_ty = nodes[id.0].ty.clone();

                if matches!(result_ty, Type::Unit | Type::Never) {
                    let call_expr = self.emit_call_expr(nodes, &function, &args);
                    self.linef(format!("{call_expr};"));
                } else {
                    let s = self.type_size(&result_ty);
                    let vt = self.val_type(&result_ty);
                    let call_expr = self.emit_call_expr(nodes, &function, &args);
                    let tmp = self.fresh_tmp();
                    self.linef(format!("{vt} {tmp} = {call_expr};"));
                    let src_expr = format!("(uint8_t*)&{tmp}");
                    self.emit_copy(dst, &src_expr, &result_ty, &s.to_string());
                }
            }
            NodeKind::ArrayInit { count, init } => {
                let count = *count;
                let init = *init;
                let elem_ty = match &nodes[id.0].ty {
                    Type::Array(inner) | Type::FixedArray(inner, _) => (**inner).clone(),
                    _ => unreachable!(),
                };
                let es = self.type_size(&elem_ty);
                let cnt_expr = self.emit_load(nodes, count);
                let cnt_tmp = self.fresh_tmp();
                self.linef(format!("uint64_t {cnt_tmp} = (uint64_t){cnt_expr};"));

                // Eval init closure into a 16-byte tmp
                let callee_ty = nodes[init.0].ty.clone();
                let cs = self.type_size(&callee_ty);
                let ca = self.type_align(&callee_ty);
                let callee_tmp = self.fresh_tmp();
                self.emit_alloc(&callee_tmp, cs, ca, "_mark_fn_value");
                self.emit_into(nodes, init, &callee_tmp);
                let fp_var = self.fresh_tmp();
                let env_var = self.fresh_tmp();
                self.linef(format!("void(*{fp_var})() = *(void(**)()){callee_tmp};"));
                self.linef(format!("void* {env_var} = *(void**)({callee_tmp} + 8);"));

                // Build function pointer type: returns the element value type,
                // takes (void* env, index value type).
                let ret_vt = self.val_type(&elem_ty);
                let idx_vt = Self::val_type_name(8, 8, &[]);
                let fp_type = format!("{ret_vt}(*)(void*, {idx_vt})");

                let idx = self.fresh_tmp();
                self.linef(format!(
                    "for (uint64_t {idx} = 0; {idx} < {cnt_tmp}; {idx}++) {{"
                ));
                self.indent += 1;
                // Wrap index value
                let idx_wrapped = self.fresh_tmp();
                self.linef(format!("{idx_vt} {idx_wrapped};"));
                self.linef(format!("*(uint64_t*)&{idx_wrapped} = {idx};"));
                let result_tmp = self.fresh_tmp();
                self.linef(format!(
                    "{ret_vt} {result_tmp} = (({fp_type}){fp_var})({env_var}, {idx_wrapped});"
                ));
                let edst = format!("({dst} + {idx} * {es})");
                let esrc = format!("(uint8_t*)&{result_tmp}");
                self.emit_copy(&edst, &esrc, &elem_ty, &es.to_string());
                self.indent -= 1;
                self.line("}");
            }
            NodeKind::CallIndirect { callee, args } => {
                let callee = *callee;
                let args: Vec<NodeId> = args.clone();

                // Get the function type from the callee
                let (param_types, return_type) = match &nodes[callee.0].ty {
                    Type::Function {
                        params,
                        return_type,
                    } => (params.clone(), (**return_type).clone()),
                    _ => unreachable!(),
                };

                // Load 16-byte function value into temp
                let callee_ty = nodes[callee.0].ty.clone();
                let cs = self.type_size(&callee_ty);
                let ca = self.type_align(&callee_ty);
                let callee_tmp = self.fresh_tmp();
                self.emit_alloc(&callee_tmp, cs, ca, "_mark_fn_value");
                self.emit_into(nodes, callee, &callee_tmp);
                let fp_var = self.fresh_tmp();
                let env_var = self.fresh_tmp();
                self.linef(format!("void(*{fp_var})() = *(void(**)()){callee_tmp};"));
                self.linef(format!("void* {env_var} = *(void**)({callee_tmp} + 8);"));

                // Build arg val-type wrappers — prepend env
                let mut arg_exprs: Vec<String> = vec![env_var.clone()];
                for (pty, &arg) in param_types.iter().zip(args.iter()) {
                    let vt = self.val_type(pty);
                    let ptmp = self.fresh_tmp();
                    self.linef(format!("{vt} {ptmp};"));
                    self.emit_into(nodes, arg, &format!("(uint8_t*)&{ptmp}"));
                    arg_exprs.push(ptmp);
                }
                let args_str = arg_exprs.join(", ");

                // Build function pointer type with uint64_t env as first param
                let ret_vt = if matches!(return_type, Type::Unit | Type::Never) {
                    "void".to_string()
                } else {
                    self.val_type(&return_type)
                };
                let mut param_vts: Vec<String> = vec!["void*".to_string()];
                for pty in &param_types {
                    param_vts.push(self.val_type(pty));
                }
                let fp_type = format!("{ret_vt}(*)({})", param_vts.join(", "));

                if matches!(return_type, Type::Unit | Type::Never) {
                    self.linef(format!("(({fp_type}){fp_var})({args_str});"));
                } else {
                    let s = self.type_size(&return_type);
                    let vt = self.val_type(&return_type);
                    let tmp = self.fresh_tmp();
                    self.linef(format!("{vt} {tmp} = (({fp_type}){fp_var})({args_str});"));
                    let src_expr = format!("(uint8_t*)&{tmp}");
                    self.emit_copy(dst, &src_expr, &return_type, &s.to_string());
                }
            }
            _ => unreachable!("emit_into on statement node: {:?}", nodes[id.0].kind),
        }
    }

    fn emit_intrinsic(
        &mut self,
        nodes: &[Node],
        intrinsic: &Intrinsic,
        args: &[NodeId],
        result_ty: &Type,
        dst: &str,
    ) {
        match intrinsic {
            Intrinsic::Panic => {
                let (ref_place, _) = self.emit_place(nodes, args[0]);
                let data_ptr = self.fresh_tmp();
                let data_len = self.fresh_tmp();
                self.linef(format!("uint8_t* {data_ptr} = *(uint8_t**){ref_place};"));
                self.linef(format!(
                    "uint64_t {data_len} = *(uint64_t*)({ref_place} + 8);"
                ));
                self.linef(format!("sol_panic({data_ptr}, {data_len});"));
            }
            Intrinsic::Throw => {
                // arg[0] is a &[Uint8] fat pointer (ptr + len). Unwind with it.
                let (ref_place, _) = self.emit_place(nodes, args[0]);
                let data_ptr = self.fresh_tmp();
                let data_len = self.fresh_tmp();
                self.linef(format!("uint8_t* {data_ptr} = *(uint8_t**){ref_place};"));
                self.linef(format!(
                    "uint64_t {data_len} = *(uint64_t*)({ref_place} + 8);"
                ));
                self.linef(format!("sol_throw({data_ptr}, {data_len});"));
            }
            Intrinsic::Try => {
                // args are two 16-byte function values (code ptr + env ptr):
                // [0] body fn(), [1] handler fn(&[Uint8]).
                let (body_place, _) = self.emit_place(nodes, args[0]);
                let (handler_place, _) = self.emit_place(nodes, args[1]);
                let body_fn = self.fresh_tmp();
                let body_env = self.fresh_tmp();
                let handler_fn = self.fresh_tmp();
                let handler_env = self.fresh_tmp();
                self.linef(format!("void* {body_fn} = *(void**){body_place};"));
                self.linef(format!("void* {body_env} = *(void**)({body_place} + 8);"));
                self.linef(format!("void* {handler_fn} = *(void**){handler_place};"));
                self.linef(format!(
                    "void* {handler_env} = *(void**)({handler_place} + 8);"
                ));
                self.linef(format!(
                    "sol_try({body_fn}, {body_env}, {handler_fn}, {handler_env});"
                ));
            }
            Intrinsic::FileOpen => {
                // args: &[Uint8] path (fat pointer), Int flags, Uint mode.
                // Returns a FileDesc (opaque uint8_t* into the fd arena).
                let (ref_place, _) = self.emit_place(nodes, args[0]);
                let flags = self.emit_load(nodes, args[1]);
                let mode = self.emit_load(nodes, args[2]);
                let data_ptr = self.fresh_tmp();
                let data_len = self.fresh_tmp();
                self.linef(format!("uint8_t* {data_ptr} = *(uint8_t**){ref_place};"));
                self.linef(format!(
                    "uint64_t {data_len} = *(uint64_t*)({ref_place} + 8);"
                ));
                self.linef(format!(
                    "*(uint8_t**){dst} = sol_file_open({data_ptr}, {data_len}, (int64_t){flags}, (uint64_t){mode});"
                ));
            }
            Intrinsic::FileClose => {
                // arg is a FileDesc (opaque uint8_t* into the fd arena). Neuters
                // the fd in place — no result.
                let fd = self.emit_load(nodes, args[0]);
                self.linef(format!("sol_file_close((uint8_t*){fd});"));
            }
            Intrinsic::FileStdin => {
                // No args; returns a FileDesc for stdin (opaque uint8_t*).
                self.linef(format!("*(uint8_t**){dst} = sol_file_stdin();"));
            }
            Intrinsic::FileStdout => {
                // No args; returns a FileDesc for stdout (opaque uint8_t*).
                self.linef(format!("*(uint8_t**){dst} = sol_file_stdout();"));
            }
            Intrinsic::FileRead => {
                // args: FileDesc, &[Uint8] dst (fat pointer). Returns bytes read.
                let fd = self.emit_load(nodes, args[0]);
                let (ref_place, _) = self.emit_place(nodes, args[1]);
                let data_ptr = self.fresh_tmp();
                let data_len = self.fresh_tmp();
                self.linef(format!("uint8_t* {data_ptr} = *(uint8_t**){ref_place};"));
                self.linef(format!(
                    "uint64_t {data_len} = *(uint64_t*)({ref_place} + 8);"
                ));
                let c_ty = self.c_int_type(result_ty);
                self.linef(format!(
                    "*({c_ty}*){dst} = ({c_ty})sol_file_read((uint8_t*){fd}, {data_ptr}, {data_len});"
                ));
            }
            Intrinsic::FileWritePartial => {
                // args: FileDesc, &[Uint8] src (fat pointer). Returns bytes written.
                let fd = self.emit_load(nodes, args[0]);
                let (ref_place, _) = self.emit_place(nodes, args[1]);
                let data_ptr = self.fresh_tmp();
                let data_len = self.fresh_tmp();
                self.linef(format!("uint8_t* {data_ptr} = *(uint8_t**){ref_place};"));
                self.linef(format!(
                    "uint64_t {data_len} = *(uint64_t*)({ref_place} + 8);"
                ));
                let c_ty = self.c_int_type(result_ty);
                self.linef(format!(
                    "*({c_ty}*){dst} = ({c_ty})sol_file_write_partial((uint8_t*){fd}, {data_ptr}, {data_len});"
                ));
            }
            Intrinsic::FileStderr => {
                // No args; returns a FileDesc for stderr (opaque uint8_t*).
                self.linef(format!("*(uint8_t**){dst} = sol_file_stderr();"));
            }
            Intrinsic::FileReadAt | Intrinsic::FileWriteAt => {
                // args: FileDesc, &[Uint8] buffer (fat pointer), Uint absolute
                // offset. Returns bytes transferred by the single pread/pwrite.
                let f = if matches!(intrinsic, Intrinsic::FileReadAt) {
                    "sol_file_read_at"
                } else {
                    "sol_file_write_at"
                };
                let fd = self.emit_load(nodes, args[0]);
                let (ref_place, _) = self.emit_place(nodes, args[1]);
                let offset = self.emit_load(nodes, args[2]);
                let data_ptr = self.fresh_tmp();
                let data_len = self.fresh_tmp();
                self.linef(format!("uint8_t* {data_ptr} = *(uint8_t**){ref_place};"));
                self.linef(format!(
                    "uint64_t {data_len} = *(uint64_t*)({ref_place} + 8);"
                ));
                let c_ty = self.c_int_type(result_ty);
                self.linef(format!(
                    "*({c_ty}*){dst} = ({c_ty}){f}((uint8_t*){fd}, {data_ptr}, {data_len}, (uint64_t){offset});"
                ));
            }
            Intrinsic::FileSync => {
                // arg: FileDesc. fsync(2); no result.
                let fd = self.emit_load(nodes, args[0]);
                self.linef(format!("sol_file_sync((uint8_t*){fd});"));
            }
            Intrinsic::FileLock => {
                // args: FileDesc, Int flock(2) LOCK_* op. Returns Bool (false =
                // non-blocking request would have to wait).
                let fd = self.emit_load(nodes, args[0]);
                let op = self.emit_load(nodes, args[1]);
                let c_ty = self.c_int_type(result_ty);
                self.linef(format!(
                    "*({c_ty}*){dst} = ({c_ty})sol_file_lock((uint8_t*){fd}, (int64_t){op});"
                ));
            }
            Intrinsic::FileRemove | Intrinsic::DirRemove => {
                // arg: &[Uint8] path (fat pointer). unlink(2)/rmdir(2); no result.
                let f = if matches!(intrinsic, Intrinsic::FileRemove) {
                    "sol_file_remove"
                } else {
                    "sol_dir_remove"
                };
                let (ref_place, _) = self.emit_place(nodes, args[0]);
                let data_ptr = self.fresh_tmp();
                let data_len = self.fresh_tmp();
                self.linef(format!("uint8_t* {data_ptr} = *(uint8_t**){ref_place};"));
                self.linef(format!(
                    "uint64_t {data_len} = *(uint64_t*)({ref_place} + 8);"
                ));
                self.linef(format!("{f}({data_ptr}, {data_len});"));
            }
            Intrinsic::FileRename => {
                // args: &[Uint8] old path, &[Uint8] new path. rename(2).
                let (old_place, _) = self.emit_place(nodes, args[0]);
                let (new_place, _) = self.emit_place(nodes, args[1]);
                let old_ptr = self.fresh_tmp();
                let old_len = self.fresh_tmp();
                let new_ptr = self.fresh_tmp();
                let new_len = self.fresh_tmp();
                self.linef(format!("uint8_t* {old_ptr} = *(uint8_t**){old_place};"));
                self.linef(format!(
                    "uint64_t {old_len} = *(uint64_t*)({old_place} + 8);"
                ));
                self.linef(format!("uint8_t* {new_ptr} = *(uint8_t**){new_place};"));
                self.linef(format!(
                    "uint64_t {new_len} = *(uint64_t*)({new_place} + 8);"
                ));
                self.linef(format!(
                    "sol_file_rename({old_ptr}, {old_len}, {new_ptr}, {new_len});"
                ));
            }
            Intrinsic::DirCreate => {
                // args: &[Uint8] path, Uint permission mode. mkdir(2).
                let (ref_place, _) = self.emit_place(nodes, args[0]);
                let mode = self.emit_load(nodes, args[1]);
                let data_ptr = self.fresh_tmp();
                let data_len = self.fresh_tmp();
                self.linef(format!("uint8_t* {data_ptr} = *(uint8_t**){ref_place};"));
                self.linef(format!(
                    "uint64_t {data_len} = *(uint64_t*)({ref_place} + 8);"
                ));
                self.linef(format!(
                    "sol_dir_create({data_ptr}, {data_len}, (uint64_t){mode});"
                ));
            }
            Intrinsic::FileStat => {
                // args: &[Uint8] path, three &Uint64 out-params (size, mtime
                // nanos, kind). Returns Bool (false = path doesn't exist).
                let (ref_place, _) = self.emit_place(nodes, args[0]);
                let size_ptr = self.emit_load(nodes, args[1]);
                let mtime_ptr = self.emit_load(nodes, args[2]);
                let kind_ptr = self.emit_load(nodes, args[3]);
                let data_ptr = self.fresh_tmp();
                let data_len = self.fresh_tmp();
                self.linef(format!("uint8_t* {data_ptr} = *(uint8_t**){ref_place};"));
                self.linef(format!(
                    "uint64_t {data_len} = *(uint64_t*)({ref_place} + 8);"
                ));
                let c_ty = self.c_int_type(result_ty);
                self.linef(format!(
                    "*({c_ty}*){dst} = ({c_ty})sol_file_stat({data_ptr}, {data_len}, (uint64_t*){size_ptr}, (uint64_t*){mtime_ptr}, (uint64_t*){kind_ptr});"
                ));
            }
            Intrinsic::DirRead => {
                // arg: FileDesc of a directory. The runtime builds the batch's
                // `&[&[Uint8]]` and writes its 16-byte fat pointer into `dst`.
                let fd = self.emit_load(nodes, args[0]);
                self.linef(format!("sol_dir_read((uint8_t*){fd}, (uint8_t*){dst});"));
            }
            Intrinsic::SocketCreate => {
                // args: Int domain, Int type, Int protocol. Returns a FileDesc.
                let domain = self.emit_load(nodes, args[0]);
                let ty = self.emit_load(nodes, args[1]);
                let protocol = self.emit_load(nodes, args[2]);
                self.linef(format!(
                    "*(uint8_t**){dst} = sol_socket_create((int64_t){domain}, (int64_t){ty}, (int64_t){protocol});"
                ));
            }
            Intrinsic::SocketBind | Intrinsic::SocketConnect => {
                // args: FileDesc, &[Uint8] raw sockaddr bytes (fat pointer).
                let f = if matches!(intrinsic, Intrinsic::SocketBind) {
                    "sol_socket_bind"
                } else {
                    "sol_socket_connect"
                };
                let fd = self.emit_load(nodes, args[0]);
                let (ref_place, _) = self.emit_place(nodes, args[1]);
                let data_ptr = self.fresh_tmp();
                let data_len = self.fresh_tmp();
                self.linef(format!("uint8_t* {data_ptr} = *(uint8_t**){ref_place};"));
                self.linef(format!(
                    "uint64_t {data_len} = *(uint64_t*)({ref_place} + 8);"
                ));
                self.linef(format!("{f}((uint8_t*){fd}, {data_ptr}, {data_len});"));
            }
            Intrinsic::SocketListen | Intrinsic::SocketShutdown => {
                // args: FileDesc, Int (backlog / how).
                let f = if matches!(intrinsic, Intrinsic::SocketListen) {
                    "sol_socket_listen"
                } else {
                    "sol_socket_shutdown"
                };
                let fd = self.emit_load(nodes, args[0]);
                let arg = self.emit_load(nodes, args[1]);
                self.linef(format!("{f}((uint8_t*){fd}, (int64_t){arg});"));
            }
            Intrinsic::SocketAccept => {
                // arg: FileDesc of a listening socket. Returns the connection.
                let fd = self.emit_load(nodes, args[0]);
                self.linef(format!(
                    "*(uint8_t**){dst} = sol_socket_accept((uint8_t*){fd});"
                ));
            }
            Intrinsic::SocketSetOption => {
                // args: FileDesc, Int level, Int name, Int value.
                let fd = self.emit_load(nodes, args[0]);
                let level = self.emit_load(nodes, args[1]);
                let name = self.emit_load(nodes, args[2]);
                let value = self.emit_load(nodes, args[3]);
                self.linef(format!(
                    "sol_socket_set_option((uint8_t*){fd}, (int64_t){level}, (int64_t){name}, (int64_t){value});"
                ));
            }
            Intrinsic::SocketLocalAddr => {
                // args: FileDesc, &[Uint8] dst buffer. Returns the address len.
                let fd = self.emit_load(nodes, args[0]);
                let (ref_place, _) = self.emit_place(nodes, args[1]);
                let data_ptr = self.fresh_tmp();
                let data_len = self.fresh_tmp();
                self.linef(format!("uint8_t* {data_ptr} = *(uint8_t**){ref_place};"));
                self.linef(format!(
                    "uint64_t {data_len} = *(uint64_t*)({ref_place} + 8);"
                ));
                let c_ty = self.c_int_type(result_ty);
                self.linef(format!(
                    "*({c_ty}*){dst} = ({c_ty})sol_socket_local_addr((uint8_t*){fd}, {data_ptr}, {data_len});"
                ));
            }
            Intrinsic::Args | Intrinsic::Env => {
                // No args. The runtime builds the `&[&[Uint8]]` and writes its
                // 16-byte fat pointer (data ptr + count) directly into `dst`.
                let f = if matches!(intrinsic, Intrinsic::Args) {
                    "sol_args"
                } else {
                    "sol_env"
                };
                self.linef(format!("{f}((uint8_t*){dst});"));
            }
            Intrinsic::NumCpus => {
                let c_ty = self.c_int_type(result_ty);
                self.linef(format!("*({c_ty}*){dst} = ({c_ty})sol_num_cpus();"));
            }
            Intrinsic::Exit => {
                let code = self.emit_load(nodes, args[0]);
                self.linef(format!("sol_exit((int64_t){code});"));
            }
            Intrinsic::MonotonicTime | Intrinsic::SystemTime => {
                let f = if matches!(intrinsic, Intrinsic::MonotonicTime) {
                    "sol_monotonic_time"
                } else {
                    "sol_system_time"
                };
                let c_ty = self.c_int_type(result_ty);
                self.linef(format!("*({c_ty}*){dst} = ({c_ty}){f}();"));
            }
            Intrinsic::ArrayLen => {
                let len = if let Type::FixedArray(_, n) = &nodes[args[0].0].ty {
                    format!("{n}")
                } else {
                    self.emit_meta(nodes, args[0]).unwrap()
                };
                let c_ty = self.c_int_type(result_ty);
                self.linef(format!("*({c_ty}*){dst} = ({c_ty}){len};"));
            }
            Intrinsic::U64FromLe | Intrinsic::U32FromLe => {
                // Materialize the `[Uint8; N]` argument into a buffer, then copy
                // its N bytes into the result storage (`dst` is exactly N bytes).
                // On the little-endian target the byte copy IS the little-endian
                // decode. In release the buffer + memcpy collapse to one load.
                let n = if matches!(intrinsic, Intrinsic::U64FromLe) {
                    8
                } else {
                    4
                };
                let arg_ty = nodes[args[0].0].ty.clone();
                let s = self.type_size(&arg_ty);
                let a = self.type_align(&arg_ty);
                let mf = self.mark_fn_expr(&arg_ty);
                let buf = self.fresh_tmp();
                self.emit_alloc(&buf, s, a, &mf);
                self.emit_into(nodes, args[0], &buf);
                self.linef(format!("__builtin_memcpy({dst}, {buf}, {n});"));
            }
            Intrinsic::SimdMatchByteX16 | Intrinsic::SimdMatchHighBitX16 => {
                // Materialize the 16-lane byte vector into a buffer, then run the
                // SSE2 group-scan helper. In release the buffer collapses into a
                // single vector load feeding vpcmpeqb/vpmovmskb.
                let arg_ty = nodes[args[0].0].ty.clone();
                let s = self.type_size(&arg_ty);
                let a = self.type_align(&arg_ty);
                let mf = self.mark_fn_expr(&arg_ty);
                let buf = self.fresh_tmp();
                self.emit_alloc(&buf, s, a, &mf);
                self.emit_into(nodes, args[0], &buf);
                let call = if matches!(intrinsic, Intrinsic::SimdMatchByteX16) {
                    let tag = self.emit_load(nodes, args[1]);
                    format!("_sol_simd_match_byte_x16({buf}, (uint8_t)({tag}))")
                } else {
                    format!("_sol_simd_match_high_bit_x16({buf})")
                };
                self.linef(format!("*(uint64_t*){dst} = {call};"));
            }
            Intrinsic::AssertArrayLen => {
                let expected = self.emit_load(nodes, args[1]);
                let actual = if let Type::FixedArray(_, n) = &nodes[args[0].0].ty {
                    format!("{n}")
                } else {
                    self.emit_meta(nodes, args[0]).unwrap()
                };
                self.linef(format!(
                    "sol_assert_array_len((uint64_t){actual}, (uint64_t){expected});"
                ));
            }
            Intrinsic::ThreadSpawn => {
                // arg is a 16-byte function value (code ptr + env ptr)
                let (fn_place, _) = self.emit_place(nodes, args[0]);
                let fn_ptr = self.fresh_tmp();
                let env_ptr = self.fresh_tmp();
                self.linef(format!("void* {fn_ptr} = *(void**){fn_place};"));
                self.linef(format!("void* {env_ptr} = *(void**)({fn_place} + 8);"));
                self.linef(format!("sol_thread_spawn({fn_ptr}, {env_ptr});"));
            }
            Intrinsic::AtomicLoad => {
                // arg is &T — a pointer. Load the pointee atomically with acquire.
                let ref_val = self.emit_load(nodes, args[0]);
                let inner_ty = match &nodes[args[0].0].ty {
                    Type::Ref(inner) => (**inner).clone(),
                    _ => unreachable!(),
                };
                let size = self.type_size(&inner_ty);
                if size == 16 {
                    self.linef(format!(
                        "sol_atomic_load_128_acq((uint8_t*){dst}, (const uint8_t*){ref_val});"
                    ));
                } else {
                    let c_ty = self.c_atomic_type(size);
                    let tmp = self.fresh_tmp();
                    self.linef(format!(
                        "{c_ty} {tmp} = __atomic_load_n(({c_ty}*){ref_val}, __ATOMIC_ACQUIRE);"
                    ));
                    self.linef(format!("*({c_ty}*){dst} = {tmp};"));
                }
            }
            Intrinsic::AtomicStore => {
                // args[0] is &T, args[1] is T. Store value atomically with release.
                let ref_val = self.emit_load(nodes, args[0]);
                let inner_ty = match &nodes[args[0].0].ty {
                    Type::Ref(inner) => (**inner).clone(),
                    _ => unreachable!(),
                };
                let size = self.type_size(&inner_ty);
                if size == 16 {
                    let (val_place, _) = self.emit_place(nodes, args[1]);
                    self.linef(format!(
                        "sol_atomic_store_128_rel((uint8_t*){ref_val}, (const uint8_t*){val_place});"
                    ));
                } else {
                    let c_ty = self.c_atomic_type(size);
                    let val = self.emit_load(nodes, args[1]);
                    self.linef(format!(
                        "__atomic_store_n(({c_ty}*){ref_val}, ({c_ty}){val}, __ATOMIC_RELEASE);"
                    ));
                }
            }
            Intrinsic::AtomicExchange => {
                // args[0] is &T, args[1] is T. Atomically swap, return old value.
                let ref_val = self.emit_load(nodes, args[0]);
                let inner_ty = match &nodes[args[0].0].ty {
                    Type::Ref(inner) => (**inner).clone(),
                    _ => unreachable!(),
                };
                let size = self.type_size(&inner_ty);
                assert!(size <= 8, "atomic_exchange only supports sizes <= 8 bytes");
                let c_ty = self.c_atomic_type(size);
                let val = self.emit_load(nodes, args[1]);
                let tmp = self.fresh_tmp();
                self.linef(format!(
                    "{c_ty} {tmp} = __atomic_exchange_n(({c_ty}*){ref_val}, ({c_ty}){val}, __ATOMIC_ACQ_REL);"
                ));
                self.linef(format!("*({c_ty}*){dst} = {tmp};"));
            }
            Intrinsic::AtomicCompareExchange => {
                // args[0] is &T, args[1] is expected T, args[2] is new T.
                // Returns the old value (whether or not the swap succeeded).
                let ref_val = self.emit_load(nodes, args[0]);
                let inner_ty = match &nodes[args[0].0].ty {
                    Type::Ref(inner) => (**inner).clone(),
                    _ => unreachable!(),
                };
                let size = self.type_size(&inner_ty);
                if size == 16 {
                    let (exp_place, _) = self.emit_place(nodes, args[1]);
                    let (new_place, _) = self.emit_place(nodes, args[2]);
                    self.linef(format!(
                        "sol_atomic_compare_exchange_128_acq_rel((uint8_t*){dst}, (uint8_t*){ref_val}, (const uint8_t*){exp_place}, (const uint8_t*){new_place});"
                    ));
                } else {
                    let c_ty = self.c_atomic_type(size);
                    let exp = self.emit_load(nodes, args[1]);
                    let new_val = self.emit_load(nodes, args[2]);
                    let exp_tmp = self.fresh_tmp();
                    self.linef(format!("{c_ty} {exp_tmp} = ({c_ty}){exp};"));
                    // __atomic_compare_exchange_n writes the actual current value
                    // into &exp_tmp on either success or failure, giving us the old
                    // value to return.
                    self.linef(format!(
                        "(void)__atomic_compare_exchange_n(({c_ty}*){ref_val}, &{exp_tmp}, ({c_ty}){new_val}, 0, __ATOMIC_ACQ_REL, __ATOMIC_ACQUIRE);"
                    ));
                    self.linef(format!("*({c_ty}*){dst} = {exp_tmp};"));
                }
            }
            Intrinsic::FutexWait => {
                let ptr = self.emit_load(nodes, args[0]);
                let expected = self.emit_load(nodes, args[1]);
                let timeout = self.emit_load(nodes, args[2]);
                self.linef(format!(
                    "sol_futex_wait((uint32_t*){ptr}, (uint32_t){expected}, (uint64_t){timeout});"
                ));
            }
            Intrinsic::FutexWake => {
                let ptr = self.emit_load(nodes, args[0]);
                let count = self.emit_load(nodes, args[1]);
                self.linef(format!(
                    "sol_futex_wake((uint32_t*){ptr}, (uint32_t){count});"
                ));
            }
            Intrinsic::CountTrailingZeros | Intrinsic::CountLeadingZeros | Intrinsic::CountOnes => {
                // Lower to the clang/gcc builtins, which become llvm.cttz/ctlz/
                // ctpop and thus a single tzcnt/lzcnt/popcnt. The value is masked
                // to the operand's width; cttz/ctlz are guarded against 0 (their
                // builtins are undefined there) and return the full width instead.
                let arg_ty = nodes[args[0].0].ty.clone();
                let width = self.type_size(&arg_ty) * 8;
                let load_ty = self.c_int_type(&arg_ty);
                let val = self.emit_load(nodes, args[0]);
                let dst_c_ty = self.c_int_type(result_ty);
                let mask = if width == 64 {
                    "~(uint64_t)0".to_string()
                } else {
                    format!("(((uint64_t)1 << {width}) - 1)")
                };
                let tmp = self.fresh_tmp();
                self.linef(format!(
                    "uint64_t {tmp} = (uint64_t)({load_ty}){val} & {mask};"
                ));
                let expr = match intrinsic {
                    Intrinsic::CountTrailingZeros => {
                        format!(
                            "({tmp} == 0 ? (uint64_t){width} : (uint64_t)__builtin_ctzll({tmp}))"
                        )
                    }
                    Intrinsic::CountLeadingZeros => format!(
                        "({tmp} == 0 ? (uint64_t){width} : (uint64_t)(__builtin_clzll({tmp}) - {}))",
                        64 - width
                    ),
                    Intrinsic::CountOnes => format!("(uint64_t)__builtin_popcountll({tmp})"),
                    _ => unreachable!(),
                };
                self.linef(format!("*({dst_c_ty}*){dst} = ({dst_c_ty}){expr};"));
            }
            Intrinsic::CarryingMulAdd => {
                // args 0..4 are scalar Uint64 values; args 4,5 are &Uint64
                // out-params (pointers). The runtime writes the low/high halves
                // of `a*b + carry + add` through them. Returns Unit.
                let a = self.emit_load(nodes, args[0]);
                let b = self.emit_load(nodes, args[1]);
                let carry = self.emit_load(nodes, args[2]);
                let add = self.emit_load(nodes, args[3]);
                let lo_ptr = self.emit_load(nodes, args[4]);
                let hi_ptr = self.emit_load(nodes, args[5]);
                self.linef(format!(
                    "sol_carrying_mul_add((uint64_t){a}, (uint64_t){b}, (uint64_t){carry}, (uint64_t){add}, (uint64_t*){lo_ptr}, (uint64_t*){hi_ptr});"
                ));
            }
            Intrinsic::Cast(_, _) => {
                let val = self.emit_load(nodes, args[0]);
                let src_ty = &nodes[args[0].0].ty;
                let src_c_ty = self.c_int_type(src_ty);
                let dst_c_ty = self.c_int_type(result_ty);
                self.linef(format!(
                    "*({dst_c_ty}*){dst} = ({dst_c_ty})({src_c_ty}){val};"
                ));
            }
        }
    }

    fn emit_branch_into(&mut self, nodes: &[Node], body: &[NodeId], dst: &str) {
        let has_tail = body
            .last()
            .is_some_and(|&id| matches!(nodes[id.0].kind, NodeKind::Expr(_)));

        let (init, tail) = if has_tail {
            let (init, tail) = body.split_at(body.len() - 1);
            (init, Some(tail[0]))
        } else {
            (body, None)
        };

        for &id in init {
            self.emit_stmt(nodes, id);
        }

        if let Some(tid) = tail
            && let NodeKind::Expr(inner) = nodes[tid.0].kind
        {
            self.emit_into(nodes, inner, dst);
        }
    }

    fn emit_line_directive(&mut self, nodes: &[Node], id: NodeId) {
        let span = nodes[id.0].span;
        let line = span.start.line;
        let file = self
            .source_map
            .get(span.file_id)
            .map(|(f, _)| f.to_string())
            .unwrap_or_else(|| self.source_file.clone());
        // Don't emit now: `line()` re-asserts this before every code line of the
        // statement so the preprocessor can't drift the attribution forward.
        self.cur_loc = Some(((line + 1) as usize, file));
    }

    fn emit_stmt(&mut self, nodes: &[Node], id: NodeId) {
        self.emit_line_directive(nodes, id);
        match &nodes[id.0].kind {
            NodeKind::Let {
                var,
                value,
                noescape,
            } => {
                let var = *var;
                let value = *value;
                let noescape = *noescape;
                let ty = nodes[value.0].ty.clone();
                // A proven-non-escaping sized binding goes on the C stack: every
                // pointer to it only flows into non-escaping callee params (or its
                // address is never taken at all), so it need not be
                // GC-heap-allocated. `_vN` still points at its storage (now a
                // stack buffer), so `&binding` and field/deref access are
                // unchanged. Embedded GC pointers stay reachable via the
                // collector's conservative stack scan.
                if noescape && self.stack_eligible(&ty) {
                    let size = self.type_size(&ty);
                    let align = self.type_align(&ty);
                    self.linef(format!(
                        "uint8_t _v{}_stk[{size}] __attribute__((aligned({align})));",
                        var.0
                    ));
                    self.linef(format!("uint8_t* _v{} = _v{}_stk;", var.0, var.0));
                    self.emit_into(nodes, value, &format!("_v{}", var.0));
                } else if self.is_sized(&ty) {
                    let size = self.type_size(&ty);
                    let align = self.type_align(&ty);
                    let mf = self.mark_fn_expr(&ty);
                    let tmp = self.fresh_tmp();
                    self.emit_alloc(&tmp, size, align, &mf);
                    self.emit_into(nodes, value, &tmp);
                    self.linef(format!("uint8_t* _v{} = {tmp};", var.0));
                    // FixedArray: also emit meta variable (constant N)
                    if let Type::FixedArray(_, n) = &ty {
                        self.linef(format!("uint64_t _vm{} = {n};", var.0));
                    }
                } else {
                    let meta = self.emit_meta(nodes, value).unwrap();
                    let align = self.type_align(&ty);
                    let mf = self.mark_fn_expr(&ty);
                    let size_expr = self.emit_full_size_expr(&ty, &meta);
                    let tmp = self.fresh_tmp();
                    self.emit_alloc(&tmp, &size_expr, align, &mf);
                    self.emit_into(nodes, value, &tmp);
                    self.linef(format!("uint8_t* _v{} = {tmp};", var.0));
                    self.linef(format!("uint64_t _vm{} = {meta};", var.0));
                }
            }
            NodeKind::Assign { target, value } => {
                let target = *target;
                let value = *value;
                let (place, target_meta) = self.emit_place(nodes, target);
                if let Some(ref tmeta) = target_meta {
                    let vmeta = self.emit_meta(nodes, value).unwrap();
                    self.linef(format!(
                        "if ((uint64_t){tmeta} != (uint64_t){vmeta}) {{ __builtin_trap(); }}"
                    ));
                }
                self.emit_into(nodes, value, &place);
            }
            NodeKind::If {
                condition,
                then_body,
                else_body,
            } => {
                let condition = *condition;
                let then_body = then_body.clone();
                let else_body = else_body.clone();
                let cond = self.emit_load(nodes, condition);
                self.linef(format!("if ((uint8_t){cond}) {{"));
                self.indent += 1;
                for &stmt_id in &then_body {
                    self.emit_stmt(nodes, stmt_id);
                }
                self.indent -= 1;
                if !else_body.is_empty() {
                    self.line("} else {");
                    self.indent += 1;
                    for &stmt_id in &else_body {
                        self.emit_stmt(nodes, stmt_id);
                    }
                    self.indent -= 1;
                }
                self.line("}");
            }
            NodeKind::Loop { body } => {
                // Statement-position loop (while/for, or a bare `loop`): any break
                // value is written into a throwaway heap slot of the loop's type.
                let body = body.clone();
                let ty = nodes[id.0].ty.clone();
                let dst = if matches!(ty, Type::Unit | Type::Never) {
                    "(uint8_t*)0".to_string()
                } else {
                    let size = self.type_size(&ty);
                    let align = self.type_align(&ty);
                    let mf = self.mark_fn_expr(&ty);
                    let tmp = self.fresh_tmp();
                    self.emit_alloc(&tmp, size, align, &mf);
                    tmp
                };
                self.loop_dst.push(dst);
                self.line("while (1) {");
                self.indent += 1;
                for &stmt_id in &body {
                    self.emit_stmt(nodes, stmt_id);
                }
                self.indent -= 1;
                self.line("}");
                self.loop_dst.pop();
            }
            NodeKind::Break(value) => {
                if let Some(v) = *value {
                    let dst = self
                        .loop_dst
                        .last()
                        .cloned()
                        .expect("break value outside a loop");
                    self.emit_into(nodes, v, &dst);
                }
                self.line("break;");
            }
            NodeKind::Continue => {
                self.line("continue;");
            }
            NodeKind::Expr(inner) => {
                let inner = *inner;
                match &nodes[inner.0].kind {
                    NodeKind::IntrinsicCall { .. } | NodeKind::Call { .. } => {
                        let ty = &nodes[inner.0].ty;
                        if matches!(*ty, Type::Unit | Type::Never) {
                            self.emit_into(nodes, inner, "((uint8_t*)0)");
                        } else {
                            // Non-unit call used as statement — discard result
                            let s = self.type_size(ty);
                            let a = self.type_align(ty);
                            let mf = self.mark_fn_expr(ty);
                            if let NodeKind::IntrinsicCall { intrinsic, args } =
                                &nodes[inner.0].kind
                            {
                                let intrinsic = intrinsic.clone();
                                let args: Vec<NodeId> = args.clone();
                                let tmp = self.fresh_tmp();
                                self.emit_alloc(&tmp, s, a, &mf);
                                self.emit_intrinsic(nodes, &intrinsic, &args, ty, &tmp);
                            } else if let NodeKind::Call { function, args } = &nodes[inner.0].kind {
                                let function = function.clone();
                                let args: Vec<NodeId> = args.clone();
                                let call_expr = self.emit_call_expr(nodes, &function, &args);
                                self.linef(format!("(void){call_expr};"));
                            }
                        }
                    }
                    _ => {
                        let ty = &nodes[inner.0].ty;
                        if matches!(*ty, Type::Unit | Type::Never) {
                            self.emit_into(nodes, inner, "((uint8_t*)0)");
                        } else {
                            let s = self.type_size(ty);
                            let a = self.type_align(ty);
                            let mf = self.mark_fn_expr(ty);
                            let tmp = self.fresh_tmp();
                            self.emit_alloc(&tmp, s, a, &mf);
                            self.emit_into(nodes, inner, &tmp);
                        }
                    }
                }
            }
            NodeKind::Return(inner) => {
                let inner = *inner;
                self.emit_into(nodes, inner, "(uint8_t*)&_ret");
                self.line("return _ret;");
            }
            _ => unreachable!(),
        }
        // End of statement: stop stamping and neutralize the trailing location so
        // synthetic glue between statements isn't attributed to real source lines.
        self.cur_loc = None;
        self.raw_line("#line 1 \"\"");
    }
}
