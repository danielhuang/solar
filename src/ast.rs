use crate::intrinsics::Intrinsic;

/// A zero-indexed source position. `line` is a line index and `col` is a byte
/// offset within that line (not a Unicode character or UTF-16 code-unit index).
#[derive(Debug, Clone, Copy, Default)]
pub struct SourcePos {
    /// Zero-indexed line number.
    pub line: u32,
    /// Zero-indexed byte offset within the line.
    pub col: u32,
}

/// A half-open source range, `[start, end)`: `start` is included and `end` is
/// excluded. Both positions use the zero-indexed byte coordinates described by
/// [`SourcePos`].
#[derive(Debug, Clone, Copy, Default)]
pub struct SourceSpan {
    /// Inclusive start position.
    pub start: SourcePos,
    /// Exclusive end position.
    pub end: SourcePos,
    /// Source-map file identifier.
    pub file_id: u32,
}

/// The source-level identity of a definition.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct DefId {
    /// Source-map identifier of the defining file.
    pub file: u32,
    /// Unmangled source name.
    pub name: String,
}

/// File identifier reserved for compiler-generated definitions.
pub const SYNTHETIC_FILE: u32 = u32::MAX;

impl DefId {
    /// Creates a definition identity.
    pub fn new(file: u32, name: impl Into<String>) -> Self {
        DefId {
            file,
            name: name.into(),
        }
    }

    /// Creates an identity for a compiler-generated definition.
    pub fn synthetic(name: impl Into<String>) -> Self {
        DefId::new(SYNTHETIC_FILE, name)
    }
}

impl std::fmt::Display for DefId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)
    }
}

/// The identity of a local binding.
///
/// User-written and compiler-generated identifiers remain distinct until the
/// mangled AST, even when their descriptive names are the same.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Ident {
    /// An identifier written in Solar source.
    User(String),
    /// An identifier introduced by compiler lowering.
    Synthetic(String),
}

impl Ident {
    /// Creates a user-written identifier.
    pub fn user(name: impl Into<String>) -> Self {
        Self::User(name.into())
    }

    /// Creates a compiler-generated identifier.
    pub fn synthetic(name: impl Into<String>) -> Self {
        Self::Synthetic(name.into())
    }
}

/// A parsed Solar source file.
#[derive(Debug)]
pub struct SourceFile {
    /// Top-level declarations in source order.
    pub items: Vec<TopLevelItem>,
}

/// A top-level Solar declaration.
#[derive(Debug, Clone)]
pub enum TopLevelItem {
    Struct(StructDef),
    Function(FunctionDef),
    Enum(EnumDef),
    Method(FunctionDef),
    Import(ImportDef),
    TypeAlias(TypeAliasDef),
    Const(ConstDef),
    Static(StaticDef),
}

/// A compile-time constant declaration.
#[derive(Debug, Clone)]
pub struct ConstDef {
    /// Declared name.
    pub name: String,
    /// Optional explicit type; inferred from the literal `value` when absent.
    pub ty: Option<Type>,
    /// The constant's value — must be a literal. Substituted at each use site
    /// during type-check/lowering.
    pub value: Box<Expr>,
    /// Whether the declaration is exported.
    pub is_pub: bool,
    /// Attached documentation.
    pub doc: Option<String>,
    /// Declaration span.
    pub span: SourceSpan,
}

/// A mutable global declaration.
#[derive(Debug, Clone)]
pub struct StaticDef {
    /// Declared name.
    pub name: String,
    /// Optional explicit type; inferred from the literal `value` when absent.
    pub ty: Option<Type>,
    /// The initial value — must be a literal, stored before `main` runs.
    pub value: Box<Expr>,
    /// Whether each thread receives an independent instance of this static.
    pub thread_local: bool,
    /// Whether the declaration is exported.
    pub is_pub: bool,
    /// Attached documentation.
    pub doc: Option<String>,
    /// Declaration span.
    pub span: SourceSpan,
}

/// An import declaration.
#[derive(Debug, Clone)]
pub struct ImportDef {
    /// Imported names or module.
    pub kind: ImportKind,
    /// Imported file path.
    pub path: String,
    /// Whether the import is re-exported.
    pub is_pub: bool,
    /// Declaration span.
    pub span: SourceSpan,
}

/// A possibly-qualified name in a named import.
#[derive(Debug, Clone)]
pub struct ImportName {
    /// Path segments, ending with the imported name.
    pub segments: Vec<String>,
}

impl ImportName {
    /// The local name introduced by this import (last segment).
    pub fn local_name(&self) -> &str {
        self.segments.last().unwrap()
    }

    /// Module segments (all but last). Empty for plain imports.
    pub fn module_segments(&self) -> &[String] {
        &self.segments[..self.segments.len() - 1]
    }

    /// Whether this is a path import (has module segments).
    pub fn is_path(&self) -> bool {
        self.segments.len() > 1
    }
}

/// The form of an import declaration.
#[derive(Debug, Clone)]
pub enum ImportKind {
    Named(Vec<ImportName>),
    Module(String),
    Wildcard,
}

/// A type alias declaration.
#[derive(Debug, Clone)]
pub struct TypeAliasDef {
    /// Declared name.
    pub name: String,
    /// Generic type parameters.
    pub type_params: Vec<String>,
    /// Aliased type.
    pub target_type: Type,
    /// Whether the declaration is exported.
    pub is_pub: bool,
    /// Attached documentation.
    pub doc: Option<String>,
    /// Declaration span.
    pub span: SourceSpan,
}

/// A struct declaration.
#[derive(Debug, Clone)]
pub struct StructDef {
    /// Declared name.
    pub name: String,
    /// Source-level definition identity.
    pub def_id: DefId,
    /// Generic type parameters.
    pub type_params: Vec<String>,
    /// Fields in declaration order.
    pub fields: Vec<FieldDef>,
    /// Whether tuple syntax was used.
    pub is_tuple: bool,
    /// Whether the declaration is exported.
    pub is_pub: bool,
    /// Attached documentation.
    pub doc: Option<String>,
    /// Declaration span.
    pub span: SourceSpan,
}

/// A struct field declaration.
#[derive(Debug, Clone)]
pub struct FieldDef {
    /// Field name.
    pub name: String,
    /// Field type.
    pub ty: Type,
    /// Whether the field is exported.
    pub is_pub: bool,
    /// Declaration span.
    pub span: SourceSpan,
}

/// An enum declaration.
#[derive(Debug, Clone)]
pub struct EnumDef {
    /// Declared name.
    pub name: String,
    /// Source-level definition identity.
    pub def_id: DefId,
    /// Generic type parameters.
    pub type_params: Vec<String>,
    /// Variants in declaration order.
    pub variants: Vec<VariantDef>,
    /// Whether the declaration is exported.
    pub is_pub: bool,
    /// Attached documentation.
    pub doc: Option<String>,
    /// Declaration span.
    pub span: SourceSpan,
}

/// An enum variant declaration.
#[derive(Debug, Clone)]
pub struct VariantDef {
    /// Variant name.
    pub name: String,
    /// Optional payload type.
    pub inner_type: Option<Type>,
    /// Declaration span.
    pub span: SourceSpan,
}

/// A function or method declaration.
#[derive(Debug, Clone)]
pub struct FunctionDef {
    /// Resolved function name.
    pub name: String,
    /// Original name used in diagnostics.
    pub display_name: String,
    /// Generic type parameters.
    pub type_params: Vec<String>,
    /// Function parameters.
    pub parameters: Vec<Parameter>,
    /// Explicit return type, if present.
    pub return_type: Option<Type>,
    /// Span of the explicit return type.
    pub return_type_span: Option<SourceSpan>,
    /// Function body.
    pub body: Vec<Statement>,
    /// Whether the declaration is exported.
    pub is_pub: bool,
    /// Whether accessing this function requires an `unsafe` block.
    pub is_unsafe: bool,
    /// Whether the declaration requests inlining.
    pub inline_hint: bool,
    /// Attached documentation.
    pub doc: Option<String>,
    /// Declaration span.
    pub span: SourceSpan,
}

/// A function parameter.
#[derive(Debug, Clone)]
pub struct Parameter {
    /// Binding pattern.
    pub pattern: DestructurePattern,
    /// Declared or inferred type.
    pub ty: Type,
    /// Default value for an optional keyword parameter (a literal). `None` for a
    /// normal required parameter. When `ty` is `Type::Infer`, the type is
    /// inferred from this default.
    pub default: Option<Box<Expr>>,
    /// Parameter span.
    pub span: SourceSpan,
}

/// A binding pattern used by parameters and local declarations.
#[derive(Debug, Clone)]
pub enum DestructurePattern {
    Name(Ident),
    Tuple(Vec<DestructurePattern>),
    Struct {
        module: Option<String>,
        name: DefId,
        fields: Vec<DestructureField>,
    },
    Array(Vec<DestructurePattern>),
}

/// A named field within a struct destructuring pattern.
#[derive(Debug, Clone)]
pub struct DestructureField {
    /// Struct field name.
    pub field_name: String,
    /// Pattern bound to the field.
    pub pattern: DestructurePattern,
}

/// A statement and its source span.
#[derive(Debug, Clone)]
pub struct Statement {
    /// Statement contents.
    pub kind: StatementKind,
    /// Statement span.
    pub span: SourceSpan,
}

/// An untyped statement.
#[derive(Debug, Clone)]
pub enum StatementKind {
    Let {
        pattern: DestructurePattern,
        ty: Option<Type>,
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
    ForRange {
        variable: Ident,
        start: Expr,
        end: Expr,
        body: Vec<Statement>,
    },
    ForIn {
        variable: Ident,
        iterable: Expr,
        body: Vec<Statement>,
    },
    /// Surface `try { ... } catch (binding[: type]) { ... }` syntax.
    Try {
        body: Vec<Statement>,
        binding: Ident,
        binding_type: Option<Type>,
        handler: Vec<Statement>,
    },
    ForReflectFields {
        pattern: DestructurePattern,
        object: Expr,
        body: Vec<Statement>,
        /// `for.reflect_fields_pair`: reflect two values of the same struct in
        /// lockstep. `object` is then a 2-tuple `(a, b)`.
        paired: bool,
    },
    MatchReflectVariant {
        pattern: DestructurePattern,
        object: Expr,
        body: Vec<Statement>,
        /// `match.reflect_variant_pair`: reflect two values of the same enum in
        /// lockstep. `object` is then a 2-tuple `(a, b)`.
        paired: bool,
    },
    Expression(Expr),
    Return(Expr),
    /// A surface `return;`, before it is normalized to a Unit-valued return.
    ReturnVoid,
    /// `break;` (no value) or `break <expr>;` (value, only inside a `loop`).
    Break(Option<Expr>),
    Continue,
    NestedFunction(FunctionDef),
    Const(ConstDef),
}

/// A floating-point literal width.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FloatType {
    Float32,
    Float64,
}

/// A binary operator.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    /// Wrapping (two's-complement, overflow-never-panics) arithmetic, written
    /// with a doubled operator: `++` `--` `**`.
    WrapAdd,
    WrapSub,
    WrapMul,
}

impl BinOp {
    /// Returns the overloadable method name corresponding to this operator.
    pub fn method_name(self) -> &'static str {
        match self {
            Self::Add => "operator_add",
            Self::Sub => "operator_sub",
            Self::Mul => "operator_mul",
            Self::Div => "operator_div",
            Self::Mod => "operator_mod",
            Self::Eq => "operator_eq",
            Self::Ne => "operator_ne",
            Self::Lt => "operator_lt",
            Self::Le => "operator_le",
            Self::Gt => "operator_gt",
            Self::Ge => "operator_ge",
            Self::And => "operator_and",
            Self::Or => "operator_or",
            Self::BitAnd => "operator_bitand",
            Self::BitOr => "operator_bitor",
            Self::BitXor => "operator_bitxor",
            Self::Shl => "operator_shl",
            Self::Shr => "operator_shr",
            Self::WrapAdd => "operator_wrapadd",
            Self::WrapSub => "operator_wrapsub",
            Self::WrapMul => "operator_wrapmul",
        }
    }
}

/// An expression and its source span.
#[derive(Debug, Clone)]
pub struct Expr {
    /// Expression contents.
    pub kind: ExprKind,
    /// Expression span.
    pub span: SourceSpan,
}

/// An untyped expression.
#[derive(Debug, Clone)]
pub enum ExprKind {
    /// A local or unresolved name.
    Identifier(Ident),
    /// A resolved top-level name.
    GlobalRef(DefId),
    IntegerLiteral(i128, IntegerType),
    /// `1f` / `1.0f32` / `2.5f64` — the suffix is mandatory (a bare `1.0` is
    /// not a float literal, and `1.f` is field access on the integer `1`).
    /// A `f32` literal's value is parsed in f32 precision then widened, so no
    /// double rounding occurs.
    FloatLiteral(f64, FloatType),
    BooleanLiteral(bool),
    /// A decoded surface string literal, before conversion to a byte array.
    StringLiteral(Vec<u8>),
    /// A decoded surface character literal, before conversion to `Uint8`.
    CharLiteral(u8),
    FieldAccess {
        object: Box<Expr>,
        field: String,
    },
    Deref(Box<Expr>),
    Reference(Box<Expr>),
    Unique(Box<Expr>),
    /// Unary `!`: logical not on `Bool`, bitwise complement on integers.
    Not(Box<Expr>),
    /// `null#[T]` — the null value of the nullable reference type `&?T`.
    NullLiteral(Type),
    Call {
        function: Box<Expr>,
        type_args: Vec<Type>,
        arguments: Vec<Expr>,
        /// Keyword arguments (`name = value`), matched to optional parameters by
        /// name. Always appear after positional `arguments` in source.
        kwargs: Vec<(String, Expr)>,
    },
    StructLiteral {
        module: Option<String>,
        name: DefId,
        type_args: Vec<Type>,
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
    /// `[a, b, c]`, optionally annotated with the element type: `[]#[T]`.
    /// The annotation is required for an empty literal and otherwise must
    /// match the inferred element type.
    ArrayLiteral(Vec<Expr>, Option<Type>),
    ArrayRepeat {
        element: Box<Expr>,
        count: Box<Expr>,
    },
    /// `loop { … }` — an infinite loop usable as an expression; its value comes
    /// from `break <expr>`.
    Loop(Vec<Statement>),
    BinaryOp {
        op: BinOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    If {
        condition: Box<Expr>,
        then_body: Vec<Statement>,
        else_body: Vec<Statement>,
    },
    Block(Vec<Statement>),
    /// A block in which unsafe function declarations may be accessed.
    UnsafeBlock(Vec<Statement>),
    Closure {
        parameters: Vec<Parameter>,
        return_type: Option<Type>,
        body: Vec<Statement>,
    },
    EnumVariant {
        module_path: Vec<String>,
        enum_name: DefId,
        type_args: Vec<Type>,
        variant_name: String,
    },
    Match {
        scrutinee: Box<Expr>,
        arms: Vec<MatchArm>,
    },
    MatchReflect {
        ty: Type,
        arms: Vec<ReflectArm>,
    },
    MethodCall {
        receiver: Box<Expr>,
        method: String,
        type_args: Vec<Type>,
        arguments: Vec<Expr>,
        /// Keyword arguments (`name = value`), matched to optional parameters by
        /// name. Always appear after positional `arguments` in source.
        kwargs: Vec<(String, Expr)>,
    },
    TupleLiteral(Vec<Expr>),
    IntrinsicCall {
        intrinsic: Intrinsic,
        arguments: Vec<Expr>,
    },
}

/// A numeric primitive type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumericType {
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
}

/// A compiler-defined primitive type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimitiveType {
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
    FileDesc,
    Unit,
    Never,
}

/// The single registry of compiler-defined primitive type names.
pub const PRIMITIVE_TYPES: &[(PrimitiveType, &str)] = &[
    (PrimitiveType::Int8, "Int8"),
    (PrimitiveType::Int16, "Int16"),
    (PrimitiveType::Int32, "Int32"),
    (PrimitiveType::Int64, "Int64"),
    (PrimitiveType::Int, "Int"),
    (PrimitiveType::Uint8, "Uint8"),
    (PrimitiveType::Uint16, "Uint16"),
    (PrimitiveType::Uint32, "Uint32"),
    (PrimitiveType::Uint64, "Uint64"),
    (PrimitiveType::Uint, "Uint"),
    (PrimitiveType::Float32, "Float32"),
    (PrimitiveType::Float64, "Float64"),
    (PrimitiveType::Bool, "Bool"),
    (PrimitiveType::FileDesc, "FileDesc"),
    (PrimitiveType::Unit, "Unit"),
    (PrimitiveType::Never, "Never"),
];

impl PrimitiveType {
    /// Parses a primitive type name.
    pub fn from_name(name: &str) -> Option<Self> {
        PRIMITIVE_TYPES
            .iter()
            .find_map(|(primitive, candidate)| (*candidate == name).then_some(*primitive))
    }

    /// Returns the source spelling of this primitive type.
    pub fn name(self) -> &'static str {
        PRIMITIVE_TYPES
            .iter()
            .find_map(|(primitive, name)| (*primitive == self).then_some(*name))
            .unwrap()
    }

    /// Returns the numeric kind, if this primitive is numeric.
    pub fn numeric(self) -> Option<NumericType> {
        match self {
            PrimitiveType::Int8 => Some(NumericType::Int8),
            PrimitiveType::Int16 => Some(NumericType::Int16),
            PrimitiveType::Int32 => Some(NumericType::Int32),
            PrimitiveType::Int64 => Some(NumericType::Int64),
            PrimitiveType::Int => Some(NumericType::Int),
            PrimitiveType::Uint8 => Some(NumericType::Uint8),
            PrimitiveType::Uint16 => Some(NumericType::Uint16),
            PrimitiveType::Uint32 => Some(NumericType::Uint32),
            PrimitiveType::Uint64 => Some(NumericType::Uint64),
            PrimitiveType::Uint => Some(NumericType::Uint),
            PrimitiveType::Float32 => Some(NumericType::Float32),
            PrimitiveType::Float64 => Some(NumericType::Float64),
            PrimitiveType::Bool
            | PrimitiveType::FileDesc
            | PrimitiveType::Unit
            | PrimitiveType::Never => None,
        }
    }
}

impl NumericType {
    /// Parses a numeric type name.
    pub fn from_name(name: &str) -> Option<NumericType> {
        PrimitiveType::from_name(name)?.numeric()
    }

    /// Returns whether this is a floating-point type.
    pub fn is_float(&self) -> bool {
        matches!(self, NumericType::Float32 | NumericType::Float64)
    }
}

/// A match expression arm.
#[derive(Debug, Clone)]
pub struct MatchArm {
    /// Arm pattern.
    pub pattern: Pattern,
    /// Arm body.
    pub body: Expr,
}

/// A reflection match arm.
#[derive(Debug, Clone)]
pub struct ReflectArm {
    /// Reflected kind pattern.
    pub pattern: ReflectPattern,
    /// Arm body.
    pub body: Expr,
}

/// A reflection kind pattern.
#[derive(Debug, Clone)]
pub enum ReflectPattern {
    Kind(String),
    Wildcard,
}

/// A match pattern.
#[derive(Debug, Clone)]
pub enum Pattern {
    Variant {
        module_path: Vec<String>,
        enum_name: DefId,
        type_args: Vec<Type>,
        variant_name: String,
        binding: Option<Ident>,
    },
    /// An exact integer value pattern.
    IntegerLiteral(i128, IntegerType),
    Wildcard(Ident),
}

/// A field initializer in a struct literal.
#[derive(Debug, Clone)]
pub struct FieldInit {
    /// Field name.
    pub name: String,
    /// Initializer value.
    pub value: Expr,
}

/// An integer literal type.
#[derive(Debug, Clone, Copy)]
pub enum IntegerType {
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
}

impl IntegerType {
    /// Inclusive range of values representable by this type.
    pub fn bounds(&self) -> (i128, i128) {
        match self {
            IntegerType::Int8 => (i8::MIN as i128, i8::MAX as i128),
            IntegerType::Int16 => (i16::MIN as i128, i16::MAX as i128),
            IntegerType::Int32 => (i32::MIN as i128, i32::MAX as i128),
            IntegerType::Int64 | IntegerType::Int => (i64::MIN as i128, i64::MAX as i128),
            IntegerType::Uint8 => (0, u8::MAX as i128),
            IntegerType::Uint16 => (0, u16::MAX as i128),
            IntegerType::Uint32 => (0, u32::MAX as i128),
            IntegerType::Uint64 | IntegerType::Uint => (0, u64::MAX as i128),
        }
    }
}

/// A source-level type expression.
#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    /// A named type.
    Named(DefId),
    Generic {
        name: DefId,
        type_args: Vec<Type>,
    },
    Reference(Box<Type>),
    /// `&?T` — a nullable reference.
    NullableReference(Box<Type>),
    Unique(Box<Type>),
    Slice(Box<Type>),
    FixedArray(Box<Type>, u64),
    Function {
        params: Vec<(Option<String>, Type)>,
        return_type: Option<Box<Type>>,
    },
    Tuple(Vec<Type>),
    Infer,
}
