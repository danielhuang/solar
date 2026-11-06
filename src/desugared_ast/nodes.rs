//! Nodes owned by the normalized AST. Recursive fields stay in this phase.

use crate::ast::{
    BinOp, DefId, DestructurePattern, EnumDef, FloatType, Ident, ImportDef, IntegerType, Pattern,
    ReflectPattern, SourceSpan, StructDef, Type, TypeAliasDef,
};
use crate::intrinsics::Intrinsic;

/// An untyped program after type-independent normalization.
#[derive(Debug)]
pub struct SourceFile {
    /// Top-level declarations in source order.
    pub items: Vec<TopLevelItem>,
}

/// A normalized top-level Solar declaration.
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

/// A function or method declaration.
#[derive(Debug, Clone)]
pub struct FunctionDef {
    /// Universally quantified parameters used by the associated owner type.
    pub associated_type_params: Vec<String>,
    /// Type owning this associated function, or `None` for a free function.
    pub associated_type: Option<Type>,
    /// Function name, retaining whether it was written by the user or synthesized.
    pub name: Ident,
    /// Original name used in diagnostics.
    pub display_name: String,
    /// Generic type parameters.
    pub type_params: Vec<String>,
    /// Caller-specified generic type parameters, declared after inferred ones.
    pub out_type_params: Vec<String>,
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
    /// Whether this is an extension method, requiring lexical visibility.
    pub is_extension: bool,
    /// Whether overlapping definitions are tried in reverse declaration order.
    pub is_overlap: bool,
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

/// A statement and its source span.
#[derive(Debug, Clone)]
pub struct Statement {
    /// Statement contents.
    pub kind: StatementKind,
    /// Statement span.
    pub span: SourceSpan,
}

/// A normalized statement, with counted loops introduced by desugaring.
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
    /// Normalized counted loop, introduced by desugaring a range expression.
    ForRange {
        variable: Ident,
        start: Expr,
        end: Expr,
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
    /// An expression, yielding its value only when last in a block.
    Expression(Expr),
    Return(Expr),
    /// `break;` (no value) or `break <expr>;` (value, inside `loop` or iterable `for`).
    Break(Option<Expr>),
    Continue,
    NestedFunction(FunctionDef),
    Const(ConstDef),
}

/// An expression and its source span.
#[derive(Debug, Clone)]
pub struct Expr {
    /// Expression contents.
    pub kind: ExprKind,
    /// Expression span.
    pub span: SourceSpan,
}

/// A normalized expression, containing no surface-only syntax.
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
    /// A suspended computation, optionally accepting a value on each resume.
    Coroutine {
        parameter: Option<Box<Parameter>>,
        body: Vec<Statement>,
    },
    /// Suspend the nearest coroutine and produce its next resume argument.
    Yield(Box<Expr>),
    /// Delegate to an owned coroutine; its inline storage is assigned during coroutine lowering.
    Await(Box<Expr>),
    EnumVariant {
        module_path: Vec<String>,
        enum_name: DefId,
        type_args: Vec<Type>,
        variant_name: String,
    },
    /// A function selected from a type, such as `T::make`.
    AssociatedFunction {
        /// Owner type, retained structurally for generic substitution.
        ty: Type,
        /// Associated name; empty for constructor-like functions.
        name: String,
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
        /// Explicit type arguments written at the call site.
        type_args: Vec<Type>,
        arguments: Vec<Expr>,
    },
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

/// A field initializer in a struct literal.
#[derive(Debug, Clone)]
pub struct FieldInit {
    /// Field name.
    pub name: String,
    /// Initializer value.
    pub value: Expr,
}
