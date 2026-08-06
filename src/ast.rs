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
    Name(String),
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
        variable: String,
        start: Expr,
        end: Expr,
        body: Vec<Statement>,
    },
    ForIn {
        variable: String,
        iterable: Expr,
        body: Vec<Statement>,
    },
    /// Surface `try { ... } catch (binding[: type]) { ... }` syntax.
    Try {
        body: Vec<Statement>,
        binding: String,
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
    Identifier(String),
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
#[derive(Debug, Clone, PartialEq)]
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

impl NumericType {
    /// Parses a numeric type name.
    pub fn from_name(name: &str) -> Option<NumericType> {
        match name {
            "Int8" => Some(NumericType::Int8),
            "Int16" => Some(NumericType::Int16),
            "Int32" => Some(NumericType::Int32),
            "Int64" => Some(NumericType::Int64),
            "Int" => Some(NumericType::Int),
            "Uint8" => Some(NumericType::Uint8),
            "Uint16" => Some(NumericType::Uint16),
            "Uint32" => Some(NumericType::Uint32),
            "Uint64" => Some(NumericType::Uint64),
            "Uint" => Some(NumericType::Uint),
            "Float32" => Some(NumericType::Float32),
            "Float64" => Some(NumericType::Float64),
            _ => None,
        }
    }

    /// Returns whether this is a floating-point type.
    pub fn is_float(&self) -> bool {
        matches!(self, NumericType::Float32 | NumericType::Float64)
    }
}

/// A compiler intrinsic.
#[derive(Debug, Clone)]
pub enum Intrinsic {
    RefEq,
    Throw,
    Try,
    ArrayLen,
    AssertArrayLen,
    ThreadSpawn,
    AtomicLoad,
    AtomicStore,
    AtomicExchange,
    AtomicCompareExchange,
    FutexWait,
    FutexWake,
    FileOpen,
    FileClose,
    FileStdin,
    FileStdout,
    FileStderr,
    FileRead,
    FileWritePartial,
    FileReadAt,
    FileWriteAt,
    FileSync,
    FileLock,
    FileRemove,
    FileRename,
    FileStat,
    DirCreate,
    DirRemove,
    DirRead,
    SocketCreate,
    SocketBind,
    SocketListen,
    SocketAccept,
    SocketConnect,
    SocketSetOption,
    SocketLocalAddr,
    SocketShutdown,
    Args,
    Env,
    MonotonicTime,
    SystemTime,
    NumCpus,
    Exit,
    Sqrt,
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Atan2,
    Pow,
    Exp,
    Log,
    Floor,
    Ceil,
    Round,
    Trunc,
    FloatAbs,
    CountTrailingZeros,
    CountLeadingZeros,
    CountOnes,
    CarryingMulAdd,
    U64FromLe,
    U32FromLe,
    SimdMatchByteX16,
    SimdMatchHighBitX16,
    Cast(NumericType, NumericType),
}

const INTRINSIC_NAMES: &[(&str, Intrinsic)] = &[
    ("ref_eq", Intrinsic::RefEq),
    ("throw", Intrinsic::Throw),
    ("try", Intrinsic::Try),
    ("array_len", Intrinsic::ArrayLen),
    ("assert_array_len", Intrinsic::AssertArrayLen),
    ("thread_spawn", Intrinsic::ThreadSpawn),
    ("atomic_load", Intrinsic::AtomicLoad),
    ("atomic_store", Intrinsic::AtomicStore),
    ("atomic_exchange", Intrinsic::AtomicExchange),
    ("atomic_compare_exchange", Intrinsic::AtomicCompareExchange),
    ("futex_wait", Intrinsic::FutexWait),
    ("futex_wake", Intrinsic::FutexWake),
    ("file_open", Intrinsic::FileOpen),
    ("file_close", Intrinsic::FileClose),
    ("file_stdin", Intrinsic::FileStdin),
    ("file_stdout", Intrinsic::FileStdout),
    ("file_stderr", Intrinsic::FileStderr),
    ("file_read", Intrinsic::FileRead),
    ("file_write_partial", Intrinsic::FileWritePartial),
    ("file_read_at", Intrinsic::FileReadAt),
    ("file_write_at", Intrinsic::FileWriteAt),
    ("file_sync", Intrinsic::FileSync),
    ("file_lock", Intrinsic::FileLock),
    ("file_remove", Intrinsic::FileRemove),
    ("file_rename", Intrinsic::FileRename),
    ("file_stat", Intrinsic::FileStat),
    ("dir_create", Intrinsic::DirCreate),
    ("dir_remove", Intrinsic::DirRemove),
    ("dir_read", Intrinsic::DirRead),
    ("socket_create", Intrinsic::SocketCreate),
    ("socket_bind", Intrinsic::SocketBind),
    ("socket_listen", Intrinsic::SocketListen),
    ("socket_accept", Intrinsic::SocketAccept),
    ("socket_connect", Intrinsic::SocketConnect),
    ("socket_set_option", Intrinsic::SocketSetOption),
    ("socket_local_addr", Intrinsic::SocketLocalAddr),
    ("socket_shutdown", Intrinsic::SocketShutdown),
    ("args", Intrinsic::Args),
    ("env", Intrinsic::Env),
    ("monotonic_time", Intrinsic::MonotonicTime),
    ("system_time", Intrinsic::SystemTime),
    ("num_cpus", Intrinsic::NumCpus),
    ("exit", Intrinsic::Exit),
    ("sqrt", Intrinsic::Sqrt),
    ("sin", Intrinsic::Sin),
    ("cos", Intrinsic::Cos),
    ("tan", Intrinsic::Tan),
    ("asin", Intrinsic::Asin),
    ("acos", Intrinsic::Acos),
    ("atan", Intrinsic::Atan),
    ("atan2", Intrinsic::Atan2),
    ("pow", Intrinsic::Pow),
    ("exp", Intrinsic::Exp),
    ("log", Intrinsic::Log),
    ("floor", Intrinsic::Floor),
    ("ceil", Intrinsic::Ceil),
    ("round", Intrinsic::Round),
    ("trunc", Intrinsic::Trunc),
    ("float_abs", Intrinsic::FloatAbs),
    ("count_trailing_zeros", Intrinsic::CountTrailingZeros),
    ("count_leading_zeros", Intrinsic::CountLeadingZeros),
    ("count_ones", Intrinsic::CountOnes),
    ("carrying_mul_add", Intrinsic::CarryingMulAdd),
    ("u64_from_le", Intrinsic::U64FromLe),
    ("u32_from_le", Intrinsic::U32FromLe),
    ("simd_match_byte_x16", Intrinsic::SimdMatchByteX16),
    ("simd_match_high_bit_x16", Intrinsic::SimdMatchHighBitX16),
];

impl Intrinsic {
    /// Returns the intrinsic's source name.
    pub fn name(&self) -> &'static str {
        match self {
            Intrinsic::Cast(..) => "cast",
            other => {
                INTRINSIC_NAMES
                    .iter()
                    .find(|(_, v)| std::mem::discriminant(v) == std::mem::discriminant(other))
                    .unwrap()
                    .0
            }
        }
    }

    /// Looks up an intrinsic by source name.
    pub fn from_name(name: &str) -> Option<Intrinsic> {
        for (n, v) in INTRINSIC_NAMES {
            if *n == name {
                return Some(v.clone());
            }
        }
        if let Some(suffix) = name.strip_prefix("cast_") {
            return parse_cast_type_names(suffix);
        }
        None
    }
}

fn parse_cast_type_names(suffix: &str) -> Option<Intrinsic> {
    for (i, _) in suffix.match_indices('_') {
        let from = &suffix[..i];
        let to = &suffix[i + 1..];
        if let (Some(from_ty), Some(to_ty)) =
            (NumericType::from_name(from), NumericType::from_name(to))
        {
            return Some(Intrinsic::Cast(from_ty, to_ty));
        }
    }
    None
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
        binding: Option<String>,
    },
    /// An exact integer value pattern.
    IntegerLiteral(i128, IntegerType),
    Wildcard(String),
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
