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
    pub span: SourceSpan,
}

#[derive(Debug, Clone)]
pub struct StructDef {
    pub name: String,
    pub type_params: Vec<String>,
    pub fields: Vec<FieldDef>,
    pub is_pub: bool,
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
    pub type_params: Vec<String>,
    pub variants: Vec<VariantDef>,
    pub is_pub: bool,
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
    pub type_params: Vec<String>,
    pub parameters: Vec<Parameter>,
    pub return_type: Option<Type>,
    pub return_type_span: Option<SourceSpan>,
    pub body: Vec<Statement>,
    pub is_pub: bool,
    pub span: SourceSpan,
}

#[derive(Debug, Clone)]
pub struct Parameter {
    pub pattern: DestructurePattern,
    pub ty: Type,
    pub span: SourceSpan,
}

#[derive(Debug, Clone)]
pub enum DestructurePattern {
    Name(String),
    Tuple(Vec<DestructurePattern>),
    Struct {
        module: Option<String>,
        name: String,
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
    Expression(Expr),
    Return(Expr),
    NestedFunction(FunctionDef),
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
}

#[derive(Debug, Clone)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: SourceSpan,
}

#[derive(Debug, Clone)]
pub enum ExprKind {
    Identifier(String),
    IntegerLiteral(i128, IntegerType),
    BooleanLiteral(bool),
    FieldAccess {
        object: Box<Expr>,
        field: String,
    },
    Deref(Box<Expr>),
    Reference(Box<Expr>),
    Unique(Box<Expr>),
    Call {
        function: Box<Expr>,
        type_args: Vec<Type>,
        arguments: Vec<Expr>,
    },
    StructLiteral {
        module: Option<String>,
        name: String,
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
    ArrayLiteral(Vec<Expr>),
    ArrayRepeat {
        element: Box<Expr>,
        count: Box<Expr>,
    },
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
        enum_name: String,
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
    WriteStdout,
    ReadStdin,
    Panic,
    ArrayLen,
    AssertArrayLen,
    ThreadSpawn,
    AtomicLoad,
    AtomicStore,
    AtomicExchange,
    AtomicCompareExchange,
    FutexWait,
    FutexWake,
    Cast(NumericType, NumericType),
}

const INTRINSIC_NAMES: &[(&str, Intrinsic)] = &[
    ("write_stdout", Intrinsic::WriteStdout),
    ("read_stdin", Intrinsic::ReadStdin),
    ("panic", Intrinsic::Panic),
    ("array_len", Intrinsic::ArrayLen),
    ("assert_array_len", Intrinsic::AssertArrayLen),
    ("thread_spawn", Intrinsic::ThreadSpawn),
    ("atomic_load", Intrinsic::AtomicLoad),
    ("atomic_store", Intrinsic::AtomicStore),
    ("atomic_exchange", Intrinsic::AtomicExchange),
    ("atomic_compare_exchange", Intrinsic::AtomicCompareExchange),
    ("futex_wait", Intrinsic::FutexWait),
    ("futex_wake", Intrinsic::FutexWake),
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
        enum_name: String,
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
    Named(String),
    Generic {
        name: String,
        type_args: Vec<Type>,
    },
    Reference(Box<Type>),
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
