use crate::ast::BinOp;
use crate::ast::Intrinsic;
use crate::ast::SourceSpan;
use crate::scope::ScopeStack;
use crate::typed_ast;
use std::collections::HashMap;

pub use crate::typed_ast::Type;

// --- Memory layout ---

#[derive(Debug)]
pub struct DataType {
    pub name: String,
    pub size: usize,
    pub align: usize,
    pub is_sized: bool,
    pub fields: Vec<FieldLayout>,
    /// For enums: maps discriminant index → Some(field_name) for data variants, None for unit variants.
    pub variant_map: Option<Vec<Option<String>>>,
}

#[derive(Debug)]
pub struct FieldLayout {
    pub name: String,
    pub ty: Type,
    pub offset: usize,
    pub size: usize,
}

// --- Flat-tree IR ---

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VarId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeId(pub usize);

#[derive(Debug)]
pub struct Module {
    pub datatypes: HashMap<String, DataType>,
    pub functions: Vec<Function>,
    /// Top-level `static` declarations (global mutable slots), in source
    /// order. `NodeKind::Global(i)` indexes into this. Their literal initial
    /// values are stored by assignments prepended to `main`'s body.
    pub statics: Vec<IrStatic>,
}

#[derive(Debug)]
pub struct IrStatic {
    pub name: String,
    pub ty: Type,
}

#[derive(Debug)]
pub struct EnvCapture {
    pub var: VarId,
    /// Ordinal of this capture; its env slot is at byte offset `index * 16`.
    /// Each slot is 16 bytes: a thin pointer (sized capture) or a fat pointer
    /// `(ptr, meta)` (unsized capture, e.g. a captured `[Uint8]`).
    pub index: usize,
    /// True if the captured variable is unsized (its slot carries `meta` too).
    pub is_unsized: bool,
}

#[derive(Debug)]
pub struct Function {
    pub name: String,
    pub params: Vec<Param>,
    pub return_type: Type,
    pub nodes: Vec<Node>,
    pub body: Vec<NodeId>,
    pub env_captures: Vec<EnvCapture>,
    /// `fn(inline)` hint, consumed by codegen (emits an inline marker).
    pub inline_hint: bool,
    /// Per-parameter escape info, aligned with `params` (same length/order):
    /// `param_noescape[i] == true` means parameter `i` is *proven* not to escape
    /// the function — no pointer to its storage can outlive the call. The default
    /// (set at lowering) is **all `false`** — i.e. conservatively assume every
    /// parameter may escape. `ir_opt::analyze_param_escapes` refines this; it only ever
    /// sets `true` when it can prove non-escape, so a `false` never lies.
    pub param_noescape: Vec<bool>,
}

#[derive(Debug)]
pub struct Param {
    pub var: VarId,
    pub name: String,
    pub ty: Type,
}

#[derive(Debug)]
pub struct Node {
    pub ty: Type,
    pub kind: NodeKind,
    pub span: SourceSpan,
}

#[derive(Debug)]
pub enum NodeKind {
    // Expressions
    IntegerLiteral(i64),
    BooleanLiteral(bool),
    /// A null nullable reference (`null#[T]`). The node's `ty` is a
    /// `NullableRef`/`NullableRefUnsized` and determines whether it materializes
    /// as an 8-byte or 16-byte zero.
    Null,
    Local(VarId),
    /// A top-level `static` slot (index into `Module::statics`) — a global
    /// mutable place.
    Global(usize),
    FieldAccess {
        object: NodeId,
        field: String,
    },
    Deref(NodeId),
    Ref(NodeId),
    Unique(NodeId),
    Call {
        function: String,
        args: Vec<NodeId>,
    },
    FunctionRef(String),
    CallIndirect {
        callee: NodeId,
        args: Vec<NodeId>,
    },
    StructLiteral {
        name: String,
        fields: Vec<(String, NodeId)>,
    },
    Index {
        object: NodeId,
        index: NodeId,
    },
    Slice {
        object: NodeId,
        start: NodeId,
        end: NodeId,
    },
    ArrayLiteral(Vec<NodeId>),
    ArrayRepeat {
        element: NodeId,
        count: NodeId,
    },
    ArrayInit {
        count: NodeId,
        init: NodeId,
    },
    ArraySizeCoerce {
        value: NodeId,
        size: u64,
    },
    MakeClosure {
        function: String,
        captures: Vec<NodeId>,
    },
    BinaryOp {
        op: BinOp,
        left: NodeId,
        right: NodeId,
    },
    EnumVariant {
        enum_name: String,
        variant_name: String,
        variant_index: u64,
        value: Option<NodeId>,
    },
    Match {
        scrutinee: NodeId,
        arms: Vec<MatchArm>,
    },

    IntrinsicCall {
        intrinsic: Intrinsic,
        args: Vec<NodeId>,
    },

    // Statements
    Let {
        var: VarId,
        value: NodeId,
        /// When `true`, this binding is proven not to escape (every pointer to
        /// its storage only flows into calls whose parameter is itself
        /// non-escaping). Codegen may then place it on the C stack instead of a
        /// `sol_alloc` heap box. Default `false` (conservatively may escape);
        /// set by `ir_opt::analyze_let_noescape`.
        noescape: bool,
    },
    Assign {
        target: NodeId,
        value: NodeId,
    },
    If {
        condition: NodeId,
        then_body: Vec<NodeId>,
        else_body: Vec<NodeId>,
    },
    IfExpr {
        condition: NodeId,
        then_body: Vec<NodeId>,
        else_body: Vec<NodeId>,
    },
    Loop {
        body: Vec<NodeId>,
    },
    /// `break`, optionally with a value (which is written into the enclosing
    /// loop expression's result destination).
    Break(Option<NodeId>),
    Continue,
    Not(NodeId),
    Expr(NodeId),
    Return(NodeId),
}

#[derive(Debug, Clone)]
pub struct MatchArm {
    pub pattern: MatchPattern,
    pub body: Vec<NodeId>,
}

#[derive(Debug, Clone)]
pub enum MatchPattern {
    Variant {
        enum_name: String,
        variant_name: String,
        variant_index: u64,
        binding: Option<(VarId, Type)>,
    },
    Wildcard(VarId, Type),
}

// --- Lowering ---

pub fn lower(source: &typed_ast::SourceFile) -> Module {
    let datatypes = build_datatypes(source);
    let mut next_var = 0..;

    // Collect closure capture info: synthetic_fn_name -> Vec<CapturedVar>
    let mut closure_captures: HashMap<String, Vec<typed_ast::CapturedVar>> = HashMap::new();
    for func in source.functions.values() {
        collect_closure_captures(&func.body, &mut closure_captures);
    }

    let statics: Vec<IrStatic> = source
        .statics
        .iter()
        .map(|s| IrStatic {
            name: s.name.clone(),
            ty: s.ty.clone(),
        })
        .collect();
    let static_idx: HashMap<String, usize> = source
        .statics
        .iter()
        .enumerate()
        .map(|(i, s)| (s.name.clone(), i))
        .collect();

    let functions = source
        .functions
        .values()
        .map(|f| {
            // Statics are initialized by assignments prepended to `main` (their
            // values are literals, so this is pure stores — no user code runs
            // before them).
            if f.name == "main" && !source.statics.is_empty() {
                let mut main_fn = f.clone();
                let inits = source.statics.iter().map(|st| typed_ast::Statement {
                    kind: typed_ast::StatementKind::Assignment {
                        target: typed_ast::Expr {
                            ty: st.ty.clone(),
                            kind: typed_ast::ExprKind::Global(st.name.clone()),
                            span: st.init.span,
                        },
                        value: st.init.clone(),
                    },
                    span: st.init.span,
                });
                main_fn.body.splice(0..0, inits);
                lower_function(
                    &main_fn,
                    &mut next_var,
                    closure_captures.get(&f.name),
                    &datatypes,
                    &static_idx,
                )
            } else {
                lower_function(
                    f,
                    &mut next_var,
                    closure_captures.get(&f.name),
                    &datatypes,
                    &static_idx,
                )
            }
        })
        .collect();
    Module {
        datatypes,
        functions,
        statics,
    }
}

fn collect_closure_captures(
    stmts: &[typed_ast::Statement],
    map: &mut HashMap<String, Vec<typed_ast::CapturedVar>>,
) {
    for stmt in stmts {
        match &stmt.kind {
            typed_ast::StatementKind::Let { value, .. } => {
                collect_closure_captures_expr(value, map)
            }
            typed_ast::StatementKind::Assignment { target, value } => {
                collect_closure_captures_expr(target, map);
                collect_closure_captures_expr(value, map);
            }
            typed_ast::StatementKind::If {
                condition,
                body,
                else_body,
            } => {
                collect_closure_captures_expr(condition, map);
                collect_closure_captures(body, map);
                collect_closure_captures(else_body, map);
            }
            typed_ast::StatementKind::While { condition, body } => {
                collect_closure_captures_expr(condition, map);
                collect_closure_captures(body, map);
            }
            typed_ast::StatementKind::Expression(e) => collect_closure_captures_expr(e, map),
            typed_ast::StatementKind::Return(e) => collect_closure_captures_expr(e, map),
            typed_ast::StatementKind::Break(value) => {
                if let Some(v) = value {
                    collect_closure_captures_expr(v, map);
                }
            }
            typed_ast::StatementKind::Continue => {}
        }
    }
}

fn collect_closure_captures_expr(
    expr: &typed_ast::Expr,
    map: &mut HashMap<String, Vec<typed_ast::CapturedVar>>,
) {
    match &expr.kind {
        typed_ast::ExprKind::Closure {
            synthetic_fn,
            captures,
        } => {
            map.insert(synthetic_fn.clone(), captures.clone());
        }
        typed_ast::ExprKind::FieldAccess { object, .. } => {
            collect_closure_captures_expr(object, map);
        }
        typed_ast::ExprKind::Deref(inner)
        | typed_ast::ExprKind::Reference(inner)
        | typed_ast::ExprKind::Unique(inner)
        | typed_ast::ExprKind::Not(inner) => {
            collect_closure_captures_expr(inner, map);
        }
        typed_ast::ExprKind::Call { arguments, .. } => {
            for a in arguments {
                collect_closure_captures_expr(a, map);
            }
        }
        typed_ast::ExprKind::CallIndirect { callee, arguments } => {
            collect_closure_captures_expr(callee, map);
            for a in arguments {
                collect_closure_captures_expr(a, map);
            }
        }
        typed_ast::ExprKind::StructLiteral { fields, .. } => {
            for f in fields {
                collect_closure_captures_expr(&f.value, map);
            }
        }
        typed_ast::ExprKind::Index { object, index } => {
            collect_closure_captures_expr(object, map);
            collect_closure_captures_expr(index, map);
        }
        typed_ast::ExprKind::Slice { object, start, end } => {
            collect_closure_captures_expr(object, map);
            collect_closure_captures_expr(start, map);
            collect_closure_captures_expr(end, map);
        }
        typed_ast::ExprKind::ArrayLiteral(elems) => {
            for e in elems {
                collect_closure_captures_expr(e, map);
            }
        }
        typed_ast::ExprKind::ArrayRepeat { element, count } => {
            collect_closure_captures_expr(element, map);
            collect_closure_captures_expr(count, map);
        }
        typed_ast::ExprKind::ArrayInit { count, init } => {
            collect_closure_captures_expr(count, map);
            collect_closure_captures_expr(init, map);
        }
        typed_ast::ExprKind::ArraySizeCoerce { expr: inner, .. } => {
            collect_closure_captures_expr(inner, map);
        }
        typed_ast::ExprKind::BinaryOp { left, right, .. } => {
            collect_closure_captures_expr(left, map);
            collect_closure_captures_expr(right, map);
        }
        typed_ast::ExprKind::If {
            condition,
            then_body,
            else_body,
        } => {
            collect_closure_captures_expr(condition, map);
            collect_closure_captures(then_body, map);
            collect_closure_captures(else_body, map);
        }
        typed_ast::ExprKind::Block(stmts) => {
            collect_closure_captures(stmts, map);
        }
        typed_ast::ExprKind::Loop(stmts) => {
            collect_closure_captures(stmts, map);
        }
        typed_ast::ExprKind::EnumVariant { value, .. } => {
            if let Some(v) = value {
                collect_closure_captures_expr(v, map);
            }
        }
        typed_ast::ExprKind::Match { scrutinee, arms } => {
            collect_closure_captures_expr(scrutinee, map);
            for arm in arms {
                collect_closure_captures(&arm.body, map);
            }
        }
        typed_ast::ExprKind::IntrinsicCall { arguments, .. } => {
            for a in arguments {
                collect_closure_captures_expr(a, map);
            }
        }
        typed_ast::ExprKind::Identifier(_)
        | typed_ast::ExprKind::Global(_)
        | typed_ast::ExprKind::IntegerLiteral(_)
        | typed_ast::ExprKind::BooleanLiteral(_)
        | typed_ast::ExprKind::NullLiteral
        | typed_ast::ExprKind::FunctionRef(_) => {}
    }
}

pub fn type_size(ty: &Type, datatypes: &HashMap<String, DataType>) -> usize {
    match ty {
        Type::Int8 | Type::Uint8 | Type::Bool => 1,
        Type::Int16 | Type::Uint16 => 2,
        Type::Int32 | Type::Uint32 | Type::Float32 => 4,
        Type::Int64 | Type::Uint64 | Type::Int | Type::Uint | Type::Float64 => 8,
        Type::Function { .. } => 16,
        Type::Ref(_) | Type::NullableRef(_) | Type::Unique(_) | Type::FileDesc => 8,
        Type::RefUnsized(_) | Type::NullableRefUnsized(_) | Type::UniqueUnsized(_) => 16,
        Type::FixedArray(inner, n) => (*n as usize) * type_size(inner, datatypes),
        Type::Array(_) => panic!("type_size called on unsized type [T]"),
        Type::Struct(name) | Type::Enum(name) => {
            assert!(
                datatypes[name.as_str()].is_sized,
                "type_size called on unsized type {name}"
            );
            datatypes[name].size
        }
        Type::Unit | Type::Never => 0,
    }
}

pub fn type_align(ty: &Type, datatypes: &HashMap<String, DataType>) -> usize {
    match ty {
        Type::Int8 | Type::Uint8 | Type::Bool => 1,
        Type::Int16 | Type::Uint16 => 2,
        Type::Int32 | Type::Uint32 | Type::Float32 => 4,
        Type::Int64 | Type::Uint64 | Type::Int | Type::Uint | Type::Float64 => 8,
        Type::Ref(_) | Type::NullableRef(_) | Type::Unique(_) | Type::FileDesc => 8,
        Type::RefUnsized(_)
        | Type::NullableRefUnsized(_)
        | Type::UniqueUnsized(_)
        | Type::Function { .. } => 16,
        Type::Array(inner) | Type::FixedArray(inner, _) => type_align(inner, datatypes),
        Type::Struct(name) | Type::Enum(name) => datatypes[name].align,
        Type::Unit | Type::Never => 1,
    }
}

pub fn align_up(offset: usize, align: usize) -> usize {
    (offset + align - 1) & !(align - 1)
}

pub fn is_sized(ty: &Type, dt: &HashMap<String, DataType>) -> bool {
    match ty {
        Type::Array(_) => false,
        Type::FixedArray(_, _) | Type::Function { .. } => true,
        Type::Enum(_) => true,
        Type::Struct(name) => dt[name.as_str()].is_sized,
        _ => true,
    }
}

/// Compute actual size of a value including unsized tail.
/// For sized types, meta is ignored. For unsized types, meta = element count.
pub fn full_size(ty: &Type, dt: &HashMap<String, DataType>, meta: usize) -> usize {
    match ty {
        Type::Array(inner) | Type::FixedArray(inner, _) => meta * type_size(inner, dt),
        Type::Enum(name) => dt[name.as_str()].size,
        Type::Struct(name) => {
            let d = &dt[name.as_str()];
            if d.is_sized {
                d.size
            } else {
                let last = d.fields.last().unwrap();
                let tail = full_size(&last.ty, dt, meta);
                align_up(last.offset + tail, d.align)
            }
        }
        _ => type_size(ty, dt),
    }
}

pub fn type_contains_unique(ty: &Type, dt: &HashMap<String, DataType>) -> bool {
    match ty {
        Type::Unique(_) | Type::UniqueUnsized(_) => true,
        Type::Struct(name) | Type::Enum(name) => {
            if let Some(d) = dt.get(name.as_str()) {
                d.fields.iter().any(|f| type_contains_unique(&f.ty, dt))
            } else {
                false
            }
        }
        Type::FixedArray(inner, _) => type_contains_unique(inner, dt),
        _ => false,
    }
}

pub fn type_contains_gc_ptr(ty: &Type, dt: &HashMap<String, DataType>) -> bool {
    match ty {
        Type::Ref(_)
        | Type::Unique(_)
        | Type::RefUnsized(_)
        | Type::UniqueUnsized(_)
        | Type::NullableRef(_)
        | Type::NullableRefUnsized(_) => true,
        // A `FileDesc` is a traced pointer into the fd arena.
        Type::FileDesc => true,
        Type::Function { .. } => true,
        Type::Struct(name) | Type::Enum(name) => {
            if let Some(d) = dt.get(name.as_str()) {
                d.fields.iter().any(|f| type_contains_gc_ptr(&f.ty, dt))
            } else {
                false
            }
        }
        Type::FixedArray(inner, _) | Type::Array(inner) => type_contains_gc_ptr(inner, dt),
        _ => false,
    }
}

pub fn type_contains_enum(ty: &Type, dt: &HashMap<String, DataType>) -> bool {
    match ty {
        Type::Enum(_) => true,
        Type::Struct(name) => {
            if let Some(d) = dt.get(name.as_str()) {
                d.fields.iter().any(|f| type_contains_enum(&f.ty, dt))
            } else {
                false
            }
        }
        Type::FixedArray(inner, _) => type_contains_enum(inner, dt),
        _ => false,
    }
}

pub fn is_place(nodes: &[Node], id: NodeId) -> bool {
    match &nodes[id.0].kind {
        NodeKind::Local(_)
        | NodeKind::Global(_)
        | NodeKind::FieldAccess { .. }
        | NodeKind::Deref(_)
        | NodeKind::Index { .. }
        | NodeKind::Slice { .. } => true,
        NodeKind::IfExpr {
            then_body,
            else_body,
            ..
        } => branch_tail_is_place(nodes, then_body) && branch_tail_is_place(nodes, else_body),
        NodeKind::Match { arms, .. } => arms
            .iter()
            .all(|arm| branch_tail_is_place(nodes, &arm.body)),
        _ => false,
    }
}

fn branch_tail_is_place(nodes: &[Node], body: &[NodeId]) -> bool {
    body.last().is_some_and(
        |&id| matches!(&nodes[id.0].kind, NodeKind::Expr(inner) if is_place(nodes, *inner)),
    )
}

enum PendingType<'a> {
    Struct(&'a typed_ast::StructDef),
    Enum(&'a typed_ast::EnumDef),
}

fn build_datatypes(source: &typed_ast::SourceFile) -> HashMap<String, DataType> {
    let mut result: HashMap<String, DataType> = HashMap::new();

    let mut remaining: Vec<PendingType> = Vec::new();
    for s in source.structs.values() {
        remaining.push(PendingType::Struct(s));
    }
    for e in source.enums.values() {
        remaining.push(PendingType::Enum(e));
    }

    while !remaining.is_empty() {
        let before = remaining.len();
        remaining.retain(|pending| match pending {
            PendingType::Struct(s) => {
                if s.fields.iter().all(|f| can_resolve_type(&f.ty, &result)) {
                    let dt = layout_struct(s, &result);
                    result.insert(s.name.clone(), dt);
                    false
                } else {
                    true
                }
            }
            PendingType::Enum(e) => {
                let all_resolved = e.variants.iter().all(|v| match &v.inner_type {
                    Some(ty) => can_resolve_type(ty, &result),
                    None => true,
                });
                if all_resolved {
                    let dt = layout_enum(e, &result);
                    result.insert(e.name.clone(), dt);
                    false
                } else {
                    true
                }
            }
        });
        assert!(
            remaining.len() < before,
            "circular type dependency: {:?}",
            remaining
                .iter()
                .map(|p| match p {
                    PendingType::Struct(s) => &s.name,
                    PendingType::Enum(e) => &e.name,
                })
                .collect::<Vec<_>>()
        );
    }

    result
}

fn can_resolve_type(ty: &Type, resolved: &HashMap<String, DataType>) -> bool {
    match ty {
        Type::Struct(name) | Type::Enum(name) => resolved.contains_key(name),
        Type::Array(inner) | Type::FixedArray(inner, _) => can_resolve_type(inner, resolved),
        Type::Function { .. } => true,
        _ => true,
    }
}

fn layout_struct(s: &typed_ast::StructDef, resolved: &HashMap<String, DataType>) -> DataType {
    let mut offset = 0usize;
    let mut max_align = 1usize;
    let mut fields = Vec::new();
    let mut struct_is_sized = true;

    for (i, f) in s.fields.iter().enumerate() {
        let is_last = i == s.fields.len() - 1;
        let field_is_sized = is_sized(&f.ty, resolved);
        let align = type_align(&f.ty, resolved);
        offset = align_up(offset, align);

        if !field_is_sized && is_last {
            // Unsized tail field: size = 0, struct becomes unsized
            fields.push(FieldLayout {
                name: f.name.clone(),
                ty: f.ty.clone(),
                offset,
                size: 0,
            });
            struct_is_sized = false;
        } else {
            let size = type_size(&f.ty, resolved);
            fields.push(FieldLayout {
                name: f.name.clone(),
                ty: f.ty.clone(),
                offset,
                size,
            });
            offset += size;
        }
        max_align = max_align.max(align);
    }

    DataType {
        name: s.name.clone(),
        size: align_up(offset, max_align),
        align: max_align,
        is_sized: struct_is_sized,
        fields,
        variant_map: None,
    }
}

fn layout_enum(e: &typed_ast::EnumDef, resolved: &HashMap<String, DataType>) -> DataType {
    // Layout: [discriminant: u64][variant0_data][variant1_data]...
    let mut offset = 8usize; // discriminant is 8 bytes
    let mut max_align = 8usize; // at least 8 for discriminant
    let mut fields = Vec::new();
    let mut variant_map = Vec::new();

    // First field is the discriminant
    fields.push(FieldLayout {
        name: "__discriminant".to_string(),
        ty: Type::Uint64,
        offset: 0,
        size: 8,
    });

    // Each variant with data gets a field; build variant_map for all variants
    for variant in &e.variants {
        if let Some(ref ty) = variant.inner_type {
            let align = type_align(ty, resolved);
            offset = align_up(offset, align);
            let size = type_size(ty, resolved);
            fields.push(FieldLayout {
                name: variant.name.clone(),
                ty: ty.clone(),
                offset,
                size,
            });
            offset += size;
            max_align = max_align.max(align);
            variant_map.push(Some(variant.name.clone()));
        } else {
            variant_map.push(None);
        }
    }

    DataType {
        name: e.name.clone(),
        size: align_up(offset, max_align),
        align: max_align,
        is_sized: true,
        fields,
        variant_map: Some(variant_map),
    }
}

// --- Function lowering ---

use std::ops::RangeFrom;

struct FunctionLowerer<'a> {
    nodes: Vec<Node>,
    next_var: &'a mut RangeFrom<u32>,
    scopes: ScopeStack<VarId>,
    pending_stmts: Vec<NodeId>,
    datatypes: &'a HashMap<String, DataType>,
    /// Static name -> `Module::statics` index, for `ExprKind::Global`.
    static_idx: &'a HashMap<String, usize>,
}

impl<'a> FunctionLowerer<'a> {
    fn new(
        next_var: &'a mut RangeFrom<u32>,
        datatypes: &'a HashMap<String, DataType>,
        static_idx: &'a HashMap<String, usize>,
    ) -> Self {
        FunctionLowerer {
            nodes: Vec::new(),
            next_var,
            scopes: ScopeStack::default(),
            pending_stmts: Vec::new(),
            datatypes,
            static_idx,
        }
    }

    fn fresh_var(&mut self) -> VarId {
        VarId(self.next_var.next().unwrap())
    }

    fn push_scope(&mut self) {
        self.scopes.push();
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    fn define(&mut self, name: &str) -> VarId {
        let var = self.fresh_var();
        self.scopes.define(name.to_string(), var);
        var
    }

    fn lookup(&self, name: &str) -> VarId {
        *self
            .scopes
            .lookup(name)
            .unwrap_or_else(|| panic!("undefined variable in IR lowering: {name}"))
    }

    fn push(&mut self, node: Node) -> NodeId {
        let id = NodeId(self.nodes.len());
        self.nodes.push(node);
        id
    }

    fn drain_pending(&mut self) -> Vec<NodeId> {
        std::mem::take(&mut self.pending_stmts)
    }

    fn lower_body(&mut self, stmts: &[typed_ast::Statement]) -> Vec<NodeId> {
        let mut body = Vec::new();
        for s in stmts {
            let id = self.lower_stmt(s);
            body.extend(self.drain_pending());
            body.push(id);
        }
        body
    }

    fn lower_expr(&mut self, expr: &typed_ast::Expr) -> NodeId {
        match &expr.kind {
            typed_ast::ExprKind::Identifier(name) => {
                let var = self.lookup(name);
                self.push(Node {
                    ty: expr.ty.clone(),
                    kind: NodeKind::Local(var),
                    span: expr.span,
                })
            }
            typed_ast::ExprKind::Global(name) => self.push(Node {
                ty: expr.ty.clone(),
                kind: NodeKind::Global(self.static_idx[name.as_str()]),
                span: expr.span,
            }),
            typed_ast::ExprKind::IntegerLiteral(n) => self.push(Node {
                ty: expr.ty.clone(),
                kind: NodeKind::IntegerLiteral(*n),
                span: expr.span,
            }),
            typed_ast::ExprKind::BooleanLiteral(b) => self.push(Node {
                ty: expr.ty.clone(),
                kind: NodeKind::BooleanLiteral(*b),
                span: expr.span,
            }),
            typed_ast::ExprKind::NullLiteral => self.push(Node {
                ty: expr.ty.clone(),
                kind: NodeKind::Null,
                span: expr.span,
            }),
            typed_ast::ExprKind::FieldAccess { object, field } => {
                let mut obj = self.lower_expr(object);
                // Field access projects into a place: bind a non-place base
                // (e.g. a call result — `f(x).field`) to a temp `Let` first,
                // like the non-place `match` scrutinee and `Reference` spills.
                if !is_place(&self.nodes, obj) {
                    let obj_ty = self.nodes[obj.0].ty.clone();
                    let var = self.fresh_var();
                    let let_node = self.push(Node {
                        ty: obj_ty.clone(),
                        kind: NodeKind::Let {
                            var,
                            value: obj,
                            noescape: false,
                        },
                        span: expr.span,
                    });
                    self.pending_stmts.push(let_node);
                    obj = self.push(Node {
                        ty: obj_ty,
                        kind: NodeKind::Local(var),
                        span: expr.span,
                    });
                }
                self.push(Node {
                    ty: expr.ty.clone(),
                    kind: NodeKind::FieldAccess {
                        object: obj,
                        field: field.clone(),
                    },
                    span: expr.span,
                })
            }
            typed_ast::ExprKind::Deref(inner) => {
                let id = self.lower_expr(inner);
                self.push(Node {
                    ty: expr.ty.clone(),
                    kind: NodeKind::Deref(id),
                    span: expr.span,
                })
            }
            typed_ast::ExprKind::Not(inner) => {
                let id = self.lower_expr(inner);
                self.push(Node {
                    ty: expr.ty.clone(),
                    kind: NodeKind::Not(id),
                    span: expr.span,
                })
            }
            typed_ast::ExprKind::Reference(inner) => {
                let id = self.lower_expr(inner);
                // If the inner expression is not a place, allocate a temporary
                // so the Ref always points to a valid memory location.
                let id = if is_place(&self.nodes, id) {
                    id
                } else {
                    let inner_ty = self.nodes[id.0].ty.clone();
                    let var = self.fresh_var();
                    let let_node = self.push(Node {
                        ty: inner_ty.clone(),
                        kind: NodeKind::Let {
                            var,
                            value: id,
                            noescape: false,
                        },
                        span: expr.span,
                    });
                    self.pending_stmts.push(let_node);
                    self.push(Node {
                        ty: inner_ty,
                        kind: NodeKind::Local(var),
                        span: expr.span,
                    })
                };
                let ref_node = self.push(Node {
                    ty: expr.ty.clone(),
                    kind: NodeKind::Ref(id),
                    span: expr.span,
                });
                // Materialize the reference itself into a temporary so it is a place.
                let ref_ty = expr.ty.clone();
                let ref_var = self.fresh_var();
                let ref_let = self.push(Node {
                    ty: ref_ty.clone(),
                    kind: NodeKind::Let {
                        var: ref_var,
                        value: ref_node,
                        noescape: false,
                    },
                    span: expr.span,
                });
                self.pending_stmts.push(ref_let);
                self.push(Node {
                    ty: ref_ty,
                    kind: NodeKind::Local(ref_var),
                    span: expr.span,
                })
            }
            typed_ast::ExprKind::Unique(inner) => {
                let id = self.lower_expr(inner);
                let id = if is_place(&self.nodes, id) {
                    id
                } else {
                    let inner_ty = self.nodes[id.0].ty.clone();
                    let var = self.fresh_var();
                    let let_node = self.push(Node {
                        ty: inner_ty.clone(),
                        kind: NodeKind::Let {
                            var,
                            value: id,
                            noescape: false,
                        },
                        span: expr.span,
                    });
                    self.pending_stmts.push(let_node);
                    self.push(Node {
                        ty: inner_ty,
                        kind: NodeKind::Local(var),
                        span: expr.span,
                    })
                };
                let unique_node = self.push(Node {
                    ty: expr.ty.clone(),
                    kind: NodeKind::Unique(id),
                    span: expr.span,
                });
                let unique_ty = expr.ty.clone();
                let unique_var = self.fresh_var();
                let unique_let = self.push(Node {
                    ty: unique_ty.clone(),
                    kind: NodeKind::Let {
                        var: unique_var,
                        value: unique_node,
                        noescape: false,
                    },
                    span: expr.span,
                });
                self.pending_stmts.push(unique_let);
                self.push(Node {
                    ty: unique_ty,
                    kind: NodeKind::Local(unique_var),
                    span: expr.span,
                })
            }
            typed_ast::ExprKind::Call {
                function,
                arguments,
            } => {
                let args: Vec<NodeId> = arguments.iter().map(|a| self.lower_expr(a)).collect();
                self.push(Node {
                    ty: expr.ty.clone(),
                    kind: NodeKind::Call {
                        function: function.clone(),
                        args,
                    },
                    span: expr.span,
                })
            }
            typed_ast::ExprKind::IntrinsicCall {
                intrinsic,
                arguments,
            } => {
                let args: Vec<NodeId> = arguments.iter().map(|a| self.lower_expr(a)).collect();
                self.push(Node {
                    ty: expr.ty.clone(),
                    kind: NodeKind::IntrinsicCall {
                        intrinsic: intrinsic.clone(),
                        args,
                    },
                    span: expr.span,
                })
            }
            typed_ast::ExprKind::FunctionRef(name) => self.push(Node {
                ty: expr.ty.clone(),
                kind: NodeKind::FunctionRef(name.clone()),
                span: expr.span,
            }),
            typed_ast::ExprKind::CallIndirect { callee, arguments } => {
                let callee_id = self.lower_expr(callee);
                let args: Vec<NodeId> = arguments.iter().map(|a| self.lower_expr(a)).collect();
                self.push(Node {
                    ty: expr.ty.clone(),
                    kind: NodeKind::CallIndirect {
                        callee: callee_id,
                        args,
                    },
                    span: expr.span,
                })
            }
            typed_ast::ExprKind::StructLiteral { name, fields } => {
                let fields: Vec<(String, NodeId)> = fields
                    .iter()
                    .map(|f| (f.name.clone(), self.lower_expr(&f.value)))
                    .collect();
                self.push(Node {
                    ty: expr.ty.clone(),
                    kind: NodeKind::StructLiteral {
                        name: name.clone(),
                        fields,
                    },
                    span: expr.span,
                })
            }
            typed_ast::ExprKind::Index { object, index } => {
                let obj = self.lower_expr(object);
                let idx = self.lower_expr(index);
                self.push(Node {
                    ty: expr.ty.clone(),
                    kind: NodeKind::Index {
                        object: obj,
                        index: idx,
                    },
                    span: expr.span,
                })
            }
            typed_ast::ExprKind::Slice { object, start, end } => {
                let obj = self.lower_expr(object);
                let s = self.lower_expr(start);
                let e = self.lower_expr(end);
                self.push(Node {
                    ty: expr.ty.clone(),
                    kind: NodeKind::Slice {
                        object: obj,
                        start: s,
                        end: e,
                    },
                    span: expr.span,
                })
            }
            typed_ast::ExprKind::ArrayLiteral(elements) => {
                let elems: Vec<NodeId> = elements.iter().map(|e| self.lower_expr(e)).collect();
                self.push(Node {
                    ty: expr.ty.clone(),
                    kind: NodeKind::ArrayLiteral(elems),
                    span: expr.span,
                })
            }
            typed_ast::ExprKind::ArrayRepeat { element, count } => {
                let elem = self.lower_expr(element);
                let cnt = self.lower_expr(count);
                self.push(Node {
                    ty: expr.ty.clone(),
                    kind: NodeKind::ArrayRepeat {
                        element: elem,
                        count: cnt,
                    },
                    span: expr.span,
                })
            }
            typed_ast::ExprKind::ArrayInit { count, init } => {
                let cnt = self.lower_expr(count);
                let ini = self.lower_expr(init);
                self.push(Node {
                    ty: expr.ty.clone(),
                    kind: NodeKind::ArrayInit {
                        count: cnt,
                        init: ini,
                    },
                    span: expr.span,
                })
            }
            typed_ast::ExprKind::ArraySizeCoerce { expr: inner, size } => {
                let val = self.lower_expr(inner);
                self.push(Node {
                    ty: expr.ty.clone(),
                    kind: NodeKind::ArraySizeCoerce {
                        value: val,
                        size: *size,
                    },
                    span: expr.span,
                })
            }
            typed_ast::ExprKind::BinaryOp { op, left, right } => {
                let l = self.lower_expr(left);
                let r = self.lower_expr(right);
                self.push(Node {
                    ty: expr.ty.clone(),
                    kind: NodeKind::BinaryOp {
                        op: *op,
                        left: l,
                        right: r,
                    },
                    span: expr.span,
                })
            }
            typed_ast::ExprKind::If {
                condition,
                then_body,
                else_body,
            } => {
                let cond = self.lower_expr(condition);
                // Stash the condition's setup statements so the branch bodies'
                // `lower_body` doesn't drain them into a branch; restored below to
                // run before the if. (Same pattern as the statement-level `if`.)
                let cond_pending = self.drain_pending();
                self.push_scope();
                let lowered_then = self.lower_body(then_body);
                self.pop_scope();
                self.push_scope();
                let lowered_else = self.lower_body(else_body);
                self.pop_scope();
                self.pending_stmts.extend(cond_pending);
                self.push(Node {
                    ty: expr.ty.clone(),
                    kind: NodeKind::IfExpr {
                        condition: cond,
                        then_body: lowered_then,
                        else_body: lowered_else,
                    },
                    span: expr.span,
                })
            }
            typed_ast::ExprKind::Block(stmts) => {
                self.push_scope();
                // Lower all but last as pending stmts, lower tail expr and return its NodeId
                let has_tail = stmts
                    .last()
                    .is_some_and(|s| matches!(s.kind, typed_ast::StatementKind::Expression(_)));
                if has_tail {
                    let (init, tail) = stmts.split_at(stmts.len() - 1);
                    let mut body = self.lower_body(init);
                    // The tail expression produces the block's value
                    let tail_id = match &tail[0].kind {
                        typed_ast::StatementKind::Expression(e) => self.lower_expr(e),
                        _ => unreachable!(),
                    };
                    body.extend(self.drain_pending());
                    self.pending_stmts.extend(body);
                    self.pop_scope();
                    tail_id
                } else {
                    let body = self.lower_body(stmts);
                    self.pending_stmts.extend(body);
                    self.pop_scope();
                    // Unit-typed block: return a dummy unit node
                    self.push(Node {
                        ty: Type::Unit,
                        kind: NodeKind::BooleanLiteral(false),
                        span: expr.span,
                    })
                }
            }
            typed_ast::ExprKind::Loop(stmts) => {
                self.push_scope();
                let body = self.lower_body(stmts);
                self.pop_scope();
                // The loop's value (if any) is produced by `break <v>` inside the
                // body, written into the loop's eval destination.
                self.push(Node {
                    ty: expr.ty.clone(),
                    kind: NodeKind::Loop { body },
                    span: expr.span,
                })
            }
            typed_ast::ExprKind::EnumVariant {
                enum_name,
                variant_name,
                variant_index,
                value,
            } => {
                let val = value.as_ref().map(|v| self.lower_expr(v));
                self.push(Node {
                    ty: expr.ty.clone(),
                    kind: NodeKind::EnumVariant {
                        enum_name: enum_name.clone(),
                        variant_name: variant_name.clone(),
                        variant_index: *variant_index as u64,
                        value: val,
                    },
                    span: expr.span,
                })
            }
            typed_ast::ExprKind::Match { scrutinee, arms } => {
                let mut scrut = self.lower_expr(scrutinee);
                // If the scrutinee isn't a place (e.g. a call result), bind it to
                // a temp so codegen reads it from a named slot rather than boxing
                // it inline — that slot is then a `Let` the escape analysis can
                // stack-allocate (`match call()` matches `let t = call(); match
                // t`). Done before draining pending so the new `Let` runs before
                // the match, not inside an arm.
                if !is_place(&self.nodes, scrut) {
                    let scrut_ty = self.nodes[scrut.0].ty.clone();
                    let var = self.fresh_var();
                    let let_node = self.push(Node {
                        ty: scrut_ty.clone(),
                        kind: NodeKind::Let {
                            var,
                            value: scrut,
                            noescape: false,
                        },
                        span: expr.span,
                    });
                    self.pending_stmts.push(let_node);
                    scrut = self.push(Node {
                        ty: scrut_ty,
                        kind: NodeKind::Local(var),
                        span: expr.span,
                    });
                }
                // Stash the scrutinee's setup statements (e.g. reference temps)
                // so the arms' `lower_body` doesn't drain them into an arm body;
                // they're restored below to run before the match. (Same pattern
                // as the statement-level `if`/`while`.)
                let scrut_pending = self.drain_pending();
                let lowered_arms: Vec<MatchArm> = arms
                    .iter()
                    .map(|arm| {
                        self.push_scope();
                        let pattern = match &arm.pattern {
                            typed_ast::TypedPattern::Variant {
                                enum_name,
                                variant_name,
                                variant_index,
                                binding,
                            } => {
                                let binding_ir = binding.as_ref().map(|(name, ty)| {
                                    let var = self.define(name);
                                    (var, ty.clone())
                                });
                                MatchPattern::Variant {
                                    enum_name: enum_name.clone(),
                                    variant_name: variant_name.clone(),
                                    variant_index: *variant_index as u64,
                                    binding: binding_ir,
                                }
                            }
                            typed_ast::TypedPattern::Wildcard(name, ty) => {
                                let var = self.define(name);
                                MatchPattern::Wildcard(var, ty.clone())
                            }
                        };
                        let body = self.lower_body(&arm.body);
                        self.pop_scope();
                        MatchArm { pattern, body }
                    })
                    .collect();
                self.pending_stmts.extend(scrut_pending);
                self.push(Node {
                    ty: expr.ty.clone(),
                    kind: NodeKind::Match {
                        scrutinee: scrut,
                        arms: lowered_arms,
                    },
                    span: expr.span,
                })
            }
            typed_ast::ExprKind::Closure {
                synthetic_fn,
                captures,
            } => {
                // For each captured variable, emit a Ref node to get its address
                let capture_nodes: Vec<NodeId> = captures
                    .iter()
                    .map(|cap| {
                        let var = self.lookup(&cap.name);
                        let local = self.push(Node {
                            ty: cap.ty.clone(),
                            kind: NodeKind::Local(var),
                            span: expr.span,
                        });
                        // A reference to an unsized capture (e.g. a `[Uint8]`) is a
                        // fat pointer, so its node type must be `RefUnsized`, not
                        // `Ref` — else the env slot is sized as a thin pointer and
                        // the meta half is written out of bounds.
                        let ref_ty = if is_sized(&cap.ty, self.datatypes) {
                            Type::Ref(Box::new(cap.ty.clone()))
                        } else {
                            Type::RefUnsized(Box::new(cap.ty.clone()))
                        };
                        self.push(Node {
                            ty: ref_ty,
                            kind: NodeKind::Ref(local),
                            span: expr.span,
                        })
                    })
                    .collect();
                self.push(Node {
                    ty: expr.ty.clone(),
                    kind: NodeKind::MakeClosure {
                        function: synthetic_fn.clone(),
                        captures: capture_nodes,
                    },
                    span: expr.span,
                })
            }
        }
    }

    fn lower_stmt(&mut self, stmt: &typed_ast::Statement) -> NodeId {
        match &stmt.kind {
            typed_ast::StatementKind::Let { name, ty, value } => {
                let val = self.lower_expr(value);
                let var = self.define(name);
                self.push(Node {
                    ty: ty.clone(),
                    kind: NodeKind::Let {
                        var,
                        value: val,
                        noescape: false,
                    },
                    span: stmt.span,
                })
            }
            typed_ast::StatementKind::Assignment { target, value } => {
                let tgt = self.lower_expr(target);
                let tgt_pending = self.drain_pending();
                let val = self.lower_expr(value);
                self.pending_stmts.splice(0..0, tgt_pending);
                self.push(Node {
                    ty: Type::Unit,
                    kind: NodeKind::Assign {
                        target: tgt,
                        value: val,
                    },
                    span: stmt.span,
                })
            }
            typed_ast::StatementKind::If {
                condition,
                body,
                else_body,
            } => {
                let cond = self.lower_expr(condition);
                let cond_pending = self.drain_pending();
                self.push_scope();
                let then_body = self.lower_body(body);
                self.pop_scope();
                let lowered_else = if !else_body.is_empty() {
                    self.push_scope();
                    let v = self.lower_body(else_body);
                    self.pop_scope();
                    v
                } else {
                    Vec::new()
                };
                self.pending_stmts.extend(cond_pending);
                self.push(Node {
                    ty: Type::Unit,
                    kind: NodeKind::If {
                        condition: cond,
                        then_body,
                        else_body: lowered_else,
                    },
                    span: stmt.span,
                })
            }
            typed_ast::StatementKind::While { condition, body } => {
                // Lower condition (may produce pending stmts from block exprs)
                let cond = self.lower_expr(condition);
                let cond_pending = self.drain_pending();
                self.push_scope();
                let lowered_body = self.lower_body(body);
                self.pop_scope();

                // Build: Loop { ...cond_pending, If(Not(cond)) { Break }, ...body }
                let not_cond = self.push(Node {
                    ty: Type::Bool,
                    kind: NodeKind::Not(cond),
                    span: stmt.span,
                });
                let break_node = self.push(Node {
                    ty: Type::Unit,
                    kind: NodeKind::Break(None),
                    span: stmt.span,
                });
                let if_break = self.push(Node {
                    ty: Type::Unit,
                    kind: NodeKind::If {
                        condition: not_cond,
                        then_body: vec![break_node],
                        else_body: Vec::new(),
                    },
                    span: stmt.span,
                });

                let mut loop_body = Vec::new();
                loop_body.extend(cond_pending);
                loop_body.push(if_break);
                loop_body.extend(lowered_body);

                self.push(Node {
                    ty: Type::Unit,
                    kind: NodeKind::Loop { body: loop_body },
                    span: stmt.span,
                })
            }
            typed_ast::StatementKind::Expression(expr) => {
                let id = self.lower_expr(expr);
                // A bare `loop` statement is executed as a statement (so `return`
                // and outer `break`/`continue` propagate, like `while`), rather
                // than wrapped as an expression evaluated into a throwaway.
                if matches!(&expr.kind, typed_ast::ExprKind::Loop(_)) {
                    id
                } else {
                    self.push(Node {
                        ty: expr.ty.clone(),
                        kind: NodeKind::Expr(id),
                        span: stmt.span,
                    })
                }
            }
            typed_ast::StatementKind::Return(expr) => {
                let id = self.lower_expr(expr);
                self.push(Node {
                    ty: expr.ty.clone(),
                    kind: NodeKind::Return(id),
                    span: stmt.span,
                })
            }
            typed_ast::StatementKind::Break(value) => {
                let value_id = value.as_ref().map(|v| self.lower_expr(v));
                self.push(Node {
                    ty: Type::Unit,
                    kind: NodeKind::Break(value_id),
                    span: stmt.span,
                })
            }
            typed_ast::StatementKind::Continue => self.push(Node {
                ty: Type::Unit,
                kind: NodeKind::Continue,
                span: stmt.span,
            }),
        }
    }
}

fn lower_function(
    func: &typed_ast::FunctionDef,
    next_var: &mut RangeFrom<u32>,
    captures: Option<&Vec<typed_ast::CapturedVar>>,
    datatypes: &HashMap<String, DataType>,
    static_idx: &HashMap<String, usize>,
) -> Function {
    let mut lowerer = FunctionLowerer::new(next_var, datatypes, static_idx);
    lowerer.push_scope();

    // For closure functions, define captured variables in scope first
    let env_captures: Vec<EnvCapture> = if let Some(caps) = captures {
        caps.iter()
            .enumerate()
            .map(|(i, cap)| {
                let var = lowerer.define(&cap.name);
                EnvCapture {
                    var,
                    index: i,
                    is_unsized: !is_sized(&cap.ty, datatypes),
                }
            })
            .collect()
    } else {
        Vec::new()
    };

    let params: Vec<Param> = func
        .parameters
        .iter()
        .map(|p| {
            let var = lowerer.define(&p.name);
            Param {
                var,
                name: p.name.clone(),
                ty: p.ty.clone(),
            }
        })
        .collect();

    let body = lowerer.lower_body(&func.body);
    lowerer.pop_scope();

    let num_params = params.len();
    Function {
        name: func.name.clone(),
        params,
        return_type: func.return_type.clone(),
        nodes: lowerer.nodes,
        body,
        env_captures,
        inline_hint: func.inline_hint,
        // Conservative default: every parameter may escape. Refined by
        // `ir_opt::analyze_param_escapes`.
        param_noescape: vec![false; num_params],
    }
}
