//! Typed AST with definition identities replaced by final symbol names.

use crate::ast;
use crate::intrinsics::Intrinsic;
use crate::typed_ast as ta;
use std::collections::HashMap;
/// A fully resolved Solar type with final symbol names.
pub type Type = crate::types::Type<String>;

/// A mangled, monomorphized program.
#[derive(Debug)]
pub struct SourceFile {
    /// Struct definitions keyed by symbol.
    pub structs: HashMap<String, StructDef>,
    /// Enum definitions keyed by symbol.
    pub enums: HashMap<String, EnumDef>,
    /// Function definitions keyed by symbol.
    pub functions: HashMap<String, FunctionDef>,
    /// Mutable globals in source order.
    pub statics: Vec<StaticItem>,
}

/// A mutable global.
#[derive(Debug, Clone)]
pub struct StaticItem {
    /// Global symbol.
    pub name: String,
    /// Stored type.
    pub ty: Type,
    /// Initial value.
    pub init: Expr,
    /// Whether each thread receives an independent instance of this static.
    pub thread_local: bool,
}

/// A struct definition.
#[derive(Debug, Clone)]
pub struct StructDef {
    /// Struct symbol.
    pub name: String,
    /// Whether the struct requires C-compatible field layout.
    pub repr_c: bool,
    /// Fields in declaration order.
    pub fields: Vec<FieldDef>,
}

/// A struct field.
#[derive(Debug, Clone)]
pub struct FieldDef {
    /// Field name.
    pub name: String,
    /// Field type.
    pub ty: Type,
}

impl crate::types::StructDefinitions<String> for HashMap<String, StructDef> {
    fn last_field_type<'a>(&'a self, id: &String) -> Option<Option<&'a Type>> {
        self.get(id)
            .map(|def| def.fields.last().map(|field| &field.ty))
    }
}

/// A function definition.
#[derive(Debug, Clone)]
pub struct FunctionDef {
    /// Function symbol.
    pub name: String,
    /// Function parameters.
    pub parameters: Vec<Parameter>,
    /// Return type.
    pub return_type: Type,
    /// Function body.
    pub body: Vec<Statement>,
    /// Whether the source declaration is unsafe to access.
    pub is_unsafe: bool,
    /// Whether code generation should request inlining.
    pub inline_hint: bool,
}

/// A function parameter.
#[derive(Debug, Clone)]
pub struct Parameter {
    /// Parameter name.
    pub name: String,
    /// Parameter type.
    pub ty: Type,
    /// Declaration span.
    pub span: ast::SourceSpan,
}

/// A statement and its source span.
#[derive(Debug, Clone)]
pub struct Statement {
    /// Statement contents.
    pub kind: StatementKind,
    /// Statement span.
    pub span: ast::SourceSpan,
}

/// A typed statement.
#[derive(Debug, Clone)]
pub enum StatementKind {
    Let {
        name: String,
        ty: Type,
        value: Expr,
    },
    Assignment {
        target: Expr,
        value: Expr,
    },
    If {
        condition: Expr,
        body: Vec<Statement>,
        else_body: Vec<Statement>,
    },
    While {
        condition: Expr,
        body: Vec<Statement>,
    },
    Expression(Expr),
    Return(Expr),
    Break(Option<Expr>),
    Continue,
}

/// An expression with its type and source span.
#[derive(Debug, Clone)]
pub struct Expr {
    /// Expression type.
    pub ty: Type,
    /// Expression contents.
    pub kind: ExprKind,
    /// Expression span.
    pub span: ast::SourceSpan,
}

/// A typed expression.
#[derive(Debug, Clone)]
pub enum ExprKind {
    Identifier(String),
    /// A float literal; the expression's `ty` selects Float32/Float64.
    FloatLiteral(f64),
    /// A reference to a top-level `static` (a global mutable place).
    Global(String),
    IntegerLiteral(i64),
    BooleanLiteral(bool),
    FieldAccess {
        object: Box<Expr>,
        field: String,
    },
    Deref(Box<Expr>),
    Reference(Box<Expr>),
    Unique(Box<Expr>),
    /// Unary `!`: logical not on `Bool`, bitwise complement on integers. The
    /// expression's `ty` is the operand's type.
    Not(Box<Expr>),
    /// `null#[T]` — a null nullable reference. The expression's `ty` carries the
    /// concrete `NullableRef`/`NullableRefUnsized` type.
    NullLiteral,
    Call {
        function: String,
        arguments: Vec<Expr>,
    },
    FunctionRef(String),
    CallIndirect {
        callee: Box<Expr>,
        arguments: Vec<Expr>,
    },
    StructLiteral {
        name: String,
        fields: Vec<FieldInit>,
    },
    Index {
        object: Box<Expr>,
        index: Box<Expr>,
    },
    Slice {
        object: Box<Expr>,
        start: Box<Expr>,
        end: Box<Expr>,
    },
    ArrayLiteral(Vec<Expr>),
    ArrayRepeat {
        element: Box<Expr>,
        count: Box<Expr>,
    },
    ArrayInit {
        count: Box<Expr>,
        init: Box<Expr>,
    },
    ArraySizeCoerce {
        expr: Box<Expr>,
        size: u64,
    },
    BinaryOp {
        op: ast::BinOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    If {
        condition: Box<Expr>,
        then_body: Vec<Statement>,
        else_body: Vec<Statement>,
    },
    Block(Vec<Statement>),
    Loop(Vec<Statement>),
    Closure {
        synthetic_fn: String,
        captures: Vec<CapturedVar>,
    },
    EnumVariant {
        enum_name: String,
        variant_name: String,
        variant_index: usize,
        value: Option<Box<Expr>>,
    },
    Match {
        scrutinee: Box<Expr>,
        arms: Vec<TypedMatchArm>,
    },
    IntrinsicCall {
        intrinsic: Intrinsic,
        arguments: Vec<Expr>,
    },
}

/// A struct field initializer.
#[derive(Debug, Clone)]
pub struct FieldInit {
    /// Field name.
    pub name: String,
    /// Initializer value.
    pub value: Expr,
}

/// A variable captured by a closure.
#[derive(Debug, Clone)]
pub struct CapturedVar {
    /// Variable name.
    pub name: String,
    /// Captured type.
    pub ty: Type,
}

/// An enum definition.
#[derive(Debug, Clone)]
pub struct EnumDef {
    /// Enum symbol.
    pub name: String,
    /// Variants in discriminant order.
    pub variants: Vec<EnumVariantDef>,
}

/// An enum variant definition.
#[derive(Debug, Clone)]
pub struct EnumVariantDef {
    /// Variant name.
    pub name: String,
    /// Optional payload type.
    pub inner_type: Option<Type>,
    /// Numeric discriminant.
    pub index: usize,
}

/// A typed match arm.
#[derive(Debug, Clone)]
pub struct TypedMatchArm {
    /// Arm pattern.
    pub pattern: TypedPattern,
    /// Arm body.
    pub body: Vec<Statement>,
}

/// A typed match pattern.
#[derive(Debug, Clone)]
pub enum TypedPattern {
    Variant {
        enum_name: String,
        variant_name: String,
        variant_index: usize,
        binding: Option<(String, Type)>,
    },
    IntegerLiteral(i64),
    Wildcard(String, Type),
}

use crate::error::SourceMap;
use std::cell::RefCell;
use std::rc::Rc;

/// Prefix for compiler-generated names after structural identifiers are
/// rendered to strings. `$` is not valid in a Solar source identifier.
pub(crate) const SYNTHETIC_NAME_PREFIX: &str = "$synthetic$";

/// Replaces structural definition identities with final symbol names.
pub fn lower(source: &ta::SourceFile, source_map: &SourceMap) -> SourceFile {
    let r = Renderer {
        sm: source_map,
        prefixes: RefCell::new(HashMap::new()),
    };
    SourceFile {
        structs: source
            .structs
            .iter()
            .map(|(k, v)| (r.type_name(k), r.conv_struct(v)))
            .collect(),
        enums: source
            .enums
            .iter()
            .map(|(k, v)| (r.type_name(k), r.conv_enum(v)))
            .collect(),
        functions: source
            .functions
            .iter()
            .map(|(k, v)| (r.func_symbol(k), r.conv_function(v)))
            .collect(),
        statics: source.statics.iter().map(|s| r.conv_static(s)).collect(),
    }
}

/// Self-delimiting identifier encoding (`<byte-len>_<s>`).
fn enc_id(s: &str) -> String {
    format!("{}_{}", s.len(), s)
}

/// Renders provenance-token identities into final module-mangled C symbols.
struct Renderer<'a> {
    sm: &'a SourceMap,
    /// Memoized `SourceMap::module_prefix` per file.
    ///
    /// Computing a prefix is O(files): it splits the path into components and
    /// scans every other file for a colliding suffix. Every rendered symbol
    /// (and there is one per definition, type instance, call and method) needs
    /// its defining file's prefix, so recomputing made mangling quadratic in
    /// (files × symbols) — 85% of the front end's time on a 150-file program
    /// (the Minecraft port's block package: 94 s, of which ~80 s was here).
    /// The prefix depends only on the file set, which is fixed by the time
    /// mangling runs, so one computation per file is enough.
    prefixes: RefCell<HashMap<u32, Rc<str>>>,
}

impl Renderer<'_> {
    fn synthetic_name(&self, name: &str) -> String {
        format!("{SYNTHETIC_NAME_PREFIX}{name}")
    }

    fn ident_name(&self, ident: &ast::Ident) -> String {
        match ident {
            ast::Ident::User(name) => name.clone(),
            ast::Ident::Synthetic(name) => self.synthetic_name(name),
        }
    }

    fn module_prefix(&self, file: u32) -> Rc<str> {
        if let Some(prefix) = self.prefixes.borrow().get(&file) {
            return Rc::clone(prefix);
        }
        let prefix: Rc<str> = Rc::from(self.sm.module_prefix(file).as_str());
        self.prefixes.borrow_mut().insert(file, Rc::clone(&prefix));
        prefix
    }

    /// The base identifier of a definition, module-prefixed by its defining
    /// file. Root definitions stay bare; synthetic definitions receive the
    /// source-impossible synthetic prefix.
    fn base_name(&self, def: &ast::DefId) -> String {
        if def.file == ast::SYNTHETIC_FILE {
            self.synthetic_name(&def.name)
        } else {
            format!("{}{}", self.module_prefix(def.file), def.name)
        }
    }

    /// The final identity string of a struct/enum type instance.
    fn type_name(&self, id: &ta::TypeId) -> String {
        if id.def.file == ast::SYNTHETIC_FILE && id.def.name == ta::TUPLE_DEF_NAME {
            // Anonymous tuple: `0T{n}_{elem types}`.
            let mut s = format!("0T{}_", id.args.len());
            for a in &id.args {
                s.push_str(&self.mangle_type(a));
            }
            return s;
        }
        self.mangle_name(&self.base_name(&id.def), &id.args)
    }

    /// The final C symbol of a function/method instance.
    fn func_symbol(&self, fid: &ta::FuncId) -> String {
        // Methods render their bare base name (`__method_`-prefixed); free
        // functions get the module prefix.
        let base = if fid.method {
            fid.def.name.clone()
        } else {
            self.base_name(&fid.def)
        };
        let mut s = self.mangle_name(&base, &fid.args);
        if fid.method {
            s = format!("__method_{s}");
        }
        if let Some(ov) = fid.overload {
            s = format!("{s}_ov{ov}");
        }
        s
    }

    /// `mangle_name`: the bare base when there are no args, else
    /// `enc_id(base) 'G' <n> '_' <arg types>` (matching the historical scheme).
    fn mangle_name(&self, base: &str, args: &[ta::Type]) -> String {
        if args.is_empty() {
            base.to_string()
        } else {
            let mut s = format!("{}G{}_", enc_id(base), args.len());
            for a in args {
                s.push_str(&self.mangle_type(a));
            }
            s
        }
    }

    /// A type fragment in the mangling grammar.
    fn mangle_type(&self, t: &ta::Type) -> String {
        use ta::Type as T;
        match t {
            T::Int8 => enc_id("Int8"),
            T::Int16 => enc_id("Int16"),
            T::Int32 => enc_id("Int32"),
            T::Int64 => enc_id("Int64"),
            T::Int => enc_id("Int"),
            T::Uint8 => enc_id("Uint8"),
            T::Uint16 => enc_id("Uint16"),
            T::Uint32 => enc_id("Uint32"),
            T::Uint64 => enc_id("Uint64"),
            T::Uint => enc_id("Uint"),
            T::Float32 => enc_id("Float32"),
            T::Float64 => enc_id("Float64"),
            T::Bool => enc_id("Bool"),
            T::Struct(id) | T::Enum(id) => enc_id(&self.type_name(id)),
            T::Ref(inner) | T::RefUnsized(inner) => format!("R{}", self.mangle_type(inner)),
            T::NullableRef(inner) | T::NullableRefUnsized(inner) => {
                format!("Q{}", self.mangle_type(inner))
            }
            T::Unique(inner) | T::UniqueUnsized(inner) => format!("U{}", self.mangle_type(inner)),
            T::Array(inner) => format!("S{}", self.mangle_type(inner)),
            T::FixedArray(inner, n) => format!("A{}_{}", n, self.mangle_type(inner)),
            T::Function {
                params,
                return_type,
            } => {
                let mut s = format!("F{}_", params.len());
                for p in params {
                    s.push_str(&self.mangle_type(p));
                }
                s.push_str(&self.mangle_type(return_type));
                s
            }
            T::FileDesc => enc_id("FileDesc"),
            T::Unit => enc_id("Unit"),
            T::Never => enc_id("Never"),
        }
    }

    fn conv_type(&self, ty: &ta::Type) -> Type {
        ty.map_id(|id| self.type_name(id))
    }

    fn conv_static(&self, s: &ta::StaticItem) -> StaticItem {
        StaticItem {
            name: self.base_name(&s.id),
            ty: self.conv_type(&s.ty),
            init: self.conv_expr(&s.init),
            thread_local: s.thread_local,
        }
    }

    fn conv_struct(&self, s: &ta::StructDef) -> StructDef {
        StructDef {
            name: self.type_name(&s.id),
            repr_c: s.repr_c,
            fields: s.fields.iter().map(|f| self.conv_field(f)).collect(),
        }
    }

    fn conv_field(&self, f: &ta::FieldDef) -> FieldDef {
        FieldDef {
            name: f.name.clone(),
            ty: self.conv_type(&f.ty),
        }
    }

    fn conv_enum(&self, e: &ta::EnumDef) -> EnumDef {
        EnumDef {
            name: self.type_name(&e.id),
            variants: e.variants.iter().map(|v| self.conv_variant(v)).collect(),
        }
    }

    fn conv_variant(&self, v: &ta::EnumVariantDef) -> EnumVariantDef {
        EnumVariantDef {
            name: v.name.clone(),
            inner_type: v.inner_type.as_ref().map(|t| self.conv_type(t)),
            index: v.index,
        }
    }

    fn conv_function(&self, f: &ta::FunctionDef) -> FunctionDef {
        FunctionDef {
            name: self.func_symbol(&f.id),
            parameters: f.parameters.iter().map(|p| self.conv_param(p)).collect(),
            return_type: self.conv_type(&f.return_type),
            body: f.body.iter().map(|s| self.conv_stmt(s)).collect(),
            is_unsafe: f.is_unsafe,
            inline_hint: f.inline_hint,
        }
    }

    fn conv_param(&self, p: &ta::Parameter) -> Parameter {
        Parameter {
            name: self.ident_name(&p.name),
            ty: self.conv_type(&p.ty),
            span: p.span,
        }
    }

    fn conv_capture(&self, c: &ta::CapturedVar) -> CapturedVar {
        CapturedVar {
            name: self.ident_name(&c.name),
            ty: self.conv_type(&c.ty),
        }
    }

    fn conv_stmt(&self, s: &ta::Statement) -> Statement {
        Statement {
            kind: self.conv_stmt_kind(&s.kind),
            span: s.span,
        }
    }

    fn conv_stmt_kind(&self, k: &ta::StatementKind) -> StatementKind {
        use ta::StatementKind as K;
        match k {
            K::Let { name, ty, value } => StatementKind::Let {
                name: self.ident_name(name),
                ty: self.conv_type(ty),
                value: self.conv_expr(value),
            },
            K::Assignment { target, value } => StatementKind::Assignment {
                target: self.conv_expr(target),
                value: self.conv_expr(value),
            },
            K::If {
                condition,
                body,
                else_body,
            } => StatementKind::If {
                condition: self.conv_expr(condition),
                body: body.iter().map(|s| self.conv_stmt(s)).collect(),
                else_body: else_body.iter().map(|s| self.conv_stmt(s)).collect(),
            },
            K::While { condition, body } => StatementKind::While {
                condition: self.conv_expr(condition),
                body: body.iter().map(|s| self.conv_stmt(s)).collect(),
            },
            K::Expression(e) => StatementKind::Expression(self.conv_expr(e)),
            K::Return(e) => StatementKind::Return(self.conv_expr(e)),
            K::Break(e) => StatementKind::Break(e.as_ref().map(|e| self.conv_expr(e))),
            K::Continue => StatementKind::Continue,
        }
    }

    fn conv_expr(&self, e: &ta::Expr) -> Expr {
        Expr {
            ty: self.conv_type(&e.ty),
            kind: self.conv_expr_kind(&e.kind),
            span: e.span,
        }
    }

    fn conv_boxed(&self, e: &ta::Expr) -> Box<Expr> {
        Box::new(self.conv_expr(e))
    }

    fn conv_expr_kind(&self, k: &ta::ExprKind) -> ExprKind {
        use ta::ExprKind as K;
        match k {
            K::Identifier(name) => ExprKind::Identifier(self.ident_name(name)),
            K::FloatLiteral(v) => ExprKind::FloatLiteral(*v),
            K::Global(def) => ExprKind::Global(self.base_name(def)),
            K::IntegerLiteral(v) => ExprKind::IntegerLiteral(*v),
            K::BooleanLiteral(v) => ExprKind::BooleanLiteral(*v),
            K::FieldAccess { object, field } => ExprKind::FieldAccess {
                object: self.conv_boxed(object),
                field: field.clone(),
            },
            K::Deref(e) => ExprKind::Deref(self.conv_boxed(e)),
            K::Reference(e) => ExprKind::Reference(self.conv_boxed(e)),
            K::Unique(e) => ExprKind::Unique(self.conv_boxed(e)),
            K::Not(e) => ExprKind::Not(self.conv_boxed(e)),
            K::NullLiteral => ExprKind::NullLiteral,
            K::Call {
                function,
                arguments,
            } => ExprKind::Call {
                function: self.func_symbol(function),
                arguments: arguments.iter().map(|a| self.conv_expr(a)).collect(),
            },
            K::FunctionRef(fid) => ExprKind::FunctionRef(self.func_symbol(fid)),
            K::CallIndirect { callee, arguments } => ExprKind::CallIndirect {
                callee: self.conv_boxed(callee),
                arguments: arguments.iter().map(|a| self.conv_expr(a)).collect(),
            },
            K::StructLiteral { id, fields } => ExprKind::StructLiteral {
                name: self.type_name(id),
                fields: fields.iter().map(|f| self.conv_field_init(f)).collect(),
            },
            K::Index { object, index } => ExprKind::Index {
                object: self.conv_boxed(object),
                index: self.conv_boxed(index),
            },
            K::Slice { object, start, end } => ExprKind::Slice {
                object: self.conv_boxed(object),
                start: self.conv_boxed(start),
                end: self.conv_boxed(end),
            },
            K::ArrayLiteral(elems) => {
                ExprKind::ArrayLiteral(elems.iter().map(|e| self.conv_expr(e)).collect())
            }
            K::ArrayRepeat { element, count } => ExprKind::ArrayRepeat {
                element: self.conv_boxed(element),
                count: self.conv_boxed(count),
            },
            K::ArrayInit { count, init } => ExprKind::ArrayInit {
                count: self.conv_boxed(count),
                init: self.conv_boxed(init),
            },
            K::ArraySizeCoerce { expr, size } => ExprKind::ArraySizeCoerce {
                expr: self.conv_boxed(expr),
                size: *size,
            },
            K::BinaryOp { op, left, right } => ExprKind::BinaryOp {
                op: *op,
                left: self.conv_boxed(left),
                right: self.conv_boxed(right),
            },
            K::If {
                condition,
                then_body,
                else_body,
            } => ExprKind::If {
                condition: self.conv_boxed(condition),
                then_body: then_body.iter().map(|s| self.conv_stmt(s)).collect(),
                else_body: else_body.iter().map(|s| self.conv_stmt(s)).collect(),
            },
            K::Block(body) => ExprKind::Block(body.iter().map(|s| self.conv_stmt(s)).collect()),
            K::Loop(body) => ExprKind::Loop(body.iter().map(|s| self.conv_stmt(s)).collect()),
            K::Closure {
                synthetic_fn,
                captures,
            } => ExprKind::Closure {
                synthetic_fn: self.synthetic_name(synthetic_fn),
                captures: captures.iter().map(|c| self.conv_capture(c)).collect(),
            },
            K::EnumVariant {
                enum_id,
                variant_name,
                variant_index,
                value,
            } => ExprKind::EnumVariant {
                enum_name: self.type_name(enum_id),
                variant_name: variant_name.clone(),
                variant_index: *variant_index,
                value: value.as_deref().map(|e| self.conv_boxed(e)),
            },
            K::Match { scrutinee, arms } => ExprKind::Match {
                scrutinee: self.conv_boxed(scrutinee),
                arms: arms.iter().map(|a| self.conv_match_arm(a)).collect(),
            },
            K::IntrinsicCall {
                intrinsic,
                arguments,
            } => ExprKind::IntrinsicCall {
                intrinsic: intrinsic.clone(),
                arguments: arguments.iter().map(|a| self.conv_expr(a)).collect(),
            },
        }
    }

    fn conv_field_init(&self, f: &ta::FieldInit) -> FieldInit {
        FieldInit {
            name: f.name.clone(),
            value: self.conv_expr(&f.value),
        }
    }

    fn conv_match_arm(&self, a: &ta::TypedMatchArm) -> TypedMatchArm {
        TypedMatchArm {
            pattern: self.conv_pattern(&a.pattern),
            body: a.body.iter().map(|s| self.conv_stmt(s)).collect(),
        }
    }

    fn conv_pattern(&self, p: &ta::TypedPattern) -> TypedPattern {
        match p {
            ta::TypedPattern::Variant {
                enum_id,
                variant_name,
                variant_index,
                binding,
            } => TypedPattern::Variant {
                enum_name: self.type_name(enum_id),
                variant_name: variant_name.clone(),
                variant_index: *variant_index,
                binding: binding
                    .as_ref()
                    .map(|(n, t)| (self.ident_name(n), self.conv_type(t))),
            },
            ta::TypedPattern::IntegerLiteral(bits) => TypedPattern::IntegerLiteral(*bits),
            ta::TypedPattern::Wildcard(name, ty) => {
                TypedPattern::Wildcard(self.ident_name(name), self.conv_type(ty))
            }
        }
    }
}
