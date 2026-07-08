use crate::ast::Intrinsic;
use crate::scope::ScopeStack;
use crate::typed_ast::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{Read, Write};

use crate::interp_io::{FileTable, STDERR, STDIN, STDOUT};
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

/// Truncate a raw bitwise/shift result to an integer type's width, sign-
/// extending for signed types. Mirrors the width-masking the IR interpreter and
/// the compiled backend apply, so e.g. `128u8 << 1u8` is `0`, not `256`.
fn truncate_int(val: u64, ty: &Type) -> i64 {
    let bits = ty.int_bit_width();
    if bits == 64 {
        return val as i64;
    }
    let mask = (1u64 << bits) - 1;
    let t = val & mask;
    if ty.is_unsigned() {
        t as i64
    } else {
        let sign = 1u64 << (bits - 1);
        (if t & sign != 0 { t | !mask } else { t }) as i64
    }
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

/// Decode a `&[Uint8]` value (e.g. an intrinsic's path or buffer argument)
/// into its raw bytes.
fn ref_bytes(val: &Value) -> Vec<u8> {
    match val {
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
    }
}

/// Build a fresh `&[Uint8]` value from raw bytes (the inverse of [`ref_bytes`]).
fn bytes_ref(bytes: &[u8]) -> Value {
    let slots: Vec<Slot> = bytes
        .iter()
        .map(|b| Rc::new(RefCell::new(Value::Int(*b as i64))))
        .collect();
    Value::Ref(Rc::new(RefCell::new(Value::Array(slots))))
}

/// Build a `Thrown` carrying `msg` as a fresh `&[Uint8]` value — the
/// interpreter's counterpart of the compiled runtime's throw helpers
/// (`panic::throw_str`/`throw_message`). The message strings are canonical
/// across all three backends.
fn thrown(msg: &str) -> Thrown {
    let bytes: Vec<Slot> = msg
        .bytes()
        .map(|b| Rc::new(RefCell::new(Value::Int(b as i64))))
        .collect();
    Thrown(Value::Ref(Rc::new(RefCell::new(Value::Array(bytes)))))
}

/// Extract the bytes of a `&[Uint8]`/`^[Uint8]` value (a ref to a byte array).
fn slice_to_bytes(val: &Value) -> Vec<u8> {
    match val {
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

/// How a statement/block finished executing — used to propagate early exits.
enum Flow {
    /// Proceed to the next statement.
    Normal,
    /// Exit the innermost loop, optionally with a value (for `loop` expressions).
    Break(Option<Value>),
    /// Skip to the next iteration of the innermost loop.
    Continue,
    /// Exit the function with this value.
    Return(Value),
}

/// How a loop terminated.
enum LoopExit {
    /// `break <v>` (or valueless break → Unit).
    Broke(Value),
    /// `return <v>` propagated out of the loop.
    Returned(Value),
}

/// A propagating Solar `throw`: the thrown message bytes. It unwinds as the
/// `Err` of `Eval` through every evaluation step until a `try` handler catches
/// it (or it escapes `main`, which aborts). This mirrors the compiled backend's
/// `sol_throw`/`sol_try` (Rust panic + `catch_unwind`).
struct Thrown(Value);

/// Result of any evaluation that may propagate a `throw`.
type Eval<T> = Result<T, Thrown>;

struct Interpreter<'a, 'io> {
    functions: HashMap<String, &'a FunctionDef>,
    scopes: ScopeStack<Slot>,
    files: FileTable<'io>,
    /// Top-level `static` slots, by name. Initialized from their literal init
    /// expressions in `run` before `main`'s body executes.
    globals: HashMap<String, Slot>,
    statics: &'a [crate::typed_ast::StaticItem],
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
            globals: HashMap::new(),
            statics: &source.statics,
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

    fn eval_place(&mut self, expr: &Expr) -> Eval<Slot> {
        Ok(match &expr.kind {
            ExprKind::Identifier(name) => self.lookup_var(name),
            ExprKind::Global(name) => Rc::clone(&self.globals[name.as_str()]),
            ExprKind::FieldAccess { object, field } => {
                let obj_slot = self.eval_place(object)?;
                let obj_ref = obj_slot.borrow();
                match &*obj_ref {
                    Value::Struct { fields, .. } => Rc::clone(&fields[field.as_str()]),
                    _ => unreachable!("type checker guarantees struct"),
                }
            }
            ExprKind::Deref(inner) => {
                let inner_slot = self.eval_place(inner)?;
                let inner_ref = inner_slot.borrow();
                match &*inner_ref {
                    Value::Ref(target) | Value::Unique(target) => Rc::clone(target),
                    Value::Null => return Err(thrown("null reference dereference")),
                    _ => unreachable!("type checker guarantees ref/unique"),
                }
            }
            ExprKind::Index { object, index } => {
                let arr_slot = self.eval_place(object)?;
                let idx_val = self.eval_expr(index)?;
                // The index is compared and rendered as u64, matching the
                // compiled runtime's `sol_slice_index` (a negative signed index
                // wraps to a huge unsigned value and fails the bounds check).
                let idx = match idx_val {
                    Value::Int(n) => n as u64,
                    _ => unreachable!("type checker guarantees integer index"),
                };
                let arr_ref = arr_slot.borrow();
                match &*arr_ref {
                    Value::Array(elements) => {
                        let len = elements.len();
                        if idx >= len as u64 {
                            return Err(thrown(&format!(
                                "index out of bounds: index is {idx} but length is {len}"
                            )));
                        }
                        Rc::clone(&elements[idx as usize])
                    }
                    _ => unreachable!("type checker guarantees array"),
                }
            }
            ExprKind::Slice { object, start, end } => {
                let arr_slot = self.eval_place(object)?;
                let s = match self.eval_expr(start)? {
                    Value::Int(n) => n as usize,
                    _ => unreachable!(),
                };
                let e = match self.eval_expr(end)? {
                    Value::Int(n) => n as usize,
                    _ => unreachable!(),
                };
                let arr_ref = arr_slot.borrow();
                let elements = match &*arr_ref {
                    Value::Array(elements) => elements,
                    _ => unreachable!("type checker guarantees array"),
                };
                let len = elements.len();
                if s > e {
                    return Err(thrown(&format!("slice start ({s}) > end ({e})")));
                }
                if e > len {
                    return Err(thrown(&format!("slice end ({e}) > length ({len})")));
                }
                let sub_slots: Vec<Slot> = elements[s..e].to_vec();
                Rc::new(RefCell::new(Value::Array(sub_slots)))
            }
            ExprKind::If {
                condition,
                then_body,
                else_body,
            } => {
                let cond = self.eval_expr(condition)?;
                let branch = match cond {
                    Value::Int(n) if n != 0 => then_body,
                    _ => else_body,
                };
                self.push_scope();
                let result = self.exec_body_place(branch)?;
                self.pop_scope();
                result
            }
            ExprKind::Match { scrutinee, arms } => {
                let enum_slot = self.eval_place(scrutinee)?;
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
                        let result = self.exec_body_place(&arm.body)?;
                        self.pop_scope();
                        return Ok(result);
                    }
                }
                unreachable!("no matching arm in match expression");
            }
            _ => {
                let val = self.eval_expr(expr)?;
                Rc::new(RefCell::new(val))
            }
        })
    }

    fn exec_body_place(&mut self, body: &[Statement]) -> Eval<Slot> {
        let (init, tail) = body.split_at(body.len() - 1);
        for stmt in init {
            self.exec_statement(stmt)?;
        }
        match &tail[0].kind {
            StatementKind::Expression(expr) => self.eval_place(expr),
            _ => unreachable!(),
        }
    }

    fn eval_expr(&mut self, expr: &Expr) -> Eval<Value> {
        Ok(match &expr.kind {
            ExprKind::Identifier(_)
            | ExprKind::Global(_)
            | ExprKind::FieldAccess { .. }
            | ExprKind::Deref(_)
            | ExprKind::Index { .. }
            | ExprKind::Slice { .. } => {
                let slot = self.eval_place(expr)?;
                let val = slot.borrow();
                deep_copy_value(&val)
            }
            ExprKind::IntegerLiteral(n) => Value::Int(*n),
            ExprKind::BooleanLiteral(b) => Value::Int(if *b { 1 } else { 0 }),
            ExprKind::NullLiteral => Value::Null,
            ExprKind::Reference(inner) => {
                let slot = self.eval_place(inner)?;
                Value::Ref(slot)
            }
            ExprKind::Unique(inner) => {
                let val = self.eval_expr(inner)?;
                Value::Unique(Rc::new(RefCell::new(val)))
            }
            ExprKind::Not(inner) => {
                let val = self.eval_expr(inner)?;
                match val {
                    Value::Int(v) if inner.ty.is_integer() => {
                        // Bitwise complement, masked to the operand's width.
                        let w = inner.ty.int_bit_width();
                        let mask = if w == 64 { u64::MAX } else { (1u64 << w) - 1 };
                        let complement = (!(v as u64)) & mask;
                        if inner.ty.is_unsigned() {
                            Value::Int(complement as i64)
                        } else {
                            // Sign-extend the width-masked result.
                            let sign = 1u64 << (w - 1);
                            let ext = if complement & sign != 0 {
                                complement | !mask
                            } else {
                                complement
                            };
                            Value::Int(ext as i64)
                        }
                    }
                    // Logical not on Bool.
                    Value::Int(0) => Value::Int(1),
                    Value::Int(_) => Value::Int(0),
                    other => panic!("`!` on non-integer/bool value: {other:?}"),
                }
            }
            ExprKind::StructLiteral { name, fields } => {
                let mut field_map = HashMap::new();
                for fi in fields {
                    let val = self.eval_expr(&fi.value)?;
                    field_map.insert(fi.name.clone(), Rc::new(RefCell::new(val)));
                }
                Value::Struct {
                    name: name.clone(),
                    fields: field_map,
                }
            }
            ExprKind::ArrayLiteral(elements) => {
                let mut slots = Vec::with_capacity(elements.len());
                for e in elements {
                    let val = self.eval_expr(e)?;
                    slots.push(Rc::new(RefCell::new(val)));
                }
                Value::Array(slots)
            }
            ExprKind::ArrayRepeat { element, count } => {
                let elem_val = self.eval_expr(element)?;
                let n = match self.eval_expr(count)? {
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
                let n = match self.eval_expr(count)? {
                    Value::Int(n) => n as usize,
                    _ => unreachable!(),
                };
                let init_val = self.eval_expr(init)?;
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
                    let result = self.exec_function_body(&func_def.body, &func_def.return_type)?;
                    self.pop_scope();
                    slots.push(Rc::new(RefCell::new(result)));
                }
                Value::Array(slots)
            }
            ExprKind::ArraySizeCoerce { expr, size } => {
                let val = self.eval_expr(expr)?;
                match &val {
                    Value::Array(elements) => {
                        if elements.len() != *size as usize {
                            return Err(thrown(&format!(
                                "array length mismatch: expected {} elements, got {}",
                                size,
                                elements.len()
                            )));
                        }
                    }
                    _ => unreachable!("ArraySizeCoerce on non-array value"),
                }
                val
            }
            ExprKind::BinaryOp { op, left, right } => {
                use crate::ast::BinOp;
                match op {
                    BinOp::And => {
                        let lv = self.eval_expr(left)?;
                        match lv {
                            Value::Int(0) => Value::Int(0),
                            _ => self.eval_expr(right)?,
                        }
                    }
                    BinOp::Or => {
                        let lv = self.eval_expr(left)?;
                        match lv {
                            Value::Int(0) => self.eval_expr(right)?,
                            _ => lv,
                        }
                    }
                    _ => {
                        let unsigned = left.ty.is_unsigned();
                        let lv = self.eval_expr(left)?;
                        let rv = self.eval_expr(right)?;
                        match (&lv, &rv) {
                            (Value::Int(a), Value::Int(b)) if unsigned => {
                                // Unsigned operands are stored as u64 bit patterns in i64
                                let a = *a as u64;
                                let b = *b as u64;
                                let int = |v: u64| Value::Int(v as i64);
                                match op {
                                    BinOp::Add => int(match a.checked_add(b) {
                                        Some(v) => v,
                                        None => {
                                            return Err(thrown("integer overflow in addition"));
                                        }
                                    }),
                                    BinOp::Sub => int(match a.checked_sub(b) {
                                        Some(v) => v,
                                        None => {
                                            return Err(thrown("integer overflow in subtraction"));
                                        }
                                    }),
                                    BinOp::Mul => int(match a.checked_mul(b) {
                                        Some(v) => v,
                                        None => {
                                            return Err(thrown(
                                                "integer overflow in multiplication",
                                            ));
                                        }
                                    }),
                                    BinOp::Div => int(match a.checked_div(b) {
                                        Some(v) => v,
                                        None => {
                                            return Err(thrown("integer division by zero"));
                                        }
                                    }),
                                    BinOp::Mod => int(match a.checked_rem(b) {
                                        Some(v) => v,
                                        None => {
                                            return Err(thrown("integer modulo by zero"));
                                        }
                                    }),
                                    BinOp::Eq => Value::Int((a == b) as i64),
                                    BinOp::Ne => Value::Int((a != b) as i64),
                                    BinOp::Lt => Value::Int((a < b) as i64),
                                    BinOp::Le => Value::Int((a <= b) as i64),
                                    BinOp::Gt => Value::Int((a > b) as i64),
                                    BinOp::Ge => Value::Int((a >= b) as i64),
                                    BinOp::BitAnd => Value::Int(truncate_int(a & b, &left.ty)),
                                    BinOp::BitOr => Value::Int(truncate_int(a | b, &left.ty)),
                                    BinOp::BitXor => Value::Int(truncate_int(a ^ b, &left.ty)),
                                    BinOp::Shl => {
                                        let w = left.ty.int_bit_width() as u64;
                                        let r = if b >= w { 0 } else { a << (b as u32) };
                                        Value::Int(truncate_int(r, &left.ty))
                                    }
                                    BinOp::Shr => {
                                        let w = left.ty.int_bit_width() as u64;
                                        let r = if b >= w { 0 } else { a >> (b as u32) };
                                        Value::Int(truncate_int(r, &left.ty))
                                    }
                                    BinOp::WrapAdd => {
                                        Value::Int(truncate_int(a.wrapping_add(b), &left.ty))
                                    }
                                    BinOp::WrapSub => {
                                        Value::Int(truncate_int(a.wrapping_sub(b), &left.ty))
                                    }
                                    BinOp::WrapMul => {
                                        Value::Int(truncate_int(a.wrapping_mul(b), &left.ty))
                                    }
                                    BinOp::And | BinOp::Or => unreachable!(),
                                }
                            }
                            (Value::Int(a), Value::Int(b)) => {
                                let a = *a;
                                let b = *b;
                                match op {
                                    BinOp::Add => Value::Int(match a.checked_add(b) {
                                        Some(v) => v,
                                        None => {
                                            return Err(thrown("integer overflow in addition"));
                                        }
                                    }),
                                    BinOp::Sub => Value::Int(match a.checked_sub(b) {
                                        Some(v) => v,
                                        None => {
                                            return Err(thrown("integer overflow in subtraction"));
                                        }
                                    }),
                                    BinOp::Mul => Value::Int(match a.checked_mul(b) {
                                        Some(v) => v,
                                        None => {
                                            return Err(thrown(
                                                "integer overflow in multiplication",
                                            ));
                                        }
                                    }),
                                    BinOp::Div => Value::Int(match a.checked_div(b) {
                                        Some(v) => v,
                                        None if b == 0 => {
                                            return Err(thrown("integer division by zero"));
                                        }
                                        None => {
                                            return Err(thrown("integer overflow in division"));
                                        }
                                    }),
                                    BinOp::Mod => Value::Int(match a.checked_rem(b) {
                                        Some(v) => v,
                                        None if b == 0 => {
                                            return Err(thrown("integer modulo by zero"));
                                        }
                                        None => {
                                            return Err(thrown("integer overflow in modulo"));
                                        }
                                    }),
                                    BinOp::Eq => Value::Int(if a == b { 1 } else { 0 }),
                                    BinOp::Ne => Value::Int(if a != b { 1 } else { 0 }),
                                    BinOp::Lt => Value::Int(if a < b { 1 } else { 0 }),
                                    BinOp::Le => Value::Int(if a <= b { 1 } else { 0 }),
                                    BinOp::Gt => Value::Int(if a > b { 1 } else { 0 }),
                                    BinOp::Ge => Value::Int(if a >= b { 1 } else { 0 }),
                                    BinOp::BitAnd => {
                                        Value::Int(truncate_int((a & b) as u64, &left.ty))
                                    }
                                    BinOp::BitOr => {
                                        Value::Int(truncate_int((a | b) as u64, &left.ty))
                                    }
                                    BinOp::BitXor => {
                                        Value::Int(truncate_int((a ^ b) as u64, &left.ty))
                                    }
                                    BinOp::Shl => {
                                        let w = left.ty.int_bit_width() as u64;
                                        let r = if (b as u64) >= w { 0 } else { a << (b as u32) };
                                        Value::Int(truncate_int(r as u64, &left.ty))
                                    }
                                    BinOp::Shr => {
                                        // Arithmetic shift; count reaching the
                                        // width fills with the sign bit.
                                        let w = left.ty.int_bit_width();
                                        let sh = if (b as u64) >= w as u64 {
                                            w - 1
                                        } else {
                                            b as u32
                                        };
                                        Value::Int(truncate_int((a >> sh) as u64, &left.ty))
                                    }
                                    BinOp::WrapAdd => Value::Int(truncate_int(
                                        (a as u64).wrapping_add(b as u64),
                                        &left.ty,
                                    )),
                                    BinOp::WrapSub => Value::Int(truncate_int(
                                        (a as u64).wrapping_sub(b as u64),
                                        &left.ty,
                                    )),
                                    BinOp::WrapMul => Value::Int(truncate_int(
                                        (a as u64).wrapping_mul(b as u64),
                                        &left.ty,
                                    )),
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
                let cond = self.eval_expr(condition)?;
                let branch = match cond {
                    Value::Int(n) if n != 0 => then_body,
                    _ => else_body,
                };
                self.push_scope();
                let result = self.exec_function_body(branch, &expr.ty)?;
                self.pop_scope();
                result
            }
            ExprKind::Block(stmts) => {
                self.push_scope();
                let result = self.exec_function_body(stmts, &expr.ty)?;
                self.pop_scope();
                result
            }
            ExprKind::Loop(body) => {
                // Loop expression: its value comes from `break <v>`. (As with
                // other expression-position bodies, `return` inside is not
                // propagated by the interpreter.)
                match self.run_loop(body)? {
                    LoopExit::Broke(v) => v,
                    LoopExit::Returned(v) => v,
                }
            }
            ExprKind::EnumVariant {
                enum_name,
                variant_name,
                variant_index,
                value,
            } => {
                let val_slot = match value.as_ref() {
                    Some(v) => Some(Rc::new(RefCell::new(self.eval_expr(v)?))),
                    None => None,
                };
                Value::Enum {
                    enum_name: enum_name.clone(),
                    variant_name: variant_name.clone(),
                    variant_index: *variant_index,
                    value: val_slot,
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                // Get the scrutinee as a place (shared slot)
                let enum_slot = self.eval_place(scrutinee)?;
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
                        let result = self.exec_function_body(&arm.body, &expr.ty)?;
                        self.pop_scope();
                        return Ok(result);
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

                let mut arg_values: Vec<Value> = Vec::with_capacity(arguments.len());
                for a in arguments {
                    arg_values.push(self.eval_expr(a)?);
                }

                self.push_scope();
                for (param, val) in func_def.parameters.iter().zip(arg_values) {
                    self.define_var(param.name.clone(), Rc::new(RefCell::new(val)));
                }

                let result = self.exec_function_body(&func_def.body, &func_def.return_type)?;
                self.pop_scope();
                result
            }
            ExprKind::IntrinsicCall {
                intrinsic,
                arguments,
            } => self.exec_intrinsic(intrinsic, arguments, &expr.ty)?,
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
                let callee_val = self.eval_expr(callee)?;
                let (func_name, captured_slots) = match callee_val {
                    Value::Function { name, captures } => (name, captures),
                    _ => unreachable!("type checker guarantees function"),
                };

                let func_def = *self
                    .functions
                    .get(func_name.as_str())
                    .unwrap_or_else(|| panic!("undefined function: {func_name}"));

                let mut arg_values: Vec<Value> = Vec::with_capacity(arguments.len());
                for a in arguments {
                    arg_values.push(self.eval_expr(a)?);
                }

                self.push_scope();
                // Define captured variables first (shared slots from enclosing scope)
                for (name, slot) in &captured_slots {
                    self.define_var(name.clone(), Rc::clone(slot));
                }
                for (param, val) in func_def.parameters.iter().zip(arg_values) {
                    self.define_var(param.name.clone(), Rc::new(RefCell::new(val)));
                }

                let result = self.exec_function_body(&func_def.body, &func_def.return_type)?;
                self.pop_scope();
                result
            }
        })
    }

    fn exec_intrinsic(
        &mut self,
        intrinsic: &Intrinsic,
        arguments: &[Expr],
        result_ty: &Type,
    ) -> Eval<Value> {
        Ok(match intrinsic {
            Intrinsic::Panic => {
                let val = self.eval_expr(&arguments[0])?;
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
            Intrinsic::FileOpen => {
                let val = self.eval_expr(&arguments[0])?;
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
                let flags = match self.eval_expr(&arguments[1])? {
                    Value::Int(n) => n,
                    _ => unreachable!(),
                };
                let mode = match self.eval_expr(&arguments[2])? {
                    Value::Int(n) => n as u32,
                    _ => unreachable!(),
                };
                // No fd arena / GC here: the FileDesc is an index into a virtual
                // table of boxed streams (the compiled runtime uses a real fd).
                let fd = match self.files.open(&path, flags, mode) {
                    Ok(fd) => fd,
                    Err(err) => return Err(thrown(&format!("file_open failed: {err}"))),
                };
                Value::Int(fd as i64)
            }
            Intrinsic::FileClose => {
                // The virtual table keeps the stream alive (no auto-close in the
                // interpreters); evaluate the argument for any side effects.
                self.eval_expr(&arguments[0])?;
                Value::Unit
            }
            Intrinsic::FileStdin => Value::Int(STDIN as i64),
            Intrinsic::FileStdout => Value::Int(STDOUT as i64),
            Intrinsic::FileStderr => Value::Int(STDERR as i64),
            Intrinsic::FileRead => {
                let fd = match self.eval_expr(&arguments[0])? {
                    Value::Int(n) => n as usize,
                    _ => unreachable!(),
                };
                let dst = self.eval_expr(&arguments[1])?;
                match &dst {
                    Value::Ref(slot) | Value::Unique(slot) => match &*slot.borrow() {
                        Value::Array(elements) => {
                            let mut buf = vec![0u8; elements.len()];
                            let n = match self.files.read(fd, &mut buf) {
                                Ok(n) => n,
                                Err(err) => {
                                    return Err(thrown(&format!("file_read failed: {err}")));
                                }
                            };
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
                let fd = match self.eval_expr(&arguments[0])? {
                    Value::Int(n) => n as usize,
                    _ => unreachable!(),
                };
                let src = self.eval_expr(&arguments[1])?;
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
                let n = match self.files.write_partial(fd, &bytes) {
                    Ok(n) => n,
                    Err(err) => {
                        return Err(thrown(&format!("file_write_partial failed: {err}")));
                    }
                };
                Value::Int(n as i64)
            }
            Intrinsic::FileReadAt => {
                let fd = match self.eval_expr(&arguments[0])? {
                    Value::Int(n) => n as usize,
                    _ => unreachable!(),
                };
                let dst = self.eval_expr(&arguments[1])?;
                let offset = match self.eval_expr(&arguments[2])? {
                    Value::Int(n) => n as u64,
                    _ => unreachable!(),
                };
                match &dst {
                    Value::Ref(slot) | Value::Unique(slot) => match &*slot.borrow() {
                        Value::Array(elements) => {
                            let mut buf = vec![0u8; elements.len()];
                            let n = match self.files.read_at(fd, &mut buf, offset) {
                                Ok(n) => n,
                                Err(err) => {
                                    return Err(thrown(&format!("file_read_at failed: {err}")));
                                }
                            };
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
            Intrinsic::FileWriteAt => {
                let fd = match self.eval_expr(&arguments[0])? {
                    Value::Int(n) => n as usize,
                    _ => unreachable!(),
                };
                let bytes = ref_bytes(&self.eval_expr(&arguments[1])?);
                let offset = match self.eval_expr(&arguments[2])? {
                    Value::Int(n) => n as u64,
                    _ => unreachable!(),
                };
                let n = match self.files.write_at(fd, &bytes, offset) {
                    Ok(n) => n,
                    Err(err) => return Err(thrown(&format!("file_write_at failed: {err}"))),
                };
                Value::Int(n as i64)
            }
            Intrinsic::FileSync => {
                let fd = match self.eval_expr(&arguments[0])? {
                    Value::Int(n) => n as usize,
                    _ => unreachable!(),
                };
                if let Err(err) = self.files.sync(fd) {
                    return Err(thrown(&format!("file_sync failed: {err}")));
                }
                Value::Unit
            }
            Intrinsic::FileLock => {
                let fd = match self.eval_expr(&arguments[0])? {
                    Value::Int(n) => n as usize,
                    _ => unreachable!(),
                };
                let op = match self.eval_expr(&arguments[1])? {
                    Value::Int(n) => n,
                    _ => unreachable!(),
                };
                match self.files.lock(fd, op) {
                    Ok(got) => Value::Int(got as i64),
                    Err(err) => return Err(thrown(&format!("file_lock failed: {err}"))),
                }
            }
            Intrinsic::FileRemove | Intrinsic::DirRemove => {
                let bytes = ref_bytes(&self.eval_expr(&arguments[0])?);
                let path = String::from_utf8_lossy(&bytes).into_owned();
                let (r, what) = if matches!(intrinsic, Intrinsic::FileRemove) {
                    (std::fs::remove_file(&path), "file_remove")
                } else {
                    (std::fs::remove_dir(&path), "dir_remove")
                };
                if let Err(err) = r {
                    return Err(thrown(&format!("{what} failed: {err}")));
                }
                Value::Unit
            }
            Intrinsic::FileRename => {
                let old_bytes = ref_bytes(&self.eval_expr(&arguments[0])?);
                let new_bytes = ref_bytes(&self.eval_expr(&arguments[1])?);
                let old = String::from_utf8_lossy(&old_bytes).into_owned();
                let new = String::from_utf8_lossy(&new_bytes).into_owned();
                if let Err(err) = std::fs::rename(&old, &new) {
                    return Err(thrown(&format!("file_rename failed: {err}")));
                }
                Value::Unit
            }
            Intrinsic::DirCreate => {
                let bytes = ref_bytes(&self.eval_expr(&arguments[0])?);
                let path = String::from_utf8_lossy(&bytes).into_owned();
                let mode = match self.eval_expr(&arguments[1])? {
                    Value::Int(n) => n as u32,
                    _ => unreachable!(),
                };
                if let Err(err) = crate::interp_io::create_dir(&path, mode) {
                    return Err(thrown(&format!("dir_create failed: {err}")));
                }
                Value::Unit
            }
            Intrinsic::FileStat => {
                let bytes = ref_bytes(&self.eval_expr(&arguments[0])?);
                let path = String::from_utf8_lossy(&bytes).into_owned();
                let (found, size, mtime, kind) = match crate::interp_io::stat_path(&path) {
                    Ok(Some((size, mtime, kind))) => (1i64, size, mtime, kind),
                    Ok(None) => (0, 0, 0, 0),
                    Err(err) => return Err(thrown(&format!("file_stat failed: {err}"))),
                };
                for (arg, v) in arguments[1..4].iter().zip([size, mtime, kind]) {
                    match self.eval_expr(arg)? {
                        Value::Ref(slot) | Value::Unique(slot) => {
                            *slot.borrow_mut() = Value::Int(v as i64);
                        }
                        _ => unreachable!(),
                    }
                }
                Value::Int(found)
            }
            Intrinsic::DirRead => {
                let fd = match self.eval_expr(&arguments[0])? {
                    Value::Int(n) => n as usize,
                    _ => unreachable!(),
                };
                let entries = match self.files.dir_read(fd) {
                    Ok(entries) => entries,
                    Err(err) => return Err(thrown(&format!("dir_read failed: {err}"))),
                };
                let slots: Vec<Slot> = entries
                    .iter()
                    .map(|e| Rc::new(RefCell::new(bytes_ref(e))))
                    .collect();
                Value::Ref(Rc::new(RefCell::new(Value::Array(slots))))
            }
            Intrinsic::Args | Intrinsic::Env => {
                // No process args/env source in the interpreters: return an
                // empty `&[&[Uint8]]` (a reference to an empty array).
                Value::Ref(Rc::new(RefCell::new(Value::Array(Vec::new()))))
            }
            Intrinsic::MonotonicTime | Intrinsic::SystemTime => {
                Value::Int(crate::ir_interp::time_ns(intrinsic) as i64)
            }
            Intrinsic::ArrayLen => {
                let arr = self.eval_expr(&arguments[0])?;
                match arr {
                    Value::Array(elements) => Value::Int(elements.len() as i64),
                    _ => unreachable!(),
                }
            }
            Intrinsic::U64FromLe | Intrinsic::U32FromLe => {
                // Decode the `[Uint8; N]` argument as a little-endian integer.
                let arr = self.eval_expr(&arguments[0])?;
                match arr {
                    Value::Array(elements) => {
                        let mut v: u64 = 0;
                        for (k, slot) in elements.iter().enumerate() {
                            let byte = match &*slot.borrow() {
                                Value::Int(n) => *n as u8 as u64,
                                _ => unreachable!("u*_from_le: expected byte"),
                            };
                            v |= byte << (8 * k);
                        }
                        Value::Int(v as i64)
                    }
                    _ => unreachable!(),
                }
            }
            Intrinsic::SimdMatchByteX16 | Intrinsic::SimdMatchHighBitX16 => {
                // Scalar reference for the SSE2 group scan: build the compact
                // match mask (bit i <-> lane i) over the 16-lane byte vector.
                let arr = self.eval_expr(&arguments[0])?;
                let elements = match arr {
                    Value::Array(e) => e,
                    _ => unreachable!("simd group scan: expected [Uint8; 16]"),
                };
                let byte_at = |slot: &std::rc::Rc<std::cell::RefCell<Value>>| match &*slot.borrow()
                {
                    Value::Int(n) => *n as u8,
                    _ => unreachable!("simd group scan: expected byte"),
                };
                let mut mask: u64 = 0;
                if matches!(intrinsic, Intrinsic::SimdMatchByteX16) {
                    let tag = match self.eval_expr(&arguments[1])? {
                        Value::Int(n) => n as u8,
                        _ => unreachable!(),
                    };
                    for (i, slot) in elements.iter().enumerate() {
                        if byte_at(slot) == tag {
                            mask |= 1 << i;
                        }
                    }
                } else {
                    for (i, slot) in elements.iter().enumerate() {
                        if byte_at(slot) & 0x80 != 0 {
                            mask |= 1 << i;
                        }
                    }
                }
                Value::Int(mask as i64)
            }
            Intrinsic::AssertArrayLen => {
                let arr = self.eval_expr(&arguments[0])?;
                let expected = self.eval_expr(&arguments[1])?;
                let expected_len = match expected {
                    Value::Int(n) => n as usize,
                    _ => unreachable!(),
                };
                let actual_len = match &arr {
                    Value::Array(elements) => elements.len(),
                    _ => unreachable!(),
                };
                if actual_len != expected_len {
                    return Err(thrown(&format!(
                        "array length mismatch: expected {expected_len} elements, got {actual_len}"
                    )));
                }
                Value::Unit
            }
            Intrinsic::ThreadSpawn => {
                panic!("thread_spawn not implemented in AST interpreter");
            }
            Intrinsic::AtomicLoad => {
                // In single-threaded interpreter, atomic load is just a deref
                let val = self.eval_expr(&arguments[0])?;
                match val {
                    Value::Ref(slot) => deep_copy_value(&slot.borrow()),
                    _ => unreachable!("atomic_load: expected ref"),
                }
            }
            Intrinsic::AtomicStore => {
                // In single-threaded interpreter, atomic store is just a write through ref
                let ptr_val = self.eval_expr(&arguments[0])?;
                let new_val = self.eval_expr(&arguments[1])?;
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
                let ptr_val = self.eval_expr(&arguments[0])?;
                let new_val = self.eval_expr(&arguments[1])?;
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
                let ptr_val = self.eval_expr(&arguments[0])?;
                let expected = self.eval_expr(&arguments[1])?;
                let new_val = self.eval_expr(&arguments[2])?;
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
            Intrinsic::SocketCreate
            | Intrinsic::SocketBind
            | Intrinsic::SocketListen
            | Intrinsic::SocketAccept
            | Intrinsic::SocketConnect
            | Intrinsic::SocketSetOption
            | Intrinsic::SocketLocalAddr
            | Intrinsic::SocketShutdown => {
                // Like the thread/futex intrinsics: network I/O is
                // compiled-runtime only.
                panic!("socket intrinsics not implemented in AST interpreter");
            }
            Intrinsic::FutexWait => {
                panic!("futex_wait not implemented in AST interpreter");
            }
            Intrinsic::FutexWake => {
                panic!("futex_wake not implemented in AST interpreter");
            }
            Intrinsic::CountTrailingZeros | Intrinsic::CountLeadingZeros | Intrinsic::CountOnes => {
                let width = arguments[0].ty.int_bit_width();
                let val = self.eval_expr(&arguments[0])?;
                let raw = match val {
                    Value::Int(n) => n as u64,
                    _ => unreachable!(),
                };
                let mask = if width == 64 {
                    u64::MAX
                } else {
                    (1u64 << width) - 1
                };
                let v = raw & mask;
                let count = match intrinsic {
                    Intrinsic::CountTrailingZeros => {
                        if v == 0 {
                            width
                        } else {
                            v.trailing_zeros()
                        }
                    }
                    Intrinsic::CountLeadingZeros => {
                        if v == 0 {
                            width
                        } else {
                            v.leading_zeros() - (64 - width)
                        }
                    }
                    Intrinsic::CountOnes => v.count_ones(),
                    _ => unreachable!(),
                };
                Value::Int(count as i64)
            }
            Intrinsic::CarryingMulAdd => {
                // a*b + carry + add as a 128-bit value; write low/high halves
                // through the two `&Uint64` out-param refs.
                let as_u64 = |v: &Value| match v {
                    Value::Int(n) => *n as u64,
                    _ => unreachable!("carrying_mul_add: expected integer"),
                };
                let a = as_u64(&self.eval_expr(&arguments[0])?);
                let b = as_u64(&self.eval_expr(&arguments[1])?);
                let carry = as_u64(&self.eval_expr(&arguments[2])?);
                let add = as_u64(&self.eval_expr(&arguments[3])?);
                let (lo_val, hi_val) = a.carrying_mul_add(b, carry, add);
                let lo = self.eval_expr(&arguments[4])?;
                let hi = self.eval_expr(&arguments[5])?;
                match (lo, hi) {
                    (Value::Ref(lo_slot), Value::Ref(hi_slot)) => {
                        *lo_slot.borrow_mut() = Value::Int(lo_val as i64);
                        *hi_slot.borrow_mut() = Value::Int(hi_val as i64);
                    }
                    _ => unreachable!("carrying_mul_add: expected ref out-params"),
                }
                Value::Unit
            }
            Intrinsic::Throw => {
                // Unwind carrying the `&[Uint8]` reference itself (not a copy), so
                // the value `catch` receives aliases the one passed to `throw`.
                let val = self.eval_expr(&arguments[0])?;
                return Err(Thrown(val));
            }
            Intrinsic::Try => {
                // try(body, handler): run `body`; if it throws, run `handler`
                // with the thrown `&[Uint8]` reference (same slot — it aliases).
                let body = self.eval_expr(&arguments[0])?;
                let handler = self.eval_expr(&arguments[1])?;
                if let Err(Thrown(reference)) = self.call_function_value(body, vec![]) {
                    self.call_function_value(handler, vec![reference])?;
                }
                Value::Unit
            }
            Intrinsic::Cast(_, _) => {
                let val = self.eval_expr(&arguments[0])?;
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
        })
    }

    /// Call a Solar function value (closure) with already-evaluated arguments,
    /// propagating a `throw` from its body. Used by `CallIndirect` and `try`.
    fn call_function_value(&mut self, callee: Value, args: Vec<Value>) -> Eval<Value> {
        let (func_name, captured_slots) = match callee {
            Value::Function { name, captures } => (name, captures),
            _ => unreachable!("type checker guarantees function"),
        };
        let func_def = *self
            .functions
            .get(func_name.as_str())
            .unwrap_or_else(|| panic!("undefined function: {func_name}"));
        self.push_scope();
        for (name, slot) in &captured_slots {
            self.define_var(name.clone(), Rc::clone(slot));
        }
        for (param, val) in func_def.parameters.iter().zip(args) {
            self.define_var(param.name.clone(), Rc::new(RefCell::new(val)));
        }
        let result = self.exec_function_body(&func_def.body, &func_def.return_type);
        self.pop_scope();
        result
    }

    /// Execute one statement, returning how control flow should proceed.
    fn exec_statement(&mut self, stmt: &Statement) -> Eval<Flow> {
        Ok(match &stmt.kind {
            StatementKind::Let { name, value, .. } => {
                let val = self.eval_expr(value)?;
                self.define_var(name.clone(), Rc::new(RefCell::new(val)));
                Flow::Normal
            }
            StatementKind::Assignment { target, value } => {
                let val = self.eval_expr(value)?;
                let slot = self.eval_place(target)?;
                assign_value_in_place(&slot, val);
                Flow::Normal
            }
            StatementKind::If {
                condition,
                body,
                else_body,
            } => {
                let val = self.eval_expr(condition)?;
                match val {
                    Value::Int(n) if n != 0 => {
                        self.push_scope();
                        let flow = self.exec_body(body);
                        self.pop_scope();
                        flow?
                    }
                    _ => {
                        if !else_body.is_empty() {
                            self.push_scope();
                            let flow = self.exec_body(else_body);
                            self.pop_scope();
                            flow?
                        } else {
                            Flow::Normal
                        }
                    }
                }
            }
            StatementKind::While { condition, body } => loop {
                let val = self.eval_expr(condition)?;
                match val {
                    Value::Int(n) if n != 0 => {
                        self.push_scope();
                        let flow = self.exec_body(body);
                        self.pop_scope();
                        match flow? {
                            Flow::Return(v) => return Ok(Flow::Return(v)),
                            Flow::Break(_) => break Flow::Normal,
                            // A `continue` or fall-through starts the next iteration.
                            Flow::Continue | Flow::Normal => {}
                        }
                    }
                    _ => break Flow::Normal,
                }
            },
            // A bare `loop` statement runs through the statement path so `return`
            // (and outer break/continue) propagate; the break value is discarded.
            StatementKind::Expression(expr) if matches!(expr.kind, ExprKind::Loop(_)) => {
                let body = match &expr.kind {
                    ExprKind::Loop(body) => body,
                    _ => unreachable!(),
                };
                match self.run_loop(body)? {
                    LoopExit::Returned(v) => Flow::Return(v),
                    LoopExit::Broke(_) => Flow::Normal,
                }
            }
            // A statement-position `if`/`match` expression (e.g. a trailing one
            // in a loop body) must propagate control flow out of its branches,
            // like the statement forms. Its value is discarded. Without this,
            // `loop { … if c { break } else { … } }` never terminates.
            StatementKind::Expression(expr) if matches!(expr.kind, ExprKind::If { .. }) => {
                let (condition, then_body, else_body) = match &expr.kind {
                    ExprKind::If {
                        condition,
                        then_body,
                        else_body,
                    } => (condition, then_body, else_body),
                    _ => unreachable!(),
                };
                let val = self.eval_expr(condition)?;
                match val {
                    Value::Int(n) if n != 0 => {
                        self.push_scope();
                        let flow = self.exec_body(then_body);
                        self.pop_scope();
                        flow?
                    }
                    _ => {
                        if !else_body.is_empty() {
                            self.push_scope();
                            let flow = self.exec_body(else_body);
                            self.pop_scope();
                            flow?
                        } else {
                            Flow::Normal
                        }
                    }
                }
            }
            StatementKind::Expression(expr) if matches!(expr.kind, ExprKind::Match { .. }) => {
                self.exec_match_stmt(expr)?
            }
            // A bare block expression (e.g. a `match`/`if` arm body `{ break; }`)
            // in statement position propagates control flow from its statements.
            StatementKind::Expression(expr) if matches!(expr.kind, ExprKind::Block(_)) => {
                let body = match &expr.kind {
                    ExprKind::Block(body) => body,
                    _ => unreachable!(),
                };
                self.push_scope();
                let flow = self.exec_body(body);
                self.pop_scope();
                flow?
            }
            StatementKind::Expression(expr) => {
                self.eval_expr(expr)?;
                Flow::Normal
            }
            StatementKind::Return(expr) => {
                let val = self.eval_expr(expr)?;
                Flow::Return(val)
            }
            StatementKind::Break(value) => Flow::Break(match value.as_ref() {
                Some(v) => Some(self.eval_expr(v)?),
                None => None,
            }),
            StatementKind::Continue => Flow::Continue,
        })
    }

    /// Execute a statement-position `match`, propagating control flow out of the
    /// taken arm (its value is discarded). Mirrors the `Match` value evaluation
    /// but runs the arm body with `exec_body`.
    fn exec_match_stmt(&mut self, expr: &Expr) -> Eval<Flow> {
        let (scrutinee, arms) = match &expr.kind {
            ExprKind::Match { scrutinee, arms } => (scrutinee, arms),
            _ => unreachable!(),
        };
        let enum_slot = self.eval_place(scrutinee)?;
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
                let flow = self.exec_body(&arm.body);
                self.pop_scope();
                return flow;
            }
        }
        unreachable!("no matching arm in match expression");
    }

    /// Run a `loop` body until it breaks or returns.
    fn run_loop(&mut self, body: &[Statement]) -> Eval<LoopExit> {
        loop {
            self.push_scope();
            let flow = self.exec_body(body);
            self.pop_scope();
            match flow? {
                Flow::Break(v) => return Ok(LoopExit::Broke(v.unwrap_or(Value::Unit))),
                Flow::Return(v) => return Ok(LoopExit::Returned(v)),
                Flow::Continue | Flow::Normal => {}
            }
        }
    }

    /// Execute a list of statements, propagating any early exit.
    fn exec_body(&mut self, body: &[Statement]) -> Eval<Flow> {
        for stmt in body {
            match self.exec_statement(stmt)? {
                Flow::Normal => {}
                flow => return Ok(flow),
            }
        }
        Ok(Flow::Normal)
    }

    /// Execute a function body, returning the function's return value.
    /// If return_type is non-Unit, the last Expression statement is the implicit return.
    fn exec_function_body(&mut self, body: &[Statement], return_type: &Type) -> Eval<Value> {
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
            match self.exec_statement(stmt)? {
                Flow::Return(val) => return Ok(val), // early return
                Flow::Normal => {}
                Flow::Break(_) => unreachable!("break outside loop"),
                Flow::Continue => unreachable!("continue outside loop"),
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
            Ok(Value::Unit)
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

        // Store the statics' literal initial values before main's body runs.
        self.push_scope();
        let statics = self.statics;
        for st in statics {
            let Ok(val) = self.eval_expr(&st.init) else {
                unreachable!("static initializers are literals and cannot throw")
            };
            self.globals
                .insert(st.name.clone(), Rc::new(RefCell::new(val)));
        }
        self.pop_scope();

        self.push_scope();
        let result = self.exec_function_body(&main_func.body, &main_func.return_type);
        self.pop_scope();
        if let Err(Thrown(reference)) = result {
            // A `throw` that escapes `main` is uncaught; mirror the compiled
            // runtime, which aborts with the message.
            let bytes = slice_to_bytes(&reference);
            let msg = String::from_utf8_lossy(&bytes);
            panic!("uncaught exception: {msg}");
        }
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
