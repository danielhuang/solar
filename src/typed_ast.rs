use crate::ast;
use crate::error::CompileError;
use crate::scope::ScopeStack;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::fmt;

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

    pub fn is_numeric(&self) -> bool {
        self.is_integer() || matches!(self, Type::Float32 | Type::Float64)
    }

    pub fn is_unsigned(&self) -> bool {
        matches!(
            self,
            Type::Uint8 | Type::Uint16 | Type::Uint32 | Type::Uint64 | Type::Uint
        )
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
                let def = &structs[name];
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

fn is_place_expr(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Identifier(_)
        | ExprKind::FieldAccess { .. }
        | ExprKind::Deref(_)
        | ExprKind::Index { .. }
        | ExprKind::Slice { .. } => true,
        ExprKind::If {
            then_body,
            else_body,
            ..
        } => body_tail_is_place(then_body) && body_tail_is_place(else_body),
        ExprKind::Match { arms, .. } => arms.iter().all(|arm| body_tail_is_place(&arm.body)),
        _ => false,
    }
}

fn body_tail_is_place(body: &[Statement]) -> bool {
    body.last().is_some_and(|s| match &s.kind {
        StatementKind::Expression(expr) => is_place_expr(expr),
        _ => false,
    })
}

fn from_ast_type(ty: &ast::Type) -> Type {
    from_ast_type_with_subst(ty, &HashMap::new())
}

fn from_ast_type_with_subst(ty: &ast::Type, subst: &HashMap<String, Type>) -> Type {
    match ty {
        ast::Type::Named(name) => {
            if let Some(concrete) = subst.get(name) {
                return concrete.clone();
            }
            match name.as_str() {
                "Int8" => Type::Int8,
                "Int16" => Type::Int16,
                "Int32" => Type::Int32,
                "Int64" => Type::Int64,
                "Int" => Type::Int,
                "Uint8" => Type::Uint8,
                "Uint16" => Type::Uint16,
                "Uint32" => Type::Uint32,
                "Uint64" => Type::Uint64,
                "Uint" => Type::Uint,
                "Float32" => Type::Float32,
                "Float64" => Type::Float64,
                "Bool" => Type::Bool,
                "FileDesc" => Type::FileDesc,
                other => Type::Struct(other.to_string()),
            }
        }
        ast::Type::Generic { name, type_args } => {
            let concrete_args: Vec<Type> = type_args
                .iter()
                .map(|t| from_ast_type_with_subst(t, subst))
                .collect();
            let mangled = mangle_name(name, &concrete_args);
            Type::Struct(mangled)
        }
        ast::Type::Reference(inner) => Type::Ref(Box::new(from_ast_type_with_subst(inner, subst))),
        ast::Type::NullableReference(inner) => {
            Type::NullableRef(Box::new(from_ast_type_with_subst(inner, subst)))
        }
        ast::Type::Unique(inner) => Type::Unique(Box::new(from_ast_type_with_subst(inner, subst))),
        ast::Type::Slice(inner) => Type::Array(Box::new(from_ast_type_with_subst(inner, subst))),
        ast::Type::FixedArray(inner, n) => {
            Type::FixedArray(Box::new(from_ast_type_with_subst(inner, subst)), *n)
        }
        ast::Type::Function {
            params,
            return_type,
        } => Type::Function {
            params: params
                .iter()
                .map(|(_, ty)| from_ast_type_with_subst(ty, subst))
                .collect(),
            return_type: Box::new(
                return_type
                    .as_ref()
                    .map(|t| from_ast_type_with_subst(t, subst))
                    .unwrap_or(Type::Unit),
            ),
        },
        ast::Type::Tuple(types) => {
            let concrete_args: Vec<Type> = types
                .iter()
                .map(|t| from_ast_type_with_subst(t, subst))
                .collect();
            let mangled = mangle_tuple_name(&concrete_args);
            Type::Struct(mangled)
        }
        ast::Type::Infer => panic!("cannot resolve Infer type without context"),
    }
}

fn apply_subst_to_ast_type(ty: &ast::Type, subst: &HashMap<String, ast::Type>) -> ast::Type {
    match ty {
        ast::Type::Named(name) => {
            if let Some(replacement) = subst.get(name) {
                replacement.clone()
            } else {
                ty.clone()
            }
        }
        ast::Type::Generic { name, type_args } => ast::Type::Generic {
            name: name.clone(),
            type_args: type_args
                .iter()
                .map(|a| apply_subst_to_ast_type(a, subst))
                .collect(),
        },
        ast::Type::Reference(inner) => {
            ast::Type::Reference(Box::new(apply_subst_to_ast_type(inner, subst)))
        }
        ast::Type::NullableReference(inner) => {
            ast::Type::NullableReference(Box::new(apply_subst_to_ast_type(inner, subst)))
        }
        ast::Type::Unique(inner) => {
            ast::Type::Unique(Box::new(apply_subst_to_ast_type(inner, subst)))
        }
        ast::Type::Slice(inner) => {
            ast::Type::Slice(Box::new(apply_subst_to_ast_type(inner, subst)))
        }
        ast::Type::FixedArray(inner, n) => {
            ast::Type::FixedArray(Box::new(apply_subst_to_ast_type(inner, subst)), *n)
        }
        ast::Type::Function {
            params,
            return_type,
        } => ast::Type::Function {
            params: params
                .iter()
                .map(|(name, t)| (name.clone(), apply_subst_to_ast_type(t, subst)))
                .collect(),
            return_type: return_type
                .as_ref()
                .map(|t| Box::new(apply_subst_to_ast_type(t, subst))),
        },
        ast::Type::Tuple(types) => ast::Type::Tuple(
            types
                .iter()
                .map(|t| apply_subst_to_ast_type(t, subst))
                .collect(),
        ),
        ast::Type::Infer => ast::Type::Infer,
    }
}

fn apply_subst_to_ast_expr(expr: &ast::Expr, subst: &HashMap<String, ast::Type>) -> ast::Expr {
    let kind = match &expr.kind {
        ast::ExprKind::Identifier(_)
        | ast::ExprKind::IntegerLiteral(_, _)
        | ast::ExprKind::BooleanLiteral(_) => expr.kind.clone(),
        ast::ExprKind::FieldAccess { object, field } => ast::ExprKind::FieldAccess {
            object: Box::new(apply_subst_to_ast_expr(object, subst)),
            field: field.clone(),
        },
        ast::ExprKind::Deref(inner) => {
            ast::ExprKind::Deref(Box::new(apply_subst_to_ast_expr(inner, subst)))
        }
        ast::ExprKind::Reference(inner) => {
            ast::ExprKind::Reference(Box::new(apply_subst_to_ast_expr(inner, subst)))
        }
        ast::ExprKind::Unique(inner) => {
            ast::ExprKind::Unique(Box::new(apply_subst_to_ast_expr(inner, subst)))
        }
        ast::ExprKind::NullLiteral(ty) => {
            ast::ExprKind::NullLiteral(apply_subst_to_ast_type(ty, subst))
        }
        ast::ExprKind::Call {
            function,
            type_args,
            arguments,
        } => ast::ExprKind::Call {
            function: Box::new(apply_subst_to_ast_expr(function, subst)),
            type_args: type_args
                .iter()
                .map(|t| apply_subst_to_ast_type(t, subst))
                .collect(),
            arguments: arguments
                .iter()
                .map(|a| apply_subst_to_ast_expr(a, subst))
                .collect(),
        },
        ast::ExprKind::StructLiteral {
            module,
            name,
            type_args,
            fields,
        } => ast::ExprKind::StructLiteral {
            module: module.clone(),
            name: name.clone(),
            type_args: type_args
                .iter()
                .map(|t| apply_subst_to_ast_type(t, subst))
                .collect(),
            fields: fields
                .iter()
                .map(|f| ast::FieldInit {
                    name: f.name.clone(),
                    value: apply_subst_to_ast_expr(&f.value, subst),
                })
                .collect(),
        },
        ast::ExprKind::Index { object, index } => ast::ExprKind::Index {
            object: Box::new(apply_subst_to_ast_expr(object, subst)),
            index: Box::new(apply_subst_to_ast_expr(index, subst)),
        },
        ast::ExprKind::Slice { object, start, end } => ast::ExprKind::Slice {
            object: Box::new(apply_subst_to_ast_expr(object, subst)),
            start: Box::new(apply_subst_to_ast_expr(start, subst)),
            end: Box::new(apply_subst_to_ast_expr(end, subst)),
        },
        ast::ExprKind::ArrayLiteral(elements) => ast::ExprKind::ArrayLiteral(
            elements
                .iter()
                .map(|e| apply_subst_to_ast_expr(e, subst))
                .collect(),
        ),
        ast::ExprKind::ArrayRepeat { element, count } => ast::ExprKind::ArrayRepeat {
            element: Box::new(apply_subst_to_ast_expr(element, subst)),
            count: Box::new(apply_subst_to_ast_expr(count, subst)),
        },
        ast::ExprKind::BinaryOp { op, left, right } => ast::ExprKind::BinaryOp {
            op: *op,
            left: Box::new(apply_subst_to_ast_expr(left, subst)),
            right: Box::new(apply_subst_to_ast_expr(right, subst)),
        },
        ast::ExprKind::If {
            condition,
            then_body,
            else_body,
        } => ast::ExprKind::If {
            condition: Box::new(apply_subst_to_ast_expr(condition, subst)),
            then_body: then_body
                .iter()
                .map(|s| apply_subst_to_ast_statement(s, subst))
                .collect(),
            else_body: else_body
                .iter()
                .map(|s| apply_subst_to_ast_statement(s, subst))
                .collect(),
        },
        ast::ExprKind::Block(stmts) => ast::ExprKind::Block(
            stmts
                .iter()
                .map(|s| apply_subst_to_ast_statement(s, subst))
                .collect(),
        ),
        ast::ExprKind::Closure {
            parameters,
            return_type,
            body,
        } => ast::ExprKind::Closure {
            parameters: parameters
                .iter()
                .map(|p| ast::Parameter {
                    pattern: p.pattern.clone(),
                    ty: apply_subst_to_ast_type(&p.ty, subst),
                    span: p.span,
                })
                .collect(),
            return_type: return_type
                .as_ref()
                .map(|t| apply_subst_to_ast_type(t, subst)),
            body: body
                .iter()
                .map(|s| apply_subst_to_ast_statement(s, subst))
                .collect(),
        },
        ast::ExprKind::EnumVariant {
            module_path,
            enum_name,
            type_args,
            variant_name,
        } => ast::ExprKind::EnumVariant {
            module_path: module_path.clone(),
            enum_name: enum_name.clone(),
            type_args: type_args
                .iter()
                .map(|t| apply_subst_to_ast_type(t, subst))
                .collect(),
            variant_name: variant_name.clone(),
        },
        ast::ExprKind::Match { scrutinee, arms } => ast::ExprKind::Match {
            scrutinee: Box::new(apply_subst_to_ast_expr(scrutinee, subst)),
            arms: arms
                .iter()
                .map(|arm| ast::MatchArm {
                    pattern: apply_subst_to_ast_pattern(&arm.pattern, subst),
                    body: apply_subst_to_ast_expr(&arm.body, subst),
                })
                .collect(),
        },
        ast::ExprKind::MatchReflect { ty, arms } => ast::ExprKind::MatchReflect {
            ty: apply_subst_to_ast_type(ty, subst),
            arms: arms
                .iter()
                .map(|arm| ast::ReflectArm {
                    pattern: arm.pattern.clone(),
                    body: apply_subst_to_ast_expr(&arm.body, subst),
                })
                .collect(),
        },
        ast::ExprKind::MethodCall {
            receiver,
            method,
            type_args,
            arguments,
        } => ast::ExprKind::MethodCall {
            receiver: Box::new(apply_subst_to_ast_expr(receiver, subst)),
            method: method.clone(),
            type_args: type_args
                .iter()
                .map(|t| apply_subst_to_ast_type(t, subst))
                .collect(),
            arguments: arguments
                .iter()
                .map(|a| apply_subst_to_ast_expr(a, subst))
                .collect(),
        },
        ast::ExprKind::TupleLiteral(elements) => ast::ExprKind::TupleLiteral(
            elements
                .iter()
                .map(|e| apply_subst_to_ast_expr(e, subst))
                .collect(),
        ),
        ast::ExprKind::IntrinsicCall {
            intrinsic,
            arguments,
        } => ast::ExprKind::IntrinsicCall {
            intrinsic: intrinsic.clone(),
            arguments: arguments
                .iter()
                .map(|a| apply_subst_to_ast_expr(a, subst))
                .collect(),
        },
    };
    ast::Expr {
        kind,
        span: expr.span,
    }
}

fn apply_subst_to_ast_statement(
    stmt: &ast::Statement,
    subst: &HashMap<String, ast::Type>,
) -> ast::Statement {
    let kind = match &stmt.kind {
        ast::StatementKind::Let { pattern, ty, value } => ast::StatementKind::Let {
            pattern: pattern.clone(),
            ty: ty.as_ref().map(|t| apply_subst_to_ast_type(t, subst)),
            value: apply_subst_to_ast_expr(value, subst),
        },
        ast::StatementKind::Assignment { target, value } => ast::StatementKind::Assignment {
            target: apply_subst_to_ast_expr(target, subst),
            value: apply_subst_to_ast_expr(value, subst),
        },
        ast::StatementKind::If {
            condition,
            body,
            else_body,
        } => ast::StatementKind::If {
            condition: apply_subst_to_ast_expr(condition, subst),
            body: body
                .iter()
                .map(|s| apply_subst_to_ast_statement(s, subst))
                .collect(),
            else_body: else_body
                .iter()
                .map(|s| apply_subst_to_ast_statement(s, subst))
                .collect(),
        },
        ast::StatementKind::While { condition, body } => ast::StatementKind::While {
            condition: apply_subst_to_ast_expr(condition, subst),
            body: body
                .iter()
                .map(|s| apply_subst_to_ast_statement(s, subst))
                .collect(),
        },
        ast::StatementKind::ForRange {
            variable,
            start,
            end,
            body,
        } => ast::StatementKind::ForRange {
            variable: variable.clone(),
            start: apply_subst_to_ast_expr(start, subst),
            end: apply_subst_to_ast_expr(end, subst),
            body: body
                .iter()
                .map(|s| apply_subst_to_ast_statement(s, subst))
                .collect(),
        },
        ast::StatementKind::ForIn {
            variable,
            iterable,
            body,
        } => ast::StatementKind::ForIn {
            variable: variable.clone(),
            iterable: apply_subst_to_ast_expr(iterable, subst),
            body: body
                .iter()
                .map(|s| apply_subst_to_ast_statement(s, subst))
                .collect(),
        },
        ast::StatementKind::ForReflectFields {
            pattern,
            object,
            body,
        } => ast::StatementKind::ForReflectFields {
            pattern: pattern.clone(),
            object: apply_subst_to_ast_expr(object, subst),
            body: body
                .iter()
                .map(|s| apply_subst_to_ast_statement(s, subst))
                .collect(),
        },
        ast::StatementKind::MatchReflectVariant {
            pattern,
            object,
            body,
        } => ast::StatementKind::MatchReflectVariant {
            pattern: pattern.clone(),
            object: apply_subst_to_ast_expr(object, subst),
            body: body
                .iter()
                .map(|s| apply_subst_to_ast_statement(s, subst))
                .collect(),
        },
        ast::StatementKind::Expression(expr) => {
            ast::StatementKind::Expression(apply_subst_to_ast_expr(expr, subst))
        }
        ast::StatementKind::Return(expr) => {
            ast::StatementKind::Return(apply_subst_to_ast_expr(expr, subst))
        }
        ast::StatementKind::NestedFunction(fdef) => {
            ast::StatementKind::NestedFunction(ast::FunctionDef {
                name: fdef.name.clone(),
                type_params: fdef.type_params.clone(),
                parameters: fdef
                    .parameters
                    .iter()
                    .map(|p| ast::Parameter {
                        pattern: p.pattern.clone(),
                        ty: apply_subst_to_ast_type(&p.ty, subst),
                        span: p.span,
                    })
                    .collect(),
                return_type: fdef
                    .return_type
                    .as_ref()
                    .map(|t| apply_subst_to_ast_type(t, subst)),
                return_type_span: fdef.return_type_span,
                body: fdef
                    .body
                    .iter()
                    .map(|s| apply_subst_to_ast_statement(s, subst))
                    .collect(),
                is_pub: fdef.is_pub,
                span: fdef.span,
            })
        }
    };
    ast::Statement {
        kind,
        span: stmt.span,
    }
}

fn apply_subst_to_ast_pattern(
    pat: &ast::Pattern,
    subst: &HashMap<String, ast::Type>,
) -> ast::Pattern {
    match pat {
        ast::Pattern::Variant {
            module_path,
            enum_name,
            type_args,
            variant_name,
            binding,
        } => ast::Pattern::Variant {
            module_path: module_path.clone(),
            enum_name: enum_name.clone(),
            type_args: type_args
                .iter()
                .map(|t| apply_subst_to_ast_type(t, subst))
                .collect(),
            variant_name: variant_name.clone(),
            binding: binding.clone(),
        },
        ast::Pattern::Wildcard(name) => ast::Pattern::Wildcard(name.clone()),
    }
}

/// Extract the name from a simple DestructurePattern::Name, or return a placeholder.
fn pattern_name_or_placeholder(pat: &ast::DestructurePattern) -> String {
    match pat {
        ast::DestructurePattern::Name(name) => name.clone(),
        _ => "<pattern>".to_string(),
    }
}

/// Extract the name from a simple DestructurePattern::Name, or panic for compound patterns.
fn pattern_name(pat: &ast::DestructurePattern) -> &str {
    match pat {
        ast::DestructurePattern::Name(name) => name,
        _ => panic!("expected simple identifier pattern, got compound pattern"),
    }
}

fn mangle_name(base: &str, concrete_args: &[Type]) -> String {
    let mut name = base.to_string();
    for arg in concrete_args {
        name.push('_');
        name.push_str(&mangle_type(arg));
    }
    name
}

fn mangle_type(ty: &Type) -> String {
    match ty {
        Type::Int8 => "Int8".to_string(),
        Type::Int16 => "Int16".to_string(),
        Type::Int32 => "Int32".to_string(),
        Type::Int64 => "Int64".to_string(),
        Type::Int => "Int".to_string(),
        Type::Uint8 => "Uint8".to_string(),
        Type::Uint16 => "Uint16".to_string(),
        Type::Uint32 => "Uint32".to_string(),
        Type::Uint64 => "Uint64".to_string(),
        Type::Uint => "Uint".to_string(),
        Type::Float32 => "Float32".to_string(),
        Type::Float64 => "Float64".to_string(),
        Type::Bool => "Bool".to_string(),
        Type::Struct(name) | Type::Enum(name) => name.clone(),
        Type::Ref(inner) | Type::RefUnsized(inner) => format!("ref_{}", mangle_type(inner)),
        Type::NullableRef(inner) | Type::NullableRefUnsized(inner) => {
            format!("ref_nullable_{}", mangle_type(inner))
        }
        Type::Unique(inner) | Type::UniqueUnsized(inner) => {
            format!("unique_{}", mangle_type(inner))
        }
        Type::Array(inner) => format!("slice_{}", mangle_type(inner)),
        Type::FixedArray(inner, n) => format!("arr{}_{}", n, mangle_type(inner)),
        Type::Function {
            params,
            return_type,
        } => {
            let mut s = "fn".to_string();
            for p in params {
                s.push('_');
                s.push_str(&mangle_type(p));
            }
            s.push_str("__");
            s.push_str(&mangle_type(return_type));
            s
        }
        Type::FileDesc => "FileDesc".to_string(),
        Type::Unit => "Unit".to_string(),
        Type::Never => "Never".to_string(),
    }
}

fn mangle_tuple_name(element_types: &[Type]) -> String {
    let mut name = "__Tuple".to_string();
    for ty in element_types {
        name.push('_');
        name.push_str(&mangle_type(ty));
    }
    name
}

// --- Typed AST nodes ---

#[derive(Debug)]
pub struct SourceFile {
    pub structs: HashMap<String, StructDef>,
    pub enums: HashMap<String, EnumDef>,
    pub functions: HashMap<String, FunctionDef>,
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
    IntegerLiteral(i64),
    BooleanLiteral(bool),
    FieldAccess {
        object: Box<Expr>,
        field: String,
    },
    Deref(Box<Expr>),
    Reference(Box<Expr>),
    Unique(Box<Expr>),
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

// --- Lowering (ast -> typed_ast with type checking) ---

struct CaptureContext {
    scope_depth_barrier: usize,
    captures: Vec<CapturedVar>,
    captured_names: HashSet<String>,
}

struct GenericStructDef {
    type_params: Vec<String>,
    ast_def: ast::StructDef,
}

struct GenericEnumDef {
    type_params: Vec<String>,
    ast_def: ast::EnumDef,
}

#[derive(Clone)]
struct FunctionEntry {
    type_params: Vec<String>,
    ast_def: ast::FunctionDef,
    overload_index: usize,
}

/// Compare specificity of two types at the same position.
/// concrete > partially generic > fully generic (type param).
fn compare_type_specificity(
    a: &ast::Type,
    a_type_params: &[String],
    b: &ast::Type,
    b_type_params: &[String],
) -> Ordering {
    let a_is_param = matches!(a, ast::Type::Named(n) if a_type_params.contains(n));
    let b_is_param = matches!(b, ast::Type::Named(n) if b_type_params.contains(n));
    match (a_is_param, b_is_param) {
        (true, false) => Ordering::Less,    // b is more specific
        (false, true) => Ordering::Greater, // a is more specific
        (true, true) => Ordering::Equal,
        (false, false) => {
            // Both concrete or both structured — recurse into structure
            match (a, b) {
                (
                    ast::Type::Generic {
                        type_args: a_args, ..
                    },
                    ast::Type::Generic {
                        type_args: b_args, ..
                    },
                ) => {
                    for (aa, ba) in a_args.iter().zip(b_args.iter()) {
                        let cmp = compare_type_specificity(aa, a_type_params, ba, b_type_params);
                        if cmp != Ordering::Equal {
                            return cmp;
                        }
                    }
                    Ordering::Equal
                }
                (ast::Type::Reference(a_inner), ast::Type::Reference(b_inner))
                | (ast::Type::Unique(a_inner), ast::Type::Unique(b_inner))
                | (ast::Type::Slice(a_inner), ast::Type::Slice(b_inner))
                | (ast::Type::FixedArray(a_inner, _), ast::Type::FixedArray(b_inner, _)) => {
                    compare_type_specificity(a_inner, a_type_params, b_inner, b_type_params)
                }
                (
                    ast::Type::Function {
                        params: a_params, ..
                    },
                    ast::Type::Function {
                        params: b_params, ..
                    },
                ) => {
                    for ((_, at), (_, bt)) in a_params.iter().zip(b_params.iter()) {
                        let cmp = compare_type_specificity(at, a_type_params, bt, b_type_params);
                        if cmp != Ordering::Equal {
                            return cmp;
                        }
                    }
                    Ordering::Equal
                }
                (ast::Type::Tuple(a_types), ast::Type::Tuple(b_types)) => {
                    for (at, bt) in a_types.iter().zip(b_types.iter()) {
                        let cmp = compare_type_specificity(at, a_type_params, bt, b_type_params);
                        if cmp != Ordering::Equal {
                            return cmp;
                        }
                    }
                    Ordering::Equal
                }
                _ => Ordering::Equal,
            }
        }
    }
}

/// Compare two overloads by specificity across their full parameter lists.
fn compare_overload_specificity(
    a_params: &[ast::Type],
    a_type_params: &[String],
    b_params: &[ast::Type],
    b_type_params: &[String],
) -> Ordering {
    for (at, bt) in a_params.iter().zip(b_params.iter()) {
        let cmp = compare_type_specificity(at, a_type_params, bt, b_type_params);
        if cmp != Ordering::Equal {
            return cmp;
        }
    }
    Ordering::Equal
}

/// Check if two param type lists are alpha-equivalent (structurally identical
/// after building a bijective mapping between type params).
fn types_alpha_equivalent(
    a: &ast::Type,
    a_type_params: &[String],
    b: &ast::Type,
    b_type_params: &[String],
    mapping: &mut HashMap<String, String>,
    reverse: &mut HashMap<String, String>,
) -> bool {
    let a_is_param = matches!(a, ast::Type::Named(n) if a_type_params.contains(n));
    let b_is_param = matches!(b, ast::Type::Named(n) if b_type_params.contains(n));
    match (a_is_param, b_is_param) {
        (true, true) => {
            let a_name = if let ast::Type::Named(n) = a {
                n
            } else {
                unreachable!()
            };
            let b_name = if let ast::Type::Named(n) = b {
                n
            } else {
                unreachable!()
            };
            if let Some(mapped) = mapping.get(a_name) {
                mapped == b_name
            } else if let Some(rev) = reverse.get(b_name) {
                rev == a_name
            } else {
                mapping.insert(a_name.clone(), b_name.clone());
                reverse.insert(b_name.clone(), a_name.clone());
                true
            }
        }
        (true, false) | (false, true) => false,
        (false, false) => match (a, b) {
            (ast::Type::Named(an), ast::Type::Named(bn)) => an == bn,
            (
                ast::Type::Generic {
                    name: an,
                    type_args: aa,
                },
                ast::Type::Generic {
                    name: bn,
                    type_args: ba,
                },
            ) => {
                an == bn
                    && aa.len() == ba.len()
                    && aa.iter().zip(ba.iter()).all(|(a, b)| {
                        types_alpha_equivalent(a, a_type_params, b, b_type_params, mapping, reverse)
                    })
            }
            (ast::Type::Reference(ai), ast::Type::Reference(bi))
            | (ast::Type::Unique(ai), ast::Type::Unique(bi))
            | (ast::Type::Slice(ai), ast::Type::Slice(bi)) => {
                types_alpha_equivalent(ai, a_type_params, bi, b_type_params, mapping, reverse)
            }
            (ast::Type::FixedArray(ai, an), ast::Type::FixedArray(bi, bn)) => {
                an == bn
                    && types_alpha_equivalent(
                        ai,
                        a_type_params,
                        bi,
                        b_type_params,
                        mapping,
                        reverse,
                    )
            }
            (
                ast::Type::Function {
                    params: ap,
                    return_type: ar,
                },
                ast::Type::Function {
                    params: bp,
                    return_type: br,
                },
            ) => {
                ap.len() == bp.len()
                    && ap.iter().zip(bp.iter()).all(|((_, at), (_, bt))| {
                        types_alpha_equivalent(
                            at,
                            a_type_params,
                            bt,
                            b_type_params,
                            mapping,
                            reverse,
                        )
                    })
                    && match (ar, br) {
                        (Some(a), Some(b)) => types_alpha_equivalent(
                            a,
                            a_type_params,
                            b,
                            b_type_params,
                            mapping,
                            reverse,
                        ),
                        (None, None) => true,
                        _ => false,
                    }
            }
            (ast::Type::Tuple(at), ast::Type::Tuple(bt)) => {
                at.len() == bt.len()
                    && at.iter().zip(bt.iter()).all(|(a, b)| {
                        types_alpha_equivalent(a, a_type_params, b, b_type_params, mapping, reverse)
                    })
            }
            _ => false,
        },
    }
}

fn params_alpha_equivalent(
    a_params: &[ast::Type],
    a_type_params: &[String],
    b_params: &[ast::Type],
    b_type_params: &[String],
) -> bool {
    if a_params.len() != b_params.len() {
        return false;
    }
    let mut mapping = HashMap::new();
    let mut reverse = HashMap::new();
    a_params.iter().zip(b_params.iter()).all(|(a, b)| {
        types_alpha_equivalent(
            a,
            a_type_params,
            b,
            b_type_params,
            &mut mapping,
            &mut reverse,
        )
    })
}

/// Check that every type param appears somewhere in the parameter types.
fn type_param_appears_in(ty: &ast::Type, param: &str) -> bool {
    match ty {
        ast::Type::Named(name) => name == param,
        ast::Type::Generic { type_args, .. } => {
            type_args.iter().any(|t| type_param_appears_in(t, param))
        }
        ast::Type::Reference(inner)
        | ast::Type::NullableReference(inner)
        | ast::Type::Unique(inner)
        | ast::Type::Slice(inner) => type_param_appears_in(inner, param),
        ast::Type::FixedArray(inner, _) => type_param_appears_in(inner, param),
        ast::Type::Function {
            params,
            return_type,
        } => {
            params.iter().any(|(_, t)| type_param_appears_in(t, param))
                || return_type
                    .as_ref()
                    .is_some_and(|t| type_param_appears_in(t, param))
        }
        ast::Type::Tuple(types) => types.iter().any(|t| type_param_appears_in(t, param)),
        ast::Type::Infer => false,
    }
}

struct Lowerer<'a> {
    structs: HashMap<String, &'a ast::StructDef>,
    enums: HashMap<String, &'a ast::EnumDef>,
    generic_structs: HashMap<String, GenericStructDef>,
    generic_enums: HashMap<String, GenericEnumDef>,
    function_defs: HashMap<String, Vec<FunctionEntry>>,
    method_defs: HashMap<String, Vec<FunctionEntry>>,
    /// Maps mangled function name → AST def (populated in lower_all for concrete functions)
    concrete_ast_defs: HashMap<String, ast::FunctionDef>,
    lowered_structs: HashMap<String, StructDef>,
    lowered_enums: HashMap<String, EnumDef>,
    monomorphized_functions: HashMap<String, FunctionDef>,
    resolved_return_types: HashMap<String, Type>,
    resolving_return_types: Vec<String>,
    scopes: ScopeStack<Type>,
    current_return_type: Option<Type>,
    current_return_type_span: Option<ast::SourceSpan>,
    closure_counter: usize,
    destructure_counter: usize,
    for_counter: usize,
    pending_closures: Vec<FunctionDef>,
    capture_context: Option<CaptureContext>,
    reverse_mono: HashMap<String, (String, Vec<Type>)>,
    nested_function_defs: Vec<HashMap<String, Vec<FunctionEntry>>>,
    type_aliases: HashMap<String, (Vec<String>, ast::Type)>,
}

impl<'a> Lowerer<'a> {
    fn new(source: &'a ast::SourceFile) -> Result<Self, CompileError> {
        let mut structs: HashMap<String, &ast::StructDef> = HashMap::new();
        let mut enums = HashMap::new();
        let mut generic_structs = HashMap::new();
        let mut generic_enums = HashMap::new();
        let mut function_defs: HashMap<String, Vec<FunctionEntry>> = HashMap::new();
        let mut method_defs: HashMap<String, Vec<FunctionEntry>> = HashMap::new();
        for item in &source.items {
            match item {
                ast::TopLevelItem::Struct(s) => {
                    if s.type_params.is_empty() {
                        if let Some(prev) = structs.get(&s.name) {
                            return Err(CompileError::new(
                                format!("duplicate struct definition: `{}`", s.name),
                                s.span,
                            )
                            .with_label("first defined here", prev.span));
                        }
                        structs.insert(s.name.clone(), s);
                    } else if generic_structs
                        .insert(
                            s.name.clone(),
                            GenericStructDef {
                                type_params: s.type_params.clone(),
                                ast_def: s.clone(),
                            },
                        )
                        .is_some()
                    {
                        return Err(CompileError::new(
                            format!("duplicate generic struct definition: `{}`", s.name),
                            s.span,
                        ));
                    }
                }
                ast::TopLevelItem::Enum(e) => {
                    if e.type_params.is_empty() {
                        if enums.insert(e.name.clone(), e).is_some() {
                            return Err(CompileError::new(
                                format!("duplicate enum definition: `{}`", e.name),
                                e.span,
                            ));
                        }
                    } else if generic_enums
                        .insert(
                            e.name.clone(),
                            GenericEnumDef {
                                type_params: e.type_params.clone(),
                                ast_def: e.clone(),
                            },
                        )
                        .is_some()
                    {
                        return Err(CompileError::new(
                            format!("duplicate generic enum definition: `{}`", e.name),
                            e.span,
                        ));
                    }
                }
                ast::TopLevelItem::Function(f) => {
                    let entries = function_defs.entry(f.name.clone()).or_default();
                    let overload_index = entries.len();
                    entries.push(FunctionEntry {
                        type_params: f.type_params.clone(),
                        ast_def: f.clone(),
                        overload_index,
                    });
                }
                ast::TopLevelItem::Method(m) => {
                    if m.parameters.is_empty() {
                        return Err(CompileError::new(
                            format!(
                                "method `{}` must have at least one parameter (self)",
                                m.name
                            ),
                            m.span,
                        ));
                    }
                    if !m.type_params.is_empty() {
                        // Validate first param type doesn't use type params
                        let self_type = &m.parameters[0].ty;
                        let uses_type_param = match self_type {
                            ast::Type::Named(name) => m.type_params.contains(name),
                            _ => false,
                        };
                        if uses_type_param {
                            return Err(CompileError::new(
                                format!(
                                    "method `{}`: first parameter type must be concrete (not a type parameter)",
                                    m.name
                                ),
                                m.parameters[0].span,
                            ));
                        }
                    }
                    let entries = method_defs.entry(m.name.clone()).or_default();
                    let overload_index = entries.len();
                    entries.push(FunctionEntry {
                        type_params: m.type_params.clone(),
                        ast_def: m.clone(),
                        overload_index,
                    });
                }
                ast::TopLevelItem::TypeAlias(_) => {
                    // Handled below after all items are collected
                }
                ast::TopLevelItem::Import(_) => {
                    panic!("Import items must be resolved before type checking");
                }
            }
        }

        // Collect type aliases
        let mut type_aliases: HashMap<String, (Vec<String>, ast::Type)> = HashMap::new();
        for item in &source.items {
            if let ast::TopLevelItem::TypeAlias(ta) = item {
                type_aliases.insert(
                    ta.name.clone(),
                    (ta.type_params.clone(), ta.target_type.clone()),
                );
            }
        }

        // Validate function and method entries: duplicate concrete overloads, unused type params,
        // alpha-equivalent collision detection
        let all_defs: Vec<(&str, &HashMap<String, Vec<FunctionEntry>>)> =
            vec![("function", &function_defs), ("method", &method_defs)];
        for (kind, defs) in all_defs {
            for (name, entries) in defs {
                // Check duplicate concrete overloads (same param types among concrete entries)
                let concrete: Vec<&FunctionEntry> = entries
                    .iter()
                    .filter(|e| e.type_params.is_empty())
                    .collect();
                for (i, a) in concrete.iter().enumerate() {
                    for b in concrete.iter().skip(i + 1) {
                        if a.ast_def.parameters.len() == b.ast_def.parameters.len()
                            && a.ast_def
                                .parameters
                                .iter()
                                .zip(b.ast_def.parameters.iter())
                                .all(|(pa, pb)| pa.ty == pb.ty)
                        {
                            return Err(CompileError::new(
                                format!(
                                    "duplicate {kind} definition: `{name}` with same parameter types"
                                ),
                                b.ast_def.span,
                            )
                            .with_label("first definition here", a.ast_def.span));
                        }
                    }
                }

                // Validate generic entries: unused type params
                let generic: Vec<&FunctionEntry> = entries
                    .iter()
                    .filter(|e| !e.type_params.is_empty())
                    .collect();
                for gdef in &generic {
                    let param_types: Vec<&ast::Type> =
                        gdef.ast_def.parameters.iter().map(|p| &p.ty).collect();
                    for tp in &gdef.type_params {
                        let in_params = param_types.iter().any(|t| type_param_appears_in(t, tp));
                        let in_return = gdef
                            .ast_def
                            .return_type
                            .as_ref()
                            .is_some_and(|t| type_param_appears_in(t, tp));
                        if !in_params && !in_return {
                            return Err(CompileError::new(
                                format!(
                                    "type parameter `{tp}` is not used in {kind} `{name}` parameters or return type"
                                ),
                                gdef.ast_def.span,
                            ));
                        }
                    }
                }
                // Pairwise alpha-equivalence check among generic overloads
                for (i, a) in generic.iter().enumerate() {
                    let a_param_types: Vec<ast::Type> =
                        a.ast_def.parameters.iter().map(|p| p.ty.clone()).collect();
                    for b in generic.iter().skip(i + 1) {
                        let b_param_types: Vec<ast::Type> =
                            b.ast_def.parameters.iter().map(|p| p.ty.clone()).collect();
                        if params_alpha_equivalent(
                            &a_param_types,
                            &a.type_params,
                            &b_param_types,
                            &b.type_params,
                        ) {
                            return Err(CompileError::new(
                                format!(
                                    "generic {kind} `{name}`: overloads have equivalent parameter patterns"
                                ),
                                b.ast_def.span,
                            )
                            .with_label("first definition here", a.ast_def.span));
                        }
                    }
                }
            }
        }

        Ok(Lowerer {
            structs,
            enums,
            generic_structs,
            generic_enums,
            function_defs,
            method_defs,
            concrete_ast_defs: HashMap::new(),
            lowered_structs: HashMap::new(),
            lowered_enums: HashMap::new(),
            monomorphized_functions: HashMap::new(),
            resolved_return_types: HashMap::new(),
            resolving_return_types: Vec::new(),
            scopes: ScopeStack::default(),
            current_return_type: None,
            current_return_type_span: None,
            closure_counter: 0,
            destructure_counter: 0,
            for_counter: 0,
            pending_closures: Vec::new(),
            capture_context: None,
            reverse_mono: HashMap::new(),
            nested_function_defs: Vec::new(),
            type_aliases,
        })
    }

    /// Look up the AST struct definition for a (possibly monomorphized) struct name.
    /// Returns the ast::StructDef which has span.file_id and field is_pub info.
    fn ast_struct_def(&self, struct_name: &str) -> Option<&ast::StructDef> {
        if let Some(def) = self.structs.get(struct_name) {
            return Some(def);
        }
        // For monomorphized generic structs, look up the original name
        if let Some((original_name, _)) = self.reverse_mono.get(struct_name)
            && let Some(gdef) = self.generic_structs.get(original_name)
        {
            return Some(&gdef.ast_def);
        }
        None
    }

    /// Check that a field access is allowed by visibility rules.
    /// Non-pub fields can only be accessed from the same file where the struct is defined.
    fn check_field_visibility(
        &self,
        struct_name: &str,
        field_name: &str,
        access_span: ast::SourceSpan,
    ) -> Result<(), CompileError> {
        if let Some(ast_def) = self.ast_struct_def(struct_name)
            && access_span.file_id != ast_def.span.file_id
            && let Some(field) = ast_def.fields.iter().find(|f| f.name == field_name)
            && !field.is_pub
        {
            return Err(CompileError::new(
                format!("field `{field_name}` is private"),
                access_span,
            ));
        }
        Ok(())
    }

    /// Resolve `Ref(unsized_inner)` → `RefUnsized(unsized_inner)` throughout a type.
    /// Also resolves `Struct(name)` → `Enum(name)` when name refers to an enum.
    fn resolve_refs(&self, ty: Type) -> Type {
        match ty {
            Type::Struct(ref name) if self.lowered_enums.contains_key(name) => {
                Type::Enum(name.clone())
            }
            Type::Ref(inner) => {
                let inner = self.resolve_refs(*inner);
                if inner.is_sized(&self.lowered_structs) {
                    Type::Ref(Box::new(inner))
                } else {
                    Type::RefUnsized(Box::new(inner))
                }
            }
            Type::NullableRef(inner) => {
                let inner = self.resolve_refs(*inner);
                if inner.is_sized(&self.lowered_structs) {
                    Type::NullableRef(Box::new(inner))
                } else {
                    Type::NullableRefUnsized(Box::new(inner))
                }
            }
            Type::Unique(inner) => {
                let inner = self.resolve_refs(*inner);
                if inner.is_sized(&self.lowered_structs) {
                    Type::Unique(Box::new(inner))
                } else {
                    Type::UniqueUnsized(Box::new(inner))
                }
            }
            Type::Array(inner) => Type::Array(Box::new(self.resolve_refs(*inner))),
            Type::FixedArray(inner, n) => Type::FixedArray(Box::new(self.resolve_refs(*inner)), n),
            Type::Function {
                params,
                return_type,
            } => Type::Function {
                params: params.into_iter().map(|p| self.resolve_refs(p)).collect(),
                return_type: Box::new(self.resolve_refs(*return_type)),
            },
            other => other,
        }
    }

    fn push_scope(&mut self) {
        self.scopes.push();
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    fn define_var(&mut self, name: String, ty: Type) {
        self.scopes.define(name, ty);
    }

    fn lookup_var(&mut self, name: &str) -> Option<Type> {
        if let Some(ty) = self.scopes.lookup(name) {
            let ty = ty.clone();
            // If we're inside a closure and the variable is from outside the barrier,
            // record it as a capture
            if let Some(ref mut ctx) = self.capture_context {
                // Check if this variable is from the outer scope (below the barrier)
                // by checking if it exists when we only have scopes below the barrier
                if !ctx.captured_names.contains(name) {
                    // Walk scopes to find which depth has this variable
                    // Variables defined inside the closure body are at depth >= barrier
                    // Variables from outside need capturing
                    let found_inside = {
                        let barrier = ctx.scope_depth_barrier;
                        // scopes at index >= barrier are "inside" the closure
                        let mut found = false;
                        let depth = self.scopes.depth();
                        for i in (barrier..depth).rev() {
                            if self.scopes.lookup_at(name, i).is_some() {
                                found = true;
                                break;
                            }
                        }
                        found
                    };
                    if !found_inside {
                        ctx.captured_names.insert(name.to_string());
                        ctx.captures.push(CapturedVar {
                            name: name.to_string(),
                            ty: ty.clone(),
                        });
                    }
                }
            }
            Some(ty)
        } else {
            None
        }
    }

    /// Extract the element type from an array-like type (Array or FixedArray).
    fn array_inner(ty: &Type) -> Option<&Type> {
        match ty {
            Type::Array(inner) | Type::FixedArray(inner, _) => Some(inner),
            _ => None,
        }
    }

    /// Try to coerce `expr` to `target` type. Returns the (possibly wrapped) expression.
    fn try_coerce(&self, expr: Expr, target: &Type) -> Expr {
        if expr.ty == *target {
            return expr;
        }
        match (&expr.ty, target) {
            // Array(T) → FixedArray(T, N): wrap in ArraySizeCoerce
            (Type::Array(inner), Type::FixedArray(target_inner, n))
                if **inner == **target_inner =>
            {
                let span = expr.span;
                Expr {
                    ty: target.clone(),
                    kind: ExprKind::ArraySizeCoerce {
                        expr: Box::new(expr),
                        size: *n,
                    },
                    span,
                }
            }
            // FixedArray(T, M) → FixedArray(T, N) where M != N: coerce via ArraySizeCoerce
            (Type::FixedArray(inner, _m), Type::FixedArray(target_inner, n))
                if **inner == **target_inner =>
            {
                let span = expr.span;
                Expr {
                    ty: target.clone(),
                    kind: ExprKind::ArraySizeCoerce {
                        expr: Box::new(expr),
                        size: *n,
                    },
                    span,
                }
            }
            // FixedArray(T, N) → Array(T): implicit, no wrapping needed — just change type
            (Type::FixedArray(inner, _), Type::Array(target_inner))
                if **inner == **target_inner =>
            {
                expr
            }
            // &T → &?T: a non-null reference is always a valid nullable reference.
            // Identical representation (8 bytes), so this is a no-op retag. The
            // reverse (&?T → &T) is intentionally NOT a coercion.
            (Type::Ref(a), Type::NullableRef(b)) if a == b => Expr {
                ty: target.clone(),
                span: expr.span,
                kind: expr.kind,
            },
            (Type::RefUnsized(a), Type::NullableRefUnsized(b)) if a == b => Expr {
                ty: target.clone(),
                span: expr.span,
                kind: expr.kind,
            },
            // Never coerces to any type
            (Type::Never, _) => Expr {
                ty: target.clone(),
                span: expr.span,
                kind: expr.kind,
            },
            _ => expr, // no coercion — type checker will catch mismatches
        }
    }

    /// Resolve a type alias, returning the target type with type args substituted.
    /// Returns None if the name is not an alias.
    fn resolve_type_alias(&self, name: &str, type_args: &[ast::Type]) -> Option<ast::Type> {
        let (params, target) = self.type_aliases.get(name)?;
        if params.is_empty() && type_args.is_empty() {
            Some(target.clone())
        } else if params.len() == type_args.len() {
            let subst: HashMap<String, ast::Type> = params
                .iter()
                .zip(type_args.iter())
                .map(|(p, a)| (p.clone(), a.clone()))
                .collect();
            Some(apply_subst_to_ast_type(target, &subst))
        } else {
            None
        }
    }

    /// Resolve an AST type to a typed_ast Type, triggering monomorphization for generics.
    fn resolve_ast_type(&mut self, ty: &ast::Type) -> Result<Type, CompileError> {
        match ty {
            ast::Type::Named(name) => {
                if let Some(resolved) = self.resolve_type_alias(name, &[]) {
                    return self.resolve_ast_type(&resolved);
                }
                Ok(self.resolve_refs(from_ast_type(ty)))
            }
            ast::Type::Generic { name, type_args } => {
                if let Some(resolved) = self.resolve_type_alias(name, type_args) {
                    return self.resolve_ast_type(&resolved);
                }
                if self.generic_structs.contains_key(name.as_str()) {
                    let mangled = self.ensure_struct_monomorphized(name, type_args)?;
                    Ok(Type::Struct(mangled))
                } else if self.generic_enums.contains_key(name.as_str()) {
                    let mangled = self.ensure_enum_monomorphized(name, type_args)?;
                    Ok(Type::Enum(mangled))
                } else {
                    Err(CompileError::new(
                        format!("undefined generic type: {name}"),
                        ast::SourceSpan::default(),
                    ))
                }
            }
            ast::Type::Reference(inner) => {
                let inner_ty = self.resolve_ast_type(inner)?;
                if inner_ty.is_sized(&self.lowered_structs) {
                    Ok(Type::Ref(Box::new(inner_ty)))
                } else {
                    Ok(Type::RefUnsized(Box::new(inner_ty)))
                }
            }
            ast::Type::Unique(inner) => {
                let inner_ty = self.resolve_ast_type(inner)?;
                if inner_ty.is_sized(&self.lowered_structs) {
                    Ok(Type::Unique(Box::new(inner_ty)))
                } else {
                    Ok(Type::UniqueUnsized(Box::new(inner_ty)))
                }
            }
            ast::Type::Slice(inner) => Ok(Type::Array(Box::new(self.resolve_ast_type(inner)?))),
            ast::Type::FixedArray(inner, n) => Ok(Type::FixedArray(
                Box::new(self.resolve_ast_type(inner)?),
                *n,
            )),
            ast::Type::Function {
                params,
                return_type,
            } => {
                let resolved_params: Vec<Type> = params
                    .iter()
                    .map(|(_, t)| self.resolve_ast_type(t))
                    .collect::<Result<Vec<_>, _>>()?;
                let resolved_return = match return_type.as_ref() {
                    Some(t) => self.resolve_ast_type(t)?,
                    None => Type::Unit,
                };
                Ok(Type::Function {
                    params: resolved_params,
                    return_type: Box::new(resolved_return),
                })
            }
            ast::Type::Tuple(types) => {
                let element_types: Vec<Type> = types
                    .iter()
                    .map(|t| self.resolve_ast_type(t))
                    .collect::<Result<Vec<_>, _>>()?;
                let mangled = self.ensure_tuple_struct(&element_types);
                Ok(Type::Struct(mangled))
            }
            _ => Ok(self.resolve_refs(from_ast_type(ty))),
        }
    }

    /// Like resolve_ast_type, but applies substitution first (for monomorphization of fields/variants).
    fn resolve_ast_type_with_subst(
        &mut self,
        ty: &ast::Type,
        subst: &HashMap<String, ast::Type>,
    ) -> Result<Type, CompileError> {
        match ty {
            ast::Type::Named(name) => {
                if let Some(replacement) = subst.get(name) {
                    self.resolve_ast_type(replacement)
                } else {
                    self.resolve_ast_type(ty)
                }
            }
            ast::Type::Generic { name, type_args } => {
                // Apply subst to each type arg, then resolve
                let resolved_args: Vec<ast::Type> = type_args
                    .iter()
                    .map(|a| apply_subst_to_ast_type(a, subst))
                    .collect();
                let resolved = ast::Type::Generic {
                    name: name.clone(),
                    type_args: resolved_args,
                };
                self.resolve_ast_type(&resolved)
            }
            ast::Type::Reference(inner) => {
                let inner_ty = self.resolve_ast_type_with_subst(inner, subst)?;
                if inner_ty.is_sized(&self.lowered_structs) {
                    Ok(Type::Ref(Box::new(inner_ty)))
                } else {
                    Ok(Type::RefUnsized(Box::new(inner_ty)))
                }
            }
            ast::Type::NullableReference(inner) => {
                let inner_ty = self.resolve_ast_type_with_subst(inner, subst)?;
                if inner_ty.is_sized(&self.lowered_structs) {
                    Ok(Type::NullableRef(Box::new(inner_ty)))
                } else {
                    Ok(Type::NullableRefUnsized(Box::new(inner_ty)))
                }
            }
            ast::Type::Unique(inner) => {
                let inner_ty = self.resolve_ast_type_with_subst(inner, subst)?;
                if inner_ty.is_sized(&self.lowered_structs) {
                    Ok(Type::Unique(Box::new(inner_ty)))
                } else {
                    Ok(Type::UniqueUnsized(Box::new(inner_ty)))
                }
            }
            ast::Type::Slice(inner) => Ok(Type::Array(Box::new(
                self.resolve_ast_type_with_subst(inner, subst)?,
            ))),
            ast::Type::FixedArray(inner, n) => Ok(Type::FixedArray(
                Box::new(self.resolve_ast_type_with_subst(inner, subst)?),
                *n,
            )),
            ast::Type::Function {
                params,
                return_type,
            } => {
                let resolved_params: Vec<Type> = params
                    .iter()
                    .map(|(_, t)| self.resolve_ast_type_with_subst(t, subst))
                    .collect::<Result<Vec<_>, _>>()?;
                let resolved_return = match return_type.as_ref() {
                    Some(t) => self.resolve_ast_type_with_subst(t, subst)?,
                    None => Type::Unit,
                };
                Ok(Type::Function {
                    params: resolved_params,
                    return_type: Box::new(resolved_return),
                })
            }
            ast::Type::Tuple(types) => {
                let element_types: Vec<Type> = types
                    .iter()
                    .map(|t| self.resolve_ast_type_with_subst(t, subst))
                    .collect::<Result<Vec<_>, _>>()?;
                let mangled = self.ensure_tuple_struct(&element_types);
                Ok(Type::Struct(mangled))
            }
            ast::Type::Infer => Err(CompileError::new(
                "cannot resolve Infer type without context".to_string(),
                ast::SourceSpan::default(),
            )),
        }
    }

    fn resolve_struct_name(
        &mut self,
        name: &str,
        type_args: &[ast::Type],
    ) -> Result<String, CompileError> {
        // Check if name is a type alias that resolves to a struct
        if let Some(resolved) = self.resolve_type_alias(name, type_args) {
            return match &resolved {
                ast::Type::Named(target_name) => self.resolve_struct_name(target_name, &[]),
                ast::Type::Generic {
                    name: target_name,
                    type_args: target_args,
                } => self.resolve_struct_name(target_name, target_args),
                _ => Err(CompileError::new(
                    format!("type alias `{name}` does not resolve to a struct"),
                    ast::SourceSpan::default(),
                )),
            };
        }
        if type_args.is_empty() {
            // Non-generic struct
            if !(self.structs.contains_key(name) || self.lowered_structs.contains_key(name)) {
                return Err(CompileError::new(
                    format!("undefined struct: {name}"),
                    ast::SourceSpan::default(),
                ));
            }
            Ok(name.to_string())
        } else {
            self.ensure_struct_monomorphized(name, type_args)
        }
    }

    fn resolve_enum_name(
        &mut self,
        name: &str,
        type_args: &[ast::Type],
    ) -> Result<String, CompileError> {
        // Check if name is a type alias that resolves to an enum
        if let Some(resolved) = self.resolve_type_alias(name, type_args) {
            return match &resolved {
                ast::Type::Named(target_name) => self.resolve_enum_name(target_name, &[]),
                ast::Type::Generic {
                    name: target_name,
                    type_args: target_args,
                } => self.resolve_enum_name(target_name, target_args),
                _ => Err(CompileError::new(
                    format!("type alias `{name}` does not resolve to an enum"),
                    ast::SourceSpan::default(),
                )),
            };
        }
        if type_args.is_empty() {
            // Non-generic enum
            if !(self.enums.contains_key(name) || self.lowered_enums.contains_key(name)) {
                return Err(CompileError::new(
                    format!("undefined enum: {name}"),
                    ast::SourceSpan::default(),
                ));
            }
            Ok(name.to_string())
        } else {
            self.ensure_enum_monomorphized(name, type_args)
        }
    }

    fn ensure_struct_monomorphized(
        &mut self,
        name: &str,
        type_args: &[ast::Type],
    ) -> Result<String, CompileError> {
        let gdef = self.generic_structs.get(name).ok_or_else(|| {
            CompileError::new(
                format!("undefined generic struct: {name}"),
                ast::SourceSpan::default(),
            )
        })?;
        if gdef.type_params.len() != type_args.len() {
            return Err(CompileError::new(
                format!(
                    "generic struct {name}: expected {} type arguments, got {}",
                    gdef.type_params.len(),
                    type_args.len()
                ),
                ast::SourceSpan::default(),
            ));
        }

        // Build concrete type args to compute mangled name
        let concrete_args: Vec<Type> = type_args.iter().map(from_ast_type).collect();
        let mangled = mangle_name(name, &concrete_args);

        // Already monomorphized?
        if self.lowered_structs.contains_key(&mangled) {
            return Ok(mangled);
        }

        // Build AST-level substitution map (type param name -> concrete ast::Type)
        let subst: HashMap<String, ast::Type> = gdef
            .type_params
            .iter()
            .zip(type_args.iter())
            .map(|(param, arg)| (param.clone(), arg.clone()))
            .collect();

        // Clone the AST def fields before monomorphizing
        let ast_fields = gdef.ast_def.fields.clone();

        // Insert a placeholder to prevent infinite recursion for self-referential types
        self.lowered_structs.insert(
            mangled.clone(),
            StructDef {
                name: mangled.clone(),
                fields: Vec::new(),
            },
        );

        let fields: Vec<FieldDef> = ast_fields
            .iter()
            .map(|f| {
                let ty = self.resolve_ast_type_with_subst(&f.ty, &subst)?;
                Ok(FieldDef {
                    name: f.name.clone(),
                    ty,
                })
            })
            .collect::<Result<Vec<_>, CompileError>>()?;

        self.lowered_structs.get_mut(&mangled).unwrap().fields = fields;
        self.reverse_mono
            .insert(mangled.clone(), (name.to_string(), concrete_args));
        Ok(mangled)
    }

    fn ensure_enum_monomorphized(
        &mut self,
        name: &str,
        type_args: &[ast::Type],
    ) -> Result<String, CompileError> {
        let gdef = self.generic_enums.get(name).ok_or_else(|| {
            CompileError::new(
                format!("undefined generic enum: {name}"),
                ast::SourceSpan::default(),
            )
        })?;
        if gdef.type_params.len() != type_args.len() {
            return Err(CompileError::new(
                format!(
                    "generic enum {name}: expected {} type arguments, got {}",
                    gdef.type_params.len(),
                    type_args.len()
                ),
                ast::SourceSpan::default(),
            ));
        }

        // Build concrete type args to compute mangled name
        let concrete_args: Vec<Type> = type_args.iter().map(from_ast_type).collect();
        let mangled = mangle_name(name, &concrete_args);

        // Already monomorphized?
        if self.lowered_enums.contains_key(&mangled) {
            return Ok(mangled);
        }

        // Build AST-level substitution map
        let subst: HashMap<String, ast::Type> = gdef
            .type_params
            .iter()
            .zip(type_args.iter())
            .map(|(param, arg)| (param.clone(), arg.clone()))
            .collect();

        // Clone the AST def variants before monomorphizing
        let ast_variants = gdef.ast_def.variants.clone();

        let variants = ast_variants
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let inner_type = match &v.inner_type {
                    Some(t) => Some(self.resolve_ast_type_with_subst(t, &subst)?),
                    None => None,
                };
                Ok(EnumVariantDef {
                    name: v.name.clone(),
                    inner_type,
                    index: i,
                })
            })
            .collect::<Result<Vec<_>, CompileError>>()?;

        let edef = EnumDef {
            name: mangled.clone(),
            variants,
        };
        self.lowered_enums.insert(mangled.clone(), edef);
        self.reverse_mono
            .insert(mangled.clone(), (name.to_string(), concrete_args));
        Ok(mangled)
    }

    /// Ensure a synthetic tuple struct exists for the given element types.
    /// Returns the mangled name, e.g. `__Tuple_Int_Bool`.
    fn ensure_tuple_struct(&mut self, element_types: &[Type]) -> String {
        let mangled = mangle_tuple_name(element_types);

        // Already created?
        if self.lowered_structs.contains_key(&mangled) {
            return mangled;
        }

        let fields: Vec<FieldDef> = element_types
            .iter()
            .enumerate()
            .map(|(i, ty)| FieldDef {
                name: format!("_{i}"),
                ty: ty.clone(),
            })
            .collect();

        self.lowered_structs.insert(
            mangled.clone(),
            StructDef {
                name: mangled.clone(),
                fields,
            },
        );

        mangled
    }

    fn concrete_type_to_ast_type(&self, ty: &Type) -> ast::Type {
        match ty {
            Type::Int8 => ast::Type::Named("Int8".to_string()),
            Type::Int16 => ast::Type::Named("Int16".to_string()),
            Type::Int32 => ast::Type::Named("Int32".to_string()),
            Type::Int64 => ast::Type::Named("Int64".to_string()),
            Type::Int => ast::Type::Named("Int".to_string()),
            Type::Uint8 => ast::Type::Named("Uint8".to_string()),
            Type::Uint16 => ast::Type::Named("Uint16".to_string()),
            Type::Uint32 => ast::Type::Named("Uint32".to_string()),
            Type::Uint64 => ast::Type::Named("Uint64".to_string()),
            Type::Uint => ast::Type::Named("Uint".to_string()),
            Type::Float32 => ast::Type::Named("Float32".to_string()),
            Type::Float64 => ast::Type::Named("Float64".to_string()),
            Type::Bool => ast::Type::Named("Bool".to_string()),
            Type::FileDesc => ast::Type::Named("FileDesc".to_string()),
            Type::Unit => ast::Type::Named("Unit".to_string()),
            Type::Never => ast::Type::Named("Never".to_string()),
            Type::NullableRef(inner) | Type::NullableRefUnsized(inner) => {
                ast::Type::NullableReference(Box::new(self.concrete_type_to_ast_type(inner)))
            }
            Type::Ref(inner) | Type::RefUnsized(inner) => {
                ast::Type::Reference(Box::new(self.concrete_type_to_ast_type(inner)))
            }
            Type::Unique(inner) | Type::UniqueUnsized(inner) => {
                ast::Type::Unique(Box::new(self.concrete_type_to_ast_type(inner)))
            }
            Type::Array(inner) => ast::Type::Slice(Box::new(self.concrete_type_to_ast_type(inner))),
            Type::FixedArray(inner, n) => {
                ast::Type::FixedArray(Box::new(self.concrete_type_to_ast_type(inner)), *n)
            }
            Type::Function {
                params,
                return_type,
            } => ast::Type::Function {
                params: params
                    .iter()
                    .map(|p| (None, self.concrete_type_to_ast_type(p)))
                    .collect(),
                return_type: if **return_type == Type::Unit {
                    None
                } else {
                    Some(Box::new(self.concrete_type_to_ast_type(return_type)))
                },
            },
            Type::Struct(name) | Type::Enum(name) => {
                if let Some((base_name, concrete_args)) = self.reverse_mono.get(name) {
                    ast::Type::Generic {
                        name: base_name.clone(),
                        type_args: concrete_args
                            .iter()
                            .map(|t| self.concrete_type_to_ast_type(t))
                            .collect(),
                    }
                } else {
                    ast::Type::Named(name.clone())
                }
            }
        }
    }

    /// Try to unify `pattern` against `concrete`, returning false on binding conflicts.
    fn try_unify_type(
        &self,
        pattern: &ast::Type,
        concrete: &Type,
        type_params: &[String],
        bindings: &mut HashMap<String, ast::Type>,
    ) -> bool {
        match pattern {
            ast::Type::Named(name) if type_params.contains(name) => {
                let inferred = self.concrete_type_to_ast_type(concrete);
                if let Some(existing) = bindings.get(name) {
                    if *existing != inferred {
                        return false;
                    }
                } else {
                    bindings.insert(name.clone(), inferred);
                }
            }
            ast::Type::Named(_) => {}
            ast::Type::Generic { name, type_args } => match concrete {
                Type::Struct(mangled) | Type::Enum(mangled) => {
                    if let Some((base_name, concrete_args)) = self.reverse_mono.get(mangled)
                        && base_name == name
                        && concrete_args.len() == type_args.len()
                    {
                        for (pat_arg, conc_arg) in type_args.iter().zip(concrete_args.iter()) {
                            if !self.try_unify_type(pat_arg, conc_arg, type_params, bindings) {
                                return false;
                            }
                        }
                    }
                }
                _ => {}
            },
            ast::Type::Reference(inner) => {
                if let Type::Ref(c_inner) | Type::RefUnsized(c_inner) = concrete
                    && !self.try_unify_type(inner, c_inner, type_params, bindings)
                {
                    return false;
                }
            }
            ast::Type::NullableReference(inner) => {
                if let Type::NullableRef(c_inner) | Type::NullableRefUnsized(c_inner) = concrete
                    && !self.try_unify_type(inner, c_inner, type_params, bindings)
                {
                    return false;
                }
            }
            ast::Type::Unique(inner) => {
                if let Type::Unique(c_inner) | Type::UniqueUnsized(c_inner) = concrete
                    && !self.try_unify_type(inner, c_inner, type_params, bindings)
                {
                    return false;
                }
            }
            ast::Type::Slice(inner) => {
                if let Type::Array(c_inner) = concrete
                    && !self.try_unify_type(inner, c_inner, type_params, bindings)
                {
                    return false;
                }
            }
            ast::Type::FixedArray(inner, _) => {
                if let Type::FixedArray(c_inner, _) = concrete
                    && !self.try_unify_type(inner, c_inner, type_params, bindings)
                {
                    return false;
                }
            }
            ast::Type::Function {
                params,
                return_type,
            } => {
                if let Type::Function {
                    params: c_params,
                    return_type: c_ret,
                } = concrete
                {
                    for ((_, pat_ty), conc_ty) in params.iter().zip(c_params.iter()) {
                        if !self.try_unify_type(pat_ty, conc_ty, type_params, bindings) {
                            return false;
                        }
                    }
                    if let Some(pat_ret) = return_type
                        && !self.try_unify_type(pat_ret, c_ret, type_params, bindings)
                    {
                        return false;
                    }
                }
            }
            ast::Type::Tuple(types) => {
                if let Type::Struct(mangled) = concrete
                    && let Some(sdef) = self.lowered_structs.get(mangled)
                {
                    for (pat_ty, field) in types.iter().zip(sdef.fields.iter()) {
                        if !self.try_unify_type(pat_ty, &field.ty, type_params, bindings) {
                            return false;
                        }
                    }
                }
            }
            ast::Type::Infer => {}
        }
        true
    }

    fn unify_type(
        &self,
        pattern: &ast::Type,
        concrete: &Type,
        type_params: &[String],
        bindings: &mut HashMap<String, ast::Type>,
    ) {
        assert!(
            self.try_unify_type(pattern, concrete, type_params, bindings),
            "conflicting inferred types during unification"
        );
    }

    fn infer_type_args(
        &self,
        func_name: &str,
        type_params: &[String],
        param_types: &[ast::Type],
        arg_types: &[Type],
    ) -> Result<Vec<ast::Type>, CompileError> {
        let mut bindings: HashMap<String, ast::Type> = HashMap::new();
        for (pat, conc) in param_types.iter().zip(arg_types.iter()) {
            self.unify_type(pat, conc, type_params, &mut bindings);
        }
        type_params
            .iter()
            .map(|tp| {
                bindings.get(tp).cloned().ok_or_else(|| {
                    CompileError::new(
                        format!("could not infer type argument `{tp}` for generic function `{func_name}`"),
                        ast::SourceSpan::default(),
                    )
                })
            })
            .collect()
    }

    fn ensure_function_monomorphized_with_def(
        &mut self,
        name: &str,
        gdef: &FunctionEntry,
        type_args: &[ast::Type],
        num_overloads: usize,
        mangle_prefix: &str,
    ) -> Result<String, CompileError> {
        assert!(
            gdef.type_params.len() == type_args.len(),
            "generic function {name}: expected {} type arguments, got {}",
            gdef.type_params.len(),
            type_args.len()
        );

        let type_params = gdef.type_params.clone();
        let ast_def_clone = gdef.ast_def.clone();
        let overload_index = gdef.overload_index;

        // Compute mangled name
        let concrete_args: Vec<Type> = type_args
            .iter()
            .map(|t| self.resolve_ast_type(t))
            .collect::<Result<Vec<_>, _>>()?;
        let mut mangled = format!("{mangle_prefix}{}", mangle_name(name, &concrete_args));
        // Disambiguate when multiple generic overloads exist
        if num_overloads > 1 {
            mangled = format!("{mangled}_ov{overload_index}");
        }

        // Already monomorphized?
        if self.monomorphized_functions.contains_key(&mangled) {
            return Ok(mangled);
        }

        // Build AST-level substitution map
        let subst: HashMap<String, ast::Type> = type_params
            .iter()
            .zip(type_args.iter())
            .map(|(param, arg)| (param.clone(), arg.clone()))
            .collect();

        // Clone the AST def and apply substitution
        let mut ast_def = ast_def_clone;
        ast_def.name = mangled.clone();
        ast_def.type_params.clear();
        for param in &mut ast_def.parameters {
            param.ty = apply_subst_to_ast_type(&param.ty, &subst);
        }
        if let Some(ref mut rt) = ast_def.return_type {
            *rt = apply_subst_to_ast_type(rt, &subst);
        }
        ast_def.body = ast_def
            .body
            .iter()
            .map(|s| apply_subst_to_ast_statement(s, &subst))
            .collect();

        // Lower the substituted function
        let lowered = self.lower_function(&ast_def)?;
        self.resolved_return_types
            .insert(mangled.clone(), lowered.return_type.clone());
        self.monomorphized_functions
            .insert(mangled.clone(), lowered);
        Ok(mangled)
    }

    fn lower_all(&mut self) -> Result<SourceFile, CompileError> {
        let struct_names: Vec<String> = self.structs.keys().cloned().collect();
        let enum_names: Vec<String> = self.enums.keys().cloned().collect();

        // Insert placeholder structs (for self-referential type resolution)
        for name in &struct_names {
            self.lowered_structs.insert(
                name.clone(),
                StructDef {
                    name: name.clone(),
                    fields: Vec::new(),
                },
            );
        }

        // Insert placeholder enums (for self-referential type resolution)
        for name in &enum_names {
            self.lowered_enums.insert(
                name.clone(),
                EnumDef {
                    name: name.clone(),
                    variants: Vec::new(),
                },
            );
        }

        // Lower struct fields using resolve_ast_type (triggers monomorphization of generics)
        for name in &struct_names {
            let def = *self.structs.get(name).unwrap();
            let fields: Vec<FieldDef> = def
                .fields
                .iter()
                .map(|f| {
                    Ok(FieldDef {
                        name: f.name.clone(),
                        ty: self.resolve_ast_type(&f.ty)?,
                    })
                })
                .collect::<Result<Vec<_>, CompileError>>()?;
            self.lowered_structs.get_mut(name).unwrap().fields = fields;
        }

        // Lower enum variants using resolve_ast_type (triggers monomorphization of generics)
        for name in &enum_names {
            let def = *self.enums.get(name).unwrap();
            let variants: Vec<EnumVariantDef> = def
                .variants
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    let inner_type = match &v.inner_type {
                        Some(t) => Some(self.resolve_ast_type(t)?),
                        None => None,
                    };
                    Ok(EnumVariantDef {
                        name: v.name.clone(),
                        inner_type,
                        index: i,
                    })
                })
                .collect::<Result<Vec<_>, CompileError>>()?;
            self.lowered_enums.get_mut(name).unwrap().variants = variants;
        }

        // Build local structs/enums from non-generic definitions
        let mut structs: HashMap<String, StructDef> = struct_names
            .iter()
            .map(|name| (name.clone(), self.lowered_structs[name].clone()))
            .collect();
        let mut enums: HashMap<String, EnumDef> = enum_names
            .iter()
            .map(|name| (name.clone(), self.lowered_enums[name].clone()))
            .collect();

        // Validate: unsized fields must be last in struct
        for (name, sdef) in &structs {
            for (i, field) in sdef.fields.iter().enumerate() {
                if !field.ty.is_sized(&self.lowered_structs) && i != sdef.fields.len() - 1 {
                    let span = self
                        .structs
                        .get(name.as_str())
                        .and_then(|ast_sdef| ast_sdef.fields.get(i))
                        .map(|f| f.span)
                        .unwrap_or_default();
                    return Err(CompileError::new(
                        format!(
                            "struct `{name}`: unsized field `{}` must be the last field",
                            field.name
                        ),
                        span,
                    ));
                }
            }
        }

        // Register all concrete functions with mangled names and lower them.
        // Collect (mangled_name, ast_def) pairs for all concrete entries.
        let concrete_funcs: Vec<(String, Vec<ast::Type>, ast::FunctionDef)> = self
            .function_defs
            .iter()
            .flat_map(|(name, entries)| {
                entries
                    .iter()
                    .filter(|e| e.type_params.is_empty())
                    .map(|e| {
                        let ast_param_types: Vec<ast::Type> =
                            e.ast_def.parameters.iter().map(|p| p.ty.clone()).collect();
                        (name.clone(), ast_param_types, e.ast_def.clone())
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        // Resolve types and build (mangled, ast_def) pairs
        let mut func_to_lower: Vec<(String, ast::FunctionDef)> = Vec::new();
        for (name, ast_param_types, ast_def) in concrete_funcs {
            let param_types: Vec<Type> = ast_param_types
                .iter()
                .map(|t| self.resolve_ast_type(t))
                .collect::<Result<Vec<_>, _>>()?;
            let mangled = mangle_name(&name, &param_types);
            self.concrete_ast_defs
                .insert(mangled.clone(), ast_def.clone());
            func_to_lower.push((mangled, ast_def));
        }

        // NOTE: concrete methods are NOT pre-registered here.
        // They are lowered on demand via ensure_concrete_lowered at call sites.

        let mut functions = HashMap::new();
        for (mangled, ast_def) in &func_to_lower {
            let mut lowered = self.lower_function(ast_def)?;
            lowered.name = mangled.clone();
            self.resolved_return_types
                .insert(mangled.clone(), lowered.return_type.clone());
            functions.insert(mangled.clone(), lowered);
        }

        // Add synthetic closure functions
        for closure_fn in std::mem::take(&mut self.pending_closures) {
            functions.insert(closure_fn.name.clone(), closure_fn);
        }

        // Merge monomorphized generic functions into the output
        for (name, fdef) in std::mem::take(&mut self.monomorphized_functions) {
            functions.entry(name).or_insert(fdef);
        }

        // Merge monomorphized generic structs/enums into the output
        for (name, sdef) in &self.lowered_structs {
            if !structs.contains_key(name) {
                structs.insert(name.clone(), sdef.clone());
            }
        }
        for (name, edef) in &self.lowered_enums {
            if !enums.contains_key(name) {
                enums.insert(name.clone(), edef.clone());
            }
        }

        Ok(SourceFile {
            structs,
            enums,
            functions,
        })
    }

    /// Ensure a concrete function/method is fully lowered and stored in monomorphized_functions.
    /// For eagerly-lowered functions this is a no-op (they're already in the functions HashMap).
    /// For methods, this lazily lowers the method on first use.
    fn ensure_concrete_lowered(
        &mut self,
        mangled: &str,
        ast_def: &ast::FunctionDef,
    ) -> Result<(), CompileError> {
        if self.monomorphized_functions.contains_key(mangled)
            || self.concrete_ast_defs.contains_key(mangled)
        {
            return Ok(());
        }
        let mut ast_def = ast_def.clone();
        ast_def.name = mangled.to_string();
        // Register for resolve_return_type before lowering (handles cycles)
        self.concrete_ast_defs
            .insert(mangled.to_string(), ast_def.clone());
        let lowered = self.lower_function(&ast_def)?;
        self.resolved_return_types
            .insert(mangled.to_string(), lowered.return_type.clone());
        self.monomorphized_functions
            .insert(mangled.to_string(), lowered);
        Ok(())
    }

    /// Get the return type of a function, lowering it on-demand if needed.
    fn resolve_return_type(&mut self, name: &str) -> Result<Type, CompileError> {
        // Explicit return type — no lowering needed
        if let Some(func_def) = self.concrete_ast_defs.get(name)
            && let Some(rt) = &func_def.return_type
        {
            let rt = rt.clone();
            return self.resolve_ast_type(&rt);
        }
        // Already resolved
        if let Some(ty) = self.resolved_return_types.get(name) {
            return Ok(ty.clone());
        }
        // Cycle detection
        if self.resolving_return_types.contains(&name.to_string()) {
            return Err(CompileError::new(
                format!(
                    "cannot infer return type of recursive function `{name}` — add an explicit return type annotation"
                ),
                ast::SourceSpan::default(),
            ));
        }
        // Lower the function in an isolated scope to determine its return type
        self.resolving_return_types.push(name.to_string());
        let func_def = self.concrete_ast_defs.get(name).unwrap().clone();
        let saved_scopes = std::mem::take(&mut self.scopes);
        let saved_return_type = self.current_return_type.take();
        let saved_return_type_span = self.current_return_type_span.take();
        let saved_capture_context = self.capture_context.take();
        let saved_nested = std::mem::take(&mut self.nested_function_defs);
        let lowered = self.lower_function(&func_def)?;
        self.scopes = saved_scopes;
        self.current_return_type = saved_return_type;
        self.current_return_type_span = saved_return_type_span;
        self.capture_context = saved_capture_context;
        self.nested_function_defs = saved_nested;
        self.resolving_return_types.pop();
        let ret_ty = lowered.return_type.clone();
        self.resolved_return_types
            .insert(name.to_string(), ret_ty.clone());
        Ok(ret_ty)
    }

    fn lower_function(&mut self, func: &ast::FunctionDef) -> Result<FunctionDef, CompileError> {
        self.push_scope();
        self.nested_function_defs.push(HashMap::new());
        let mut param_destructure_stmts: Vec<Statement> = Vec::new();
        let mut parameters: Vec<Parameter> = Vec::new();
        for (i, p) in func.parameters.iter().enumerate() {
            let ty = self.resolve_ast_type(&p.ty)?;
            if !ty.is_sized(&self.lowered_structs) {
                return Err(CompileError::new(
                    format!(
                        "function `{}`: parameter has unsized type {}",
                        func.name, ty
                    ),
                    p.span,
                ));
            }
            match &p.pattern {
                ast::DestructurePattern::Name(name) => {
                    self.define_var(name.clone(), ty.clone());
                    parameters.push(Parameter {
                        name: name.clone(),
                        ty,
                        span: p.span,
                    });
                }
                _ => {
                    let synthetic_name = format!("__param_{i}");
                    self.define_var(synthetic_name.clone(), ty.clone());
                    let base_expr = Expr {
                        ty: ty.clone(),
                        kind: ExprKind::Identifier(synthetic_name.clone()),
                        span: ast::SourceSpan::default(),
                    };
                    self.expand_destructure_pattern(
                        &p.pattern,
                        base_expr,
                        &ty,
                        &mut param_destructure_stmts,
                    )?;
                    parameters.push(Parameter {
                        name: synthetic_name,
                        ty,
                        span: p.span,
                    });
                }
            }
        }
        let explicit_return_type = match func.return_type.as_ref() {
            Some(t) => Some(self.resolve_ast_type(t)?),
            None => None,
        };

        if let Some(ref rt) = explicit_return_type
            && !rt.is_sized(&self.lowered_structs)
        {
            return Err(CompileError::new(
                format!("function `{}`: return type {} is unsized", func.name, rt),
                func.return_type_span.unwrap_or(func.span),
            ));
        }

        let prev_return_type =
            std::mem::replace(&mut self.current_return_type, explicit_return_type.clone());
        let prev_return_type_span =
            std::mem::replace(&mut self.current_return_type_span, func.return_type_span);
        let mut body: Vec<Statement> = func
            .body
            .iter()
            .map(|s| self.lower_statement(s))
            .collect::<Result<Vec<Vec<Statement>>, CompileError>>()?
            .into_iter()
            .flatten()
            .collect();
        self.current_return_type = prev_return_type;
        self.current_return_type_span = prev_return_type_span;

        // Prepend param destructuring statements
        if !param_destructure_stmts.is_empty() {
            param_destructure_stmts.append(&mut body);
            body = param_destructure_stmts;
        }

        let return_type = if let Some(rt) = explicit_return_type {
            // Explicit return type: validate the body produces the right type
            if rt != Type::Unit {
                let last_info = body.last().and_then(|s| match &s.kind {
                    StatementKind::Expression(expr) => Some((&expr.ty, s.span)),
                    StatementKind::Return(expr) => Some((&expr.ty, s.span)),
                    _ => None,
                });
                match last_info {
                    Some((ty, last_span)) => {
                        if *ty != rt {
                            return Err(CompileError::new(
                                format!(
                                    "function `{}` should return {rt}, but last expression is {ty}",
                                    func.name
                                ),
                                last_span,
                            )
                            .with_label(
                                format!("expected {rt} because of return type"),
                                func.return_type_span.unwrap_or(func.span),
                            ));
                        }
                    }
                    None => {
                        return Err(CompileError::new(
                            format!(
                                "function `{}` should return {rt}, but body does not end with an expression",
                                func.name
                            ),
                            func.span,
                        ));
                    }
                }
            }
            rt
        } else {
            // Infer return type from the last expression in the body
            let inferred = body
                .last()
                .and_then(|s| match &s.kind {
                    StatementKind::Expression(expr) => Some(expr.ty.clone()),
                    StatementKind::Return(expr) => Some(expr.ty.clone()),
                    _ => None,
                })
                .unwrap_or(Type::Unit);
            if !inferred.is_sized(&self.lowered_structs) {
                return Err(CompileError::new(
                    format!(
                        "function `{}`: inferred return type {} is unsized",
                        func.name, inferred
                    ),
                    func.span,
                ));
            }
            inferred
        };

        self.nested_function_defs.pop();
        self.pop_scope();
        Ok(FunctionDef {
            name: func.name.clone(),
            parameters,
            return_type,
            body,
        })
    }

    fn lower_statement(&mut self, stmt: &ast::Statement) -> Result<Vec<Statement>, CompileError> {
        match &stmt.kind {
            ast::StatementKind::Let { pattern, ty, value } => {
                let (resolved_ty, lowered) = if let Some(ty) = ty {
                    let expected = self.resolve_ast_type(ty)?;
                    let lowered = if Self::has_infer_params(value) {
                        self.lower_expr_with_expected(value, &expected)?
                    } else {
                        self.lower_expr(value)?
                    };
                    let coerced = self.try_coerce(lowered, &expected);
                    if coerced.ty != expected {
                        return Err(CompileError::new(
                            format!(
                                "type mismatch in let: expected {expected}, got {}",
                                coerced.ty
                            ),
                            coerced.span,
                        )
                        .with_label("expected type declared here", stmt.span));
                    }
                    (expected, coerced)
                } else {
                    let lowered = self.lower_expr(value)?;
                    let ty = lowered.ty.clone();
                    (ty, lowered)
                };

                match pattern {
                    ast::DestructurePattern::Name(name) => {
                        self.define_var(name.clone(), resolved_ty.clone());
                        Ok(vec![Statement {
                            kind: StatementKind::Let {
                                name: name.clone(),
                                ty: resolved_ty,
                                value: lowered,
                            },
                            span: stmt.span,
                        }])
                    }
                    _ => {
                        // Bind RHS to a temp, then expand destructuring
                        let tmp_name = format!("__destructure_{}", self.destructure_counter);
                        self.destructure_counter += 1;
                        self.define_var(tmp_name.clone(), resolved_ty.clone());
                        let mut stmts = vec![Statement {
                            kind: StatementKind::Let {
                                name: tmp_name.clone(),
                                ty: resolved_ty.clone(),
                                value: lowered,
                            },
                            span: stmt.span,
                        }];
                        let base_expr = Expr {
                            ty: resolved_ty.clone(),
                            kind: ExprKind::Identifier(tmp_name),
                            span: ast::SourceSpan::default(),
                        };
                        self.expand_destructure_pattern(
                            pattern,
                            base_expr,
                            &resolved_ty,
                            &mut stmts,
                        )?;
                        Ok(stmts)
                    }
                }
            }
            ast::StatementKind::Assignment { target, value } => {
                let lowered_target = self.lower_expr(target)?;
                let lowered_value = self.lower_expr(value)?;
                let lowered_value = self.try_coerce(lowered_value, &lowered_target.ty);
                if lowered_target.ty != lowered_value.ty {
                    return Err(CompileError::new(
                        format!(
                            "type mismatch in assignment: expected {}, got {}",
                            lowered_target.ty, lowered_value.ty
                        ),
                        lowered_value.span,
                    )
                    .with_label(
                        format!("target has type {}", lowered_target.ty),
                        lowered_target.span,
                    ));
                }
                if !is_place_expr(&lowered_target) {
                    return Err(CompileError::new(
                        "cannot assign to non-place expression".to_string(),
                        stmt.span,
                    ));
                }
                Ok(vec![Statement {
                    kind: StatementKind::Assignment {
                        target: lowered_target,
                        value: lowered_value,
                    },
                    span: stmt.span,
                }])
            }
            ast::StatementKind::If {
                condition,
                body,
                else_body,
            } => {
                let lowered_cond = self.lower_expr(condition)?;
                if lowered_cond.ty != Type::Bool {
                    return Err(CompileError::new(
                        format!("if condition must be Bool, got {}", lowered_cond.ty),
                        stmt.span,
                    ));
                }
                self.push_scope();
                let lowered_body: Vec<Statement> = body
                    .iter()
                    .map(|s| self.lower_statement(s))
                    .collect::<Result<Vec<Vec<Statement>>, CompileError>>()?
                    .into_iter()
                    .flatten()
                    .collect();
                self.pop_scope();
                let lowered_else = if !else_body.is_empty() {
                    self.push_scope();
                    let v: Vec<Statement> = else_body
                        .iter()
                        .map(|s| self.lower_statement(s))
                        .collect::<Result<Vec<Vec<Statement>>, CompileError>>()?
                        .into_iter()
                        .flatten()
                        .collect();
                    self.pop_scope();
                    v
                } else {
                    Vec::new()
                };
                Ok(vec![Statement {
                    kind: StatementKind::If {
                        condition: lowered_cond,
                        body: lowered_body,
                        else_body: lowered_else,
                    },
                    span: stmt.span,
                }])
            }
            ast::StatementKind::While { condition, body } => {
                let lowered_cond = self.lower_expr(condition)?;
                if lowered_cond.ty != Type::Bool {
                    return Err(CompileError::new(
                        format!("while condition must be Bool, got {}", lowered_cond.ty),
                        stmt.span,
                    ));
                }
                self.push_scope();
                let lowered_body: Vec<Statement> = body
                    .iter()
                    .map(|s| self.lower_statement(s))
                    .collect::<Result<Vec<Vec<Statement>>, CompileError>>()?
                    .into_iter()
                    .flatten()
                    .collect();
                self.pop_scope();
                Ok(vec![Statement {
                    kind: StatementKind::While {
                        condition: lowered_cond,
                        body: lowered_body,
                    },
                    span: stmt.span,
                }])
            }
            ast::StatementKind::ForRange {
                variable,
                start,
                end,
                body,
            } => {
                let n = self.for_counter;
                self.for_counter += 1;
                let counter_name = format!("__for_counter_{n}");
                let lowered_start = self.lower_expr(start)?;
                let lowered_end = self.lower_expr(end)?;
                let iter_ty = lowered_start.ty.clone();
                if !iter_ty.is_integer() {
                    return Err(CompileError::new(
                        format!("for range start must be integer, got {iter_ty}"),
                        stmt.span,
                    ));
                }
                if lowered_end.ty != iter_ty {
                    return Err(CompileError::new(
                        format!(
                            "for range end must have type {iter_ty}, got {}",
                            lowered_end.ty
                        ),
                        lowered_end.span,
                    )
                    .with_label(format!("start has type {iter_ty}"), lowered_start.span));
                }

                // let __for_counter_N = start;
                self.define_var(counter_name.clone(), iter_ty.clone());
                let let_counter = Statement {
                    kind: StatementKind::Let {
                        name: counter_name.clone(),
                        ty: iter_ty.clone(),
                        value: lowered_start,
                    },
                    span: stmt.span,
                };

                // Build while body: let variable = __for_counter_N; <body>; __for_counter_N = __for_counter_N + 1;
                self.push_scope();
                self.define_var(variable.clone(), iter_ty.clone());
                let let_var = Statement {
                    kind: StatementKind::Let {
                        name: variable.clone(),
                        ty: iter_ty.clone(),
                        value: Expr {
                            ty: iter_ty.clone(),
                            kind: ExprKind::Identifier(counter_name.clone()),
                            span: ast::SourceSpan::default(),
                        },
                    },
                    span: stmt.span,
                };
                let mut while_body = vec![let_var];
                for s in body {
                    while_body.extend(self.lower_statement(s)?);
                }
                let increment = Statement {
                    kind: StatementKind::Assignment {
                        target: Expr {
                            ty: iter_ty.clone(),
                            kind: ExprKind::Identifier(counter_name.clone()),
                            span: ast::SourceSpan::default(),
                        },
                        value: Expr {
                            ty: iter_ty.clone(),
                            kind: ExprKind::BinaryOp {
                                op: ast::BinOp::Add,
                                left: Box::new(Expr {
                                    ty: iter_ty.clone(),
                                    kind: ExprKind::Identifier(counter_name.clone()),
                                    span: ast::SourceSpan::default(),
                                }),
                                right: Box::new(Expr {
                                    ty: iter_ty.clone(),
                                    kind: ExprKind::IntegerLiteral(1),
                                    span: ast::SourceSpan::default(),
                                }),
                            },
                            span: ast::SourceSpan::default(),
                        },
                    },
                    span: stmt.span,
                };
                while_body.push(increment);
                self.pop_scope();

                let condition = Expr {
                    ty: Type::Bool,
                    kind: ExprKind::BinaryOp {
                        op: ast::BinOp::Lt,
                        left: Box::new(Expr {
                            ty: iter_ty.clone(),
                            kind: ExprKind::Identifier(counter_name.clone()),
                            span: ast::SourceSpan::default(),
                        }),
                        right: Box::new(lowered_end),
                    },
                    span: ast::SourceSpan::default(),
                };

                let while_stmt = Statement {
                    kind: StatementKind::While {
                        condition,
                        body: while_body,
                    },
                    span: stmt.span,
                };

                Ok(vec![let_counter, while_stmt])
            }
            ast::StatementKind::ForReflectFields {
                pattern,
                object,
                body,
            } => self.lower_reflect_fields(stmt.span, pattern, object, body),
            ast::StatementKind::MatchReflectVariant {
                pattern,
                object,
                body,
            } => self.lower_match_reflect_variant(stmt.span, pattern, object, body),
            ast::StatementKind::ForIn {
                variable,
                iterable,
                body,
            } => {
                let n = self.for_counter;
                self.for_counter += 1;
                let arr_name = format!("__for_arr_{n}");
                let len_name = format!("__for_len_{n}");
                let idx_name = format!("__for_idx_{n}");

                let lowered_iter = self.lower_expr(iterable)?;
                let arr_ty = lowered_iter.ty.clone();
                let elem_ty = match &arr_ty {
                    Type::Array(inner) => (**inner).clone(),
                    Type::FixedArray(inner, _) => (**inner).clone(),
                    _ => {
                        return Err(CompileError::new(
                            format!("for-in iterable must be an array type, got {}", arr_ty),
                            stmt.span,
                        ));
                    }
                };

                let mut stmts = Vec::new();

                // let __for_arr_N = iterable;
                self.define_var(arr_name.clone(), arr_ty.clone());
                stmts.push(Statement {
                    kind: StatementKind::Let {
                        name: arr_name.clone(),
                        ty: arr_ty.clone(),
                        value: lowered_iter,
                    },
                    span: stmt.span,
                });

                // let __for_len_N = intrinsic::array_len(__for_arr_N);
                self.define_var(len_name.clone(), Type::Uint);
                stmts.push(Statement {
                    kind: StatementKind::Let {
                        name: len_name.clone(),
                        ty: Type::Uint,
                        value: Expr {
                            ty: Type::Uint,
                            kind: ExprKind::IntrinsicCall {
                                intrinsic: ast::Intrinsic::ArrayLen,
                                arguments: vec![Expr {
                                    ty: arr_ty.clone(),
                                    kind: ExprKind::Identifier(arr_name.clone()),
                                    span: ast::SourceSpan::default(),
                                }],
                            },
                            span: ast::SourceSpan::default(),
                        },
                    },
                    span: stmt.span,
                });

                // let __for_idx_N = 0u;
                self.define_var(idx_name.clone(), Type::Uint);
                stmts.push(Statement {
                    kind: StatementKind::Let {
                        name: idx_name.clone(),
                        ty: Type::Uint,
                        value: Expr {
                            ty: Type::Uint,
                            kind: ExprKind::IntegerLiteral(0),
                            span: ast::SourceSpan::default(),
                        },
                    },
                    span: stmt.span,
                });

                // Build while body
                self.push_scope();
                self.define_var(variable.clone(), elem_ty.clone());
                let let_var = Statement {
                    kind: StatementKind::Let {
                        name: variable.clone(),
                        ty: elem_ty.clone(),
                        value: Expr {
                            ty: elem_ty.clone(),
                            kind: ExprKind::Index {
                                object: Box::new(Expr {
                                    ty: arr_ty.clone(),
                                    kind: ExprKind::Identifier(arr_name.clone()),
                                    span: ast::SourceSpan::default(),
                                }),
                                index: Box::new(Expr {
                                    ty: Type::Uint,
                                    kind: ExprKind::Identifier(idx_name.clone()),
                                    span: ast::SourceSpan::default(),
                                }),
                            },
                            span: ast::SourceSpan::default(),
                        },
                    },
                    span: stmt.span,
                };
                let mut while_body = vec![let_var];
                for s in body {
                    while_body.extend(self.lower_statement(s)?);
                }
                let increment = Statement {
                    kind: StatementKind::Assignment {
                        target: Expr {
                            ty: Type::Uint,
                            kind: ExprKind::Identifier(idx_name.clone()),
                            span: ast::SourceSpan::default(),
                        },
                        value: Expr {
                            ty: Type::Uint,
                            kind: ExprKind::BinaryOp {
                                op: ast::BinOp::Add,
                                left: Box::new(Expr {
                                    ty: Type::Uint,
                                    kind: ExprKind::Identifier(idx_name.clone()),
                                    span: ast::SourceSpan::default(),
                                }),
                                right: Box::new(Expr {
                                    ty: Type::Uint,
                                    kind: ExprKind::IntegerLiteral(1),
                                    span: ast::SourceSpan::default(),
                                }),
                            },
                            span: ast::SourceSpan::default(),
                        },
                    },
                    span: stmt.span,
                };
                while_body.push(increment);
                self.pop_scope();

                let condition = Expr {
                    ty: Type::Bool,
                    kind: ExprKind::BinaryOp {
                        op: ast::BinOp::Lt,
                        left: Box::new(Expr {
                            ty: Type::Uint,
                            kind: ExprKind::Identifier(idx_name.clone()),
                            span: ast::SourceSpan::default(),
                        }),
                        right: Box::new(Expr {
                            ty: Type::Uint,
                            kind: ExprKind::Identifier(len_name.clone()),
                            span: ast::SourceSpan::default(),
                        }),
                    },
                    span: ast::SourceSpan::default(),
                };

                stmts.push(Statement {
                    kind: StatementKind::While {
                        condition,
                        body: while_body,
                    },
                    span: stmt.span,
                });

                Ok(stmts)
            }
            ast::StatementKind::Expression(expr) => Ok(vec![Statement {
                kind: StatementKind::Expression(self.lower_expr(expr)?),
                span: stmt.span,
            }]),
            ast::StatementKind::Return(expr) => {
                let lowered = self.lower_expr(expr)?;
                let lowered = if let Some(ref expected) = self.current_return_type {
                    let coerced = self.try_coerce(lowered, expected);
                    if coerced.ty != *expected {
                        let mut err = CompileError::new(
                            format!(
                                "return type mismatch: expected {}, got {}",
                                expected, coerced.ty
                            ),
                            coerced.span,
                        );
                        if let Some(rt_span) = self.current_return_type_span {
                            err = err.with_label(
                                format!("expected {} because of return type", expected),
                                rt_span,
                            );
                        }
                        return Err(err);
                    }
                    coerced
                } else {
                    lowered
                };
                Ok(vec![Statement {
                    kind: StatementKind::Return(lowered),
                    span: stmt.span,
                }])
            }
            ast::StatementKind::NestedFunction(fdef) => {
                // Generic or overloaded nested function: store for later resolution at call sites
                let has_prev_entries = self
                    .nested_function_defs
                    .last()
                    .is_some_and(|m| m.contains_key(&fdef.name));

                if !fdef.type_params.is_empty() || has_prev_entries {
                    // Store as a FunctionEntry in nested_function_defs
                    if let Some(registry) = self.nested_function_defs.last_mut() {
                        let entries = registry.entry(fdef.name.clone()).or_default();
                        let overload_index = entries.len();
                        entries.push(FunctionEntry {
                            type_params: fdef.type_params.clone(),
                            ast_def: fdef.clone(),
                            overload_index,
                        });
                    }
                    return Ok(vec![]);
                }

                // Non-generic, non-overloaded: store for potential future overloading,
                // then lower as a closure and bind to a local variable
                if let Some(registry) = self.nested_function_defs.last_mut() {
                    let entries = registry.entry(fdef.name.clone()).or_default();
                    entries.push(FunctionEntry {
                        type_params: vec![],
                        ast_def: fdef.clone(),
                        overload_index: 0,
                    });
                }

                let closure_expr = self.lower_closure(
                    stmt.span,
                    &fdef.parameters,
                    fdef.return_type.as_ref(),
                    &fdef.body,
                )?;
                let fn_ty = closure_expr.ty.clone();
                self.define_var(fdef.name.clone(), fn_ty.clone());
                Ok(vec![Statement {
                    kind: StatementKind::Let {
                        name: fdef.name.clone(),
                        ty: fn_ty,
                        value: closure_expr,
                    },
                    span: stmt.span,
                }])
            }
        }
    }

    fn lower_expr(&mut self, expr: &ast::Expr) -> Result<Expr, CompileError> {
        match &expr.kind {
            ast::ExprKind::Identifier(name) => {
                // Check for nested overloaded/generic functions (can't be used as values)
                let has_nested_multi = self.nested_function_defs.iter().rev().any(|m| {
                    m.get(name.as_str()).is_some_and(|entries| {
                        entries.len() > 1 || !entries[0].type_params.is_empty()
                    })
                });
                if has_nested_multi {
                    return Err(CompileError::new(
                        format!(
                            "ambiguous function reference: nested function `{name}` has multiple overloads"
                        ),
                        expr.span,
                    ));
                }
                if let Some(ty) = self.lookup_var(name) {
                    Ok(Expr {
                        ty,
                        kind: ExprKind::Identifier(name.clone()),
                        span: expr.span,
                    })
                } else if self.function_defs.contains_key(name.as_str()) {
                    let entries = self.function_defs[name.as_str()].clone();
                    // Only allow function-as-value for single concrete overload
                    let concrete_count =
                        entries.iter().filter(|e| e.type_params.is_empty()).count();
                    if entries.len() > 1 || concrete_count != entries.len() {
                        return Err(CompileError::new(
                            format!(
                                "ambiguous function reference: `{name}` has multiple overloads"
                            ),
                            expr.span,
                        ));
                    }
                    let entry = &entries[0];
                    let ast_params: Vec<ast::Type> = entry
                        .ast_def
                        .parameters
                        .iter()
                        .map(|p| p.ty.clone())
                        .collect();
                    let params: Vec<Type> = ast_params
                        .iter()
                        .map(|t| self.resolve_ast_type(t))
                        .collect::<Result<Vec<_>, _>>()?;
                    let mangled = mangle_name(name, &params);
                    let return_type = Box::new(self.resolve_return_type(&mangled)?);
                    Ok(Expr {
                        ty: Type::Function {
                            params,
                            return_type,
                        },
                        kind: ExprKind::FunctionRef(mangled),
                        span: expr.span,
                    })
                } else if self.concrete_ast_defs.contains_key(name.as_str()) {
                    // Mangled function name (referenced directly)
                    let func_def = self.concrete_ast_defs.get(name.as_str()).unwrap().clone();
                    let params: Vec<Type> = func_def
                        .parameters
                        .iter()
                        .map(|p| self.resolve_ast_type(&p.ty))
                        .collect::<Result<Vec<_>, _>>()?;
                    let return_type = Box::new(self.resolve_return_type(name)?);
                    Ok(Expr {
                        ty: Type::Function {
                            params,
                            return_type,
                        },
                        kind: ExprKind::FunctionRef(name.clone()),
                        span: expr.span,
                    })
                } else {
                    Err(CompileError::new(
                        format!("undefined variable: {name}"),
                        expr.span,
                    ))
                }
            }
            ast::ExprKind::IntegerLiteral(n, int_ty) => {
                let ty = match int_ty {
                    ast::IntegerType::Int8 => Type::Int8,
                    ast::IntegerType::Int16 => Type::Int16,
                    ast::IntegerType::Int32 => Type::Int32,
                    ast::IntegerType::Int64 => Type::Int64,
                    ast::IntegerType::Int => Type::Int,
                    ast::IntegerType::Uint8 => Type::Uint8,
                    ast::IntegerType::Uint16 => Type::Uint16,
                    ast::IntegerType::Uint32 => Type::Uint32,
                    ast::IntegerType::Uint64 => Type::Uint64,
                    ast::IntegerType::Uint => Type::Uint,
                };
                let (min, max) = int_ty.bounds();
                if *n < min || *n > max {
                    return Err(CompileError::new(
                        format!("integer literal out of range for {ty} ({min}..={max})"),
                        expr.span,
                    ));
                }
                Ok(Expr {
                    ty,
                    // Stored as the 64-bit two's-complement bit pattern; unsigned
                    // consumers reinterpret as u64.
                    kind: ExprKind::IntegerLiteral(*n as i64),
                    span: expr.span,
                })
            }
            ast::ExprKind::BooleanLiteral(b) => Ok(Expr {
                ty: Type::Bool,
                kind: ExprKind::BooleanLiteral(*b),
                span: expr.span,
            }),
            ast::ExprKind::FieldAccess { object, field } => {
                let obj = self.lower_expr(object)?;
                let struct_name = match &obj.ty {
                    Type::Struct(name) => name.clone(),
                    other => {
                        return Err(CompileError::new(
                            format!("field access on non-struct type {other}"),
                            expr.span,
                        ));
                    }
                };
                let struct_def =
                    self.lowered_structs
                        .get(struct_name.as_str())
                        .ok_or_else(|| {
                            CompileError::new(format!("undefined struct: {struct_name}"), expr.span)
                        })?;
                let field_def = struct_def
                    .fields
                    .iter()
                    .find(|f| f.name == *field)
                    .ok_or_else(|| {
                        CompileError::new(
                            format!("struct {struct_name} has no field `{field}`"),
                            expr.span,
                        )
                    })?;
                self.check_field_visibility(&struct_name, field, expr.span)?;
                let ty = field_def.ty.clone();
                Ok(Expr {
                    ty,
                    kind: ExprKind::FieldAccess {
                        object: Box::new(obj),
                        field: field.clone(),
                    },
                    span: expr.span,
                })
            }
            ast::ExprKind::Deref(inner) => {
                let inner_expr = self.lower_expr(inner)?;
                // A `&?T` deref yields the same pointee place as `&T`, but the
                // null check is emitted downstream (IR interp / codegen) keyed on
                // this inner type.
                let target_ty = match &inner_expr.ty {
                    Type::Ref(t)
                    | Type::RefUnsized(t)
                    | Type::NullableRef(t)
                    | Type::NullableRefUnsized(t)
                    | Type::Unique(t)
                    | Type::UniqueUnsized(t) => (**t).clone(),
                    other => {
                        return Err(CompileError::new(
                            format!("cannot deref non-reference type {other}"),
                            expr.span,
                        ));
                    }
                };
                Ok(Expr {
                    ty: target_ty,
                    kind: ExprKind::Deref(Box::new(inner_expr)),
                    span: expr.span,
                })
            }
            ast::ExprKind::Reference(inner) => {
                let inner_expr = self.lower_expr(inner)?;
                let ty = if inner_expr.ty.is_sized(&self.lowered_structs) {
                    Type::Ref(Box::new(inner_expr.ty.clone()))
                } else {
                    Type::RefUnsized(Box::new(inner_expr.ty.clone()))
                };
                Ok(Expr {
                    ty,
                    kind: ExprKind::Reference(Box::new(inner_expr)),
                    span: expr.span,
                })
            }
            ast::ExprKind::Unique(inner) => {
                let inner_expr = self.lower_expr(inner)?;
                let ty = if inner_expr.ty.is_sized(&self.lowered_structs) {
                    Type::Unique(Box::new(inner_expr.ty.clone()))
                } else {
                    Type::UniqueUnsized(Box::new(inner_expr.ty.clone()))
                };
                Ok(Expr {
                    ty,
                    kind: ExprKind::Unique(Box::new(inner_expr)),
                    span: expr.span,
                })
            }
            ast::ExprKind::NullLiteral(inner_ty) => {
                // `null#[T]` — resolve T, then wrap as the nullable reference type.
                // `resolve_refs` picks NullableRef vs NullableRefUnsized by sizedness.
                let inner = self.resolve_ast_type(inner_ty)?;
                let ty = self.resolve_refs(Type::NullableRef(Box::new(inner)));
                Ok(Expr {
                    ty,
                    kind: ExprKind::NullLiteral,
                    span: expr.span,
                })
            }
            ast::ExprKind::EnumVariant {
                module_path: _,
                enum_name,
                type_args,
                variant_name,
            } => {
                let resolved_name = self.resolve_enum_name(enum_name, type_args)?;
                let edef = self
                    .lowered_enums
                    .get(&resolved_name)
                    .ok_or_else(|| {
                        CompileError::new(format!("undefined enum: {enum_name}"), expr.span)
                    })?
                    .clone();
                let vdef = edef
                    .variants
                    .iter()
                    .find(|v| v.name == *variant_name)
                    .ok_or_else(|| {
                        CompileError::new(
                            format!("enum {enum_name} has no variant `{variant_name}`"),
                            expr.span,
                        )
                    })?;
                if vdef.inner_type.is_some() {
                    return Err(CompileError::new(
                        format!("enum variant {enum_name}::{variant_name} requires an argument"),
                        expr.span,
                    ));
                }
                Ok(Expr {
                    ty: Type::Enum(resolved_name.clone()),
                    kind: ExprKind::EnumVariant {
                        enum_name: resolved_name,
                        variant_name: variant_name.clone(),
                        variant_index: vdef.index,
                        value: None,
                    },
                    span: expr.span,
                })
            }
            ast::ExprKind::Match { scrutinee, arms } => {
                self.lower_match(expr.span, scrutinee, arms)
            }
            ast::ExprKind::MatchReflect { ty, arms } => {
                self.lower_match_reflect(expr.span, ty, arms)
            }
            ast::ExprKind::Call {
                function,
                type_args,
                arguments,
            } => {
                // Intercept enum variant construction: EnumVariant(value)
                if let ast::ExprKind::EnumVariant {
                    module_path: _,
                    enum_name,
                    type_args: enum_type_args,
                    variant_name,
                } = &function.as_ref().kind
                {
                    let resolved_name = self.resolve_enum_name(enum_name, enum_type_args)?;
                    let edef = self
                        .lowered_enums
                        .get(&resolved_name)
                        .ok_or_else(|| {
                            CompileError::new(format!("undefined enum: {enum_name}"), expr.span)
                        })?
                        .clone();
                    let vdef = edef
                        .variants
                        .iter()
                        .find(|v| v.name == *variant_name)
                        .ok_or_else(|| {
                            CompileError::new(
                                format!("enum {enum_name} has no variant `{variant_name}`"),
                                expr.span,
                            )
                        })?;
                    let inner_ty = vdef.inner_type.clone().ok_or_else(|| {
                        CompileError::new(
                            format!(
                                "enum variant {enum_name}::{variant_name} does not take an argument"
                            ),
                            expr.span,
                        )
                    })?;
                    let variant_index = vdef.index;
                    if arguments.len() != 1 {
                        return Err(CompileError::new(
                            format!(
                                "{enum_name}::{variant_name}: expected 1 argument, got {}",
                                arguments.len()
                            ),
                            expr.span,
                        ));
                    }
                    let arg = self.lower_expr(&arguments[0])?;
                    let coerced = self.try_coerce(arg, &inner_ty);
                    if coerced.ty != inner_ty {
                        return Err(CompileError::new(
                            format!(
                                "type mismatch in {enum_name}::{variant_name}: expected {inner_ty}, got {}",
                                coerced.ty
                            ),
                            expr.span,
                        ));
                    }
                    return Ok(Expr {
                        ty: Type::Enum(resolved_name.clone()),
                        kind: ExprKind::EnumVariant {
                            enum_name: resolved_name,
                            variant_name: variant_name.clone(),
                            variant_index,
                            value: Some(Box::new(coerced)),
                        },
                        span: expr.span,
                    });
                }

                // (Intrinsic calls are handled by IntrinsicCall variant now)

                // Nested function calls (unified: generic + concrete + overloaded)
                if let ast::ExprKind::Identifier(name) = &function.as_ref().kind {
                    let nested_entries = self
                        .nested_function_defs
                        .iter()
                        .rev()
                        .find_map(|m| m.get(name.as_str()))
                        .cloned();
                    if let Some(entries) = nested_entries
                        && (entries.len() > 1 || !entries[0].type_params.is_empty())
                    {
                        // Generic or overloaded nested function — resolve at call site
                        for entry in &entries {
                            let gdef = &entry.ast_def;
                            if entry.type_params.is_empty() {
                                // Concrete nested overload
                                if gdef.parameters.len() != arguments.len() {
                                    continue;
                                }
                                let lowered_args: Vec<Expr> = arguments
                                    .iter()
                                    .map(|a| self.lower_expr(a))
                                    .collect::<Result<Vec<_>, _>>()?;

                                let param_types: Vec<Type> = gdef
                                    .parameters
                                    .iter()
                                    .map(|p| self.resolve_ast_type(&p.ty))
                                    .collect::<Result<Vec<_>, _>>()?;
                                let all_coercible = lowered_args
                                    .iter()
                                    .zip(param_types.iter())
                                    .all(|(arg, pty)| {
                                        arg.ty == *pty
                                            || self.try_coerce(arg.clone(), pty).ty == *pty
                                    });
                                if !all_coercible {
                                    continue;
                                }

                                let closure_expr = self.lower_closure(
                                    expr.span,
                                    &gdef.parameters,
                                    gdef.return_type.as_ref(),
                                    &gdef.body,
                                )?;
                                let (cparams, return_type) = match &closure_expr.ty {
                                    Type::Function {
                                        params,
                                        return_type,
                                    } => (params.clone(), (**return_type).clone()),
                                    _ => unreachable!(),
                                };
                                let coerced_args: Vec<Expr> = lowered_args
                                    .into_iter()
                                    .zip(cparams.iter())
                                    .map(|(arg, pty)| self.try_coerce(arg, pty))
                                    .collect();

                                return Ok(Expr {
                                    ty: return_type,
                                    kind: ExprKind::CallIndirect {
                                        callee: Box::new(closure_expr),
                                        arguments: coerced_args,
                                    },
                                    span: expr.span,
                                });
                            } else {
                                // Generic nested function
                                if gdef.parameters.len() != arguments.len() {
                                    continue;
                                }
                                let type_params = &entry.type_params;
                                let effective_type_args = if !type_args.is_empty() {
                                    if type_args.len() != type_params.len() {
                                        continue;
                                    }
                                    type_args.clone()
                                } else {
                                    let param_ast_types: Vec<ast::Type> =
                                        gdef.parameters.iter().map(|p| p.ty.clone()).collect();
                                    let lowered_args: Vec<Expr> = arguments
                                        .iter()
                                        .map(|a| self.lower_expr(a))
                                        .collect::<Result<Vec<_>, _>>()?;
                                    let arg_types: Vec<Type> =
                                        lowered_args.iter().map(|a| a.ty.clone()).collect();
                                    match self.infer_type_args(
                                        name,
                                        type_params,
                                        &param_ast_types,
                                        &arg_types,
                                    ) {
                                        Ok(args) => args,
                                        Err(_) => continue,
                                    }
                                };

                                let subst: HashMap<String, ast::Type> = type_params
                                    .iter()
                                    .zip(effective_type_args.iter())
                                    .map(|(p, a)| (p.clone(), a.clone()))
                                    .collect();

                                let concrete_params: Vec<ast::Parameter> = gdef
                                    .parameters
                                    .iter()
                                    .map(|p| ast::Parameter {
                                        pattern: p.pattern.clone(),
                                        ty: apply_subst_to_ast_type(&p.ty, &subst),
                                        span: p.span,
                                    })
                                    .collect();
                                let concrete_return_type = gdef
                                    .return_type
                                    .as_ref()
                                    .map(|t| apply_subst_to_ast_type(t, &subst));
                                let concrete_body: Vec<ast::Statement> = gdef
                                    .body
                                    .iter()
                                    .map(|s| apply_subst_to_ast_statement(s, &subst))
                                    .collect();

                                let closure_expr = self.lower_closure(
                                    expr.span,
                                    &concrete_params,
                                    concrete_return_type.as_ref(),
                                    &concrete_body,
                                )?;

                                let (param_types, return_type) = match &closure_expr.ty {
                                    Type::Function {
                                        params,
                                        return_type,
                                    } => (params.clone(), (**return_type).clone()),
                                    _ => unreachable!(),
                                };

                                if arguments.len() != param_types.len() {
                                    continue;
                                }

                                let mut lowered_args: Vec<Expr> = Vec::new();
                                let mut all_ok = true;
                                for (arg, pty) in arguments.iter().zip(param_types.iter()) {
                                    let lowered = self.lower_expr(arg)?;
                                    let coerced = self.try_coerce(lowered, pty);
                                    if coerced.ty != *pty {
                                        all_ok = false;
                                        break;
                                    }
                                    lowered_args.push(coerced);
                                }
                                if !all_ok {
                                    continue;
                                }

                                return Ok(Expr {
                                    ty: return_type,
                                    kind: ExprKind::CallIndirect {
                                        callee: Box::new(closure_expr),
                                        arguments: lowered_args,
                                    },
                                    span: expr.span,
                                });
                            }
                        }
                        // No nested entry matched
                        let arg_types_str = arguments
                            .iter()
                            .map(|_| "?".to_string())
                            .collect::<Vec<_>>()
                            .join(", ");
                        return Err(CompileError::new(
                            format!(
                                "no matching overload for nested function `{name}` with argument types ({arg_types_str})"
                            ),
                            expr.span,
                        ));
                    }
                }

                // Unified overload resolution: generic + concrete candidates
                if let ast::ExprKind::Identifier(name) = &function.as_ref().kind
                    && self.lookup_var(name).is_none()
                    && self.function_defs.contains_key(name.as_str())
                {
                    let entries = self.function_defs[name.as_str()].clone();
                    return self.resolve_overloaded_call(
                        entries, name, arguments, type_args, expr.span, "",
                    );
                }

                // Lower callee as a normal expression (variables shadow functions)
                let callee = self.lower_expr(function)?;

                match callee.kind {
                    ExprKind::FunctionRef(ref func_name) => {
                        // Direct call to a function-as-value (single concrete overload)
                        let func_def = self
                            .concrete_ast_defs
                            .get(func_name.as_str())
                            .unwrap()
                            .clone();
                        if arguments.len() != func_def.parameters.len() {
                            return Err(CompileError::new(
                                format!(
                                    "{func_name}: expected {} arguments, got {}",
                                    func_def.parameters.len(),
                                    arguments.len()
                                ),
                                expr.span,
                            ));
                        }
                        let params: Vec<(String, Type, ast::SourceSpan)> = func_def
                            .parameters
                            .iter()
                            .map(|p| {
                                Ok((
                                    pattern_name_or_placeholder(&p.pattern),
                                    self.resolve_ast_type(&p.ty)?,
                                    p.span,
                                ))
                            })
                            .collect::<Result<Vec<_>, CompileError>>()?;
                        let ret_ty = self.resolve_return_type(func_name)?;

                        let mut lowered_args: Vec<Expr> = Vec::new();
                        for (arg, (pname, pty, pspan)) in arguments.iter().zip(params.iter()) {
                            let lowered = if Self::has_infer_params(arg) {
                                self.lower_expr_with_expected(arg, pty)?
                            } else {
                                self.lower_expr(arg)?
                            };
                            let coerced = self.try_coerce(lowered, pty);
                            if coerced.ty != *pty {
                                return Err(CompileError::new(
                                    format!(
                                        "type mismatch in argument `{pname}` of {func_name}: expected {pty}, got {}",
                                        coerced.ty
                                    ),
                                    coerced.span,
                                ).with_label(
                                    format!("parameter `{pname}` defined here"),
                                    *pspan,
                                ));
                            }
                            lowered_args.push(coerced);
                        }

                        Ok(Expr {
                            ty: ret_ty,
                            kind: ExprKind::Call {
                                function: func_name.clone(),
                                arguments: lowered_args,
                            },
                            span: expr.span,
                        })
                    }
                    _ => {
                        // Indirect call through a function-typed expression
                        let (param_types, return_type) = match &callee.ty {
                            Type::Function {
                                params,
                                return_type,
                            } => (params.clone(), (**return_type).clone()),
                            other => {
                                return Err(CompileError::new(
                                    format!("cannot call non-function type {other}"),
                                    expr.span,
                                ));
                            }
                        };

                        if arguments.len() != param_types.len() {
                            return Err(CompileError::new(
                                format!(
                                    "indirect call: expected {} arguments, got {}",
                                    param_types.len(),
                                    arguments.len()
                                ),
                                expr.span,
                            ));
                        }

                        let mut lowered_args: Vec<Expr> = Vec::new();
                        for (i, (arg, pty)) in arguments.iter().zip(param_types.iter()).enumerate()
                        {
                            let lowered = if Self::has_infer_params(arg) {
                                self.lower_expr_with_expected(arg, pty)?
                            } else {
                                self.lower_expr(arg)?
                            };
                            let coerced = self.try_coerce(lowered, pty);
                            if coerced.ty != *pty {
                                return Err(CompileError::new(
                                    format!(
                                        "type mismatch in argument {} of indirect call: expected {pty}, got {}",
                                        i, coerced.ty
                                    ),
                                    coerced.span,
                                ));
                            }
                            lowered_args.push(coerced);
                        }

                        Ok(Expr {
                            ty: return_type,
                            kind: ExprKind::CallIndirect {
                                callee: Box::new(callee),
                                arguments: lowered_args,
                            },
                            span: expr.span,
                        })
                    }
                }
            }
            ast::ExprKind::StructLiteral {
                module: _,
                name,
                type_args,
                fields,
            } => {
                let resolved_name = self.resolve_struct_name(name, type_args)?;
                let struct_def = self
                    .lowered_structs
                    .get(&resolved_name)
                    .ok_or_else(|| {
                        CompileError::new(format!("undefined struct: {name}"), expr.span)
                    })?
                    .clone();

                let expected_fields: Vec<(String, Type)> = struct_def
                    .fields
                    .iter()
                    .map(|f| (f.name.clone(), f.ty.clone()))
                    .collect();

                let struct_ast_span = self
                    .structs
                    .get(name.as_str())
                    .map(|s| s.span)
                    .unwrap_or_default();

                for (ef_name, _) in &expected_fields {
                    if !fields.iter().any(|f| f.name == *ef_name) {
                        return Err(CompileError::new(
                            format!("missing field `{ef_name}` in {name} literal"),
                            expr.span,
                        )
                        .with_label("struct defined here", struct_ast_span));
                    }
                }
                for fi in fields {
                    if !expected_fields.iter().any(|(n, _)| *n == fi.name) {
                        return Err(CompileError::new(
                            format!("unknown field `{}` in {name} literal", fi.name),
                            fi.value.span,
                        )
                        .with_label("struct defined here", struct_ast_span));
                    }
                }

                let mut lowered_fields: Vec<FieldInit> = Vec::new();
                for fi in fields {
                    self.check_field_visibility(&resolved_name, &fi.name, expr.span)?;
                    let lowered = self.lower_expr(&fi.value)?;
                    let (_, expected_ty) =
                        expected_fields.iter().find(|(n, _)| *n == fi.name).unwrap();
                    let coerced = self.try_coerce(lowered, expected_ty);
                    if coerced.ty != *expected_ty {
                        return Err(CompileError::new(
                            format!(
                                "type mismatch in field `{}` of {name}: expected {expected_ty}, got {}",
                                fi.name, coerced.ty
                            ),
                            coerced.span,
                        ).with_label("struct defined here", struct_ast_span));
                    }
                    lowered_fields.push(FieldInit {
                        name: fi.name.clone(),
                        value: coerced,
                    });
                }

                Ok(Expr {
                    ty: Type::Struct(resolved_name.clone()),
                    kind: ExprKind::StructLiteral {
                        name: resolved_name,
                        fields: lowered_fields,
                    },
                    span: expr.span,
                })
            }
            ast::ExprKind::Index { object, index } => {
                let obj = self.lower_expr(object)?;
                let elem_ty = Self::array_inner(&obj.ty)
                    .ok_or_else(|| {
                        CompileError::new(format!("index on non-array type {}", obj.ty), expr.span)
                    })?
                    .clone();
                let idx = self.lower_expr(index)?;
                if idx.ty != Type::Uint {
                    return Err(CompileError::new(
                        format!("array index must be Uint, got {}", idx.ty),
                        expr.span,
                    ));
                }
                Ok(Expr {
                    ty: elem_ty,
                    kind: ExprKind::Index {
                        object: Box::new(obj),
                        index: Box::new(idx),
                    },
                    span: expr.span,
                })
            }
            ast::ExprKind::Slice { object, start, end } => {
                let obj = self.lower_expr(object)?;
                let elem_ty = Self::array_inner(&obj.ty)
                    .ok_or_else(|| {
                        CompileError::new(format!("slice on non-array type {}", obj.ty), expr.span)
                    })?
                    .clone();
                let start_expr = self.lower_expr(start)?;
                if start_expr.ty != Type::Uint {
                    return Err(CompileError::new(
                        format!("slice start must be Uint, got {}", start_expr.ty),
                        expr.span,
                    ));
                }
                let end_expr = self.lower_expr(end)?;
                if end_expr.ty != Type::Uint {
                    return Err(CompileError::new(
                        format!("slice end must be Uint, got {}", end_expr.ty),
                        expr.span,
                    ));
                }
                Ok(Expr {
                    ty: Type::Array(Box::new(elem_ty)),
                    kind: ExprKind::Slice {
                        object: Box::new(obj),
                        start: Box::new(start_expr),
                        end: Box::new(end_expr),
                    },
                    span: expr.span,
                })
            }
            ast::ExprKind::ArrayRepeat { element, count } => {
                let lowered_first = self.lower_expr(element)?;
                // If `count` is a closure, supply expected param type Uint so
                // `\i ...` can have its parameter inferred as Uint in array-init form.
                let lowered_second = if matches!(count.kind, ast::ExprKind::Closure { .. }) {
                    let expected = Type::Function {
                        params: vec![Type::Uint],
                        return_type: Box::new(Type::Unit),
                    };
                    self.lower_expr_with_expected(count, &expected)?
                } else {
                    self.lower_expr(count)?
                };
                // Disambiguate: if second is fn(Uint) -> T, it's ArrayInit;
                // otherwise it's ArrayRepeat with count as Uint
                if let Type::Function {
                    params,
                    return_type,
                } = &lowered_second.ty
                {
                    if !(params.len() == 1 && params[0] == Type::Uint) {
                        return Err(CompileError::new(
                            format!(
                                "array init function must take exactly one Uint parameter, got ({})",
                                params
                                    .iter()
                                    .map(|p| p.to_string())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ),
                            expr.span,
                        ));
                    }
                    if lowered_first.ty != Type::Uint {
                        return Err(CompileError::new(
                            format!("array init count must be Uint, got {}", lowered_first.ty),
                            expr.span,
                        ));
                    }
                    let elem_ty = (**return_type).clone();
                    Ok(Expr {
                        ty: Type::Array(Box::new(elem_ty)),
                        kind: ExprKind::ArrayInit {
                            count: Box::new(lowered_first),
                            init: Box::new(lowered_second),
                        },
                        span: expr.span,
                    })
                } else {
                    if lowered_second.ty != Type::Uint {
                        return Err(CompileError::new(
                            format!("array repeat count must be Uint, got {}", lowered_second.ty),
                            expr.span,
                        ));
                    }
                    let elem_ty = lowered_first.ty.clone();
                    Ok(Expr {
                        ty: Type::Array(Box::new(elem_ty)),
                        kind: ExprKind::ArrayRepeat {
                            element: Box::new(lowered_first),
                            count: Box::new(lowered_second),
                        },
                        span: expr.span,
                    })
                }
            }
            ast::ExprKind::TupleLiteral(elements) => {
                if elements.len() < 2 {
                    return Err(CompileError::new(
                        "tuple must have at least 2 elements".to_string(),
                        expr.span,
                    ));
                }
                let lowered: Vec<Expr> = elements
                    .iter()
                    .map(|e| self.lower_expr(e))
                    .collect::<Result<Vec<_>, _>>()?;
                let element_types: Vec<Type> = lowered.iter().map(|e| e.ty.clone()).collect();
                let mangled = self.ensure_tuple_struct(&element_types);
                let fields: Vec<FieldInit> = lowered
                    .into_iter()
                    .enumerate()
                    .map(|(i, expr)| FieldInit {
                        name: format!("_{i}"),
                        value: expr,
                    })
                    .collect();
                Ok(Expr {
                    ty: Type::Struct(mangled.clone()),
                    kind: ExprKind::StructLiteral {
                        name: mangled,
                        fields,
                    },
                    span: expr.span,
                })
            }
            ast::ExprKind::ArrayLiteral(elements) => {
                if elements.is_empty() {
                    return Err(CompileError::new(
                        "empty array literals are not supported".to_string(),
                        expr.span,
                    ));
                }

                let lowered: Vec<Expr> = elements
                    .iter()
                    .map(|e| self.lower_expr(e))
                    .collect::<Result<Vec<_>, _>>()?;
                let elem_ty = lowered[0].ty.clone();
                for (i, e) in lowered.iter().enumerate().skip(1) {
                    if e.ty != elem_ty {
                        return Err(CompileError::new(
                            format!(
                                "type mismatch in array element {i}: expected {elem_ty}, got {}",
                                e.ty
                            ),
                            expr.span,
                        ));
                    }
                }

                Ok(Expr {
                    ty: Type::Array(Box::new(elem_ty)),
                    kind: ExprKind::ArrayLiteral(lowered),
                    span: expr.span,
                })
            }
            ast::ExprKind::BinaryOp { op, left, right } => {
                let mut lhs = self.lower_expr(left)?;
                let mut rhs = self.lower_expr(right)?;
                // For `==`/`!=`, a non-null reference (`&T`) compares against a
                // nullable reference (`&?T`, e.g. `null#[T]`) by coercing the
                // non-null side to the nullable type first.
                if matches!(op, ast::BinOp::Eq | ast::BinOp::Ne) {
                    if lhs.ty.is_nullable_ref() && !rhs.ty.is_nullable_ref() {
                        let target = lhs.ty.clone();
                        rhs = self.try_coerce(rhs, &target);
                    } else if rhs.ty.is_nullable_ref() && !lhs.ty.is_nullable_ref() {
                        let target = rhs.ty.clone();
                        lhs = self.try_coerce(lhs, &target);
                    }
                }
                // Allow array types with same element type (Array(T) and FixedArray(T, N))
                let lhs_inner = Self::array_inner(&lhs.ty);
                let rhs_inner = Self::array_inner(&rhs.ty);
                match (lhs_inner, rhs_inner) {
                    (Some(li), Some(ri)) => {
                        if li != ri {
                            return Err(CompileError::new(
                                format!(
                                    "binary op element type mismatch: left is {}, right is {}",
                                    lhs.ty, rhs.ty
                                ),
                                expr.span,
                            ));
                        }
                    }
                    _ => {
                        if lhs.ty != rhs.ty {
                            return Err(CompileError::new(
                                format!(
                                    "binary op type mismatch: left is {}, right is {}",
                                    lhs.ty, rhs.ty
                                ),
                                expr.span,
                            ));
                        }
                    }
                }
                let result_ty = match op {
                    ast::BinOp::Add => {
                        if let Some(inner) = lhs_inner {
                            // Array concat always produces unsized Array(T)
                            Type::Array(Box::new(inner.clone()))
                        } else {
                            if !lhs.ty.is_integer() {
                                return Err(CompileError::new(
                                    format!(
                                        "arithmetic operators require integer types, got {}",
                                        lhs.ty
                                    ),
                                    expr.span,
                                ));
                            }
                            lhs.ty.clone()
                        }
                    }
                    ast::BinOp::Sub | ast::BinOp::Mul | ast::BinOp::Div | ast::BinOp::Mod => {
                        if !lhs.ty.is_integer() {
                            return Err(CompileError::new(
                                format!(
                                    "arithmetic operators require integer types, got {}",
                                    lhs.ty
                                ),
                                expr.span,
                            ));
                        }
                        lhs.ty.clone()
                    }
                    ast::BinOp::Eq | ast::BinOp::Ne => {
                        let ok = lhs.ty.is_integer()
                            || lhs.ty == Type::Bool
                            || lhs.ty.is_nullable_ref()
                            || lhs_inner
                                .is_some_and(|inner| inner.is_integer() || *inner == Type::Bool);
                        if !ok {
                            return Err(CompileError::new(
                                format!("equality operators not supported on {}", lhs.ty),
                                expr.span,
                            ));
                        }
                        Type::Bool
                    }
                    ast::BinOp::Lt | ast::BinOp::Le | ast::BinOp::Gt | ast::BinOp::Ge => {
                        if !lhs.ty.is_integer() {
                            return Err(CompileError::new(
                                format!("ordering operators require integer types, got {}", lhs.ty),
                                expr.span,
                            ));
                        }
                        Type::Bool
                    }
                    ast::BinOp::And | ast::BinOp::Or => {
                        if lhs.ty != Type::Bool {
                            return Err(CompileError::new(
                                format!("logical operators require Bool, got {}", lhs.ty),
                                expr.span,
                            ));
                        }
                        Type::Bool
                    }
                };
                Ok(Expr {
                    ty: result_ty,
                    kind: ExprKind::BinaryOp {
                        op: *op,
                        left: Box::new(lhs),
                        right: Box::new(rhs),
                    },
                    span: expr.span,
                })
            }
            ast::ExprKind::If {
                condition,
                then_body,
                else_body,
            } => {
                let lowered_cond = self.lower_expr(condition)?;
                if lowered_cond.ty != Type::Bool {
                    return Err(CompileError::new(
                        format!("if condition must be Bool, got {}", lowered_cond.ty),
                        expr.span,
                    ));
                }
                self.push_scope();
                let lowered_then: Vec<Statement> = then_body
                    .iter()
                    .map(|s| self.lower_statement(s))
                    .collect::<Result<Vec<Vec<Statement>>, CompileError>>()?
                    .into_iter()
                    .flatten()
                    .collect();
                self.pop_scope();
                self.push_scope();
                let lowered_else: Vec<Statement> = else_body
                    .iter()
                    .map(|s| self.lower_statement(s))
                    .collect::<Result<Vec<Vec<Statement>>, CompileError>>()?
                    .into_iter()
                    .flatten()
                    .collect();
                self.pop_scope();

                // Extract tail expression type from each branch
                let then_ty = lowered_then
                    .last()
                    .and_then(|s| match &s.kind {
                        StatementKind::Expression(e) => Some(e.ty.clone()),
                        _ => None,
                    })
                    .unwrap_or(Type::Unit);
                let else_ty = lowered_else
                    .last()
                    .and_then(|s| match &s.kind {
                        StatementKind::Expression(e) => Some(e.ty.clone()),
                        _ => None,
                    })
                    .unwrap_or(Type::Unit);
                if then_ty != else_ty {
                    return Err(CompileError::new(
                        format!(
                            "if expression branch type mismatch: then is {then_ty}, else is {else_ty}"
                        ),
                        expr.span,
                    ));
                }

                Ok(Expr {
                    ty: then_ty,
                    kind: ExprKind::If {
                        condition: Box::new(lowered_cond),
                        then_body: lowered_then,
                        else_body: lowered_else,
                    },
                    span: expr.span,
                })
            }
            ast::ExprKind::Block(stmts) => {
                self.push_scope();
                let lowered: Vec<Statement> = stmts
                    .iter()
                    .map(|s| self.lower_statement(s))
                    .collect::<Result<Vec<Vec<Statement>>, CompileError>>()?
                    .into_iter()
                    .flatten()
                    .collect();
                self.pop_scope();
                let ty = lowered
                    .last()
                    .and_then(|s| match &s.kind {
                        StatementKind::Expression(e) => Some(e.ty.clone()),
                        _ => None,
                    })
                    .unwrap_or(Type::Unit);
                Ok(Expr {
                    ty,
                    kind: ExprKind::Block(lowered),
                    span: expr.span,
                })
            }
            ast::ExprKind::Closure {
                parameters,
                return_type,
                body,
            } => self.lower_closure(expr.span, parameters, return_type.as_ref(), body),
            ast::ExprKind::MethodCall {
                receiver,
                method,
                type_args,
                arguments,
            } => self.lower_method_call(expr.span, receiver, method, type_args, arguments),
            ast::ExprKind::IntrinsicCall {
                intrinsic,
                arguments,
            } => self.lower_intrinsic_call(expr.span, intrinsic, arguments),
        }
    }

    /// Check if an expression is a closure with any Infer-typed parameters.
    fn has_infer_params(expr: &ast::Expr) -> bool {
        if let ast::ExprKind::Closure { parameters, .. } = &expr.kind {
            parameters.iter().any(|p| matches!(p.ty, ast::Type::Infer))
        } else {
            false
        }
    }

    /// Lower an expression with an expected type hint. This is used to propagate
    /// expected types into closures for parameter type inference.
    fn lower_expr_with_expected(
        &mut self,
        expr: &ast::Expr,
        expected: &Type,
    ) -> Result<Expr, CompileError> {
        if let ast::ExprKind::Closure {
            parameters,
            return_type,
            body,
        } = &expr.kind
        {
            self.lower_closure_with_expected(
                expr.span,
                parameters,
                return_type.as_ref(),
                body,
                Some(expected),
            )
        } else {
            self.lower_expr(expr)
        }
    }

    fn lower_match(
        &mut self,
        span: ast::SourceSpan,
        scrutinee: &ast::Expr,
        arms: &[ast::MatchArm],
    ) -> Result<Expr, CompileError> {
        let lowered_scrutinee = self.lower_expr(scrutinee)?;
        let enum_name = match &lowered_scrutinee.ty {
            Type::Enum(name) => name.clone(),
            other => {
                return Err(CompileError::new(
                    format!("match scrutinee must be an enum type, got {other}"),
                    span,
                ));
            }
        };
        let edef = self.lowered_enums[&enum_name].clone();

        let mut covered_variants: HashSet<String> = HashSet::new();
        let mut has_wildcard = false;
        let mut typed_arms = Vec::new();
        let mut result_ty: Option<Type> = None;

        for arm in arms {
            let pattern = match &arm.pattern {
                ast::Pattern::Variant {
                    module_path: _,
                    enum_name: pname,
                    type_args,
                    variant_name,
                    binding,
                } => {
                    // Resolve enum name (handles aliases and generics)
                    let resolved_pname = self.resolve_enum_name(pname, type_args)?;
                    if resolved_pname != enum_name {
                        return Err(CompileError::new(
                            format!(
                                "pattern enum name `{pname}` does not match scrutinee enum `{enum_name}`"
                            ),
                            span,
                        ));
                    }
                    let vdef = edef
                        .variants
                        .iter()
                        .find(|v| v.name == *variant_name)
                        .ok_or_else(|| {
                            CompileError::new(
                                format!("enum {enum_name} has no variant `{variant_name}`"),
                                span,
                            )
                        })?;
                    if !covered_variants.insert(variant_name.clone()) {
                        return Err(CompileError::new(
                            format!("duplicate pattern for variant {enum_name}::{variant_name}"),
                            span,
                        ));
                    }
                    let binding_typed = match (binding, &vdef.inner_type) {
                        (Some(bname), Some(ty)) => Some((bname.clone(), ty.clone())),
                        (None, None) => None,
                        (Some(_), None) => {
                            return Err(CompileError::new(
                                format!(
                                    "variant {enum_name}::{variant_name} is a unit variant, cannot bind"
                                ),
                                span,
                            ));
                        }
                        (None, Some(_)) => {
                            return Err(CompileError::new(
                                format!(
                                    "variant {enum_name}::{variant_name} has data, must provide binding"
                                ),
                                span,
                            ));
                        }
                    };
                    TypedPattern::Variant {
                        enum_name: enum_name.clone(),
                        variant_name: variant_name.clone(),
                        variant_index: vdef.index,
                        binding: binding_typed,
                    }
                }
                ast::Pattern::Wildcard(name) => {
                    if has_wildcard {
                        return Err(CompileError::new(
                            "duplicate wildcard pattern in match".to_string(),
                            span,
                        ));
                    }
                    has_wildcard = true;
                    TypedPattern::Wildcard(name.clone(), Type::Enum(enum_name.clone()))
                }
            };

            // Lower the arm body in a new scope with the binding defined
            self.push_scope();
            match &pattern {
                TypedPattern::Variant {
                    binding: Some((bname, bty)),
                    ..
                } => {
                    self.define_var(bname.clone(), bty.clone());
                }
                TypedPattern::Wildcard(name, ty) => {
                    self.define_var(name.clone(), ty.clone());
                }
                _ => {}
            }
            let lowered_body_expr = self.lower_expr(&arm.body)?;
            let body_stmts = vec![Statement {
                kind: StatementKind::Expression(lowered_body_expr),
                span: arm.body.span,
            }];
            self.pop_scope();

            let arm_ty = match &body_stmts.last().unwrap().kind {
                StatementKind::Expression(e) => e.ty.clone(),
                _ => Type::Unit,
            };
            if arm_ty == Type::Never {
                // Never-typed arms are compatible with any result type
            } else if let Some(ref expected) = result_ty {
                if arm_ty != *expected {
                    return Err(CompileError::new(
                        format!("match arm type mismatch: expected {expected}, got {arm_ty}"),
                        span,
                    ));
                }
            } else {
                result_ty = Some(arm_ty);
            }

            typed_arms.push(TypedMatchArm {
                pattern,
                body: body_stmts,
            });
        }

        // Exhaustiveness check
        if !has_wildcard {
            for vdef in &edef.variants {
                if !covered_variants.contains(&vdef.name) {
                    return Err(CompileError::new(
                        format!(
                            "non-exhaustive match: variant {enum_name}::{} not covered",
                            vdef.name
                        ),
                        span,
                    ));
                }
            }
        }

        Ok(Expr {
            ty: result_ty.unwrap_or(Type::Unit),
            kind: ExprKind::Match {
                scrutinee: Box::new(lowered_scrutinee),
                arms: typed_arms,
            },
            span,
        })
    }

    /// Compile-time reflection: classify the inspected type, pick the first matching
    /// arm, and lower only that arm's body. The match.reflect itself is erased — the
    /// typed AST contains just the taken branch, and other branches are never
    /// type-checked.
    fn lower_match_reflect(
        &mut self,
        span: ast::SourceSpan,
        ty: &ast::Type,
        arms: &[ast::ReflectArm],
    ) -> Result<Expr, CompileError> {
        let resolved = self.resolve_ast_type(ty)?;
        let kind = match &resolved {
            Type::Enum(_) => Some("enum"),
            Type::Struct(name) => {
                if !(self.structs.contains_key(name.as_str())
                    || self.lowered_structs.contains_key(name))
                {
                    return Err(CompileError::new(
                        format!("undefined type in match.reflect: {name}"),
                        span,
                    ));
                }
                Some("struct")
            }
            _ => None,
        };

        let mut selected: Option<&ast::ReflectArm> = None;
        let mut seen_kinds: HashSet<String> = HashSet::new();
        let mut has_wildcard = false;
        for arm in arms {
            match &arm.pattern {
                ast::ReflectPattern::Kind(k) => {
                    if !matches!(k.as_str(), "struct" | "enum") {
                        return Err(CompileError::new(
                            format!(
                                "unknown match.reflect kind \"{k}\" (expected \"struct\" or \"enum\")"
                            ),
                            span,
                        ));
                    }
                    if !seen_kinds.insert(k.clone()) {
                        return Err(CompileError::new(
                            format!("duplicate match.reflect arm for \"{k}\""),
                            span,
                        ));
                    }
                    if selected.is_none() && kind == Some(k.as_str()) {
                        selected = Some(arm);
                    }
                }
                ast::ReflectPattern::Wildcard => {
                    if has_wildcard {
                        return Err(CompileError::new(
                            "duplicate wildcard pattern in match.reflect".to_string(),
                            span,
                        ));
                    }
                    has_wildcard = true;
                    if selected.is_none() {
                        selected = Some(arm);
                    }
                }
            }
        }

        let Some(arm) = selected else {
            let needed = match kind {
                Some(k) => format!("\"{k}\""),
                None => "`_`".to_string(),
            };
            return Err(CompileError::new(
                format!("non-exhaustive match.reflect: no {needed} arm for type {resolved}"),
                span,
            ));
        };
        self.lower_expr(&arm.body)
    }

    /// Compile-time field iteration: unrolls the body once per field of the
    /// struct behind the `&T` object, each repetition in its own scoped block
    /// with `variable` bound to `(&[Uint8], &F)` — the field's name and a
    /// reference to its value (F differs per field). Desugared at the AST
    /// level into nested blocks, so the typed AST schema is unchanged.
    fn lower_reflect_fields(
        &mut self,
        span: ast::SourceSpan,
        pattern: &ast::DestructurePattern,
        object: &ast::Expr,
        body: &[ast::Statement],
    ) -> Result<Vec<Statement>, CompileError> {
        // Probe the object's type; the probe result is discarded — the emitted
        // code evaluates the object exactly once via the generated `let`.
        let probe = self.lower_expr(object)?;
        let struct_name = match &probe.ty {
            Type::Ref(inner) | Type::RefUnsized(inner) => match inner.as_ref() {
                Type::Struct(name) => name.clone(),
                _ => {
                    return Err(CompileError::new(
                        format!(
                            "for.reflect_fields requires &T where T is a struct, got {}",
                            probe.ty
                        ),
                        span,
                    ));
                }
            },
            _ => {
                return Err(CompileError::new(
                    format!(
                        "for.reflect_fields requires &T where T is a struct, got {}",
                        probe.ty
                    ),
                    span,
                ));
            }
        };
        let field_names: Vec<String> = self.lowered_structs[&struct_name]
            .fields
            .iter()
            .map(|f| f.name.clone())
            .collect();

        let tmp_name = format!("__reflect_fields_{}", self.destructure_counter);
        self.destructure_counter += 1;

        // { let tmp = object; { let x = ("f1"&, tmp@.f1&); body } { ... } }
        let mut outer_stmts = vec![ast::Statement {
            kind: ast::StatementKind::Let {
                pattern: ast::DestructurePattern::Name(tmp_name.clone()),
                ty: None,
                value: object.clone(),
            },
            span,
        }];
        for fname in field_names {
            let name_bytes: Vec<ast::Expr> = fname
                .bytes()
                .map(|b| ast::Expr {
                    kind: ast::ExprKind::IntegerLiteral(b as i128, ast::IntegerType::Uint8),
                    span,
                })
                .collect();
            let name_ref = ast::Expr {
                kind: ast::ExprKind::Reference(Box::new(ast::Expr {
                    kind: ast::ExprKind::ArrayLiteral(name_bytes),
                    span,
                })),
                span,
            };
            let field_ref = ast::Expr {
                kind: ast::ExprKind::Reference(Box::new(ast::Expr {
                    kind: ast::ExprKind::FieldAccess {
                        object: Box::new(ast::Expr {
                            kind: ast::ExprKind::Deref(Box::new(ast::Expr {
                                kind: ast::ExprKind::Identifier(tmp_name.clone()),
                                span,
                            })),
                            span,
                        }),
                        field: fname.clone(),
                    },
                    span,
                })),
                span,
            };
            let tuple = ast::Expr {
                kind: ast::ExprKind::TupleLiteral(vec![name_ref, field_ref]),
                span,
            };
            let mut block_stmts = vec![ast::Statement {
                kind: ast::StatementKind::Let {
                    pattern: pattern.clone(),
                    ty: None,
                    value: tuple,
                },
                span,
            }];
            block_stmts.extend(body.iter().cloned());
            outer_stmts.push(ast::Statement {
                kind: ast::StatementKind::Expression(ast::Expr {
                    kind: ast::ExprKind::Block(block_stmts),
                    span,
                }),
                span,
            });
        }

        self.lower_statement(&ast::Statement {
            kind: ast::StatementKind::Expression(ast::Expr {
                kind: ast::ExprKind::Block(outer_stmts),
                span,
            }),
            span,
        })
    }

    /// Compile-time variant dispatch: desugars into a real `match` over the
    /// enum behind the `&T` object, with the body duplicated in every arm.
    /// In data-variant arms the pattern is bound against the tuple
    /// `(&[Uint8], Payload)` — variant name and payload by value — so a bare
    /// name binds the whole tuple and `(variant, val)` destructures it. Unit
    /// variants have no payload (and unit-typed tuple elements are not
    /// supported), so their arms bind only the name part of a `(variant, val)`
    /// pattern and nothing for other pattern shapes; bodies that use the
    /// payload only compile when every variant carries data.
    fn lower_match_reflect_variant(
        &mut self,
        span: ast::SourceSpan,
        pattern: &ast::DestructurePattern,
        object: &ast::Expr,
        body: &[ast::Statement],
    ) -> Result<Vec<Statement>, CompileError> {
        // Probe the object's type; the probe result is discarded — the emitted
        // code evaluates the object exactly once via the generated `let`.
        let probe = self.lower_expr(object)?;
        let enum_name = match &probe.ty {
            Type::Ref(inner) | Type::RefUnsized(inner) => match inner.as_ref() {
                Type::Enum(name) => name.clone(),
                _ => {
                    return Err(CompileError::new(
                        format!(
                            "match.reflect_variant requires &T where T is an enum, got {}",
                            probe.ty
                        ),
                        span,
                    ));
                }
            },
            _ => {
                return Err(CompileError::new(
                    format!(
                        "match.reflect_variant requires &T where T is an enum, got {}",
                        probe.ty
                    ),
                    span,
                ));
            }
        };
        let variants: Vec<(String, bool)> = self.lowered_enums[&enum_name]
            .variants
            .iter()
            .map(|v| (v.name.clone(), v.inner_type.is_some()))
            .collect();

        let n = self.destructure_counter;
        self.destructure_counter += 1;
        let tmp_name = format!("__reflect_variant_{n}");
        let binding_name = format!("__reflect_variant_binding_{n}");

        // { let tmp = object;
        //   match tmp@ {
        //     E::Unit => { let variant = "Unit"&; body },
        //     E::Data(b) => { let variant = "Data"&; let val = b; body },
        //   }; }
        let mut arms = Vec::new();
        for (vname, has_data) in variants {
            let name_bytes: Vec<ast::Expr> = vname
                .bytes()
                .map(|b| ast::Expr {
                    kind: ast::ExprKind::IntegerLiteral(b as i128, ast::IntegerType::Uint8),
                    span,
                })
                .collect();
            let name_ref = ast::Expr {
                kind: ast::ExprKind::Reference(Box::new(ast::Expr {
                    kind: ast::ExprKind::ArrayLiteral(name_bytes),
                    span,
                })),
                span,
            };
            let mut arm_stmts = Vec::new();
            match (pattern, has_data) {
                // (variant, val) destructure: bind the parts separately so no
                // tuple value needs to be constructed
                (ast::DestructurePattern::Tuple(parts), _) if parts.len() == 2 => {
                    arm_stmts.push(ast::Statement {
                        kind: ast::StatementKind::Let {
                            pattern: parts[0].clone(),
                            ty: None,
                            value: name_ref,
                        },
                        span,
                    });
                    if has_data {
                        arm_stmts.push(ast::Statement {
                            kind: ast::StatementKind::Let {
                                pattern: parts[1].clone(),
                                ty: None,
                                value: ast::Expr {
                                    kind: ast::ExprKind::Identifier(binding_name.clone()),
                                    span,
                                },
                            },
                            span,
                        });
                    }
                }
                // any other pattern binds the (name, payload) tuple itself
                (_, true) => {
                    let tuple = ast::Expr {
                        kind: ast::ExprKind::TupleLiteral(vec![
                            name_ref,
                            ast::Expr {
                                kind: ast::ExprKind::Identifier(binding_name.clone()),
                                span,
                            },
                        ]),
                        span,
                    };
                    arm_stmts.push(ast::Statement {
                        kind: ast::StatementKind::Let {
                            pattern: pattern.clone(),
                            ty: None,
                            value: tuple,
                        },
                        span,
                    });
                }
                // unit variant: no payload, so there is no tuple to bind
                (_, false) => {}
            }
            arm_stmts.extend(body.iter().cloned());
            arms.push(ast::MatchArm {
                pattern: ast::Pattern::Variant {
                    module_path: vec![],
                    enum_name: enum_name.clone(),
                    type_args: vec![],
                    variant_name: vname,
                    binding: has_data.then(|| binding_name.clone()),
                },
                body: ast::Expr {
                    kind: ast::ExprKind::Block(arm_stmts),
                    span,
                },
            });
        }

        let match_stmt = ast::Statement {
            kind: ast::StatementKind::Expression(ast::Expr {
                kind: ast::ExprKind::Match {
                    scrutinee: Box::new(ast::Expr {
                        kind: ast::ExprKind::Deref(Box::new(ast::Expr {
                            kind: ast::ExprKind::Identifier(tmp_name.clone()),
                            span,
                        })),
                        span,
                    }),
                    arms,
                },
                span,
            }),
            span,
        };
        let outer_stmts = vec![
            ast::Statement {
                kind: ast::StatementKind::Let {
                    pattern: ast::DestructurePattern::Name(tmp_name),
                    ty: None,
                    value: object.clone(),
                },
                span,
            },
            match_stmt,
        ];
        self.lower_statement(&ast::Statement {
            kind: ast::StatementKind::Expression(ast::Expr {
                kind: ast::ExprKind::Block(outer_stmts),
                span,
            }),
            span,
        })
    }

    fn lower_closure(
        &mut self,
        span: ast::SourceSpan,
        parameters: &[ast::Parameter],
        return_type: Option<&ast::Type>,
        body: &[ast::Statement],
    ) -> Result<Expr, CompileError> {
        self.lower_closure_with_expected(span, parameters, return_type, body, None)
    }

    fn lower_closure_with_expected(
        &mut self,
        span: ast::SourceSpan,
        parameters: &[ast::Parameter],
        return_type: Option<&ast::Type>,
        body: &[ast::Statement],
        expected_type: Option<&Type>,
    ) -> Result<Expr, CompileError> {
        // Extract expected param types from expected function type
        let expected_params: Option<&Vec<Type>> = expected_type.and_then(|t| match t {
            Type::Function { params, .. } => Some(params),
            _ => None,
        });

        let synthetic_name = format!("__closure_{}", self.closure_counter);
        self.closure_counter += 1;

        // Set up capture context
        let barrier = self.scopes.depth();
        let prev_capture_context = self.capture_context.take();
        self.capture_context = Some(CaptureContext {
            scope_depth_barrier: barrier,
            captures: Vec::new(),
            captured_names: HashSet::new(),
        });

        // Push a new scope for closure params
        self.push_scope();
        self.nested_function_defs.push(HashMap::new());

        let mut typed_params: Vec<Parameter> = Vec::new();
        for (i, p) in parameters.iter().enumerate() {
            let name = pattern_name(&p.pattern).to_string();
            let ty = if matches!(p.ty, ast::Type::Infer) {
                expected_params
                    .and_then(|ps| ps.get(i))
                    .cloned()
                    .ok_or_else(|| {
                        CompileError::new(
                            format!(
                                "cannot infer type of closure parameter `{name}` without context"
                            ),
                            span,
                        )
                    })?
            } else {
                self.resolve_ast_type(&p.ty)?
            };
            if !ty.is_sized(&self.lowered_structs) {
                return Err(CompileError::new(
                    format!("closure parameter `{name}` has unsized type {}", ty),
                    span,
                ));
            }
            self.define_var(name.clone(), ty.clone());
            typed_params.push(Parameter {
                name,
                ty,
                span: p.span,
            });
        }

        let explicit_return_type = match return_type {
            Some(t) => Some(self.resolve_ast_type(t)?),
            None => None,
        };

        let prev_return_type =
            std::mem::replace(&mut self.current_return_type, explicit_return_type.clone());
        let prev_return_type_span = self.current_return_type_span.take();
        let lowered_body: Vec<Statement> = body
            .iter()
            .map(|s| self.lower_statement(s))
            .collect::<Result<Vec<Vec<Statement>>, CompileError>>()?
            .into_iter()
            .flatten()
            .collect();
        self.current_return_type = prev_return_type;
        self.current_return_type_span = prev_return_type_span;

        self.nested_function_defs.pop();
        self.pop_scope();

        // Extract captures
        let ctx = self.capture_context.take().unwrap();
        self.capture_context = prev_capture_context;
        let captures = ctx.captures;

        // Determine return type
        let fn_return_type = if let Some(rt) = explicit_return_type {
            if rt != Type::Unit {
                let last_ty = lowered_body.last().and_then(|s| match &s.kind {
                    StatementKind::Expression(expr) => Some(&expr.ty),
                    StatementKind::Return(expr) => Some(&expr.ty),
                    _ => None,
                });
                match last_ty {
                    Some(ty) => {
                        if *ty != rt {
                            return Err(CompileError::new(
                                format!("closure should return {rt}, but last expression is {ty}"),
                                span,
                            ));
                        }
                    }
                    None => {
                        return Err(CompileError::new(
                            format!(
                                "closure should return {rt}, but body does not end with an expression"
                            ),
                            span,
                        ));
                    }
                }
            }
            rt
        } else {
            lowered_body
                .last()
                .and_then(|s| match &s.kind {
                    StatementKind::Expression(expr) => Some(expr.ty.clone()),
                    StatementKind::Return(expr) => Some(expr.ty.clone()),
                    _ => None,
                })
                .unwrap_or(Type::Unit)
        };

        if !fn_return_type.is_sized(&self.lowered_structs) {
            return Err(CompileError::new(
                format!(
                    "closure: inferred return type {} is unsized",
                    fn_return_type
                ),
                span,
            ));
        }

        // Build the synthetic function.
        // Captured variables become parameters of the synthetic function:
        // the closure body already references them as Identifier(name), so we
        // just add them as leading params. At the IR level these will be wired
        // through the env.
        let synthetic_fn = FunctionDef {
            name: synthetic_name.clone(),
            parameters: typed_params.clone(),
            return_type: fn_return_type.clone(),
            body: lowered_body,
        };
        self.pending_closures.push(synthetic_fn);

        let param_types: Vec<Type> = typed_params.iter().map(|p| p.ty.clone()).collect();
        let fn_ty = Type::Function {
            params: param_types,
            return_type: Box::new(fn_return_type),
        };

        Ok(Expr {
            ty: fn_ty,
            kind: ExprKind::Closure {
                synthetic_fn: synthetic_name,
                captures,
            },
            span,
        })
    }

    /// Shared overload resolution for both function calls and method calls.
    /// `mangle_prefix` is "" for functions, "__method_" for methods.
    fn resolve_overloaded_call(
        &mut self,
        entries: Vec<FunctionEntry>,
        name: &str,
        arguments: &[ast::Expr],
        type_args: &[ast::Type],
        span: ast::SourceSpan,
        mangle_prefix: &str,
    ) -> Result<Expr, CompileError> {
        let has_infer_closures = arguments.iter().any(Self::has_infer_params);

        // Collect concrete and generic entries
        let concrete_entries: Vec<FunctionEntry> = entries
            .iter()
            .filter(|e| e.type_params.is_empty())
            .cloned()
            .collect();
        let generic_entries: Vec<FunctionEntry> = entries
            .iter()
            .filter(|e| !e.type_params.is_empty())
            .cloned()
            .collect();
        let num_generic_overloads = generic_entries.len();

        // If explicit type args provided, skip concrete entries entirely
        if !type_args.is_empty() {
            // Find matching generic overload with explicit type args
            let mut matched_gdef: Option<FunctionEntry> = None;
            for gdef in &generic_entries {
                if gdef.type_params.len() != type_args.len() {
                    continue;
                }
                if gdef.ast_def.parameters.len() != arguments.len() {
                    continue;
                }
                matched_gdef = Some(gdef.clone());
                break;
            }
            let gdef = matched_gdef.ok_or_else(|| {
                CompileError::new(
                    format!("no matching generic overload for `{name}` with {} type args and {} arguments", type_args.len(), arguments.len()),
                    span,
                )
            })?;
            let mangled = self.ensure_function_monomorphized_with_def(
                name,
                &gdef,
                type_args,
                num_generic_overloads,
                mangle_prefix,
            )?;
            let mono_fn = self.monomorphized_functions[&mangled].clone();
            if arguments.len() != mono_fn.parameters.len() {
                return Err(CompileError::new(
                    format!(
                        "{name}: expected {} arguments, got {}",
                        mono_fn.parameters.len(),
                        arguments.len()
                    ),
                    span,
                ));
            }
            let mut lowered_args: Vec<Expr> = Vec::new();
            for (arg, param) in arguments.iter().zip(mono_fn.parameters.iter()) {
                let lowered = if Self::has_infer_params(arg) {
                    self.lower_expr_with_expected(arg, &param.ty)?
                } else {
                    self.lower_expr(arg)?
                };
                let coerced = self.try_coerce(lowered, &param.ty);
                if coerced.ty != param.ty {
                    return Err(CompileError::new(
                        format!(
                            "type mismatch in argument `{}` of {name}: expected {}, got {}",
                            param.name, param.ty, coerced.ty
                        ),
                        coerced.span,
                    )
                    .with_label(
                        format!("parameter `{}` defined here", param.name),
                        param.span,
                    ));
                }
                lowered_args.push(coerced);
            }
            return Ok(Expr {
                ty: mono_fn.return_type.clone(),
                kind: ExprKind::Call {
                    function: mangled,
                    arguments: lowered_args,
                },
                span,
            });
        }

        // No explicit type args
        if !has_infer_closures {
            let lowered_args: Vec<Expr> = arguments
                .iter()
                .map(|a| self.lower_expr(a))
                .collect::<Result<Vec<_>, _>>()?;
            let arg_types: Vec<Type> = lowered_args.iter().map(|a| a.ty.clone()).collect();

            // Candidate enum for unified matching
            #[derive(Clone)]
            enum Candidate {
                Concrete(Vec<Type>, ast::FunctionDef),
                Generic(Box<FunctionEntry>, Vec<ast::Type>),
            }

            let mut candidates: Vec<(Candidate, Vec<ast::Type>)> = Vec::new();

            // Check concrete entries
            for entry in &concrete_entries {
                if entry.ast_def.parameters.len() != lowered_args.len() {
                    continue;
                }
                let param_types: Vec<Type> = entry
                    .ast_def
                    .parameters
                    .iter()
                    .map(|p| self.resolve_ast_type(&p.ty))
                    .collect::<Result<Vec<_>, _>>()?;
                let matches = lowered_args
                    .iter()
                    .zip(param_types.iter())
                    .all(|(arg, pty)| {
                        arg.ty == *pty || self.try_coerce(arg.clone(), pty).ty == *pty
                    });
                if matches {
                    let ast_types: Vec<ast::Type> = entry
                        .ast_def
                        .parameters
                        .iter()
                        .map(|p| p.ty.clone())
                        .collect();
                    candidates.push((
                        Candidate::Concrete(param_types, entry.ast_def.clone()),
                        ast_types,
                    ));
                }
            }

            // Check generic entries
            for gdef in &generic_entries {
                if gdef.ast_def.parameters.len() != lowered_args.len() {
                    continue;
                }
                let param_ast_types: Vec<ast::Type> = gdef
                    .ast_def
                    .parameters
                    .iter()
                    .map(|p| p.ty.clone())
                    .collect();
                let mut bindings: HashMap<String, ast::Type> = HashMap::new();
                let all_unified =
                    lowered_args
                        .iter()
                        .zip(param_ast_types.iter())
                        .all(|(arg, pat)| {
                            self.try_unify_type(pat, &arg.ty, &gdef.type_params, &mut bindings)
                        });
                if !all_unified {
                    continue;
                }
                let all_bound = gdef.type_params.iter().all(|tp| bindings.contains_key(tp));
                if !all_bound {
                    continue;
                }
                let inferred: Vec<ast::Type> = gdef
                    .type_params
                    .iter()
                    .map(|tp| bindings[tp].clone())
                    .collect();
                let subst: HashMap<String, ast::Type> = gdef
                    .type_params
                    .iter()
                    .zip(inferred.iter())
                    .map(|(p, a)| (p.clone(), a.clone()))
                    .collect();
                let mut params_match = true;
                for (arg, pat) in lowered_args.iter().zip(param_ast_types.iter()) {
                    let substituted = apply_subst_to_ast_type(pat, &subst);
                    if let Ok(resolved) = self.resolve_ast_type(&substituted) {
                        if resolved != arg.ty
                            && self.try_coerce(arg.clone(), &resolved).ty != resolved
                        {
                            params_match = false;
                            break;
                        }
                    } else {
                        params_match = false;
                        break;
                    }
                }
                if !params_match {
                    continue;
                }
                candidates.push((
                    Candidate::Generic(Box::new(gdef.clone()), inferred),
                    param_ast_types,
                ));
            }

            if candidates.is_empty() {
                // When there's exactly one concrete overload and no generics,
                // give a specific per-argument error message
                if concrete_entries.len() == 1 && generic_entries.is_empty() {
                    let entry = &concrete_entries[0];
                    if entry.ast_def.parameters.len() != lowered_args.len() {
                        return Err(CompileError::new(
                            format!(
                                "{name}: expected {} arguments, got {}",
                                entry.ast_def.parameters.len(),
                                lowered_args.len()
                            ),
                            span,
                        ));
                    }
                    for (arg, param) in lowered_args.iter().zip(entry.ast_def.parameters.iter()) {
                        let pty = self.resolve_ast_type(&param.ty)?;
                        let coerced = self.try_coerce(arg.clone(), &pty);
                        if coerced.ty != pty {
                            let pname = pattern_name_or_placeholder(&param.pattern);
                            return Err(CompileError::new(
                                format!(
                                    "type mismatch in argument `{pname}` of {name}: expected {pty}, got {}",
                                    coerced.ty
                                ),
                                coerced.span,
                            )
                            .with_label(
                                format!("parameter `{pname}` defined here"),
                                param.span,
                            ));
                        }
                    }
                }
                return Err(CompileError::new(
                    format!(
                        "no matching overload for `{name}` with argument types ({})",
                        arg_types
                            .iter()
                            .map(|t| t.to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    span,
                ));
            }

            // Sort by specificity — most specific first
            if candidates.len() > 1 {
                let mut indexed: Vec<(usize, Vec<String>)> = candidates
                    .iter()
                    .enumerate()
                    .map(|(i, (c, _))| {
                        let tp = match c {
                            Candidate::Concrete(..) => vec![],
                            Candidate::Generic(g, _) => g.type_params.clone(),
                        };
                        (i, tp)
                    })
                    .collect();
                indexed.sort_by(|(ai, a_tp), (bi, b_tp)| {
                    let a_types = &candidates[*ai].1;
                    let b_types = &candidates[*bi].1;
                    compare_overload_specificity(a_types, a_tp, b_types, b_tp).reverse()
                });
                let sorted: Vec<_> = indexed
                    .into_iter()
                    .map(|(i, _)| candidates[i].clone())
                    .collect();
                candidates = sorted;
            }

            let (best_candidate, _) = candidates.into_iter().next().unwrap();

            match best_candidate {
                Candidate::Concrete(param_types, ast_def) => {
                    let mangled = format!("{mangle_prefix}{}", mangle_name(name, &param_types));
                    self.ensure_concrete_lowered(&mangled, &ast_def)?;
                    let ret_ty = self.resolve_return_type(&mangled)?;
                    let coerced_args: Vec<Expr> = lowered_args
                        .into_iter()
                        .zip(param_types.iter())
                        .map(|(arg, pty)| self.try_coerce(arg, pty))
                        .collect();
                    Ok(Expr {
                        ty: ret_ty,
                        kind: ExprKind::Call {
                            function: mangled,
                            arguments: coerced_args,
                        },
                        span,
                    })
                }
                Candidate::Generic(gdef, inferred) => {
                    let mangled = self.ensure_function_monomorphized_with_def(
                        name,
                        &gdef,
                        &inferred,
                        num_generic_overloads,
                        mangle_prefix,
                    )?;
                    let mono_fn = self.monomorphized_functions[&mangled].clone();
                    let mut coerced_args: Vec<Expr> = Vec::new();
                    for (lowered, param) in lowered_args.into_iter().zip(mono_fn.parameters.iter())
                    {
                        let coerced = self.try_coerce(lowered, &param.ty);
                        if coerced.ty != param.ty {
                            return Err(CompileError::new(
                                format!(
                                    "type mismatch in argument `{}` of {name}: expected {}, got {}",
                                    param.name, param.ty, coerced.ty
                                ),
                                coerced.span,
                            )
                            .with_label(
                                format!("parameter `{}` defined here", param.name),
                                param.span,
                            ));
                        }
                        coerced_args.push(coerced);
                    }
                    Ok(Expr {
                        ty: mono_fn.return_type.clone(),
                        kind: ExprKind::Call {
                            function: mangled,
                            arguments: coerced_args,
                        },
                        span,
                    })
                }
            }
        } else {
            // Has infer closures — try each generic overload with two-pass inference
            let mut matched_result: Option<Result<Expr, CompileError>> = None;

            for gdef in &generic_entries {
                if gdef.ast_def.parameters.len() != arguments.len() {
                    continue;
                }
                let type_params = gdef.type_params.clone();
                let param_ast_types: Vec<ast::Type> = gdef
                    .ast_def
                    .parameters
                    .iter()
                    .map(|p| p.ty.clone())
                    .collect();

                // Pass 1: lower non-closure args and build partial bindings
                let mut partial_bindings: HashMap<String, ast::Type> = HashMap::new();
                let mut lowered_args: Vec<Option<Expr>> = Vec::new();
                let mut pass1_ok = true;
                for (i, arg) in arguments.iter().enumerate() {
                    if Self::has_infer_params(arg) {
                        lowered_args.push(None);
                    } else {
                        let lowered = self.lower_expr(arg)?;
                        if i < param_ast_types.len()
                            && !self.try_unify_type(
                                &param_ast_types[i],
                                &lowered.ty,
                                &type_params,
                                &mut partial_bindings,
                            )
                        {
                            pass1_ok = false;
                            break;
                        }
                        lowered_args.push(Some(lowered));
                    }
                }
                if !pass1_ok {
                    continue;
                }

                let subst: HashMap<String, ast::Type> = partial_bindings;

                // Pass 2: compute expected types for closure args and lower them
                for (i, arg) in arguments.iter().enumerate() {
                    if lowered_args[i].is_none() {
                        let expected_ast_ty = apply_subst_to_ast_type(&param_ast_types[i], &subst);
                        let expected_ty = self.resolve_ast_type(&expected_ast_ty)?;
                        let lowered = self.lower_expr_with_expected(arg, &expected_ty)?;
                        lowered_args[i] = Some(lowered);
                    }
                }

                let all_lowered: Vec<Expr> = lowered_args.into_iter().map(|a| a.unwrap()).collect();
                let arg_types: Vec<Type> = all_lowered.iter().map(|a| a.ty.clone()).collect();
                let inferred =
                    self.infer_type_args(name, &type_params, &param_ast_types, &arg_types)?;

                let mangled = self.ensure_function_monomorphized_with_def(
                    name,
                    gdef,
                    &inferred,
                    num_generic_overloads,
                    mangle_prefix,
                )?;
                let mono_fn = self.monomorphized_functions[&mangled].clone();

                let mut coerced_args: Vec<Expr> = Vec::new();
                let mut all_ok = true;
                for (lowered, param) in all_lowered.into_iter().zip(mono_fn.parameters.iter()) {
                    let coerced = self.try_coerce(lowered, &param.ty);
                    if coerced.ty != param.ty {
                        all_ok = false;
                        break;
                    }
                    coerced_args.push(coerced);
                }
                if !all_ok {
                    continue;
                }

                matched_result = Some(Ok(Expr {
                    ty: mono_fn.return_type.clone(),
                    kind: ExprKind::Call {
                        function: mangled,
                        arguments: coerced_args,
                    },
                    span,
                }));
                break;
            }

            if let Some(result) = matched_result {
                return result;
            }

            // Try concrete entries with infer closures
            for entry in &concrete_entries {
                if entry.ast_def.parameters.len() != arguments.len() {
                    continue;
                }
                let param_types: Vec<Type> = entry
                    .ast_def
                    .parameters
                    .iter()
                    .map(|p| self.resolve_ast_type(&p.ty))
                    .collect::<Result<Vec<_>, _>>()?;

                let mut lowered_args: Vec<Expr> = Vec::new();
                let mut all_ok = true;
                for (arg, pty) in arguments.iter().zip(param_types.iter()) {
                    let lowered = if Self::has_infer_params(arg) {
                        self.lower_expr_with_expected(arg, pty)?
                    } else {
                        self.lower_expr(arg)?
                    };
                    let coerced = self.try_coerce(lowered, pty);
                    if coerced.ty != *pty {
                        all_ok = false;
                        break;
                    }
                    lowered_args.push(coerced);
                }
                if !all_ok {
                    continue;
                }

                let mangled = format!("{mangle_prefix}{}", mangle_name(name, &param_types));
                self.ensure_concrete_lowered(&mangled, &entry.ast_def)?;
                let ret_ty = self.resolve_return_type(&mangled)?;
                return Ok(Expr {
                    ty: ret_ty,
                    kind: ExprKind::Call {
                        function: mangled,
                        arguments: lowered_args,
                    },
                    span,
                });
            }

            Err(CompileError::new(
                format!("no matching overload for `{name}` with given arguments"),
                span,
            ))
        }
    }

    fn lower_method_call(
        &mut self,
        span: ast::SourceSpan,
        receiver: &ast::Expr,
        method: &str,
        type_args: &[ast::Type],
        arguments: &[ast::Expr],
    ) -> Result<Expr, CompileError> {
        let entries = self.method_defs.get(method).cloned().unwrap_or_default();

        // Build combined argument list: [receiver, ...arguments]
        let mut all_arguments = vec![receiver.clone()];
        all_arguments.extend(arguments.iter().cloned());

        self.resolve_overloaded_call(
            entries,
            method,
            &all_arguments,
            type_args,
            span,
            "__method_",
        )
    }

    fn expand_destructure_pattern(
        &mut self,
        pattern: &ast::DestructurePattern,
        base_expr: Expr,
        base_ty: &Type,
        stmts: &mut Vec<Statement>,
    ) -> Result<(), CompileError> {
        match pattern {
            ast::DestructurePattern::Name(name) => {
                if name == "_" {
                    // Wildcard — skip
                    return Ok(());
                }
                self.define_var(name.clone(), base_ty.clone());
                stmts.push(Statement {
                    kind: StatementKind::Let {
                        name: name.clone(),
                        ty: base_ty.clone(),
                        value: base_expr,
                    },
                    span: ast::SourceSpan::default(),
                });
            }
            ast::DestructurePattern::Tuple(elems) => {
                let struct_name = match base_ty {
                    Type::Struct(name) => name.clone(),
                    other => {
                        return Err(CompileError::new(
                            format!("tuple destructure on non-struct type {other}"),
                            ast::SourceSpan::default(),
                        ));
                    }
                };
                let sdef = self
                    .lowered_structs
                    .get(&struct_name)
                    .ok_or_else(|| {
                        CompileError::new(
                            format!("undefined struct: {struct_name}"),
                            ast::SourceSpan::default(),
                        )
                    })?
                    .clone();
                if elems.len() != sdef.fields.len() {
                    return Err(CompileError::new(
                        format!(
                            "tuple destructure: expected {} elements, got {}",
                            sdef.fields.len(),
                            elems.len()
                        ),
                        ast::SourceSpan::default(),
                    ));
                }
                for (i, elem_pat) in elems.iter().enumerate() {
                    let field = &sdef.fields[i];
                    let field_expr = Expr {
                        ty: field.ty.clone(),
                        kind: ExprKind::FieldAccess {
                            object: Box::new(base_expr.clone()),
                            field: field.name.clone(),
                        },
                        span: ast::SourceSpan::default(),
                    };
                    self.expand_destructure_pattern(elem_pat, field_expr, &field.ty, stmts)?;
                }
            }
            ast::DestructurePattern::Struct {
                module: _,
                name,
                fields,
            } => {
                let struct_name = match base_ty {
                    Type::Struct(sname) => sname.clone(),
                    other => {
                        return Err(CompileError::new(
                            format!("struct destructure on non-struct type {other}"),
                            ast::SourceSpan::default(),
                        ));
                    }
                };
                // Validate struct name — check against known struct names and reverse_mono
                let expected_name =
                    if let Some((base_name, _)) = self.reverse_mono.get(&struct_name) {
                        base_name.clone()
                    } else {
                        struct_name.clone()
                    };
                if *name != expected_name {
                    return Err(CompileError::new(
                        format!(
                            "struct destructure: expected struct `{expected_name}`, got pattern `{name}`"
                        ),
                        ast::SourceSpan::default(),
                    ));
                }
                let sdef = self
                    .lowered_structs
                    .get(&struct_name)
                    .ok_or_else(|| {
                        CompileError::new(
                            format!("undefined struct: {struct_name}"),
                            ast::SourceSpan::default(),
                        )
                    })?
                    .clone();
                for df in fields {
                    let field = sdef
                        .fields
                        .iter()
                        .find(|f| f.name == df.field_name)
                        .ok_or_else(|| {
                            CompileError::new(
                                format!("struct {struct_name} has no field `{}`", df.field_name),
                                ast::SourceSpan::default(),
                            )
                        })?;
                    let field_expr = Expr {
                        ty: field.ty.clone(),
                        kind: ExprKind::FieldAccess {
                            object: Box::new(base_expr.clone()),
                            field: df.field_name.clone(),
                        },
                        span: ast::SourceSpan::default(),
                    };
                    self.expand_destructure_pattern(&df.pattern, field_expr, &field.ty, stmts)?;
                }
            }
            ast::DestructurePattern::Array(elems) => {
                match base_ty {
                    Type::FixedArray(inner, size) => {
                        if elems.len() as u64 != *size {
                            return Err(CompileError::new(
                                format!(
                                    "array destructure: expected {size} elements, got {}",
                                    elems.len()
                                ),
                                ast::SourceSpan::default(),
                            ));
                        }
                        let inner_ty = (**inner).clone();
                        for (i, elem_pat) in elems.iter().enumerate() {
                            let index_expr = Expr {
                                ty: inner_ty.clone(),
                                kind: ExprKind::Index {
                                    object: Box::new(base_expr.clone()),
                                    index: Box::new(Expr {
                                        ty: Type::Uint,
                                        kind: ExprKind::IntegerLiteral(i as i64),
                                        span: ast::SourceSpan::default(),
                                    }),
                                },
                                span: ast::SourceSpan::default(),
                            };
                            self.expand_destructure_pattern(
                                elem_pat, index_expr, &inner_ty, stmts,
                            )?;
                        }
                    }
                    Type::Array(inner) => {
                        let inner_ty = (**inner).clone();
                        // Emit runtime length check
                        stmts.push(Statement {
                            kind: StatementKind::Expression(Expr {
                                ty: Type::Unit,
                                kind: ExprKind::IntrinsicCall {
                                    intrinsic: ast::Intrinsic::AssertArrayLen,
                                    arguments: vec![
                                        base_expr.clone(),
                                        Expr {
                                            ty: Type::Uint,
                                            kind: ExprKind::IntegerLiteral(elems.len() as i64),
                                            span: ast::SourceSpan::default(),
                                        },
                                    ],
                                },
                                span: ast::SourceSpan::default(),
                            }),
                            span: ast::SourceSpan::default(),
                        });
                        for (i, elem_pat) in elems.iter().enumerate() {
                            let index_expr = Expr {
                                ty: inner_ty.clone(),
                                kind: ExprKind::Index {
                                    object: Box::new(base_expr.clone()),
                                    index: Box::new(Expr {
                                        ty: Type::Uint,
                                        kind: ExprKind::IntegerLiteral(i as i64),
                                        span: ast::SourceSpan::default(),
                                    }),
                                },
                                span: ast::SourceSpan::default(),
                            };
                            self.expand_destructure_pattern(
                                elem_pat, index_expr, &inner_ty, stmts,
                            )?;
                        }
                    }
                    other => {
                        return Err(CompileError::new(
                            format!("array destructure on non-array type {other}"),
                            ast::SourceSpan::default(),
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    fn lower_intrinsic_call(
        &mut self,
        span: ast::SourceSpan,
        intrinsic: &ast::Intrinsic,
        arguments: &[ast::Expr],
    ) -> Result<Expr, CompileError> {
        let name = intrinsic.name();
        let spec = intrinsic_spec(intrinsic);

        if arguments.len() != spec.params.len() {
            return Err(CompileError::new(
                format!(
                    "{name}: expected {} argument{}, got {}",
                    spec.params.len(),
                    if spec.params.len() == 1 { "" } else { "s" },
                    arguments.len()
                ),
                span,
            ));
        }

        let mut lowered_args = Vec::with_capacity(arguments.len());
        let mut ref_inner: Option<Type> = None;
        for (i, (ast_arg, param)) in arguments.iter().zip(&spec.params).enumerate() {
            let arg = self.lower_expr(ast_arg)?;
            match param {
                ParamRequirement::Exact(expected) => {
                    if arg.ty != *expected {
                        return Err(CompileError::new(
                            format!("{name}: expected {expected}, got {}", arg.ty),
                            span,
                        ));
                    }
                }
                ParamRequirement::IsArray => {
                    if !matches!(arg.ty, Type::Array(_) | Type::FixedArray(_, _)) {
                        return Err(CompileError::new(
                            format!(
                                "{name}: argument {} must be an array type, got {}",
                                i + 1,
                                arg.ty
                            ),
                            span,
                        ));
                    }
                }
                ParamRequirement::RefToAtomic => {
                    let inner_ty = match &arg.ty {
                        Type::Ref(inner) => (**inner).clone(),
                        _ => {
                            return Err(CompileError::new(
                                format!("{name}: expected &T, got {}", arg.ty),
                                span,
                            ));
                        }
                    };
                    if !is_atomic_compatible(&inner_ty, &self.lowered_structs) {
                        return Err(CompileError::new(
                            format!(
                                "{name}: type {inner_ty} is not atomic-compatible (must be sized, power of 2, <= 16 bytes, no enums or unique pointers)"
                            ),
                            span,
                        ));
                    }
                    ref_inner = Some(inner_ty);
                }
                ParamRequirement::MatchesRefInner => {
                    let expected = ref_inner.as_ref().unwrap();
                    if arg.ty != *expected {
                        return Err(CompileError::new(
                            format!(
                                "{name}: argument {} must be {expected}, got {}",
                                i + 1,
                                arg.ty
                            ),
                            span,
                        ));
                    }
                }
            }
            lowered_args.push(arg);
        }

        let return_ty = match spec.ret {
            ReturnSpec::Fixed(ty) => ty,
            ReturnSpec::RefInner => ref_inner.unwrap(),
        };

        Ok(Expr {
            ty: return_ty,
            kind: ExprKind::IntrinsicCall {
                intrinsic: intrinsic.clone(),
                arguments: lowered_args,
            },
            span,
        })
    }
}

enum ParamRequirement {
    Exact(Type),
    IsArray,
    RefToAtomic,
    MatchesRefInner,
}

enum ReturnSpec {
    Fixed(Type),
    RefInner,
}

struct IntrinsicSpec {
    params: Vec<ParamRequirement>,
    ret: ReturnSpec,
}

fn intrinsic_spec(intrinsic: &ast::Intrinsic) -> IntrinsicSpec {
    use ParamRequirement::*;
    use ReturnSpec::*;

    let byte_slice = || {
        Exact(Type::RefUnsized(Box::new(Type::Array(Box::new(
            Type::Uint8,
        )))))
    };
    let ref_u32 = || Exact(Type::Ref(Box::new(Type::Uint32)));
    let u32 = || Exact(Type::Uint32);
    let fn_unit = || {
        Exact(Type::Function {
            params: vec![],
            return_type: Box::new(Type::Unit),
        })
    };

    match intrinsic {
        ast::Intrinsic::WriteStdout => IntrinsicSpec {
            params: vec![byte_slice()],
            ret: Fixed(Type::Unit),
        },
        ast::Intrinsic::Panic => IntrinsicSpec {
            params: vec![byte_slice()],
            ret: Fixed(Type::Never),
        },
        ast::Intrinsic::ReadStdin => IntrinsicSpec {
            params: vec![byte_slice()],
            ret: Fixed(Type::Uint),
        },
        ast::Intrinsic::Cast(from_nt, to_nt) => IntrinsicSpec {
            params: vec![Exact(from_nt.into())],
            ret: Fixed(to_nt.into()),
        },
        ast::Intrinsic::ArrayLen => IntrinsicSpec {
            params: vec![IsArray],
            ret: Fixed(Type::Uint),
        },
        ast::Intrinsic::AssertArrayLen => IntrinsicSpec {
            params: vec![IsArray, Exact(Type::Uint)],
            ret: Fixed(Type::Unit),
        },
        ast::Intrinsic::ThreadSpawn => IntrinsicSpec {
            params: vec![fn_unit()],
            ret: Fixed(Type::Unit),
        },
        ast::Intrinsic::AtomicLoad => IntrinsicSpec {
            params: vec![RefToAtomic],
            ret: RefInner,
        },
        ast::Intrinsic::AtomicStore => IntrinsicSpec {
            params: vec![RefToAtomic, MatchesRefInner],
            ret: Fixed(Type::Unit),
        },
        ast::Intrinsic::AtomicExchange => IntrinsicSpec {
            params: vec![RefToAtomic, MatchesRefInner],
            ret: RefInner,
        },
        ast::Intrinsic::AtomicCompareExchange => IntrinsicSpec {
            params: vec![RefToAtomic, MatchesRefInner, MatchesRefInner],
            ret: RefInner,
        },
        ast::Intrinsic::FutexWait => IntrinsicSpec {
            params: vec![ref_u32(), u32()],
            ret: Fixed(Type::Unit),
        },
        ast::Intrinsic::FutexWake => IntrinsicSpec {
            params: vec![ref_u32(), u32()],
            ret: Fixed(Type::Unit),
        },
        ast::Intrinsic::FileOpen => IntrinsicSpec {
            params: vec![byte_slice()],
            ret: Fixed(Type::FileDesc),
        },
        ast::Intrinsic::FileClose => IntrinsicSpec {
            params: vec![Exact(Type::FileDesc)],
            ret: Fixed(Type::Unit),
        },
        ast::Intrinsic::FileStdin | ast::Intrinsic::FileStdout => IntrinsicSpec {
            params: vec![],
            ret: Fixed(Type::FileDesc),
        },
        ast::Intrinsic::FileRead => IntrinsicSpec {
            params: vec![Exact(Type::FileDesc), byte_slice()],
            ret: Fixed(Type::Uint),
        },
        ast::Intrinsic::FileWritePartial => IntrinsicSpec {
            params: vec![Exact(Type::FileDesc), byte_slice()],
            ret: Fixed(Type::Uint),
        },
    }
}

/// Returns true if a type is atomic-compatible:
/// no enums, no unique pointers, and structs only if all fields pass too.
/// Additionally requires the total size to be 1, 2, 4, 8, or 16 bytes.
fn is_atomic_compatible(ty: &Type, structs: &HashMap<String, StructDef>) -> bool {
    if !is_atomic_shape_ok(ty, structs) {
        return false;
    }
    matches!(atomic_type_size(ty, structs), Some(1 | 2 | 4 | 8 | 16))
}

fn is_atomic_shape_ok(ty: &Type, structs: &HashMap<String, StructDef>) -> bool {
    match ty {
        Type::Bool
        | Type::Int8
        | Type::Uint8
        | Type::Int16
        | Type::Uint16
        | Type::Int32
        | Type::Uint32
        | Type::Float32
        | Type::Int64
        | Type::Uint64
        | Type::Int
        | Type::Uint
        | Type::Float64
        | Type::Ref(_)
        | Type::RefUnsized(_)
        | Type::NullableRef(_)
        | Type::NullableRefUnsized(_)
        | Type::Function { .. } => true,
        Type::Struct(name) => {
            if let Some(def) = structs.get(name) {
                def.fields
                    .iter()
                    .all(|f| is_atomic_shape_ok(&f.ty, structs))
            } else {
                false
            }
        }
        // `FileDesc` is excluded: an atomic store could bypass the GC write
        // barrier and let a still-referenced file be closed mid-mark.
        Type::FileDesc
        | Type::Unique(_)
        | Type::UniqueUnsized(_)
        | Type::Enum(_)
        | Type::Array(_)
        | Type::FixedArray(_, _)
        | Type::Unit
        | Type::Never => false,
    }
}

fn atomic_type_size(ty: &Type, structs: &HashMap<String, StructDef>) -> Option<usize> {
    match ty {
        Type::Bool | Type::Int8 | Type::Uint8 => Some(1),
        Type::Int16 | Type::Uint16 => Some(2),
        Type::Int32 | Type::Uint32 | Type::Float32 => Some(4),
        Type::Int64
        | Type::Uint64
        | Type::Int
        | Type::Uint
        | Type::Float64
        | Type::Ref(_)
        | Type::NullableRef(_)
        | Type::Unique(_) => Some(8),
        Type::RefUnsized(_)
        | Type::NullableRefUnsized(_)
        | Type::UniqueUnsized(_)
        | Type::Function { .. } => Some(16),
        Type::Struct(name) => {
            let def = structs.get(name)?;
            let mut size = 0usize;
            let mut struct_align = 1usize;
            for f in &def.fields {
                let fs = atomic_type_size(&f.ty, structs)?;
                let fa = atomic_type_align(&f.ty, structs)?;
                size = (size + fa - 1) & !(fa - 1);
                size += fs;
                struct_align = struct_align.max(fa);
            }
            size = (size + struct_align - 1) & !(struct_align - 1);
            Some(size)
        }
        _ => None,
    }
}

fn atomic_type_align(ty: &Type, structs: &HashMap<String, StructDef>) -> Option<usize> {
    match ty {
        Type::Bool | Type::Int8 | Type::Uint8 => Some(1),
        Type::Int16 | Type::Uint16 => Some(2),
        Type::Int32 | Type::Uint32 | Type::Float32 => Some(4),
        Type::Int64
        | Type::Uint64
        | Type::Int
        | Type::Uint
        | Type::Float64
        | Type::Ref(_)
        | Type::NullableRef(_)
        | Type::Unique(_) => Some(8),
        Type::RefUnsized(_)
        | Type::NullableRefUnsized(_)
        | Type::UniqueUnsized(_)
        | Type::Function { .. } => Some(16),
        Type::Struct(name) => {
            let def = structs.get(name)?;
            let mut a = 1usize;
            for f in &def.fields {
                a = a.max(atomic_type_align(&f.ty, structs)?);
            }
            Some(a)
        }
        _ => None,
    }
}

pub fn lower(source: &ast::SourceFile) -> Result<SourceFile, CompileError> {
    let mut lowerer = Lowerer::new(source)?;
    lowerer.lower_all()
}
