//! Mangled AST — a structural mirror of `typed_ast` that sits between type
//! checking and IR lowering in the pipeline. It has its own parallel set of
//! node types so it can later diverge from `typed_ast` (e.g. carrying
//! name-mangled symbols). For now [`lower`] is a **no-op**: it maps every
//! `typed_ast` node onto the identically-shaped `mangled_ast` node.

use crate::ast;
use crate::typed_ast as ta;
use std::collections::HashMap;
use std::fmt;

// --- Types (mirror of `typed_ast::Type`) ---

#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    Int8,
    Int16,
    Int32,
    Int64,
    Int,
    Uint8,
    Uint16,
    Uint32,
    Uint64,
    Uint,
    Float32,
    Float64,
    Bool,
    Struct(String),
    Enum(String),
    Array(Box<Type>),
    FixedArray(Box<Type>, u64),
    Ref(Box<Type>),
    RefUnsized(Box<Type>),
    /// `&?T` — a nullable reference to a sized `T` (8-byte pointer, may be null).
    NullableRef(Box<Type>),
    /// `&?T` — a nullable reference to an unsized `T` (16-byte fat pointer, may be null).
    NullableRefUnsized(Box<Type>),
    Unique(Box<Type>),
    UniqueUnsized(Box<Type>),
    Function {
        params: Vec<Type>,
        return_type: Box<Type>,
    },
    /// An open file descriptor. A built-in opaque handle with the byte
    /// representation of `&Int32`: an 8-byte pointer into the GC-traced fd
    /// arena. The collector closes the file once no live `FileDesc` remains.
    FileDesc,
    Unit,
    Never,
}

impl From<&ast::NumericType> for Type {
    fn from(nt: &ast::NumericType) -> Type {
        match nt {
            ast::NumericType::Int8 => Type::Int8,
            ast::NumericType::Int16 => Type::Int16,
            ast::NumericType::Int32 => Type::Int32,
            ast::NumericType::Int64 => Type::Int64,
            ast::NumericType::Int => Type::Int,
            ast::NumericType::Uint8 => Type::Uint8,
            ast::NumericType::Uint16 => Type::Uint16,
            ast::NumericType::Uint32 => Type::Uint32,
            ast::NumericType::Uint64 => Type::Uint64,
            ast::NumericType::Uint => Type::Uint,
            ast::NumericType::Float32 => Type::Float32,
            ast::NumericType::Float64 => Type::Float64,
        }
    }
}

impl Type {
    pub fn is_integer(&self) -> bool {
        matches!(
            self,
            Type::Int8
                | Type::Int16
                | Type::Int32
                | Type::Int64
                | Type::Int
                | Type::Uint8
                | Type::Uint16
                | Type::Uint32
                | Type::Uint64
                | Type::Uint
        )
    }

    pub fn is_float(&self) -> bool {
        matches!(self, Type::Float32 | Type::Float64)
    }

    pub fn is_numeric(&self) -> bool {
        self.is_integer() || matches!(self, Type::Float32 | Type::Float64)
    }

    pub fn is_unsigned(&self) -> bool {
        matches!(
            self,
            Type::Uint8 | Type::Uint16 | Type::Uint32 | Type::Uint64 | Type::Uint
        )
    }

    /// Bit width of an integer type (`Int`/`Uint` are pointer-width 64). Panics
    /// on non-integer types.
    pub fn int_bit_width(&self) -> u32 {
        match self {
            Type::Int8 | Type::Uint8 => 8,
            Type::Int16 | Type::Uint16 => 16,
            Type::Int32 | Type::Uint32 => 32,
            Type::Int64 | Type::Uint64 | Type::Int | Type::Uint => 64,
            other => panic!("int_bit_width on non-integer type {other}"),
        }
    }

    pub fn is_nullable_ref(&self) -> bool {
        matches!(self, Type::NullableRef(_) | Type::NullableRefUnsized(_))
    }

    pub fn is_sized(&self, structs: &HashMap<String, StructDef>) -> bool {
        match self {
            Type::Array(_) => false,
            Type::FixedArray(_, _) | Type::Function { .. } => true,
            Type::Enum(_) => true,
            Type::Struct(name) => {
                let def = structs
                    .get(name)
                    .unwrap_or_else(|| panic!("is_sized: missing struct `{name}`"));
                def.fields.last().is_none_or(|f| f.ty.is_sized(structs))
            }
            _ => true,
        }
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Type::Int8 => write!(f, "Int8"),
            Type::Int16 => write!(f, "Int16"),
            Type::Int32 => write!(f, "Int32"),
            Type::Int64 => write!(f, "Int64"),
            Type::Int => write!(f, "Int"),
            Type::Uint8 => write!(f, "Uint8"),
            Type::Uint16 => write!(f, "Uint16"),
            Type::Uint32 => write!(f, "Uint32"),
            Type::Uint64 => write!(f, "Uint64"),
            Type::Uint => write!(f, "Uint"),
            Type::Float32 => write!(f, "Float32"),
            Type::Float64 => write!(f, "Float64"),
            Type::Bool => write!(f, "Bool"),
            Type::Struct(name) => write!(f, "{name}"),
            Type::Enum(name) => write!(f, "{name}"),
            Type::Array(inner) => write!(f, "[{inner}]"),
            Type::FixedArray(inner, n) => write!(f, "[{inner}; {n}]"),
            Type::Ref(inner) => write!(f, "&{inner}"),
            Type::RefUnsized(inner) => write!(f, "&{inner}"),
            Type::NullableRef(inner) => write!(f, "&?{inner}"),
            Type::NullableRefUnsized(inner) => write!(f, "&?{inner}"),
            Type::Unique(inner) => write!(f, "^{inner}"),
            Type::UniqueUnsized(inner) => write!(f, "^{inner}"),
            Type::Function {
                params,
                return_type,
            } => {
                write!(f, "fn(")?;
                for (i, p) in params.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{p}")?;
                }
                write!(f, ")")?;
                if **return_type != Type::Unit {
                    write!(f, " -> {return_type}")?;
                }
                Ok(())
            }
            Type::FileDesc => write!(f, "FileDesc"),
            Type::Unit => write!(f, "()"),
            Type::Never => write!(f, "!"),
        }
    }
}

// --- AST nodes (mirror of `typed_ast` nodes) ---

#[derive(Debug)]
pub struct SourceFile {
    pub structs: HashMap<String, StructDef>,
    pub enums: HashMap<String, EnumDef>,
    pub functions: HashMap<String, FunctionDef>,
    /// Top-level `static` declarations, in source order. Each init is the
    /// lowered literal expression; downstream layers store it into the global
    /// before `main`'s body runs.
    pub statics: Vec<StaticItem>,
}

#[derive(Debug, Clone)]
pub struct StaticItem {
    pub name: String,
    pub ty: Type,
    pub init: Expr,
}

#[derive(Debug, Clone)]
pub struct StructDef {
    pub name: String,
    pub fields: Vec<FieldDef>,
}

#[derive(Debug, Clone)]
pub struct FieldDef {
    pub name: String,
    pub ty: Type,
}

#[derive(Debug, Clone)]
pub struct FunctionDef {
    pub name: String,
    pub parameters: Vec<Parameter>,
    pub return_type: Type,
    pub body: Vec<Statement>,
    /// `fn(inline)` hint, carried through to codegen. Interpreters ignore it.
    pub inline_hint: bool,
}

#[derive(Debug, Clone)]
pub struct Parameter {
    pub name: String,
    pub ty: Type,
    pub span: ast::SourceSpan,
}

#[derive(Debug, Clone)]
pub struct Statement {
    pub kind: StatementKind,
    pub span: ast::SourceSpan,
}

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

#[derive(Debug, Clone)]
pub struct Expr {
    pub ty: Type,
    pub kind: ExprKind,
    pub span: ast::SourceSpan,
}

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
        intrinsic: ast::Intrinsic,
        arguments: Vec<Expr>,
    },
}

#[derive(Debug, Clone)]
pub struct FieldInit {
    pub name: String,
    pub value: Expr,
}

#[derive(Debug, Clone)]
pub struct CapturedVar {
    pub name: String,
    pub ty: Type,
}

#[derive(Debug, Clone)]
pub struct EnumDef {
    pub name: String,
    pub variants: Vec<EnumVariantDef>,
}

#[derive(Debug, Clone)]
pub struct EnumVariantDef {
    pub name: String,
    pub inner_type: Option<Type>,
    pub index: usize,
}

#[derive(Debug, Clone)]
pub struct TypedMatchArm {
    pub pattern: TypedPattern,
    pub body: Vec<Statement>,
}

#[derive(Debug, Clone)]
pub enum TypedPattern {
    Variant {
        enum_name: String,
        variant_name: String,
        variant_index: usize,
        binding: Option<(String, Type)>,
    },
    Wildcard(String, Type),
}

// --- Lowering: typed_ast -> mangled_ast (identity for now) ---

/// Map a type-checked source file into the mangled AST. Currently a no-op that
/// copies every node onto its identically-shaped `mangled_ast` counterpart.
pub fn lower(source: &ta::SourceFile) -> SourceFile {
    SourceFile {
        structs: source
            .structs
            .iter()
            .map(|(k, v)| (k.clone(), conv_struct(v)))
            .collect(),
        enums: source
            .enums
            .iter()
            .map(|(k, v)| (k.clone(), conv_enum(v)))
            .collect(),
        functions: source
            .functions
            .iter()
            .map(|(k, v)| (k.clone(), conv_function(v)))
            .collect(),
        statics: source.statics.iter().map(conv_static).collect(),
    }
}

fn conv_type(t: &ta::Type) -> Type {
    use ta::Type as T;
    match t {
        T::Int8 => Type::Int8,
        T::Int16 => Type::Int16,
        T::Int32 => Type::Int32,
        T::Int64 => Type::Int64,
        T::Int => Type::Int,
        T::Uint8 => Type::Uint8,
        T::Uint16 => Type::Uint16,
        T::Uint32 => Type::Uint32,
        T::Uint64 => Type::Uint64,
        T::Uint => Type::Uint,
        T::Float32 => Type::Float32,
        T::Float64 => Type::Float64,
        T::Bool => Type::Bool,
        T::Struct(n) => Type::Struct(n.clone()),
        T::Enum(n) => Type::Enum(n.clone()),
        T::Array(inner) => Type::Array(Box::new(conv_type(inner))),
        T::FixedArray(inner, n) => Type::FixedArray(Box::new(conv_type(inner)), *n),
        T::Ref(inner) => Type::Ref(Box::new(conv_type(inner))),
        T::RefUnsized(inner) => Type::RefUnsized(Box::new(conv_type(inner))),
        T::NullableRef(inner) => Type::NullableRef(Box::new(conv_type(inner))),
        T::NullableRefUnsized(inner) => Type::NullableRefUnsized(Box::new(conv_type(inner))),
        T::Unique(inner) => Type::Unique(Box::new(conv_type(inner))),
        T::UniqueUnsized(inner) => Type::UniqueUnsized(Box::new(conv_type(inner))),
        T::Function {
            params,
            return_type,
        } => Type::Function {
            params: params.iter().map(conv_type).collect(),
            return_type: Box::new(conv_type(return_type)),
        },
        T::FileDesc => Type::FileDesc,
        T::Unit => Type::Unit,
        T::Never => Type::Never,
    }
}

fn conv_static(s: &ta::StaticItem) -> StaticItem {
    StaticItem {
        name: s.name.clone(),
        ty: conv_type(&s.ty),
        init: conv_expr(&s.init),
    }
}

fn conv_struct(s: &ta::StructDef) -> StructDef {
    StructDef {
        name: s.name.clone(),
        fields: s.fields.iter().map(conv_field).collect(),
    }
}

fn conv_field(f: &ta::FieldDef) -> FieldDef {
    FieldDef {
        name: f.name.clone(),
        ty: conv_type(&f.ty),
    }
}

fn conv_enum(e: &ta::EnumDef) -> EnumDef {
    EnumDef {
        name: e.name.clone(),
        variants: e.variants.iter().map(conv_variant).collect(),
    }
}

fn conv_variant(v: &ta::EnumVariantDef) -> EnumVariantDef {
    EnumVariantDef {
        name: v.name.clone(),
        inner_type: v.inner_type.as_ref().map(conv_type),
        index: v.index,
    }
}

fn conv_function(f: &ta::FunctionDef) -> FunctionDef {
    FunctionDef {
        name: f.name.clone(),
        parameters: f.parameters.iter().map(conv_param).collect(),
        return_type: conv_type(&f.return_type),
        body: f.body.iter().map(conv_stmt).collect(),
        inline_hint: f.inline_hint,
    }
}

fn conv_param(p: &ta::Parameter) -> Parameter {
    Parameter {
        name: p.name.clone(),
        ty: conv_type(&p.ty),
        span: p.span,
    }
}

fn conv_capture(c: &ta::CapturedVar) -> CapturedVar {
    CapturedVar {
        name: c.name.clone(),
        ty: conv_type(&c.ty),
    }
}

fn conv_stmt(s: &ta::Statement) -> Statement {
    Statement {
        kind: conv_stmt_kind(&s.kind),
        span: s.span,
    }
}

fn conv_stmt_kind(k: &ta::StatementKind) -> StatementKind {
    use ta::StatementKind as K;
    match k {
        K::Let { name, ty, value } => StatementKind::Let {
            name: name.clone(),
            ty: conv_type(ty),
            value: conv_expr(value),
        },
        K::Assignment { target, value } => StatementKind::Assignment {
            target: conv_expr(target),
            value: conv_expr(value),
        },
        K::If {
            condition,
            body,
            else_body,
        } => StatementKind::If {
            condition: conv_expr(condition),
            body: body.iter().map(conv_stmt).collect(),
            else_body: else_body.iter().map(conv_stmt).collect(),
        },
        K::While { condition, body } => StatementKind::While {
            condition: conv_expr(condition),
            body: body.iter().map(conv_stmt).collect(),
        },
        K::Expression(e) => StatementKind::Expression(conv_expr(e)),
        K::Return(e) => StatementKind::Return(conv_expr(e)),
        K::Break(e) => StatementKind::Break(e.as_ref().map(conv_expr)),
        K::Continue => StatementKind::Continue,
    }
}

fn conv_expr(e: &ta::Expr) -> Expr {
    Expr {
        ty: conv_type(&e.ty),
        kind: conv_expr_kind(&e.kind),
        span: e.span,
    }
}

fn conv_boxed(e: &ta::Expr) -> Box<Expr> {
    Box::new(conv_expr(e))
}

fn conv_expr_kind(k: &ta::ExprKind) -> ExprKind {
    use ta::ExprKind as K;
    match k {
        K::Identifier(name) => ExprKind::Identifier(name.clone()),
        K::FloatLiteral(v) => ExprKind::FloatLiteral(*v),
        K::Global(name) => ExprKind::Global(name.clone()),
        K::IntegerLiteral(v) => ExprKind::IntegerLiteral(*v),
        K::BooleanLiteral(v) => ExprKind::BooleanLiteral(*v),
        K::FieldAccess { object, field } => ExprKind::FieldAccess {
            object: conv_boxed(object),
            field: field.clone(),
        },
        K::Deref(e) => ExprKind::Deref(conv_boxed(e)),
        K::Reference(e) => ExprKind::Reference(conv_boxed(e)),
        K::Unique(e) => ExprKind::Unique(conv_boxed(e)),
        K::Not(e) => ExprKind::Not(conv_boxed(e)),
        K::NullLiteral => ExprKind::NullLiteral,
        K::Call {
            function,
            arguments,
        } => ExprKind::Call {
            function: function.clone(),
            arguments: arguments.iter().map(conv_expr).collect(),
        },
        K::FunctionRef(name) => ExprKind::FunctionRef(name.clone()),
        K::CallIndirect { callee, arguments } => ExprKind::CallIndirect {
            callee: conv_boxed(callee),
            arguments: arguments.iter().map(conv_expr).collect(),
        },
        K::StructLiteral { name, fields } => ExprKind::StructLiteral {
            name: name.clone(),
            fields: fields.iter().map(conv_field_init).collect(),
        },
        K::Index { object, index } => ExprKind::Index {
            object: conv_boxed(object),
            index: conv_boxed(index),
        },
        K::Slice { object, start, end } => ExprKind::Slice {
            object: conv_boxed(object),
            start: conv_boxed(start),
            end: conv_boxed(end),
        },
        K::ArrayLiteral(elems) => ExprKind::ArrayLiteral(elems.iter().map(conv_expr).collect()),
        K::ArrayRepeat { element, count } => ExprKind::ArrayRepeat {
            element: conv_boxed(element),
            count: conv_boxed(count),
        },
        K::ArrayInit { count, init } => ExprKind::ArrayInit {
            count: conv_boxed(count),
            init: conv_boxed(init),
        },
        K::ArraySizeCoerce { expr, size } => ExprKind::ArraySizeCoerce {
            expr: conv_boxed(expr),
            size: *size,
        },
        K::BinaryOp { op, left, right } => ExprKind::BinaryOp {
            op: *op,
            left: conv_boxed(left),
            right: conv_boxed(right),
        },
        K::If {
            condition,
            then_body,
            else_body,
        } => ExprKind::If {
            condition: conv_boxed(condition),
            then_body: then_body.iter().map(conv_stmt).collect(),
            else_body: else_body.iter().map(conv_stmt).collect(),
        },
        K::Block(body) => ExprKind::Block(body.iter().map(conv_stmt).collect()),
        K::Loop(body) => ExprKind::Loop(body.iter().map(conv_stmt).collect()),
        K::Closure {
            synthetic_fn,
            captures,
        } => ExprKind::Closure {
            synthetic_fn: synthetic_fn.clone(),
            captures: captures.iter().map(conv_capture).collect(),
        },
        K::EnumVariant {
            enum_name,
            variant_name,
            variant_index,
            value,
        } => ExprKind::EnumVariant {
            enum_name: enum_name.clone(),
            variant_name: variant_name.clone(),
            variant_index: *variant_index,
            value: value.as_deref().map(conv_boxed),
        },
        K::Match { scrutinee, arms } => ExprKind::Match {
            scrutinee: conv_boxed(scrutinee),
            arms: arms.iter().map(conv_match_arm).collect(),
        },
        K::IntrinsicCall {
            intrinsic,
            arguments,
        } => ExprKind::IntrinsicCall {
            intrinsic: intrinsic.clone(),
            arguments: arguments.iter().map(conv_expr).collect(),
        },
    }
}

fn conv_field_init(f: &ta::FieldInit) -> FieldInit {
    FieldInit {
        name: f.name.clone(),
        value: conv_expr(&f.value),
    }
}

fn conv_match_arm(a: &ta::TypedMatchArm) -> TypedMatchArm {
    TypedMatchArm {
        pattern: conv_pattern(&a.pattern),
        body: a.body.iter().map(conv_stmt).collect(),
    }
}

fn conv_pattern(p: &ta::TypedPattern) -> TypedPattern {
    match p {
        ta::TypedPattern::Variant {
            enum_name,
            variant_name,
            variant_index,
            binding,
        } => TypedPattern::Variant {
            enum_name: enum_name.clone(),
            variant_name: variant_name.clone(),
            variant_index: *variant_index,
            binding: binding.as_ref().map(|(n, t)| (n.clone(), conv_type(t))),
        },
        ta::TypedPattern::Wildcard(name, ty) => TypedPattern::Wildcard(name.clone(), conv_type(ty)),
    }
}
