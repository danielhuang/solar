use crate::ast::BinOp;
use crate::ast::Intrinsic;
use crate::error::SourceMap;
use crate::ir::*;
use std::collections::{HashMap, HashSet};

pub fn generate(module: &Module, source_file: &str, source_map: &SourceMap) -> String {
    let mut cg = Codegen {
        module,
        out: String::new(),
        indent: 0,
        tmp_counter: 0,
        source_file: source_file.to_string(),
        source_map,
        emitted_mark_fns: HashSet::new(),
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

    /// Name for a value-type struct: `_v{size}_{align}`
    fn val_type_name(size: usize, align: usize) -> String {
        format!("_v{size}_{align}")
    }

    /// Collect all (size, align) pairs used for by-value params and returns.
    fn collect_val_types_from_type(
        ty: &Type,
        dt: &std::collections::HashMap<String, DataType>,
        set: &mut HashSet<(usize, usize)>,
    ) {
        if let Type::Function {
            params,
            return_type,
        } = ty
        {
            for p in params {
                let s = type_size(p, dt);
                let a = type_align(p, dt);
                set.insert((s, a));
                Self::collect_val_types_from_type(p, dt, set);
            }
            if !matches!(**return_type, Type::Unit | Type::Never) {
                let s = type_size(return_type, dt);
                let a = type_align(return_type, dt);
                set.insert((s, a));
                Self::collect_val_types_from_type(return_type, dt, set);
            }
        }
    }

    fn collect_val_types(&self) -> Vec<(usize, usize)> {
        let mut set = HashSet::new();
        for func in &self.module.functions {
            if !matches!(func.return_type, Type::Unit | Type::Never) {
                let s = self.type_size(&func.return_type);
                let a = self.type_align(&func.return_type);
                set.insert((s, a));
            }
            for param in &func.params {
                let s = self.type_size(&param.ty);
                let a = self.type_align(&param.ty);
                set.insert((s, a));
                Self::collect_val_types_from_type(&param.ty, &self.module.datatypes, &mut set);
            }
            Self::collect_val_types_from_type(&func.return_type, &self.module.datatypes, &mut set);
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

        // Emit value-type typedefs
        let val_types = self.collect_val_types();
        for (size, align) in &val_types {
            self.linef(format!(
                "typedef struct {{ _Alignas({align}) char _d[{size}]; }} {};",
                Self::val_type_name(*size, *align)
            ));
        }
        if !val_types.is_empty() {
            self.line("");
        }

        // Emit GC mark functions
        self.emit_mark_functions();

        // Forward-declare user functions
        for func in &self.module.functions {
            let sig = self.func_signature(func);
            self.linef(format!("static {sig};"));
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
        self.line("sol_start(solar_main);");
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
        self.line("extern size_t sol_file_read(uint8_t* fd, uint8_t* ptr, size_t len);");
        self.line(
            "extern size_t sol_file_write_partial(uint8_t* fd, const uint8_t* ptr, size_t len);",
        );
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
        self.line("extern uint8_t* sol_slice_index(uint8_t* base, uint64_t index, uint64_t len, uint64_t elem_size);");
        self.line("extern uint8_t* sol_slice_range(uint8_t* base, uint64_t start, uint64_t end, uint64_t len, uint64_t elem_size);");
        self.line("extern uint8_t* sol_null_check(uint8_t* ptr);");
        self.line("extern void sol_assert_array_len(uint64_t actual, uint64_t expected);");
        self.line("extern void sol_start(void (*solar_main)(void*));");
        self.line("extern void sol_thread_spawn(void* fn_ptr, void* env);");
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
        self.line("extern void sol_futex_wait(uint32_t* ptr, uint32_t expected);");
        self.line("extern void sol_futex_wake(uint32_t* ptr, uint32_t count);");
        self.line("");
    }

    fn func_name(&self, name: &str) -> String {
        if name == "main" {
            "solar_main".into()
        } else {
            format!("solar_{name}")
        }
    }

    fn func_signature(&self, func: &Function) -> String {
        let name = self.func_name(&func.name);
        let mut params: Vec<String> = vec!["void* __env".to_string()];
        for (i, p) in func.params.iter().enumerate() {
            let s = self.type_size(&p.ty);
            let a = self.type_align(&p.ty);
            let vt = Self::val_type_name(s, a);
            params.push(format!("{vt} _p{i}"));
        }
        let params_str = params.join(", ");
        if matches!(func.return_type, Type::Unit | Type::Never) {
            format!("void {name}({params_str})")
        } else {
            let s = self.type_size(&func.return_type);
            let a = self.type_align(&func.return_type);
            let vt = Self::val_type_name(s, a);
            format!("{vt} {name}({params_str})")
        }
    }

    fn emit_function(&mut self, func: &Function) {
        self.tmp_counter = 0;
        let sig = self.func_signature(func);
        self.linef(format!("static {sig} {{"));
        self.indent += 1;

        // Bind captured variables from env
        for cap in &func.env_captures {
            self.linef(format!(
                "uint8_t* _v{} = *(uint8_t**)((uint8_t*)__env + {});",
                cap.var.0,
                cap.index * 8
            ));
        }

        // Bind params: every param gets a heap slot, so a `&`/`^` of it (e.g. a
        // closure capturing it by reference, or a `^param`) is a valid, traceable
        // GC pointer that stays live if it escapes. A param whose address never
        // escapes is a non-escaping `sol_alloc`, which the GC-alloc lowering turns
        // into a recognized `calloc` that `opt -O3` SROAs back into registers — so
        // this is free in release; only unoptimized debug builds keep the slot.
        for (i, param) in func.params.iter().enumerate() {
            let s = self.type_size(&param.ty);
            let a = self.type_align(&param.ty);
            let var_id = param.var.0;
            let ty = param.ty.clone();
            let mf = self.mark_fn_expr(&ty);
            self.linef(format!("uint8_t* _v{var_id} = sol_alloc({s}, {a}, {mf});"));
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
            let s = self.type_size(&func.return_type);
            let a = self.type_align(&func.return_type);
            let vt = Self::val_type_name(s, a);
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
                    self.linef(format!("uint8_t* {tmp} = sol_alloc({s}, {a}, {mf});"));
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
                    self.linef(format!("uint8_t* {tmp} = sol_alloc({s}, {a}, {mf});"));
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
                self.linef(format!("uint8_t* {tmp} = sol_alloc({s}, {a}, {mf});"));
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
            NodeKind::FieldAccess { .. }
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
                let val = self.emit_load(nodes, inner);
                format!("!(uint8_t){val}")
            }
            NodeKind::IfExpr { .. } | NodeKind::Match { .. } => {
                let ty = nodes[id.0].ty.clone();
                let s = self.type_size(&ty);
                let a = self.type_align(&ty);
                let mf = self.mark_fn_expr(&ty);
                let tmp = self.fresh_tmp();
                self.linef(format!("uint8_t* {tmp} = sol_alloc({s}, {a}, {mf});"));
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
                self.linef(format!("uint8_t* {tmp} = sol_alloc({s}, {a}, {mf});"));
                self.emit_into(nodes, id, &tmp);
                let c_ty = self.c_int_type(&ty);
                format!("*({c_ty}*){tmp}")
            }
            NodeKind::Call { function, args } => {
                let function = function.clone();
                let args: Vec<NodeId> = args.clone();
                let result_ty = nodes[id.0].ty.clone();
                let s = self.type_size(&result_ty);
                let a = self.type_align(&result_ty);
                let vt = Self::val_type_name(s, a);
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
                self.linef(format!("uint8_t* {tmp} = sol_alloc({s}, {a}, {mf});"));
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
                self.linef(format!("uint8_t* {tmp} = sol_alloc({s}, {a}, {mf});"));
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
                let sz = self.type_size(left_ty);
                let al = self.type_align(left_ty);
                let vt = Self::val_type_name(sz, al);
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
                self.linef(format!(
                    "uint8_t* {la_tmp} = sol_alloc({la_meta} * {es}, {ea}, {mf});"
                ));
                self.emit_into(nodes, left, &la_tmp);
                let ra_meta = self.emit_meta(nodes, right).unwrap();
                let ra_tmp = self.fresh_tmp();
                self.linef(format!(
                    "uint8_t* {ra_tmp} = sol_alloc({ra_meta} * {es}, {ea}, {mf});"
                ));
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
            let s = self.type_size(&param.ty);
            let a = self.type_align(&param.ty);
            let vt = Self::val_type_name(s, a);
            // Create a local val-type, emit_into it, pass by value
            let ptmp = self.fresh_tmp();
            self.linef(format!("{vt} {ptmp};"));
            self.emit_into(nodes, arg, &format!("(uint8_t*)&{ptmp}"));
            arg_exprs.push(ptmp);
        }
        let args_str = arg_exprs.join(", ");
        format!("{cname}({args_str})")
    }

    /// Emit a type-aware copy from `src` to `dst`. If the type contains unique
    /// pointers, recursively deep-copies the pointees. Otherwise falls back to memcpy.
    fn emit_copy(&mut self, dst: &str, src: &str, ty: &Type, size_expr: &str) {
        // UniqueUnsized needs deep-copy, so it goes through the match below
        if matches!(
            ty,
            Type::Function { .. } | Type::RefUnsized(_) | Type::NullableRefUnsized(_)
        ) {
            self.linef(format!("sol_copy_128_unordered({dst}, {src});"));
            return;
        }
        if !self.type_contains_unique(ty) && !self.type_contains_enum(ty) {
            self.linef(format!("sol_memcpy({dst}, {src}, {size_expr});"));
            return;
        }
        match ty {
            Type::Unique(inner) => {
                let size = self.type_size(inner);
                let align = self.type_align(inner);
                let mf = self.mark_fn_expr(inner);
                let new_ptr = self.fresh_tmp();
                self.linef(format!(
                    "uint8_t* {new_ptr} = sol_alloc({size}, {align}, {mf});"
                ));
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
                self.linef(format!(
                    "uint8_t* {new_ptr} = sol_alloc({inner_size}, {align}, {mf});"
                ));
                self.emit_copy(&new_ptr, &src_ptr, inner, &inner_size);
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
                        if self.type_contains_unique(&field_ty)
                            || self.type_contains_enum(&field_ty)
                        {
                            self.emit_copy(&fdst, &fsrc, &field_ty, &field_size.to_string());
                        } else {
                            self.linef(format!("sol_memcpy({fdst}, {fsrc}, {field_size});"));
                        }
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
                    if self.type_contains_unique(field_ty) || self.type_contains_enum(field_ty) {
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
                    } else {
                        self.linef(format!(
                            "sol_memcpy({dst} + {offset}, {src} + {offset}, {field_size});"
                        ));
                    }
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
                self.linef(format!("sol_memcpy({dst}, {src}, {size_expr});"));
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
                    let meta = src_meta.unwrap();
                    let size_expr = self.emit_full_size_expr(&ty, &meta);
                    self.emit_copy(dst, &src, &ty, &size_expr);
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
                        self.linef(format!(
                            "uint8_t* {tmp} = sol_alloc({size}, {align}, {mf});"
                        ));
                        self.emit_into(nodes, inner, &tmp);
                        self.linef(format!("*(uint8_t**){dst} = {tmp};"));
                    } else {
                        let meta = self.emit_meta(nodes, inner).unwrap();
                        let align = self.type_align(&inner_ty_clone);
                        let mf = self.mark_fn_expr(&inner_ty_clone);
                        let size_expr = self.emit_full_size_expr(&inner_ty_clone, &meta);
                        let tmp = self.fresh_tmp();
                        self.linef(format!(
                            "uint8_t* {tmp} = sol_alloc({size_expr}, {align}, {mf});"
                        ));
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
                // Unique pointer creation: always allocates fresh memory
                let inner = *inner;
                let inner_ty_clone = nodes[inner.0].ty.clone();
                if self.is_sized(&inner_ty_clone) {
                    let size = self.type_size(&inner_ty_clone);
                    let align = self.type_align(&inner_ty_clone);
                    let mf = self.mark_fn_expr(&inner_ty_clone);
                    let tmp = self.fresh_tmp();
                    self.linef(format!(
                        "uint8_t* {tmp} = sol_alloc({size}, {align}, {mf});"
                    ));
                    self.emit_into(nodes, inner, &tmp);
                    self.linef(format!("*(uint8_t**){dst} = {tmp};"));
                } else {
                    let meta = self.emit_meta(nodes, inner).unwrap();
                    let align = self.type_align(&inner_ty_clone);
                    let mf = self.mark_fn_expr(&inner_ty_clone);
                    let size_expr = self.emit_full_size_expr(&inner_ty_clone, &meta);
                    let tmp = self.fresh_tmp();
                    self.linef(format!(
                        "uint8_t* {tmp} = sol_alloc({size_expr}, {align}, {mf});"
                    ));
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
                self.emit_into(nodes, value, dst);
                // Runtime assertion: meta == size
                let meta = self.emit_meta(nodes, value).unwrap();
                self.linef(format!(
                    "if ((uint64_t){meta} != {size}u) {{ __builtin_trap(); }}"
                ));
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
                    self.linef(format!(
                        "uint8_t* {la_tmp} = sol_alloc({lm} * {es}, {ea}, {mf});"
                    ));
                    self.emit_into(nodes, left, &la_tmp);
                    let rm = self.emit_meta(nodes, right).unwrap();
                    let ra_tmp = self.fresh_tmp();
                    self.linef(format!(
                        "uint8_t* {ra_tmp} = sol_alloc({rm} * {es}, {ea}, {mf});"
                    ));
                    self.emit_into(nodes, right, &ra_tmp);
                    let left_size = format!("{lm} * {es}");
                    self.emit_copy(
                        dst,
                        &la_tmp,
                        &Type::Array(Box::new(inner.clone())),
                        &left_size,
                    );
                    let right_dst = format!("{dst} + {lm} * {es}");
                    let right_size = format!("{rm} * {es}");
                    self.emit_copy(
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
                    self.linef(format!("uint8_t* {tmp} = sol_alloc({s}, {a}, {mf});"));
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
                    self.linef(format!(
                        "uint8_t* {env_tmp} = sol_alloc({}, 8, _mark_ptr_array);",
                        n * 8
                    ));
                    for (i, &cap_id) in capture_ids.iter().enumerate() {
                        // Each capture is a Ref node — load the pointer value
                        let ptr_expr = self.emit_load(nodes, cap_id);
                        self.linef(format!("*(uint8_t**)({env_tmp} + {}) = {ptr_expr};", i * 8));
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
                    let a = self.type_align(&result_ty);
                    let vt = Self::val_type_name(s, a);
                    let call_expr = self.emit_call_expr(nodes, &function, &args);
                    let tmp = self.fresh_tmp();
                    self.linef(format!("{vt} {tmp} = {call_expr};"));
                    self.linef(format!("sol_memcpy({dst}, (uint8_t*)&{tmp}, {s});"));
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
                let ea = self.type_align(&elem_ty);
                let cnt_expr = self.emit_load(nodes, count);
                let cnt_tmp = self.fresh_tmp();
                self.linef(format!("uint64_t {cnt_tmp} = (uint64_t){cnt_expr};"));

                // Eval init closure into a 16-byte tmp
                let callee_ty = nodes[init.0].ty.clone();
                let cs = self.type_size(&callee_ty);
                let ca = self.type_align(&callee_ty);
                let callee_tmp = self.fresh_tmp();
                self.linef(format!(
                    "uint8_t* {callee_tmp} = sol_alloc({cs}, {ca}, _mark_fn_value);"
                ));
                self.emit_into(nodes, init, &callee_tmp);
                let fp_var = self.fresh_tmp();
                let env_var = self.fresh_tmp();
                self.linef(format!("void(*{fp_var})() = *(void(**)()){callee_tmp};"));
                self.linef(format!("void* {env_var} = *(void**)({callee_tmp} + 8);"));

                // Build function pointer type: returns val_type_name(es, ea), takes (void* env, val_type_name(8, 8) index)
                let ret_vt = Self::val_type_name(es, ea);
                let idx_vt = Self::val_type_name(8, 8);
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
                self.linef(format!(
                    "sol_memcpy({dst} + {idx} * {es}, (uint8_t*)&{result_tmp}, {es});"
                ));
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
                self.linef(format!(
                    "uint8_t* {callee_tmp} = sol_alloc({cs}, {ca}, _mark_fn_value);"
                ));
                self.emit_into(nodes, callee, &callee_tmp);
                let fp_var = self.fresh_tmp();
                let env_var = self.fresh_tmp();
                self.linef(format!("void(*{fp_var})() = *(void(**)()){callee_tmp};"));
                self.linef(format!("void* {env_var} = *(void**)({callee_tmp} + 8);"));

                // Build arg val-type wrappers — prepend env
                let mut arg_exprs: Vec<String> = vec![env_var.clone()];
                for (pty, &arg) in param_types.iter().zip(args.iter()) {
                    let s = self.type_size(pty);
                    let a = self.type_align(pty);
                    let vt = Self::val_type_name(s, a);
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
                    let s = self.type_size(&return_type);
                    let a = self.type_align(&return_type);
                    Self::val_type_name(s, a)
                };
                let mut param_vts: Vec<String> = vec!["void*".to_string()];
                for pty in &param_types {
                    let s = self.type_size(pty);
                    let a = self.type_align(pty);
                    param_vts.push(Self::val_type_name(s, a));
                }
                let fp_type = format!("{ret_vt}(*)({})", param_vts.join(", "));

                if matches!(return_type, Type::Unit | Type::Never) {
                    self.linef(format!("(({fp_type}){fp_var})({args_str});"));
                } else {
                    let s = self.type_size(&return_type);
                    let a = self.type_align(&return_type);
                    let vt = Self::val_type_name(s, a);
                    let tmp = self.fresh_tmp();
                    self.linef(format!("{vt} {tmp} = (({fp_type}){fp_var})({args_str});"));
                    self.linef(format!("sol_memcpy({dst}, (uint8_t*)&{tmp}, {s});"));
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
            Intrinsic::ArrayLen => {
                let len = if let Type::FixedArray(_, n) = &nodes[args[0].0].ty {
                    format!("{n}")
                } else {
                    self.emit_meta(nodes, args[0]).unwrap()
                };
                let c_ty = self.c_int_type(result_ty);
                self.linef(format!("*({c_ty}*){dst} = ({c_ty}){len};"));
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
                self.linef(format!(
                    "sol_futex_wait((uint32_t*){ptr}, (uint32_t){expected});"
                ));
            }
            Intrinsic::FutexWake => {
                let ptr = self.emit_load(nodes, args[0]);
                let count = self.emit_load(nodes, args[1]);
                self.linef(format!(
                    "sol_futex_wake((uint32_t*){ptr}, (uint32_t){count});"
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
            NodeKind::Let { var, value } => {
                let var = *var;
                let value = *value;
                let ty = nodes[value.0].ty.clone();
                if self.is_sized(&ty) {
                    let size = self.type_size(&ty);
                    let align = self.type_align(&ty);
                    let mf = self.mark_fn_expr(&ty);
                    let tmp = self.fresh_tmp();
                    self.linef(format!(
                        "uint8_t* {tmp} = sol_alloc({size}, {align}, {mf});"
                    ));
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
                    self.linef(format!(
                        "uint8_t* {tmp} = sol_alloc({size_expr}, {align}, {mf});"
                    ));
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
                    self.linef(format!(
                        "uint8_t* {tmp} = sol_alloc({size}, {align}, {mf});"
                    ));
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
                                self.linef(format!("uint8_t* {tmp} = sol_alloc({s}, {a}, {mf});"));
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
                            self.linef(format!("uint8_t* {tmp} = sol_alloc({s}, {a}, {mf});"));
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
