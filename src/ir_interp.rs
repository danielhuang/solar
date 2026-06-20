use crate::ast::BinOp;
use crate::ast::Intrinsic;
use crate::ir::*;
use std::collections::HashMap;
use std::io::{Read, Write};

use crate::interp_io::{FileTable, STDIN, STDOUT};

struct Memory {
    data: Vec<u8>,
    next_addr: usize,
}

impl Memory {
    fn new() -> Self {
        Memory {
            data: vec![0; 8],
            next_addr: 8,
        }
    }

    fn alloc(&mut self, size: usize, align: usize) -> usize {
        if size == 0 {
            return 0;
        }
        let addr = (self.next_addr + align - 1) & !(align - 1);
        self.next_addr = addr + size;
        self.data.resize(self.next_addr, 0);
        addr
    }

    fn load(&self, addr: usize, size: usize) -> u64 {
        let mut bytes = [0u8; 8];
        bytes[..size].copy_from_slice(&self.data[addr..addr + size]);
        u64::from_le_bytes(bytes)
    }

    fn store(&mut self, addr: usize, val: u64, size: usize) {
        let bytes = val.to_le_bytes();
        self.data[addr..addr + size].copy_from_slice(&bytes[..size]);
    }

    fn memcpy(&mut self, dst: usize, src: usize, size: usize) {
        self.data.copy_within(src..src + size, dst);
    }

    fn memeq(&self, a: usize, b: usize, size: usize) -> bool {
        self.data[a..a + size] == self.data[b..b + size]
    }
}

fn sign_extend(val: u64, size: usize) -> u64 {
    match size {
        1 => val as u8 as i8 as i64 as u64,
        2 => val as u16 as i16 as i64 as u64,
        4 => val as u32 as i32 as i64 as u64,
        8 => val,
        _ => unreachable!(),
    }
}

/// Truncate a raw bitwise/shift result to the integer type's width (sign-
/// extending for signed types), mirroring what storing-then-reloading does. The
/// compiled backend gets this truncation from the C cast to the result type;
/// the interpreter must do it explicitly so a value used directly as an operand
/// (e.g. `(!0u8) == 255u8`) compares with the right high bits.
fn truncate_to_ty(val: u64, ty: &Type) -> u64 {
    let bits = ty.int_bit_width();
    if bits == 64 {
        return val;
    }
    let masked = val & ((1u64 << bits) - 1);
    if is_signed(ty) {
        sign_extend(masked, (bits / 8) as usize)
    } else {
        masked
    }
}

fn is_signed(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Int8 | Type::Int16 | Type::Int32 | Type::Int64 | Type::Int
    )
}

fn is_float(ty: &Type) -> bool {
    matches!(ty, Type::Float32 | Type::Float64)
}

/// Convert a raw u64 value between numeric types, handling int↔float conversions.
fn cast_numeric(raw: u64, src: &Type, dst: &Type) -> u64 {
    match (is_float(src), is_float(dst)) {
        // int → int: raw bits already work (truncation/extension handled by store size)
        (false, false) => raw,
        // int → float
        (false, true) => {
            let ival = raw as i64;
            match dst {
                Type::Float32 => (ival as f32).to_bits() as u64,
                Type::Float64 => (ival as f64).to_bits(),
                _ => unreachable!(),
            }
        }
        // float → int
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
        // float → float
        (true, true) => match (src, dst) {
            (Type::Float32, Type::Float64) => {
                let f = f32::from_bits(raw as u32);
                (f as f64).to_bits()
            }
            (Type::Float64, Type::Float32) => {
                let f = f64::from_bits(raw);
                (f as f32).to_bits() as u64
            }
            _ => raw, // same type
        },
    }
}

enum ControlFlow {
    /// Proceed to the next statement.
    Normal,
    /// Exit the current function.
    Return,
    /// Exit the innermost loop.
    Break,
    /// Skip to the next iteration of the innermost loop.
    Continue,
}

struct Interpreter<'a, 'io> {
    module: &'a Module,
    functions: HashMap<&'a str, &'a Function>,
    fn_name_to_index: HashMap<&'a str, u64>,
    fn_index_to_name: Vec<&'a str>,
    mem: Memory,
    vars: HashMap<VarId, usize>,
    var_meta: HashMap<VarId, usize>,
    files: FileTable<'io>,
    /// Result destinations of the enclosing loop expressions; `break <v>` writes
    /// the value into the innermost one.
    loop_dst: Vec<usize>,
}

impl<'a, 'io> Interpreter<'a, 'io> {
    fn new(module: &'a Module, stdin: impl Read + 'io, stdout: impl Write + 'io) -> Self {
        let functions = module
            .functions
            .iter()
            .map(|f| (f.name.as_str(), f))
            .collect();
        let fn_index_to_name: Vec<&str> =
            module.functions.iter().map(|f| f.name.as_str()).collect();
        let fn_name_to_index: HashMap<&str, u64> = fn_index_to_name
            .iter()
            .enumerate()
            .map(|(i, name)| (*name, i as u64))
            .collect();
        Interpreter {
            module,
            functions,
            fn_name_to_index,
            fn_index_to_name,
            mem: Memory::new(),
            vars: HashMap::new(),
            var_meta: HashMap::new(),
            files: FileTable::new(stdin, stdout),
            loop_dst: Vec::new(),
        }
    }

    fn alloc_ty(&mut self, ty: &Type) -> usize {
        let s = type_size(ty, &self.module.datatypes);
        let a = type_align(ty, &self.module.datatypes);
        self.mem.alloc(s, a)
    }

    fn alloc_unsized(&mut self, ty: &Type, meta: usize) -> usize {
        let s = full_size(ty, &self.module.datatypes, meta);
        let a = type_align(ty, &self.module.datatypes);
        self.mem.alloc(s, a)
    }

    fn copy_value(&mut self, dst: usize, src: usize, ty: &Type, meta: Option<usize>) {
        match ty {
            Type::Unique(inner) => {
                let src_ptr = self.mem.load(src, 8) as usize;
                let inner_size = type_size(inner, &self.module.datatypes);
                let inner_align = type_align(inner, &self.module.datatypes);
                let new_ptr = self.mem.alloc(inner_size, inner_align);
                self.copy_value(new_ptr, src_ptr, inner, None);
                self.mem.store(dst, new_ptr as u64, 8);
            }
            Type::UniqueUnsized(inner) => {
                let src_ptr = self.mem.load(src, 8) as usize;
                let src_meta = self.mem.load(src + 8, 8) as usize;
                let inner_size = full_size(inner, &self.module.datatypes, src_meta);
                let inner_align = type_align(inner, &self.module.datatypes);
                let new_ptr = self.mem.alloc(inner_size, inner_align);
                self.copy_value(new_ptr, src_ptr, inner, Some(src_meta));
                self.mem.store(dst, new_ptr as u64, 8);
                self.mem.store(dst + 8, src_meta as u64, 8);
            }
            Type::Enum(name) => {
                let dt = &self.module.datatypes[name.as_str()];
                let variant_map: Vec<_> = dt.variant_map.as_ref().unwrap().clone();
                // Copy discriminant
                let disc = self.mem.load(src, 8);
                self.mem.store(dst, disc, 8);
                // Copy only the active variant's data
                if let Some(Some(field_name)) = variant_map.get(disc as usize) {
                    let field = dt.fields.iter().find(|f| f.name == *field_name).unwrap();
                    let offset = field.offset;
                    let field_ty = field.ty.clone();
                    self.copy_value(dst + offset, src + offset, &field_ty, None);
                }
                // Unit variants: nothing beyond the discriminant
            }
            Type::Struct(name)
                if type_contains_unique(ty, &self.module.datatypes)
                    || type_contains_enum(ty, &self.module.datatypes) =>
            {
                let fields: Vec<_> = self.module.datatypes[name.as_str()]
                    .fields
                    .iter()
                    .map(|f| (f.offset, f.ty.clone(), f.size))
                    .collect();
                for (offset, field_ty, _) in &fields {
                    let field_meta = match field_ty {
                        Type::Array(_) | Type::FixedArray(_, _) => {
                            // For unsized array fields in structs this is the tail;
                            // meta from the outer value tells us the count
                            meta
                        }
                        _ => None,
                    };
                    self.copy_value(dst + offset, src + offset, field_ty, field_meta);
                }
            }
            Type::FixedArray(inner, count)
                if type_contains_unique(inner, &self.module.datatypes)
                    || type_contains_enum(inner, &self.module.datatypes) =>
            {
                let es = type_size(inner, &self.module.datatypes);
                for i in 0..(*count as usize) {
                    self.copy_value(dst + i * es, src + i * es, inner, None);
                }
            }
            Type::Array(inner)
                if type_contains_unique(inner, &self.module.datatypes)
                    || type_contains_enum(inner, &self.module.datatypes) =>
            {
                let count = meta.unwrap();
                let es = type_size(inner, &self.module.datatypes);
                for i in 0..count {
                    self.copy_value(dst + i * es, src + i * es, inner, None);
                }
            }
            _ => {
                let size = match meta {
                    Some(m) => full_size(ty, &self.module.datatypes, m),
                    None => type_size(ty, &self.module.datatypes),
                };
                self.mem.memcpy(dst, src, size);
            }
        }
    }

    /// Load a scalar value from memory with proper sign/zero extension.
    fn scalar_load(&self, addr: usize, ty: &Type) -> u64 {
        let size = type_size(ty, &self.module.datatypes);
        let val = self.mem.load(addr, size);
        if is_signed(ty) {
            sign_extend(val, size)
        } else {
            val
        }
    }

    /// Store a scalar value to memory, truncating to the type's byte size.
    fn scalar_store(&mut self, addr: usize, val: u64, ty: &Type) {
        let size = type_size(ty, &self.module.datatypes);
        self.mem.store(addr, val, size);
    }

    fn compute_meta(&mut self, nodes: &[Node], id: NodeId) -> Option<usize> {
        let ty = &nodes[id.0].ty;
        // For FixedArray, the meta is known statically
        if let Type::FixedArray(_, n) = ty {
            return Some(*n as usize);
        }
        if is_sized(ty, &self.module.datatypes) {
            return None;
        }
        match &nodes[id.0].kind {
            NodeKind::ArrayLiteral(elems) => Some(elems.len()),
            NodeKind::ArrayRepeat { count, .. } | NodeKind::ArrayInit { count, .. } => {
                let count = *count;
                Some(self.eval_load(nodes, count) as usize)
            }
            NodeKind::ArraySizeCoerce { size, .. } => Some(*size as usize),
            NodeKind::BinaryOp { op, left, right } if *op == BinOp::Add => {
                let left = *left;
                let right = *right;
                let lm = self.compute_meta(nodes, left).unwrap();
                let rm = self.compute_meta(nodes, right).unwrap();
                Some(lm + rm)
            }
            NodeKind::StructLiteral { name, fields } => {
                let dt = &self.module.datatypes[name.as_str()];
                let last_field_name = dt.fields.last().unwrap().name.clone();
                let last_init = fields.iter().find(|(n, _)| *n == last_field_name).unwrap();
                let last_id = last_init.1;
                self.compute_meta(nodes, last_id)
            }
            NodeKind::Local(var) => self.var_meta.get(var).copied(),
            NodeKind::FieldAccess { object, .. } => {
                let object = *object;
                self.compute_meta(nodes, object)
            }
            NodeKind::Deref(inner) => {
                let inner = *inner;
                let (ref_place, _) = self.eval_place(nodes, inner);
                match &nodes[inner.0].ty {
                    Type::RefUnsized(_) | Type::UniqueUnsized(_) | Type::NullableRefUnsized(_) => {
                        Some(self.mem.load(ref_place + 8, 8) as usize)
                    }
                    _ => None,
                }
            }
            NodeKind::Slice { start, end, .. } => {
                let start = *start;
                let end = *end;
                let s = self.eval_load(nodes, start) as usize;
                let e = self.eval_load(nodes, end) as usize;
                Some(e - s)
            }
            _ => None,
        }
    }

    fn eval_place(&mut self, nodes: &[Node], id: NodeId) -> (usize, Option<usize>) {
        match &nodes[id.0].kind {
            NodeKind::Local(var) => {
                let addr = self.vars[var];
                let meta = self.var_meta.get(var).copied();
                (addr, meta)
            }
            NodeKind::FieldAccess { object, field } => {
                let object = *object;
                let field = field.clone();
                let (base, base_meta) = self.eval_place(nodes, object);
                let struct_name = match &nodes[object.0].ty {
                    Type::Struct(n) => n.as_str(),
                    _ => unreachable!(),
                };
                let dt = &self.module.datatypes[struct_name];
                let fl = dt.fields.iter().find(|f| f.name == field).unwrap();
                let is_last = dt.fields.last().unwrap().name == field;
                if is_last && !is_sized(&fl.ty, &self.module.datatypes) {
                    (base + fl.offset, base_meta)
                } else {
                    (base + fl.offset, None)
                }
            }
            NodeKind::Deref(inner) => {
                let inner = *inner;
                let ref_place = if is_place(nodes, inner) {
                    let (addr, _) = self.eval_place(nodes, inner);
                    addr
                } else {
                    let ty = nodes[inner.0].ty.clone();
                    let tmp = self.alloc_ty(&ty);
                    self.eval_into(nodes, inner, tmp);
                    tmp
                };
                match &nodes[inner.0].ty {
                    Type::Ref(_) | Type::Unique(_) => {
                        let addr = self.mem.load(ref_place, 8) as usize;
                        (addr, None)
                    }
                    Type::NullableRef(_) => {
                        let addr = self.mem.load(ref_place, 8) as usize;
                        assert!(addr != 0, "null pointer dereference");
                        (addr, None)
                    }
                    Type::RefUnsized(_) | Type::UniqueUnsized(_) => {
                        let addr = self.mem.load(ref_place, 8) as usize;
                        let meta = self.mem.load(ref_place + 8, 8) as usize;
                        (addr, Some(meta))
                    }
                    Type::NullableRefUnsized(_) => {
                        let addr = self.mem.load(ref_place, 8) as usize;
                        assert!(addr != 0, "null pointer dereference");
                        let meta = self.mem.load(ref_place + 8, 8) as usize;
                        (addr, Some(meta))
                    }
                    _ => unreachable!(),
                }
            }
            NodeKind::Index { object, index } => {
                let object = *object;
                let index = *index;
                let (base, meta) = self.eval_place(nodes, object);
                let idx = self.eval_load(nodes, index) as i64;
                let len = meta
                    .or_else(|| {
                        // For FixedArray, length is known from the type
                        if let Type::FixedArray(_, n) = &nodes[object.0].ty {
                            Some(*n as usize)
                        } else {
                            None
                        }
                    })
                    .unwrap_or_else(|| self.compute_meta(nodes, object).unwrap());
                assert!(
                    idx >= 0 && (idx as usize) < len,
                    "index out of bounds: index is {idx} but length is {len}"
                );
                let idx = idx as usize;
                let elem_ty = &nodes[id.0].ty;
                let es = type_size(elem_ty, &self.module.datatypes);
                (base + idx * es, None)
            }
            NodeKind::Slice { object, start, end } => {
                let object = *object;
                let start = *start;
                let end = *end;
                let (base, meta) = self.eval_place(nodes, object);
                let s = self.eval_load(nodes, start) as usize;
                let e = self.eval_load(nodes, end) as usize;
                let len = meta
                    .or_else(|| {
                        if let Type::FixedArray(_, n) = &nodes[object.0].ty {
                            Some(*n as usize)
                        } else {
                            None
                        }
                    })
                    .unwrap_or_else(|| self.compute_meta(nodes, object).unwrap());
                assert!(s <= e, "slice start ({s}) > end ({e})");
                assert!(e <= len, "slice end ({e}) > length ({len})");
                let elem_ty = match &nodes[id.0].ty {
                    Type::Array(inner) | Type::FixedArray(inner, _) => inner,
                    _ => unreachable!(),
                };
                let es = type_size(elem_ty, &self.module.datatypes);
                (base + s * es, Some(e - s))
            }
            NodeKind::IfExpr {
                condition,
                then_body,
                else_body,
            } => {
                let condition = *condition;
                let then_body = then_body.clone();
                let else_body = else_body.clone();
                let cond = self.eval_load(nodes, condition);
                let branch = if cond != 0 { &then_body } else { &else_body };
                self.exec_branch_place(nodes, branch)
            }
            NodeKind::Match { scrutinee, arms } => {
                let scrutinee = *scrutinee;
                let arms = arms.clone();
                let enum_base = if is_place(nodes, scrutinee) {
                    let (addr, _) = self.eval_place(nodes, scrutinee);
                    addr
                } else {
                    let ty = nodes[scrutinee.0].ty.clone();
                    let tmp = self.alloc_ty(&ty);
                    self.eval_into(nodes, scrutinee, tmp);
                    tmp
                };
                let disc = self.mem.load(enum_base, 8);
                let enum_ty = &nodes[scrutinee.0].ty;
                let enum_name = match enum_ty {
                    Type::Enum(name) => name.clone(),
                    _ => unreachable!(),
                };
                for arm in &arms {
                    let matches = match &arm.pattern {
                        MatchPattern::Variant { variant_index, .. } => disc == *variant_index,
                        MatchPattern::Wildcard(_, _) => true,
                    };
                    if matches {
                        match &arm.pattern {
                            MatchPattern::Variant {
                                variant_name,
                                binding: Some((var, _ty)),
                                ..
                            } => {
                                let dt = &self.module.datatypes[enum_name.as_str()];
                                let fl =
                                    dt.fields.iter().find(|f| f.name == *variant_name).unwrap();
                                self.vars.insert(*var, enum_base + fl.offset);
                            }
                            MatchPattern::Wildcard(var, _) => {
                                self.vars.insert(*var, enum_base);
                            }
                            _ => {}
                        }
                        return self.exec_branch_place(nodes, &arm.body);
                    }
                }
                unreachable!("no matching arm in match expression");
            }
            _ => unreachable!("eval_place on non-place node: {:?}", nodes[id.0].kind),
        }
    }

    fn exec_branch_place(&mut self, nodes: &[Node], body: &[NodeId]) -> (usize, Option<usize>) {
        let (init, tail) = body.split_at(body.len() - 1);
        for &id in init {
            self.exec_stmt(nodes, id, 0);
        }
        match &nodes[tail[0].0].kind {
            NodeKind::Expr(inner) => self.eval_place(nodes, *inner),
            _ => unreachable!(),
        }
    }

    fn eval_load(&mut self, nodes: &[Node], id: NodeId) -> u64 {
        match &nodes[id.0].kind {
            NodeKind::IntegerLiteral(n) => *n as u64,
            // A sized `null#[T]` is the zero pointer.
            NodeKind::Null => 0,
            NodeKind::BooleanLiteral(b) => {
                if *b {
                    1
                } else {
                    0
                }
            }
            NodeKind::Local(var) => {
                let addr = self.vars[var];
                let ty = &nodes[id.0].ty;
                self.scalar_load(addr, ty)
            }
            NodeKind::FieldAccess { .. }
            | NodeKind::Deref(_)
            | NodeKind::Index { .. }
            | NodeKind::Slice { .. } => {
                let ty = nodes[id.0].ty.clone();
                let (addr, _) = self.eval_place(nodes, id);
                self.scalar_load(addr, &ty)
            }
            NodeKind::BinaryOp { op, left, right } => {
                let op = *op;
                let left = *left;
                let right = *right;
                let left_ty = &nodes[left.0].ty;
                if matches!(left_ty, Type::Array(_) | Type::FixedArray(_, _)) {
                    let result_ty = nodes[id.0].ty.clone();
                    let tmp = self.alloc_ty(&result_ty);
                    self.eval_into(nodes, id, tmp);
                    self.scalar_load(tmp, &result_ty)
                } else {
                    self.eval_load_binop(nodes, op, left, right)
                }
            }
            NodeKind::Not(inner) => {
                let inner = *inner;
                let ty = nodes[inner.0].ty.clone();
                let val = self.eval_load(nodes, inner);
                if ty.is_integer() {
                    // Bitwise complement, masked to the type's width.
                    truncate_to_ty(!val, &ty)
                } else {
                    // Logical not on Bool.
                    if val == 0 { 1 } else { 0 }
                }
            }
            NodeKind::Ref(_)
            | NodeKind::Unique(_)
            | NodeKind::FunctionRef(_)
            | NodeKind::MakeClosure { .. } => {
                let ty = nodes[id.0].ty.clone();
                let tmp = self.alloc_ty(&ty);
                self.eval_into(nodes, id, tmp);
                self.scalar_load(tmp, &ty)
            }
            NodeKind::Call { .. }
            | NodeKind::CallIndirect { .. }
            | NodeKind::IfExpr { .. }
            | NodeKind::Match { .. } => {
                let ty = nodes[id.0].ty.clone();
                let tmp = self.alloc_ty(&ty);
                self.eval_into(nodes, id, tmp);
                self.scalar_load(tmp, &ty)
            }
            _ => unreachable!("eval_load on non-scalar node: {:?}", nodes[id.0].kind),
        }
    }

    fn eval_load_binop(&mut self, nodes: &[Node], op: BinOp, left: NodeId, right: NodeId) -> u64 {
        match op {
            BinOp::And => {
                let lv = self.eval_load(nodes, left);
                if lv == 0 {
                    0
                } else {
                    self.eval_load(nodes, right)
                }
            }
            BinOp::Or => {
                let lv = self.eval_load(nodes, left);
                if lv != 0 {
                    lv
                } else {
                    self.eval_load(nodes, right)
                }
            }
            BinOp::WrapAdd | BinOp::WrapSub | BinOp::WrapMul => {
                // Two's-complement wrapping is bit-identical for signed and
                // unsigned, so compute on the raw 64-bit pattern and truncate to
                // the operand's width (e.g. 255u8 ++ 1u8 == 0u8).
                let ty = nodes[left.0].ty.clone();
                let a = self.eval_load(nodes, left);
                let b = self.eval_load(nodes, right);
                let raw = match op {
                    BinOp::WrapAdd => a.wrapping_add(b),
                    BinOp::WrapSub => a.wrapping_sub(b),
                    BinOp::WrapMul => a.wrapping_mul(b),
                    _ => unreachable!(),
                };
                truncate_to_ty(raw, &ty)
            }
            BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::Shl | BinOp::Shr => {
                // Bitwise ops work on the raw bit pattern; operands are loaded
                // sign-extended (signed) or zero-extended (unsigned), and the
                // result is truncated to the type's width on store. Shifts whose
                // count reaches the bit width overflow to 0 (arithmetic `>>` on a
                // signed value fills with the sign bit).
                let ty = nodes[left.0].ty.clone();
                let width = ty.int_bit_width() as u64;
                let a = self.eval_load(nodes, left);
                let b = self.eval_load(nodes, right);
                let raw = match op {
                    BinOp::BitAnd => a & b,
                    BinOp::BitOr => a | b,
                    BinOp::BitXor => a ^ b,
                    BinOp::Shl => {
                        if b >= width {
                            0
                        } else {
                            a << b
                        }
                    }
                    BinOp::Shr => {
                        if is_signed(&ty) {
                            let sh = if b >= width { width - 1 } else { b };
                            ((a as i64) >> sh) as u64
                        } else if b >= width {
                            0
                        } else {
                            a >> b
                        }
                    }
                    _ => unreachable!(),
                };
                truncate_to_ty(raw, &ty)
            }
            _ if matches!(nodes[left.0].ty, Type::NullableRefUnsized(_)) => {
                // Fat nullable reference: compare the pointer half (first 8 bytes).
                let ty = nodes[left.0].ty.clone();
                let lt = self.alloc_ty(&ty);
                self.eval_into(nodes, left, lt);
                let rt = self.alloc_ty(&ty);
                self.eval_into(nodes, right, rt);
                let a = self.mem.load(lt, 8);
                let b = self.mem.load(rt, 8);
                match op {
                    BinOp::Eq => (a == b) as u64,
                    BinOp::Ne => (a != b) as u64,
                    _ => unreachable!("only ==/!= allowed on nullable references"),
                }
            }
            _ if is_signed(&nodes[left.0].ty) => {
                let a = self.eval_load(nodes, left) as i64;
                let b = self.eval_load(nodes, right) as i64;
                match op {
                    BinOp::Add => a
                        .checked_add(b)
                        .unwrap_or_else(|| panic!("integer overflow"))
                        as u64,
                    BinOp::Sub => a
                        .checked_sub(b)
                        .unwrap_or_else(|| panic!("integer overflow"))
                        as u64,
                    BinOp::Mul => a
                        .checked_mul(b)
                        .unwrap_or_else(|| panic!("integer overflow"))
                        as u64,
                    BinOp::Div => a
                        .checked_div(b)
                        .unwrap_or_else(|| panic!("division by zero"))
                        as u64,
                    BinOp::Mod => a
                        .checked_rem(b)
                        .unwrap_or_else(|| panic!("division by zero"))
                        as u64,
                    BinOp::Eq => (a == b) as u64,
                    BinOp::Ne => (a != b) as u64,
                    BinOp::Lt => (a < b) as u64,
                    BinOp::Le => (a <= b) as u64,
                    BinOp::Gt => (a > b) as u64,
                    BinOp::Ge => (a >= b) as u64,
                    BinOp::And
                    | BinOp::Or
                    | BinOp::BitAnd
                    | BinOp::BitOr
                    | BinOp::BitXor
                    | BinOp::Shl
                    | BinOp::Shr
                    | BinOp::WrapAdd
                    | BinOp::WrapSub
                    | BinOp::WrapMul => unreachable!(),
                }
            }
            _ => {
                // Unsigned (and Bool) operands: full-range u64 semantics
                let a = self.eval_load(nodes, left);
                let b = self.eval_load(nodes, right);
                match op {
                    BinOp::Add => a
                        .checked_add(b)
                        .unwrap_or_else(|| panic!("integer overflow")),
                    BinOp::Sub => a
                        .checked_sub(b)
                        .unwrap_or_else(|| panic!("integer overflow")),
                    BinOp::Mul => a
                        .checked_mul(b)
                        .unwrap_or_else(|| panic!("integer overflow")),
                    BinOp::Div => a
                        .checked_div(b)
                        .unwrap_or_else(|| panic!("division by zero")),
                    BinOp::Mod => a
                        .checked_rem(b)
                        .unwrap_or_else(|| panic!("division by zero")),
                    BinOp::Eq => (a == b) as u64,
                    BinOp::Ne => (a != b) as u64,
                    BinOp::Lt => (a < b) as u64,
                    BinOp::Le => (a <= b) as u64,
                    BinOp::Gt => (a > b) as u64,
                    BinOp::Ge => (a >= b) as u64,
                    BinOp::And
                    | BinOp::Or
                    | BinOp::BitAnd
                    | BinOp::BitOr
                    | BinOp::BitXor
                    | BinOp::Shl
                    | BinOp::Shr
                    | BinOp::WrapAdd
                    | BinOp::WrapSub
                    | BinOp::WrapMul => unreachable!(),
                }
            }
        }
    }

    fn eval_into(&mut self, nodes: &[Node], id: NodeId, dst: usize) {
        match &nodes[id.0].kind {
            NodeKind::Local(_)
            | NodeKind::FieldAccess { .. }
            | NodeKind::Deref(_)
            | NodeKind::Index { .. }
            | NodeKind::Slice { .. } => {
                let ty = nodes[id.0].ty.clone();
                let meta = self.compute_meta(nodes, id);
                let (src, _) = self.eval_place(nodes, id);
                self.copy_value(dst, src, &ty, meta);
            }
            NodeKind::IntegerLiteral(n) => {
                let ty = nodes[id.0].ty.clone();
                self.scalar_store(dst, *n as u64, &ty);
            }
            NodeKind::BooleanLiteral(b) => {
                self.mem.store(dst, if *b { 1 } else { 0 }, 1);
            }
            NodeKind::Null => {
                // null#[T]: zero pointer (and zero meta for the fat-pointer case).
                self.mem.store(dst, 0, 8);
                if matches!(nodes[id.0].ty, Type::NullableRefUnsized(_)) {
                    self.mem.store(dst + 8, 0, 8);
                }
            }
            NodeKind::Ref(inner) => {
                let inner = *inner;
                let inner_ty = &nodes[inner.0].ty;
                if is_place(nodes, inner) {
                    let (target, target_meta) = self.eval_place(nodes, inner);
                    if is_sized(inner_ty, &self.module.datatypes) {
                        self.mem.store(dst, target as u64, 8);
                    } else {
                        let meta = target_meta.unwrap();
                        self.mem.store(dst, target as u64, 8);
                        self.mem.store(dst + 8, meta as u64, 8);
                    }
                } else {
                    let inner_ty = inner_ty.clone();
                    if is_sized(&inner_ty, &self.module.datatypes) {
                        let tmp = self.alloc_ty(&inner_ty);
                        self.eval_into(nodes, inner, tmp);
                        self.mem.store(dst, tmp as u64, 8);
                    } else {
                        let meta = self.compute_meta(nodes, inner).unwrap();
                        let tmp = self.alloc_unsized(&inner_ty, meta);
                        self.eval_into(nodes, inner, tmp);
                        self.mem.store(dst, tmp as u64, 8);
                        self.mem.store(dst + 8, meta as u64, 8);
                    }
                }
            }
            NodeKind::Unique(inner) => {
                // Unique pointer creation: always allocates fresh memory
                let inner = *inner;
                let inner_ty = nodes[inner.0].ty.clone();
                if is_sized(&inner_ty, &self.module.datatypes) {
                    let size = type_size(&inner_ty, &self.module.datatypes);
                    let align = type_align(&inner_ty, &self.module.datatypes);
                    let ptr = self.mem.alloc(size, align);
                    self.eval_into(nodes, inner, ptr);
                    self.mem.store(dst, ptr as u64, 8);
                } else {
                    let meta = self.compute_meta(nodes, inner).unwrap();
                    let size = full_size(&inner_ty, &self.module.datatypes, meta);
                    let align = type_align(&inner_ty, &self.module.datatypes);
                    let ptr = self.mem.alloc(size, align);
                    self.eval_into(nodes, inner, ptr);
                    self.mem.store(dst, ptr as u64, 8);
                    self.mem.store(dst + 8, meta as u64, 8);
                }
            }
            NodeKind::StructLiteral { name, fields } => {
                let name = name.clone();
                let field_inits: Vec<(String, NodeId)> = fields.clone();
                for (fname, fnode) in &field_inits {
                    let fl = self.module.datatypes[name.as_str()]
                        .fields
                        .iter()
                        .find(|f| f.name == *fname)
                        .unwrap();
                    let offset = fl.offset;
                    self.eval_into(nodes, *fnode, dst + offset);
                }
            }
            NodeKind::ArrayLiteral(elements) => {
                let elem_ids: Vec<NodeId> = elements.clone();
                let elem_ty = match &nodes[id.0].ty {
                    Type::Array(inner) | Type::FixedArray(inner, _) => (**inner).clone(),
                    _ => unreachable!(),
                };
                let es = type_size(&elem_ty, &self.module.datatypes);
                for (i, eid) in elem_ids.iter().enumerate() {
                    self.eval_into(nodes, *eid, dst + i * es);
                }
            }
            NodeKind::ArrayRepeat { element, count } => {
                let element = *element;
                let count = *count;
                let elem_ty = match &nodes[id.0].ty {
                    Type::Array(inner) | Type::FixedArray(inner, _) => (**inner).clone(),
                    _ => unreachable!(),
                };
                let n = self.eval_load(nodes, count) as usize;
                let es = type_size(&elem_ty, &self.module.datatypes);
                if n > 0 {
                    // Evaluate element into first slot
                    self.eval_into(nodes, element, dst);
                    // Copy first slot to remaining slots
                    for i in 1..n {
                        self.mem.memcpy(dst + i * es, dst, es);
                    }
                }
            }
            NodeKind::ArrayInit { count, init } => {
                let count = *count;
                let init = *init;
                let elem_ty = match &nodes[id.0].ty {
                    Type::Array(inner) | Type::FixedArray(inner, _) => (**inner).clone(),
                    _ => unreachable!(),
                };
                let n = self.eval_load(nodes, count) as usize;
                let es = type_size(&elem_ty, &self.module.datatypes);

                // Eval init closure into a 16-byte tmp
                let callee_ty = nodes[init.0].ty.clone();
                let callee_addr = self.alloc_ty(&callee_ty);
                self.eval_into(nodes, init, callee_addr);
                let fn_idx = self.mem.load(callee_addr, 8) as usize;
                let env_ptr = self.mem.load(callee_addr + 8, 8);

                let func_name = self.fn_index_to_name[fn_idx].to_string();
                let func = *self.functions.get(func_name.as_str()).unwrap();

                for i in 0..n {
                    // Allocate space for Uint arg and store the index
                    let arg_addr = self.mem.alloc(8, 8);
                    self.mem.store(arg_addr, i as u64, 8);

                    let saved_vars = std::mem::take(&mut self.vars);
                    let saved_meta = std::mem::take(&mut self.var_meta);

                    // Set up captured variables from env
                    for cap in &func.env_captures {
                        let ptr_addr = env_ptr as usize + cap.index * 8;
                        let var_addr = self.mem.load(ptr_addr, 8) as usize;
                        self.vars.insert(cap.var, var_addr);
                    }

                    // Set up parameter (single Uint param)
                    self.vars.insert(func.params[0].var, arg_addr);

                    self.exec_function_body(func, dst + i * es);

                    self.vars = saved_vars;
                    self.var_meta = saved_meta;
                }
            }
            NodeKind::ArraySizeCoerce { value, size } => {
                let value = *value;
                let size = *size;
                self.eval_into(nodes, value, dst);
                let actual_meta = self.compute_meta(nodes, value).unwrap();
                assert!(
                    actual_meta == size as usize,
                    "array size coercion failed: expected {size} elements, got {actual_meta}"
                );
            }
            NodeKind::BinaryOp { op, left, right } => {
                let op = *op;
                let left = *left;
                let right = *right;
                let left_ty = nodes[left.0].ty.clone();
                let result_ty = nodes[id.0].ty.clone();
                let left_inner = match &left_ty {
                    Type::Array(inner) | Type::FixedArray(inner, _) => Some((**inner).clone()),
                    _ => None,
                };
                match left_inner {
                    None => {
                        let result = self.eval_load_binop(nodes, op, left, right);
                        self.scalar_store(dst, result, &result_ty);
                    }
                    Some(inner) => {
                        let es = type_size(&inner, &self.module.datatypes);
                        let la_meta = self.compute_meta(nodes, left).unwrap();
                        let ra_meta = self.compute_meta(nodes, right).unwrap();
                        let ea = type_align(&inner, &self.module.datatypes);
                        let la = self.mem.alloc(la_meta * es, ea);
                        self.eval_into(nodes, left, la);
                        let ra = self.mem.alloc(ra_meta * es, ea);
                        self.eval_into(nodes, right, ra);
                        match op {
                            BinOp::Add => {
                                let left_bytes = la_meta * es;
                                let right_bytes = ra_meta * es;
                                self.mem.data.copy_within(la..la + left_bytes, dst);
                                self.mem
                                    .data
                                    .copy_within(ra..ra + right_bytes, dst + left_bytes);
                            }
                            BinOp::Eq | BinOp::Ne => {
                                let total = la_meta * es;
                                let eq = la_meta == ra_meta
                                    && self.mem.data[la..la + total]
                                        == self.mem.data[ra..ra + total];
                                let result = match op {
                                    BinOp::Eq => {
                                        if eq {
                                            1
                                        } else {
                                            0
                                        }
                                    }
                                    BinOp::Ne => {
                                        if !eq {
                                            1
                                        } else {
                                            0
                                        }
                                    }
                                    _ => unreachable!(),
                                };
                                self.scalar_store(dst, result, &result_ty);
                            }
                            _ => unreachable!(),
                        }
                    }
                }
            }
            NodeKind::Not(_) => {
                let result_ty = nodes[id.0].ty.clone();
                let val = self.eval_load(nodes, id);
                self.scalar_store(dst, val, &result_ty);
            }
            NodeKind::IfExpr {
                condition,
                then_body,
                else_body,
            } => {
                let condition = *condition;
                let then_body = then_body.clone();
                let else_body = else_body.clone();
                let cond = self.eval_load(nodes, condition);
                let branch = if cond != 0 { &then_body } else { &else_body };
                self.exec_branch_into(nodes, branch, dst);
            }
            NodeKind::Loop { body } => {
                // Loop expression: `break <v>` writes its value into `dst`. (As
                // with other expression-position bodies, `return` from inside is
                // not propagated by the interpreter.)
                let body = body.clone();
                self.run_loop(nodes, &body, dst, dst);
            }
            NodeKind::EnumVariant {
                enum_name,
                variant_name,
                variant_index,
                value,
            } => {
                let enum_name = enum_name.clone();
                let variant_name = variant_name.clone();
                let variant_index = *variant_index;
                let value = *value;
                // Write discriminant
                self.mem.store(dst, variant_index, 8);
                // Write value if present
                if let Some(val_id) = value {
                    let dt = &self.module.datatypes[enum_name.as_str()];
                    let fl = dt.fields.iter().find(|f| f.name == variant_name).unwrap();
                    self.eval_into(nodes, val_id, dst + fl.offset);
                }
            }
            NodeKind::Match { scrutinee, arms } => {
                let scrutinee = *scrutinee;
                let arms = arms.clone();
                // Get the scrutinee's place (address in memory)
                let enum_base = if is_place(nodes, scrutinee) {
                    let (addr, _) = self.eval_place(nodes, scrutinee);
                    addr
                } else {
                    let ty = nodes[scrutinee.0].ty.clone();
                    let tmp = self.alloc_ty(&ty);
                    self.eval_into(nodes, scrutinee, tmp);
                    tmp
                };
                // Read discriminant
                let disc = self.mem.load(enum_base, 8);
                let enum_ty = &nodes[scrutinee.0].ty;
                let enum_name = match enum_ty {
                    Type::Enum(name) => name.clone(),
                    _ => unreachable!(),
                };
                // Find matching arm
                for arm in &arms {
                    let matches = match &arm.pattern {
                        MatchPattern::Variant { variant_index, .. } => disc == *variant_index,
                        MatchPattern::Wildcard(_, _) => true,
                    };
                    if matches {
                        // Bind the pattern variable
                        match &arm.pattern {
                            MatchPattern::Variant {
                                variant_name,
                                binding: Some((var, _ty)),
                                ..
                            } => {
                                let dt = &self.module.datatypes[enum_name.as_str()];
                                let fl =
                                    dt.fields.iter().find(|f| f.name == *variant_name).unwrap();
                                self.vars.insert(*var, enum_base + fl.offset);
                            }
                            MatchPattern::Wildcard(var, _) => {
                                self.vars.insert(*var, enum_base);
                            }
                            _ => {}
                        }
                        self.exec_branch_into(nodes, &arm.body, dst);
                        return;
                    }
                }
                unreachable!("no matching arm in match expression");
            }
            NodeKind::FunctionRef(name) => {
                let idx = self.fn_name_to_index[name.as_str()];
                self.mem.store(dst, idx, 8);
                self.mem.store(dst + 8, 0, 8);
            }
            NodeKind::MakeClosure { function, captures } => {
                let function = function.clone();
                let capture_ids: Vec<NodeId> = captures.clone();
                let fn_idx = self.fn_name_to_index[function.as_str()];

                let n_captures = capture_ids.len();
                let env_ptr = if n_captures > 0 {
                    let env = self.mem.alloc(n_captures * 8, 8);
                    for (i, &cap_id) in capture_ids.iter().enumerate() {
                        // Each capture is a Ref node — eval_load gives us the address
                        let addr = self.eval_load(nodes, cap_id);
                        self.mem.store(env + i * 8, addr, 8);
                    }
                    env as u64
                } else {
                    0u64
                };

                self.mem.store(dst, fn_idx, 8);
                self.mem.store(dst + 8, env_ptr, 8);
            }
            NodeKind::Call { function, args } => {
                let function = function.clone();
                let args: Vec<NodeId> = args.clone();
                self.call_function_by_name(nodes, &function, &args, 0, dst);
            }
            NodeKind::IntrinsicCall { intrinsic, args } => {
                let intrinsic = intrinsic.clone();
                let args: Vec<NodeId> = args.clone();
                let result_ty = nodes[id.0].ty.clone();
                self.exec_intrinsic(nodes, &intrinsic, &args, &result_ty, dst);
            }
            NodeKind::CallIndirect { callee, args } => {
                let callee = *callee;
                let args: Vec<NodeId> = args.clone();

                // Load 16-byte function value
                let callee_ty = nodes[callee.0].ty.clone();
                let callee_addr = self.alloc_ty(&callee_ty);
                self.eval_into(nodes, callee, callee_addr);
                let fn_idx = self.mem.load(callee_addr, 8) as usize;
                let env_ptr = self.mem.load(callee_addr + 8, 8);
                let func_name = self.fn_index_to_name[fn_idx].to_string();

                self.call_function_by_name(nodes, &func_name, &args, env_ptr, dst);
            }
            _ => unreachable!("eval_into on statement node"),
        }
    }

    fn call_function_by_name(
        &mut self,
        nodes: &[Node],
        function: &str,
        args: &[NodeId],
        env_value: u64,
        dst: usize,
    ) {
        let func = *self.functions.get(function).unwrap();
        let param_addrs: Vec<usize> = func
            .params
            .iter()
            .zip(args.iter())
            .map(|(param, &arg)| {
                let ty = &param.ty;
                let meta = self.compute_meta(nodes, arg);
                let addr = match meta {
                    Some(m) => self.alloc_unsized(ty, m),
                    None => self.alloc_ty(ty),
                };
                self.eval_into(nodes, arg, addr);
                addr
            })
            .collect();
        let param_metas: Vec<Option<usize>> = args
            .iter()
            .map(|&arg| self.compute_meta(nodes, arg))
            .collect();

        let saved_vars = std::mem::take(&mut self.vars);
        let saved_meta = std::mem::take(&mut self.var_meta);

        // Set up captured variables from env
        for cap in &func.env_captures {
            let ptr_addr = env_value as usize + cap.index * 8;
            let var_addr = self.mem.load(ptr_addr, 8) as usize;
            self.vars.insert(cap.var, var_addr);
        }

        for ((param, addr), meta) in func.params.iter().zip(param_addrs).zip(param_metas) {
            self.vars.insert(param.var, addr);
            if let Some(m) = meta {
                self.var_meta.insert(param.var, m);
            }
        }
        self.exec_function_body(func, dst);
        self.vars = saved_vars;
        self.var_meta = saved_meta;
    }

    fn exec_intrinsic(
        &mut self,
        nodes: &[Node],
        intrinsic: &Intrinsic,
        args: &[NodeId],
        result_ty: &Type,
        dst: usize,
    ) {
        match intrinsic {
            Intrinsic::Panic => {
                assert_eq!(*result_ty, Type::Never);
                let (ref_addr, _) = self.eval_place(nodes, args[0]);
                let data_ptr = self.mem.load(ref_addr, 8) as usize;
                let data_len = self.mem.load(ref_addr + 8, 8) as usize;
                let bytes = self.mem.data[data_ptr..data_ptr + data_len].to_vec();
                let msg = String::from_utf8_lossy(&bytes);
                panic!("{msg}");
            }
            Intrinsic::FileOpen => {
                let (ref_addr, _) = self.eval_place(nodes, args[0]);
                let data_ptr = self.mem.load(ref_addr, 8) as usize;
                let data_len = self.mem.load(ref_addr + 8, 8) as usize;
                let bytes = self.mem.data[data_ptr..data_ptr + data_len].to_vec();
                let path = String::from_utf8_lossy(&bytes).into_owned();
                let flags = self.eval_load(nodes, args[1]) as i64;
                let mode = self.eval_load(nodes, args[2]) as u32;
                // No fd arena / GC here: the FileDesc is an index into a virtual
                // table of boxed streams (the compiled runtime uses a real fd).
                let fd = self.files.open(&path, flags, mode);
                self.scalar_store(dst, fd as u64, result_ty);
            }
            Intrinsic::FileClose => {
                // The virtual table keeps the stream alive (no auto-close in the
                // interpreters); evaluate the argument for any side effects.
                let _ = self.eval_load(nodes, args[0]);
            }
            Intrinsic::FileStdin => {
                self.scalar_store(dst, STDIN as u64, result_ty);
            }
            Intrinsic::FileStdout => {
                self.scalar_store(dst, STDOUT as u64, result_ty);
            }
            Intrinsic::FileRead => {
                let fd = self.eval_load(nodes, args[0]) as usize;
                let (ref_addr, _) = self.eval_place(nodes, args[1]);
                let data_ptr = self.mem.load(ref_addr, 8) as usize;
                let data_len = self.mem.load(ref_addr + 8, 8) as usize;
                let mut buf = vec![0u8; data_len];
                let n = self.files.read(fd, &mut buf);
                self.mem.data[data_ptr..data_ptr + n].copy_from_slice(&buf[..n]);
                self.scalar_store(dst, n as u64, result_ty);
            }
            Intrinsic::FileWritePartial => {
                let fd = self.eval_load(nodes, args[0]) as usize;
                let (ref_addr, _) = self.eval_place(nodes, args[1]);
                let data_ptr = self.mem.load(ref_addr, 8) as usize;
                let data_len = self.mem.load(ref_addr + 8, 8) as usize;
                let bytes = self.mem.data[data_ptr..data_ptr + data_len].to_vec();
                let n = self.files.write_partial(fd, &bytes);
                self.scalar_store(dst, n as u64, result_ty);
            }
            Intrinsic::ArrayLen => {
                let len = if let Type::FixedArray(_, n) = &nodes[args[0].0].ty {
                    *n as usize
                } else {
                    self.compute_meta(nodes, args[0]).unwrap()
                };
                self.scalar_store(dst, len as u64, result_ty);
            }
            Intrinsic::AssertArrayLen => {
                assert_eq!(*result_ty, Type::Unit);
                let arr_id = args[0];
                let expected_len = self.eval_load(nodes, args[1]) as usize;
                let actual_len = if let Type::FixedArray(_, n) = &nodes[arr_id.0].ty {
                    *n as usize
                } else {
                    self.compute_meta(nodes, arr_id).unwrap()
                };
                assert!(
                    actual_len == expected_len,
                    "array destructure: expected {expected_len} elements, got {actual_len}"
                );
            }
            Intrinsic::ThreadSpawn => {
                panic!("thread_spawn not implemented in IR interpreter");
            }
            Intrinsic::AtomicLoad => {
                // In single-threaded interpreter, atomic load is just a regular load via ref
                let ref_addr = self.eval_load(nodes, args[0]) as usize;
                let size = type_size(result_ty, &self.module.datatypes);
                self.mem.memcpy(dst, ref_addr, size);
            }
            Intrinsic::AtomicStore => {
                // In single-threaded interpreter, atomic store is just a regular store via ref
                let ref_addr = self.eval_load(nodes, args[0]) as usize;
                let val_ty = &nodes[args[1].0].ty;
                let size = type_size(val_ty, &self.module.datatypes);
                let val_addr = self.alloc_ty(val_ty);
                self.eval_into(nodes, args[1], val_addr);
                self.mem.memcpy(ref_addr, val_addr, size);
            }
            Intrinsic::AtomicExchange => {
                // In single-threaded interpreter, exchange is load old + store new
                let ref_addr = self.eval_load(nodes, args[0]) as usize;
                let size = type_size(result_ty, &self.module.datatypes);
                // Load old value into dst
                self.mem.memcpy(dst, ref_addr, size);
                // Store new value
                let val_ty = &nodes[args[1].0].ty;
                let val_addr = self.alloc_ty(val_ty);
                self.eval_into(nodes, args[1], val_addr);
                self.mem.memcpy(ref_addr, val_addr, size);
            }
            Intrinsic::AtomicCompareExchange => {
                // In single-threaded interpreter, CAS is load + memcmp + conditional store
                let ref_addr = self.eval_load(nodes, args[0]) as usize;
                let size = type_size(result_ty, &self.module.datatypes);
                let exp_ty = &nodes[args[1].0].ty;
                let new_ty = &nodes[args[2].0].ty;
                let exp_addr = self.alloc_ty(exp_ty);
                let new_addr = self.alloc_ty(new_ty);
                self.eval_into(nodes, args[1], exp_addr);
                self.eval_into(nodes, args[2], new_addr);
                // Return the old value
                self.mem.memcpy(dst, ref_addr, size);
                // Conditionally swap
                if self.mem.memeq(ref_addr, exp_addr, size) {
                    self.mem.memcpy(ref_addr, new_addr, size);
                }
            }
            Intrinsic::FutexWait => {
                panic!("futex_wait not implemented in IR interpreter");
            }
            Intrinsic::FutexWake => {
                panic!("futex_wake not implemented in IR interpreter");
            }
            Intrinsic::CountTrailingZeros | Intrinsic::CountLeadingZeros | Intrinsic::CountOnes => {
                let width = nodes[args[0].0].ty.int_bit_width();
                let raw = self.eval_load(nodes, args[0]);
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
                self.scalar_store(dst, count as u64, result_ty);
            }
            Intrinsic::CarryingMulAdd => {
                // a*b + carry + add as a 128-bit value; write low/high halves
                // through the two `&Uint64` out-params.
                let a = self.eval_load(nodes, args[0]);
                let b = self.eval_load(nodes, args[1]);
                let carry = self.eval_load(nodes, args[2]);
                let add = self.eval_load(nodes, args[3]);
                let (lo, hi) = a.carrying_mul_add(b, carry, add);
                let lo_addr = self.eval_load(nodes, args[4]) as usize;
                let hi_addr = self.eval_load(nodes, args[5]) as usize;
                self.scalar_store(lo_addr, lo, &Type::Uint64);
                self.scalar_store(hi_addr, hi, &Type::Uint64);
            }
            Intrinsic::Cast(_, _) => {
                assert!(result_ty.is_numeric(), "cast must return numeric type");
                let src_ty = &nodes[args[0].0].ty;
                let raw = self.eval_load(nodes, args[0]);
                let converted = cast_numeric(raw, src_ty, result_ty);
                self.scalar_store(dst, converted, result_ty);
            }
        }
    }

    fn exec_stmt(&mut self, nodes: &[Node], id: NodeId, ret_dst: usize) -> ControlFlow {
        match &nodes[id.0].kind {
            NodeKind::Let { var, value } => {
                let var = *var;
                let value = *value;
                let ty = nodes[value.0].ty.clone();
                let meta = self.compute_meta(nodes, value);
                let addr = match meta {
                    Some(m) if !is_sized(&ty, &self.module.datatypes) => self.alloc_unsized(&ty, m),
                    _ => self.alloc_ty(&ty),
                };
                self.eval_into(nodes, value, addr);
                self.vars.insert(var, addr);
                if let Some(m) = meta {
                    self.var_meta.insert(var, m);
                }
                ControlFlow::Normal
            }
            NodeKind::Assign { target, value } => {
                let target = *target;
                let value = *value;
                let (place, target_meta) = self.eval_place(nodes, target);
                if let Some(target_len) = target_meta {
                    let value_len = self.compute_meta(nodes, value).unwrap();
                    assert!(
                        target_len == value_len,
                        "unsized assignment: length mismatch ({target_len} vs {value_len})"
                    );
                }
                self.eval_into(nodes, value, place);
                ControlFlow::Normal
            }
            NodeKind::If {
                condition,
                then_body,
                else_body,
            } => {
                let condition = *condition;
                let then_body = then_body.clone();
                let else_body = else_body.clone();
                let cond = self.eval_load(nodes, condition);
                if cond != 0 {
                    self.exec_body(nodes, &then_body, ret_dst)
                } else if !else_body.is_empty() {
                    self.exec_body(nodes, &else_body, ret_dst)
                } else {
                    ControlFlow::Normal
                }
            }
            NodeKind::Loop { body } => {
                // A statement-position loop (while/for, or a bare `loop`): any
                // break value is written into a throwaway slot of the loop's type.
                let body = body.clone();
                let ty = nodes[id.0].ty.clone();
                let dst = if matches!(ty, Type::Unit | Type::Never) {
                    0
                } else {
                    self.alloc_ty(&ty)
                };
                self.run_loop(nodes, &body, dst, ret_dst)
            }
            NodeKind::Break(value) => {
                if let Some(v) = *value {
                    let dst = *self.loop_dst.last().expect("break value outside a loop");
                    self.eval_into(nodes, v, dst);
                }
                ControlFlow::Break
            }
            NodeKind::Continue => ControlFlow::Continue,
            NodeKind::Expr(inner) => {
                let inner = *inner;
                let ty = &nodes[inner.0].ty;
                if *ty == Type::Unit {
                    self.eval_into(nodes, inner, 0);
                } else {
                    let tmp = self.alloc_ty(ty);
                    self.eval_into(nodes, inner, tmp);
                }
                ControlFlow::Normal
            }
            NodeKind::Return(inner) => {
                let inner = *inner;
                self.eval_into(nodes, inner, ret_dst);
                ControlFlow::Return
            }
            _ => unreachable!(),
        }
    }

    /// Run a loop body repeatedly until it breaks. `dst` is where `break <v>`
    /// writes its value. Returns the resulting control flow (`Break` becomes
    /// `Normal`; `Return` propagates).
    fn run_loop(
        &mut self,
        nodes: &[Node],
        body: &[NodeId],
        dst: usize,
        ret_dst: usize,
    ) -> ControlFlow {
        self.loop_dst.push(dst);
        let result = loop {
            match self.exec_body(nodes, body, ret_dst) {
                ControlFlow::Break => break ControlFlow::Normal,
                ControlFlow::Return => break ControlFlow::Return,
                // A fall-through or `continue` starts the next iteration.
                ControlFlow::Normal | ControlFlow::Continue => {}
            }
        };
        self.loop_dst.pop();
        result
    }

    fn exec_body(&mut self, nodes: &[Node], body: &[NodeId], ret_dst: usize) -> ControlFlow {
        for &id in body {
            match self.exec_stmt(nodes, id, ret_dst) {
                ControlFlow::Normal => {}
                cf => return cf,
            }
        }
        ControlFlow::Normal
    }

    fn exec_branch_into(&mut self, nodes: &[Node], body: &[NodeId], dst: usize) {
        let has_tail = body
            .last()
            .is_some_and(|&id| matches!(nodes[id.0].kind, NodeKind::Expr(_)));

        let (init, tail) = if has_tail {
            let (init, tail) = body.split_at(body.len() - 1);
            (init, Some(tail[0]))
        } else {
            (body, None)
        };

        for &id in init {
            self.exec_stmt(nodes, id, dst);
        }

        if let Some(tid) = tail {
            match &nodes[tid.0].kind {
                NodeKind::Expr(inner) => self.eval_into(nodes, *inner, dst),
                _ => unreachable!(),
            }
        }
    }

    fn exec_function_body(&mut self, func: &Function, ret_dst: usize) {
        let nodes = &func.nodes;
        let body = &func.body;

        let has_tail = func.return_type != Type::Unit
            && body
                .last()
                .is_some_and(|&id| matches!(nodes[id.0].kind, NodeKind::Expr(_)));

        let (init, tail) = if has_tail {
            let (init, tail) = body.split_at(body.len() - 1);
            (init, Some(tail[0]))
        } else {
            (body.as_slice(), None)
        };

        for &id in init {
            match self.exec_stmt(nodes, id, ret_dst) {
                ControlFlow::Return => return,
                ControlFlow::Normal => {}
                ControlFlow::Break => unreachable!("break outside loop"),
                ControlFlow::Continue => unreachable!("continue outside loop"),
            }
        }

        if let Some(tid) = tail {
            match &nodes[tid.0].kind {
                NodeKind::Expr(inner) => self.eval_into(nodes, *inner, ret_dst),
                _ => unreachable!(),
            }
        }
    }

    fn run(&mut self) {
        let main_func = *self
            .functions
            .get("main")
            .unwrap_or_else(|| panic!("no main function"));
        assert!(main_func.params.is_empty(), "main must take no parameters");
        self.exec_function_body(main_func, 0);
    }
}

pub fn interpret(module: &Module) {
    let mut interp = Interpreter::new(module, std::io::stdin(), std::io::stdout());
    interp.run();
}

pub fn interpret_to(module: &Module, stdin: impl Read, stdout: impl Write) {
    let mut interp = Interpreter::new(module, stdin, stdout);
    interp.run();
}
