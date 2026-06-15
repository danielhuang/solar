use crate::ast::Intrinsic;
use crate::scope::ScopeStack;
use crate::typed_ast::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{Read, Write};

use crate::interp_io::{FileTable, STDIN, STDOUT};
use std::rc::Rc;

type Slot = Rc<RefCell<Value>>;

#[derive(Debug)]
enum Value {
    Int(i64),
    Struct {
        name: String,
        fields: HashMap<String, Slot>,
    },
    Array(Vec<Slot>),
    Ref(Slot),
    Unique(Slot),
    Enum {
        enum_name: String,
        variant_name: String,
        variant_index: usize,
        value: Option<Slot>,
    },
    Function {
        name: String,
        captures: Vec<(String, Slot)>,
    },
    /// A null nullable reference (`null#[T]`).
    Null,
    Unit,
}

fn is_float(ty: &Type) -> bool {
    matches!(ty, Type::Float32 | Type::Float64)
}

fn cast_numeric_ast(raw: u64, src: &Type, dst: &Type) -> u64 {
    match (is_float(src), is_float(dst)) {
        (false, false) => raw,
        (false, true) => {
            let ival = raw as i64;
            match dst {
                Type::Float32 => (ival as f32).to_bits() as u64,
                Type::Float64 => (ival as f64).to_bits(),
                _ => unreachable!(),
            }
        }
        (true, false) => match src {
            Type::Float32 => {
                let f = f32::from_bits(raw as u32);
                (f as i64) as u64
            }
            Type::Float64 => {
                let f = f64::from_bits(raw);
                (f as i64) as u64
            }
            _ => unreachable!(),
        },
        (true, true) => match (src, dst) {
            (Type::Float32, Type::Float64) => {
                let f = f32::from_bits(raw as u32);
                (f as f64).to_bits()
            }
            (Type::Float64, Type::Float32) => {
                let f = f64::from_bits(raw);
                (f as f32).to_bits() as u64
            }
            _ => raw,
        },
    }
}

fn deep_copy_value(val: &Value) -> Value {
    match val {
        Value::Int(n) => Value::Int(*n),
        Value::Struct { name, fields } => {
            let new_fields = fields
                .iter()
                .map(|(k, slot)| {
                    let copied = deep_copy_value(&slot.borrow());
                    (k.clone(), Rc::new(RefCell::new(copied)))
                })
                .collect();
            Value::Struct {
                name: name.clone(),
                fields: new_fields,
            }
        }
        Value::Array(elements) => {
            let new_elements = elements
                .iter()
                .map(|slot| {
                    let copied = deep_copy_value(&slot.borrow());
                    Rc::new(RefCell::new(copied))
                })
                .collect();
            Value::Array(new_elements)
        }
        Value::Enum {
            enum_name,
            variant_name,
            variant_index,
            value,
        } => Value::Enum {
            enum_name: enum_name.clone(),
            variant_name: variant_name.clone(),
            variant_index: *variant_index,
            value: value.as_ref().map(|slot| {
                let copied = deep_copy_value(&slot.borrow());
                Rc::new(RefCell::new(copied))
            }),
        },
        Value::Ref(target) => Value::Ref(Rc::clone(target)),
        Value::Unique(target) => {
            Value::Unique(Rc::new(RefCell::new(deep_copy_value(&target.borrow()))))
        }
        Value::Function { name, captures } => Value::Function {
            name: name.clone(),
            captures: captures
                .iter()
                .map(|(n, s)| (n.clone(), Rc::clone(s)))
                .collect(),
        },
        Value::Null => Value::Null,
        Value::Unit => Value::Unit,
    }
}

/// Equality check for atomic-compatible values: scalars (Int), refs (slot pointer
/// identity), function values (name + capture-slot identity), and structs of the same
/// (recursively atomic-compatible kinds). Mirrors raw byte equality, since these are
/// the only kinds is_atomic_compatible permits.
fn atomic_value_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Ref(x), Value::Ref(y)) => Rc::ptr_eq(x, y),
        // Nullable references: a null compares equal only to another null.
        (Value::Null, Value::Null) => true,
        (Value::Null, Value::Ref(_)) | (Value::Ref(_), Value::Null) => false,
        (
            Value::Function {
                name: na,
                captures: ca,
            },
            Value::Function {
                name: nb,
                captures: cb,
            },
        ) => {
            na == nb
                && ca.len() == cb.len()
                && ca
                    .iter()
                    .zip(cb.iter())
                    .all(|((an, av), (bn, bv))| an == bn && Rc::ptr_eq(av, bv))
        }
        (
            Value::Struct {
                name: na,
                fields: fa,
            },
            Value::Struct {
                name: nb,
                fields: fb,
            },
        ) => {
            na == nb
                && fa.len() == fb.len()
                && fa.iter().all(|(k, va)| match fb.get(k) {
                    Some(vb) => atomic_value_eq(&va.borrow(), &vb.borrow()),
                    None => false,
                })
        }
        _ => unreachable!("atomic_value_eq: non-atomic-compatible value kinds"),
    }
}

/// Recursively update dst slot in-place so that existing Rc references into
/// sub-fields/elements/variants continue to see updated values.
/// For structs: update each field slot. For arrays: update each element slot.
/// For same-variant enums: update the inner value slot.
/// For everything else (including different-variant enums): replace the whole value.
fn assign_value_in_place(dst: &Slot, src: Value) {
    let pairs: Option<Vec<(Slot, Value)>> = {
        let d = dst.borrow();
        match (&*d, &src) {
            (Value::Struct { fields: old_f, .. }, Value::Struct { fields: new_f, .. }) => Some(
                old_f
                    .iter()
                    .map(|(name, slot)| {
                        (
                            Rc::clone(slot),
                            deep_copy_value(&new_f[name.as_str()].borrow()),
                        )
                    })
                    .collect(),
            ),
            (Value::Array(old_e), Value::Array(new_e)) => {
                assert_eq!(
                    old_e.len(),
                    new_e.len(),
                    "unsized assignment: length mismatch ({} vs {})",
                    old_e.len(),
                    new_e.len()
                );
                Some(
                    old_e
                        .iter()
                        .zip(new_e.iter())
                        .map(|(o, n)| (Rc::clone(o), deep_copy_value(&n.borrow())))
                        .collect(),
                )
            }
            (
                Value::Enum {
                    variant_index: old_idx,
                    value: Some(old_inner),
                    ..
                },
                Value::Enum {
                    variant_index: new_idx,
                    value: Some(new_inner),
                    ..
                },
            ) if old_idx == new_idx => Some(vec![(
                Rc::clone(old_inner),
                deep_copy_value(&new_inner.borrow()),
            )]),
            _ => None,
        }
    };

    if let Some(pairs) = pairs {
        for (slot, val) in pairs {
            assign_value_in_place(&slot, val);
        }
    } else {
        *dst.borrow_mut() = src;
    }
}

struct Interpreter<'a, 'io> {
    functions: HashMap<String, &'a FunctionDef>,
    scopes: ScopeStack<Slot>,
    files: FileTable<'io>,
}

impl<'a, 'io> Interpreter<'a, 'io> {
    fn new(source: &'a SourceFile, stdin: impl Read + 'io, stdout: impl Write + 'io) -> Self {
        let functions = source
            .functions
            .iter()
            .map(|(name, def)| (name.clone(), def))
            .collect();
        Interpreter {
            functions,
            scopes: ScopeStack::default(),
            files: FileTable::new(stdin, stdout),
        }
    }

    fn push_scope(&mut self) {
        self.scopes.push();
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    fn define_var(&mut self, name: String, slot: Slot) {
        self.scopes.define(name, slot);
    }

    fn lookup_var(&self, name: &str) -> Slot {
        Rc::clone(
            self.scopes
                .lookup(name)
                .unwrap_or_else(|| panic!("undefined variable: {name}")),
        )
    }

    fn eval_place(&mut self, expr: &Expr) -> Slot {
        match &expr.kind {
            ExprKind::Identifier(name) => self.lookup_var(name),
            ExprKind::FieldAccess { object, field } => {
                let obj_slot = self.eval_place(object);
                let obj_ref = obj_slot.borrow();
                match &*obj_ref {
                    Value::Struct { fields, .. } => Rc::clone(&fields[field.as_str()]),
                    _ => unreachable!("type checker guarantees struct"),
                }
            }
            ExprKind::Deref(inner) => {
                let inner_slot = self.eval_place(inner);
                let inner_ref = inner_slot.borrow();
                match &*inner_ref {
                    Value::Ref(target) | Value::Unique(target) => Rc::clone(target),
                    Value::Null => panic!("null pointer dereference"),
                    _ => unreachable!("type checker guarantees ref/unique"),
                }
            }
            ExprKind::Index { object, index } => {
                let arr_slot = self.eval_place(object);
                let idx_val = self.eval_expr(index);
                let idx = match idx_val {
                    Value::Int(n) => n as usize,
                    _ => unreachable!("type checker guarantees integer index"),
                };
                let arr_ref = arr_slot.borrow();
                match &*arr_ref {
                    Value::Array(elements) => Rc::clone(&elements[idx]),
                    _ => unreachable!("type checker guarantees array"),
                }
            }
            ExprKind::Slice { object, start, end } => {
                let arr_slot = self.eval_place(object);
                let s = match self.eval_expr(start) {
                    Value::Int(n) => n as usize,
                    _ => unreachable!(),
                };
                let e = match self.eval_expr(end) {
                    Value::Int(n) => n as usize,
                    _ => unreachable!(),
                };
                let arr_ref = arr_slot.borrow();
                let elements = match &*arr_ref {
                    Value::Array(elements) => elements,
                    _ => unreachable!("type checker guarantees array"),
                };
                let len = elements.len();
                assert!(s <= e, "slice start ({s}) > end ({e})");
                assert!(e <= len, "slice end ({e}) > length ({len})");
                let sub_slots: Vec<Slot> = elements[s..e].to_vec();
                Rc::new(RefCell::new(Value::Array(sub_slots)))
            }
            ExprKind::If {
                condition,
                then_body,
                else_body,
            } => {
                let cond = self.eval_expr(condition);
                let branch = match cond {
                    Value::Int(n) if n != 0 => then_body,
                    _ => else_body,
                };
                self.push_scope();
                let result = self.exec_body_place(branch);
                self.pop_scope();
                result
            }
            ExprKind::Match { scrutinee, arms } => {
                let enum_slot = self.eval_place(scrutinee);
                let disc = {
                    let val = enum_slot.borrow();
                    match &*val {
                        Value::Enum { variant_index, .. } => *variant_index,
                        _ => unreachable!("match on non-enum value"),
                    }
                };
                for arm in arms {
                    let matches = match &arm.pattern {
                        TypedPattern::Variant { variant_index, .. } => disc == *variant_index,
                        TypedPattern::Wildcard(_, _) => true,
                    };
                    if matches {
                        self.push_scope();
                        match &arm.pattern {
                            TypedPattern::Variant {
                                binding: Some((bname, _)),
                                ..
                            } => {
                                let inner_slot = {
                                    let val = enum_slot.borrow();
                                    match &*val {
                                        Value::Enum {
                                            value: Some(slot), ..
                                        } => Rc::clone(slot),
                                        _ => unreachable!(),
                                    }
                                };
                                self.define_var(bname.clone(), inner_slot);
                            }
                            TypedPattern::Wildcard(name, _) => {
                                self.define_var(name.clone(), Rc::clone(&enum_slot));
                            }
                            _ => {}
                        }
                        let result = self.exec_body_place(&arm.body);
                        self.pop_scope();
                        return result;
                    }
                }
                unreachable!("no matching arm in match expression");
            }
            _ => {
                let val = self.eval_expr(expr);
                Rc::new(RefCell::new(val))
            }
        }
    }

    fn exec_body_place(&mut self, body: &[Statement]) -> Slot {
        let (init, tail) = body.split_at(body.len() - 1);
        for stmt in init {
            self.exec_statement(stmt);
        }
        match &tail[0].kind {
            StatementKind::Expression(expr) => self.eval_place(expr),
            _ => unreachable!(),
        }
    }

    fn eval_expr(&mut self, expr: &Expr) -> Value {
        match &expr.kind {
            ExprKind::Identifier(_)
            | ExprKind::FieldAccess { .. }
            | ExprKind::Deref(_)
            | ExprKind::Index { .. }
            | ExprKind::Slice { .. } => {
                let slot = self.eval_place(expr);
                let val = slot.borrow();
                deep_copy_value(&val)
            }
            ExprKind::IntegerLiteral(n) => Value::Int(*n),
            ExprKind::BooleanLiteral(b) => Value::Int(if *b { 1 } else { 0 }),
            ExprKind::NullLiteral => Value::Null,
            ExprKind::Reference(inner) => {
                let slot = self.eval_place(inner);
                Value::Ref(slot)
            }
            ExprKind::Unique(inner) => {
                let val = self.eval_expr(inner);
                Value::Unique(Rc::new(RefCell::new(val)))
            }
            ExprKind::StructLiteral { name, fields } => {
                let mut field_map = HashMap::new();
                for fi in fields {
                    let val = self.eval_expr(&fi.value);
                    field_map.insert(fi.name.clone(), Rc::new(RefCell::new(val)));
                }
                Value::Struct {
                    name: name.clone(),
                    fields: field_map,
                }
            }
            ExprKind::ArrayLiteral(elements) => {
                let slots = elements
                    .iter()
                    .map(|e| {
                        let val = self.eval_expr(e);
                        Rc::new(RefCell::new(val))
                    })
                    .collect();
                Value::Array(slots)
            }
            ExprKind::ArrayRepeat { element, count } => {
                let elem_val = self.eval_expr(element);
                let n = match self.eval_expr(count) {
                    Value::Int(n) => n as usize,
                    _ => unreachable!(),
                };
                let mut slots = Vec::with_capacity(n);
                for _ in 0..n {
                    slots.push(Rc::new(RefCell::new(deep_copy_value(&elem_val))));
                }
                Value::Array(slots)
            }
            ExprKind::ArrayInit { count, init } => {
                let n = match self.eval_expr(count) {
                    Value::Int(n) => n as usize,
                    _ => unreachable!(),
                };
                let init_val = self.eval_expr(init);
                let (func_name, captured_slots) = match init_val {
                    Value::Function { name, captures } => (name, captures),
                    _ => unreachable!("array init must be a function"),
                };
                let func_def = *self
                    .functions
                    .get(func_name.as_str())
                    .unwrap_or_else(|| panic!("undefined function: {func_name}"));
                let mut slots = Vec::with_capacity(n);
                for i in 0..n {
                    self.push_scope();
                    for (name, slot) in &captured_slots {
                        self.define_var(name.clone(), Rc::clone(slot));
                    }
                    self.define_var(
                        func_def.parameters[0].name.clone(),
                        Rc::new(RefCell::new(Value::Int(i as i64))),
                    );
                    let result = self.exec_function_body(&func_def.body, &func_def.return_type);
                    self.pop_scope();
                    slots.push(Rc::new(RefCell::new(result)));
                }
                Value::Array(slots)
            }
            ExprKind::ArraySizeCoerce { expr, size } => {
                let val = self.eval_expr(expr);
                match &val {
                    Value::Array(elements) => {
                        assert!(
                            elements.len() == *size as usize,
                            "array size coercion failed: expected {} elements, got {}",
                            size,
                            elements.len()
                        );
                    }
                    _ => unreachable!("ArraySizeCoerce on non-array value"),
                }
                val
            }
            ExprKind::BinaryOp { op, left, right } => {
                use crate::ast::BinOp;
                match op {
                    BinOp::And => {
                        let lv = self.eval_expr(left);
                        match lv {
                            Value::Int(0) => Value::Int(0),
                            _ => self.eval_expr(right),
                        }
                    }
                    BinOp::Or => {
                        let lv = self.eval_expr(left);
                        match lv {
                            Value::Int(0) => self.eval_expr(right),
                            _ => lv,
                        }
                    }
                    _ => {
                        let unsigned = left.ty.is_unsigned();
                        let lv = self.eval_expr(left);
                        let rv = self.eval_expr(right);
                        match (&lv, &rv) {
                            (Value::Int(a), Value::Int(b)) if unsigned => {
                                // Unsigned operands are stored as u64 bit patterns in i64
                                let a = *a as u64;
                                let b = *b as u64;
                                let int = |v: u64| Value::Int(v as i64);
                                match op {
                                    BinOp::Add => int(a
                                        .checked_add(b)
                                        .unwrap_or_else(|| panic!("integer overflow"))),
                                    BinOp::Sub => int(a
                                        .checked_sub(b)
                                        .unwrap_or_else(|| panic!("integer overflow"))),
                                    BinOp::Mul => int(a
                                        .checked_mul(b)
                                        .unwrap_or_else(|| panic!("integer overflow"))),
                                    BinOp::Div => int(a
                                        .checked_div(b)
                                        .unwrap_or_else(|| panic!("division by zero"))),
                                    BinOp::Mod => int(a
                                        .checked_rem(b)
                                        .unwrap_or_else(|| panic!("division by zero"))),
                                    BinOp::Eq => Value::Int((a == b) as i64),
                                    BinOp::Ne => Value::Int((a != b) as i64),
                                    BinOp::Lt => Value::Int((a < b) as i64),
                                    BinOp::Le => Value::Int((a <= b) as i64),
                                    BinOp::Gt => Value::Int((a > b) as i64),
                                    BinOp::Ge => Value::Int((a >= b) as i64),
                                    BinOp::And | BinOp::Or => unreachable!(),
                                }
                            }
                            (Value::Int(a), Value::Int(b)) => {
                                let a = *a;
                                let b = *b;
                                match op {
                                    BinOp::Add => Value::Int(
                                        a.checked_add(b)
                                            .unwrap_or_else(|| panic!("integer overflow")),
                                    ),
                                    BinOp::Sub => Value::Int(
                                        a.checked_sub(b)
                                            .unwrap_or_else(|| panic!("integer overflow")),
                                    ),
                                    BinOp::Mul => Value::Int(
                                        a.checked_mul(b)
                                            .unwrap_or_else(|| panic!("integer overflow")),
                                    ),
                                    BinOp::Div => Value::Int(
                                        a.checked_div(b)
                                            .unwrap_or_else(|| panic!("division by zero")),
                                    ),
                                    BinOp::Mod => Value::Int(
                                        a.checked_rem(b)
                                            .unwrap_or_else(|| panic!("division by zero")),
                                    ),
                                    BinOp::Eq => Value::Int(if a == b { 1 } else { 0 }),
                                    BinOp::Ne => Value::Int(if a != b { 1 } else { 0 }),
                                    BinOp::Lt => Value::Int(if a < b { 1 } else { 0 }),
                                    BinOp::Le => Value::Int(if a <= b { 1 } else { 0 }),
                                    BinOp::Gt => Value::Int(if a > b { 1 } else { 0 }),
                                    BinOp::Ge => Value::Int(if a >= b { 1 } else { 0 }),
                                    BinOp::And | BinOp::Or => unreachable!(),
                                }
                            }
                            (Value::Array(a_elems), Value::Array(b_elems)) => match op {
                                BinOp::Add => {
                                    let mut combined = Vec::new();
                                    for e in a_elems.iter().chain(b_elems.iter()) {
                                        combined.push(Rc::new(RefCell::new(deep_copy_value(
                                            &e.borrow(),
                                        ))));
                                    }
                                    Value::Array(combined)
                                }
                                BinOp::Eq | BinOp::Ne => {
                                    let equal = a_elems.len() == b_elems.len()
                                        && a_elems.iter().zip(b_elems.iter()).all(|(a, b)| match (
                                            &*a.borrow(),
                                            &*b.borrow(),
                                        ) {
                                            (Value::Int(x), Value::Int(y)) => x == y,
                                            _ => unreachable!(),
                                        });
                                    match op {
                                        BinOp::Eq => Value::Int(if equal { 1 } else { 0 }),
                                        BinOp::Ne => Value::Int(if !equal { 1 } else { 0 }),
                                        _ => unreachable!(),
                                    }
                                }
                                _ => unreachable!(),
                            },
                            // Nullable-reference equality (`ref == null#[T]`, etc.):
                            // pointer identity, with null distinct from any live ref.
                            _ if matches!(op, BinOp::Eq | BinOp::Ne) => {
                                let equal = atomic_value_eq(&lv, &rv);
                                match op {
                                    BinOp::Eq => Value::Int(if equal { 1 } else { 0 }),
                                    BinOp::Ne => Value::Int(if !equal { 1 } else { 0 }),
                                    _ => unreachable!(),
                                }
                            }
                            _ => unreachable!(),
                        }
                    }
                }
            }
            ExprKind::If {
                condition,
                then_body,
                else_body,
            } => {
                let cond = self.eval_expr(condition);
                let branch = match cond {
                    Value::Int(n) if n != 0 => then_body,
                    _ => else_body,
                };
                self.push_scope();
                let result = self.exec_function_body(branch, &expr.ty);
                self.pop_scope();
                result
            }
            ExprKind::Block(stmts) => {
                self.push_scope();
                let result = self.exec_function_body(stmts, &expr.ty);
                self.pop_scope();
                result
            }
            ExprKind::EnumVariant {
                enum_name,
                variant_name,
                variant_index,
                value,
            } => {
                let val_slot = value.as_ref().map(|v| {
                    let val = self.eval_expr(v);
                    Rc::new(RefCell::new(val))
                });
                Value::Enum {
                    enum_name: enum_name.clone(),
                    variant_name: variant_name.clone(),
                    variant_index: *variant_index,
                    value: val_slot,
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                // Get the scrutinee as a place (shared slot)
                let enum_slot = self.eval_place(scrutinee);
                let disc = {
                    let val = enum_slot.borrow();
                    match &*val {
                        Value::Enum { variant_index, .. } => *variant_index,
                        _ => unreachable!("match on non-enum value"),
                    }
                };
                for arm in arms {
                    let matches = match &arm.pattern {
                        TypedPattern::Variant { variant_index, .. } => disc == *variant_index,
                        TypedPattern::Wildcard(_, _) => true,
                    };
                    if matches {
                        self.push_scope();
                        match &arm.pattern {
                            TypedPattern::Variant {
                                binding: Some((bname, _)),
                                ..
                            } => {
                                // Bind inner value slot directly (place semantics)
                                let inner_slot = {
                                    let val = enum_slot.borrow();
                                    match &*val {
                                        Value::Enum {
                                            value: Some(slot), ..
                                        } => Rc::clone(slot),
                                        _ => unreachable!(),
                                    }
                                };
                                self.define_var(bname.clone(), inner_slot);
                            }
                            TypedPattern::Wildcard(name, _) => {
                                // Bind the entire enum slot
                                self.define_var(name.clone(), Rc::clone(&enum_slot));
                            }
                            _ => {}
                        }
                        let result = self.exec_function_body(&arm.body, &expr.ty);
                        self.pop_scope();
                        return result;
                    }
                }
                unreachable!("no matching arm in match expression");
            }
            ExprKind::FunctionRef(name) => Value::Function {
                name: name.clone(),
                captures: Vec::new(),
            },
            ExprKind::Call {
                function,
                arguments,
            } => {
                let func_def = *self
                    .functions
                    .get(function.as_str())
                    .unwrap_or_else(|| panic!("undefined function: {function}"));

                let arg_values: Vec<Value> = arguments.iter().map(|a| self.eval_expr(a)).collect();

                self.push_scope();
                for (param, val) in func_def.parameters.iter().zip(arg_values) {
                    self.define_var(param.name.clone(), Rc::new(RefCell::new(val)));
                }

                let result = self.exec_function_body(&func_def.body, &func_def.return_type);
                self.pop_scope();
                result
            }
            ExprKind::IntrinsicCall {
                intrinsic,
                arguments,
            } => self.exec_intrinsic(intrinsic, arguments, &expr.ty),
            ExprKind::Closure {
                synthetic_fn,
                captures,
            } => {
                let captured_slots: Vec<(String, Slot)> = captures
                    .iter()
                    .map(|cap| {
                        let slot = self.lookup_var(&cap.name);
                        (cap.name.clone(), slot)
                    })
                    .collect();
                Value::Function {
                    name: synthetic_fn.clone(),
                    captures: captured_slots,
                }
            }
            ExprKind::CallIndirect { callee, arguments } => {
                let callee_val = self.eval_expr(callee);
                let (func_name, captured_slots) = match callee_val {
                    Value::Function { name, captures } => (name, captures),
                    _ => unreachable!("type checker guarantees function"),
                };

                let func_def = *self
                    .functions
                    .get(func_name.as_str())
                    .unwrap_or_else(|| panic!("undefined function: {func_name}"));

                let arg_values: Vec<Value> = arguments.iter().map(|a| self.eval_expr(a)).collect();

                self.push_scope();
                // Define captured variables first (shared slots from enclosing scope)
                for (name, slot) in &captured_slots {
                    self.define_var(name.clone(), Rc::clone(slot));
                }
                for (param, val) in func_def.parameters.iter().zip(arg_values) {
                    self.define_var(param.name.clone(), Rc::new(RefCell::new(val)));
                }

                let result = self.exec_function_body(&func_def.body, &func_def.return_type);
                self.pop_scope();
                result
            }
        }
    }

    fn exec_intrinsic(
        &mut self,
        intrinsic: &Intrinsic,
        arguments: &[Expr],
        result_ty: &Type,
    ) -> Value {
        match intrinsic {
            Intrinsic::WriteStdout => {
                let val = self.eval_expr(&arguments[0]);
                match &val {
                    Value::Ref(slot) | Value::Unique(slot) => {
                        let inner = slot.borrow();
                        match &*inner {
                            Value::Array(elements) => {
                                let bytes: Vec<u8> = elements
                                    .iter()
                                    .map(|s| {
                                        let v = s.borrow();
                                        match &*v {
                                            Value::Int(n) => *n as u8,
                                            _ => unreachable!(),
                                        }
                                    })
                                    .collect();
                                self.files.write_all(STDOUT, &bytes);
                            }
                            _ => unreachable!(),
                        }
                    }
                    _ => unreachable!(),
                }
                Value::Unit
            }
            Intrinsic::Panic => {
                let val = self.eval_expr(&arguments[0]);
                match &val {
                    Value::Ref(slot) | Value::Unique(slot) => {
                        let inner = slot.borrow();
                        match &*inner {
                            Value::Array(elements) => {
                                let bytes: Vec<u8> = elements
                                    .iter()
                                    .map(|s| {
                                        let v = s.borrow();
                                        match &*v {
                                            Value::Int(n) => *n as u8,
                                            _ => unreachable!(),
                                        }
                                    })
                                    .collect();
                                let msg = String::from_utf8_lossy(&bytes);
                                panic!("{msg}");
                            }
                            _ => unreachable!(),
                        }
                    }
                    _ => unreachable!(),
                }
            }
            Intrinsic::ReadStdin => {
                let val = self.eval_expr(&arguments[0]);
                match &val {
                    Value::Ref(slot) | Value::Unique(slot) => {
                        let inner = slot.borrow();
                        match &*inner {
                            Value::Array(elements) => {
                                let mut buf = vec![0u8; elements.len()];
                                let n = self.files.read(STDIN, &mut buf);
                                for (i, byte) in buf[..n].iter().enumerate() {
                                    *elements[i].borrow_mut() = Value::Int(*byte as i64);
                                }
                                Value::Int(n as i64)
                            }
                            _ => unreachable!(),
                        }
                    }
                    _ => unreachable!(),
                }
            }
            Intrinsic::FileOpen => {
                let val = self.eval_expr(&arguments[0]);
                let bytes: Vec<u8> = match &val {
                    Value::Ref(slot) | Value::Unique(slot) => match &*slot.borrow() {
                        Value::Array(elements) => elements
                            .iter()
                            .map(|s| match &*s.borrow() {
                                Value::Int(n) => *n as u8,
                                _ => unreachable!(),
                            })
                            .collect(),
                        _ => unreachable!(),
                    },
                    _ => unreachable!(),
                };
                let path = String::from_utf8_lossy(&bytes).into_owned();
                // No fd arena / GC here: the FileDesc is an index into a virtual
                // table of boxed streams (the compiled runtime uses a real fd).
                let fd = self.files.open(&path);
                Value::Int(fd as i64)
            }
            Intrinsic::FileClose => {
                // The virtual table keeps the stream alive (no auto-close in the
                // interpreters); evaluate the argument for any side effects.
                self.eval_expr(&arguments[0]);
                Value::Unit
            }
            Intrinsic::FileStdin => Value::Int(STDIN as i64),
            Intrinsic::FileStdout => Value::Int(STDOUT as i64),
            Intrinsic::FileRead => {
                let fd = match self.eval_expr(&arguments[0]) {
                    Value::Int(n) => n as usize,
                    _ => unreachable!(),
                };
                let dst = self.eval_expr(&arguments[1]);
                match &dst {
                    Value::Ref(slot) | Value::Unique(slot) => match &*slot.borrow() {
                        Value::Array(elements) => {
                            let mut buf = vec![0u8; elements.len()];
                            let n = self.files.read(fd, &mut buf);
                            for (i, byte) in buf[..n].iter().enumerate() {
                                *elements[i].borrow_mut() = Value::Int(*byte as i64);
                            }
                            Value::Int(n as i64)
                        }
                        _ => unreachable!(),
                    },
                    _ => unreachable!(),
                }
            }
            Intrinsic::FileWritePartial => {
                let fd = match self.eval_expr(&arguments[0]) {
                    Value::Int(n) => n as usize,
                    _ => unreachable!(),
                };
                let src = self.eval_expr(&arguments[1]);
                let bytes: Vec<u8> = match &src {
                    Value::Ref(slot) | Value::Unique(slot) => match &*slot.borrow() {
                        Value::Array(elements) => elements
                            .iter()
                            .map(|s| match &*s.borrow() {
                                Value::Int(n) => *n as u8,
                                _ => unreachable!(),
                            })
                            .collect(),
                        _ => unreachable!(),
                    },
                    _ => unreachable!(),
                };
                let n = self.files.write_partial(fd, &bytes);
                Value::Int(n as i64)
            }
            Intrinsic::ArrayLen => {
                let arr = self.eval_expr(&arguments[0]);
                match arr {
                    Value::Array(elements) => Value::Int(elements.len() as i64),
                    _ => unreachable!(),
                }
            }
            Intrinsic::AssertArrayLen => {
                let arr = self.eval_expr(&arguments[0]);
                let expected = self.eval_expr(&arguments[1]);
                let expected_len = match expected {
                    Value::Int(n) => n as usize,
                    _ => unreachable!(),
                };
                let actual_len = match &arr {
                    Value::Array(elements) => elements.len(),
                    _ => unreachable!(),
                };
                assert!(
                    actual_len == expected_len,
                    "array destructure: expected {expected_len} elements, got {actual_len}"
                );
                Value::Unit
            }
            Intrinsic::ThreadSpawn => {
                panic!("thread_spawn not implemented in AST interpreter");
            }
            Intrinsic::AtomicLoad => {
                // In single-threaded interpreter, atomic load is just a deref
                let val = self.eval_expr(&arguments[0]);
                match val {
                    Value::Ref(slot) => deep_copy_value(&slot.borrow()),
                    _ => unreachable!("atomic_load: expected ref"),
                }
            }
            Intrinsic::AtomicStore => {
                // In single-threaded interpreter, atomic store is just a write through ref
                let ptr_val = self.eval_expr(&arguments[0]);
                let new_val = self.eval_expr(&arguments[1]);
                match ptr_val {
                    Value::Ref(slot) => {
                        *slot.borrow_mut() = new_val;
                    }
                    _ => unreachable!("atomic_store: expected ref"),
                }
                Value::Unit
            }
            Intrinsic::AtomicExchange => {
                // In single-threaded interpreter, exchange is load old + store new
                let ptr_val = self.eval_expr(&arguments[0]);
                let new_val = self.eval_expr(&arguments[1]);
                match ptr_val {
                    Value::Ref(slot) => {
                        let old = deep_copy_value(&slot.borrow());
                        *slot.borrow_mut() = new_val;
                        old
                    }
                    _ => unreachable!("atomic_exchange: expected ref"),
                }
            }
            Intrinsic::AtomicCompareExchange => {
                // In single-threaded interpreter, CAS is load + conditional store
                let ptr_val = self.eval_expr(&arguments[0]);
                let expected = self.eval_expr(&arguments[1]);
                let new_val = self.eval_expr(&arguments[2]);
                match ptr_val {
                    Value::Ref(slot) => {
                        let old = deep_copy_value(&slot.borrow());
                        if atomic_value_eq(&old, &expected) {
                            *slot.borrow_mut() = new_val;
                        }
                        old
                    }
                    _ => unreachable!("atomic_compare_exchange: expected ref"),
                }
            }
            Intrinsic::FutexWait => {
                panic!("futex_wait not implemented in AST interpreter");
            }
            Intrinsic::FutexWake => {
                panic!("futex_wake not implemented in AST interpreter");
            }
            Intrinsic::Cast(_, _) => {
                let val = self.eval_expr(&arguments[0]);
                let src_ty = &arguments[0].ty;
                match val {
                    Value::Int(n) => {
                        let raw = n as u64;
                        let converted = cast_numeric_ast(raw, src_ty, result_ty);
                        Value::Int(converted as i64)
                    }
                    _ => unreachable!(),
                }
            }
        }
    }

    /// Returns `Some(value)` if a `return` statement was hit, `None` otherwise.
    fn exec_statement(&mut self, stmt: &Statement) -> Option<Value> {
        match &stmt.kind {
            StatementKind::Let { name, value, .. } => {
                let val = self.eval_expr(value);
                self.define_var(name.clone(), Rc::new(RefCell::new(val)));
                None
            }
            StatementKind::Assignment { target, value } => {
                let val = self.eval_expr(value);
                let slot = self.eval_place(target);
                assign_value_in_place(&slot, val);
                None
            }
            StatementKind::If {
                condition,
                body,
                else_body,
            } => {
                let val = self.eval_expr(condition);
                match val {
                    Value::Int(n) if n != 0 => {
                        self.push_scope();
                        let ret = self.exec_body(body);
                        self.pop_scope();
                        ret
                    }
                    _ => {
                        if !else_body.is_empty() {
                            self.push_scope();
                            let ret = self.exec_body(else_body);
                            self.pop_scope();
                            ret
                        } else {
                            None
                        }
                    }
                }
            }
            StatementKind::While { condition, body } => loop {
                let val = self.eval_expr(condition);
                match val {
                    Value::Int(n) if n != 0 => {
                        self.push_scope();
                        let ret = self.exec_body(body);
                        self.pop_scope();
                        if ret.is_some() {
                            return ret;
                        }
                    }
                    _ => break None,
                }
            },
            StatementKind::Expression(expr) => {
                self.eval_expr(expr);
                None
            }
            StatementKind::Return(expr) => {
                let val = self.eval_expr(expr);
                Some(val)
            }
        }
    }

    /// Execute a list of statements, propagating early returns.
    fn exec_body(&mut self, body: &[Statement]) -> Option<Value> {
        for stmt in body {
            if let Some(val) = self.exec_statement(stmt) {
                return Some(val);
            }
        }
        None
    }

    /// Execute a function body, returning the function's return value.
    /// If return_type is non-Unit, the last Expression statement is the implicit return.
    fn exec_function_body(&mut self, body: &[Statement], return_type: &Type) -> Value {
        let has_tail = *return_type != Type::Unit
            && body
                .last()
                .is_some_and(|s| matches!(s.kind, StatementKind::Expression(_)));

        let (init, tail) = if has_tail {
            let (init, tail) = body.split_at(body.len() - 1);
            (init, Some(&tail[0]))
        } else {
            (body, None)
        };

        // Execute all statements before the tail
        for stmt in init {
            if let Some(val) = self.exec_statement(stmt) {
                return val; // early return
            }
        }

        // Evaluate tail expression for its value
        if let Some(Statement {
            kind: StatementKind::Expression(expr),
            ..
        }) = tail
        {
            self.eval_expr(expr)
        } else {
            Value::Unit
        }
    }

    fn run(&mut self) {
        let main_func = *self
            .functions
            .get("main")
            .unwrap_or_else(|| panic!("no main function"));

        assert!(
            main_func.parameters.is_empty(),
            "main function must take no parameters"
        );

        self.push_scope();
        self.exec_function_body(&main_func.body, &main_func.return_type);
        self.pop_scope();
    }
}

pub fn interpret(source: &SourceFile) {
    let mut interp = Interpreter::new(source, std::io::stdin(), std::io::stdout());
    interp.run();
}

pub fn interpret_to(source: &SourceFile, stdin: impl Read, stdout: impl Write) {
    let mut interp = Interpreter::new(source, stdin, stdout);
    interp.run();
}
