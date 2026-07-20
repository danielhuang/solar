#[derive(Debug, Clone, Copy, Default)]
pub struct SourcePos {
    pub line: u32,
    pub col: u32,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SourceSpan {
    pub start: SourcePos,
    pub end: SourcePos,
    pub file_id: u32,
}

/// Provenance identity of a top-level definition: the file it was defined in
/// plus its original (un-mangled) source name. This is what the front-end
/// (parser, resolve, typed_ast) carries INSTEAD of a pre-mangled unique string;
/// the actual module-mangling into a single flat virtual file is deferred to
/// the `mangled_ast` stage, which renders each `DefId` to a unique C-safe
/// symbol. `file` is a `SourceMap` FileId (see `resolve`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct DefId {
    pub file: u32,
    pub name: String,
}

/// Sentinel file id for compiler-synthesized types that have no source file
/// (e.g. anonymous tuples). `mangled_ast` renders these specially.
pub const SYNTHETIC_FILE: u32 = u32::MAX;

impl DefId {
    pub fn new(file: u32, name: impl Into<String>) -> Self {
        DefId {
            file,
            name: name.into(),
        }
    }

    /// A synthetic (source-file-less) def identity, e.g. a tuple shape, a
    /// closure, or a numeric constructor — rendered bare (no module prefix).
    pub fn synthetic(name: impl Into<String>) -> Self {
        DefId::new(SYNTHETIC_FILE, name)
    }
}

impl std::fmt::Display for DefId {
    /// Renders the (un-mangled) source name — for diagnostics.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)
    }
}

#[derive(Debug)]
pub struct SourceFile {
    pub items: Vec<TopLevelItem>,
}

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

#[derive(Debug, Clone)]
pub struct ConstDef {
    pub name: String,
    /// Optional explicit type; inferred from the literal `value` when absent.
    pub ty: Option<Type>,
    /// The constant's value — must be a literal. Substituted at each use site
    /// during type-check/lowering.
    pub value: Box<Expr>,
    pub is_pub: bool,
    /// `///` doc comment attached to this item (lines joined by `\n`), if any.
    pub doc: Option<String>,
    pub span: SourceSpan,
}

/// `static NAME[: T] = <literal>;` — a global mutable variable (top-level
/// only). Like keyword-parameter defaults, the initial value must be a
/// literal; state needing init code is a nullable reference populated in
/// `main`. The type must be sized.
#[derive(Debug, Clone)]
pub struct StaticDef {
    pub name: String,
    /// Optional explicit type; inferred from the literal `value` when absent.
    pub ty: Option<Type>,
    /// The initial value — must be a literal, stored before `main` runs.
    pub value: Box<Expr>,
    pub is_pub: bool,
    /// `///` doc comment attached to this item (lines joined by `\n`), if any.
    pub doc: Option<String>,
    pub span: SourceSpan,
}

#[derive(Debug, Clone)]
pub struct ImportDef {
    pub kind: ImportKind,
    pub path: String,
    pub is_pub: bool,
    pub span: SourceSpan,
}

#[derive(Debug, Clone)]
pub struct ImportName {
    pub segments: Vec<String>, // ["a", "b"] for a::b, ["Foo"] for plain Foo
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

#[derive(Debug, Clone)]
pub enum ImportKind {
    Named(Vec<ImportName>),
    Module(String),
    Wildcard,
}

#[derive(Debug, Clone)]
pub struct TypeAliasDef {
    pub name: String,
    pub type_params: Vec<String>,
    pub target_type: Type,
    pub is_pub: bool,
    /// `///` doc comment attached to this item (lines joined by `\n`), if any.
    pub doc: Option<String>,
    pub span: SourceSpan,
}

#[derive(Debug, Clone)]
pub struct StructDef {
    pub name: String,
    /// Provenance: original source name + defining file. Set by `resolve`;
    /// `name` may later be rewritten to a mangled identity, but this is not.
    pub def_id: DefId,
    pub type_params: Vec<String>,
    pub fields: Vec<FieldDef>,
    /// Whether this was declared as `struct Name(T0, T1)`.  Its fields are
    /// still the ordinary `_0`, `_1`, ... fields used throughout the pipeline.
    pub is_tuple: bool,
    pub is_pub: bool,
    /// `///` doc comment attached to this item (lines joined by `\n`), if any.
    pub doc: Option<String>,
    pub span: SourceSpan,
}

#[derive(Debug, Clone)]
pub struct FieldDef {
    pub name: String,
    pub ty: Type,
    pub is_pub: bool,
    pub span: SourceSpan,
}

#[derive(Debug, Clone)]
pub struct EnumDef {
    pub name: String,
    /// Provenance: original source name + defining file (see `StructDef::def_id`).
    pub def_id: DefId,
    pub type_params: Vec<String>,
    pub variants: Vec<VariantDef>,
    pub is_pub: bool,
    /// `///` doc comment attached to this item (lines joined by `\n`), if any.
    pub doc: Option<String>,
    pub span: SourceSpan,
}

#[derive(Debug, Clone)]
pub struct VariantDef {
    pub name: String,
    pub inner_type: Option<Type>,
    pub span: SourceSpan,
}

#[derive(Debug, Clone)]
pub struct FunctionDef {
    pub name: String,
    /// The original, human-readable name for diagnostics (e.g. `spawn`). `name`
    /// gets rewritten to a mangled identity by `resolve` (module prefix) and
    /// monomorphization, but this is left untouched so error messages can show
    /// the un-mangled name without round-tripping through the demangler.
    pub display_name: String,
    pub type_params: Vec<String>,
    pub parameters: Vec<Parameter>,
    pub return_type: Option<Type>,
    pub return_type_span: Option<SourceSpan>,
    pub body: Vec<Statement>,
    pub is_pub: bool,
    /// `fn(inline)` / `method(inline)`: a hint that codegen should mark this
    /// function for inlining. Ignored by the interpreters.
    pub inline_hint: bool,
    /// `///` doc comment attached to this item (lines joined by `\n`), if any.
    pub doc: Option<String>,
    pub span: SourceSpan,
}

#[derive(Debug, Clone)]
pub struct Parameter {
    pub pattern: DestructurePattern,
    pub ty: Type,
    /// Default value for an optional keyword parameter (a literal). `None` for a
    /// normal required parameter. When `ty` is `Type::Infer`, the type is
    /// inferred from this default.
    pub default: Option<Box<Expr>>,
    pub span: SourceSpan,
}

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

#[derive(Debug, Clone)]
pub struct DestructureField {
    pub field_name: String,
    pub pattern: DestructurePattern,
}

#[derive(Debug, Clone)]
pub struct Statement {
    pub kind: StatementKind,
    pub span: SourceSpan,
}

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
    /// `break;` (no value) or `break <expr>;` (value, only inside a `loop`).
    Break(Option<Expr>),
    Continue,
    NestedFunction(FunctionDef),
    Const(ConstDef),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FloatType {
    Float32,
    Float64,
}

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

#[derive(Debug, Clone)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: SourceSpan,
}

#[derive(Debug, Clone)]
pub enum ExprKind {
    /// A bare name — a local variable, or (pre-resolve) an as-yet-unresolved
    /// reference. `resolve` rewrites references to top-level functions / consts
    /// / statics into [`ExprKind::GlobalRef`], leaving only locals here.
    Identifier(String),
    /// A resolved reference to a top-level definition (function, const, or
    /// static), carrying its provenance `DefId`. Produced by `resolve`.
    GlobalRef(DefId),
    IntegerLiteral(i128, IntegerType),
    /// `1f` / `1.0f32` / `2.5f64` — the suffix is mandatory (a bare `1.0` is
    /// not a float literal, and `1.f` is field access on the integer `1`).
    /// A `f32` literal's value is parsed in f32 precision then widened, so no
    /// double rounding occurs.
    FloatLiteral(f64, FloatType),
    BooleanLiteral(bool),
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

    pub fn is_float(&self) -> bool {
        matches!(self, NumericType::Float32 | NumericType::Float64)
    }
}

#[derive(Debug, Clone)]
pub enum Intrinsic {
    Panic,
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
    ("panic", Intrinsic::Panic),
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

#[derive(Debug, Clone)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub body: Expr,
}

#[derive(Debug, Clone)]
pub struct ReflectArm {
    pub pattern: ReflectPattern,
    pub body: Expr,
}

#[derive(Debug, Clone)]
pub enum ReflectPattern {
    Kind(String),
    Wildcard,
}

#[derive(Debug, Clone)]
pub enum Pattern {
    Variant {
        module_path: Vec<String>,
        enum_name: DefId,
        type_args: Vec<Type>,
        variant_name: String,
        binding: Option<String>,
    },
    Wildcard(String),
}

#[derive(Debug, Clone)]
pub struct FieldInit {
    pub name: String,
    pub value: Expr,
}

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

#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    /// A named type reference. After `resolve`, `def.file` is the resolved
    /// defining file (a real `DefId` for a struct/enum, or file `0` for a
    /// builtin/type-parameter name, which `typed_ast` dispatches on `def.name`).
    /// The parser stamps `file: 0`; `resolve` fills the real file.
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
