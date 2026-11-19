//! Constant evaluation for the scalar expressions used by system headers.
//! Integer promotions, signedness, and literal suffixes follow the LP64 C ABI.

use super::lex::{Cursor, Token, ident};
use super::parse::{CType, Unit};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Scalar {
    Void,
    Bool,
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    Long,
    ULong,
    I64,
    U64,
    F32,
    F64,
}

impl Scalar {
    pub fn size(self) -> u64 {
        match self {
            Self::Void => 0,
            Self::Bool | Self::I8 | Self::U8 => 1,
            Self::I16 | Self::U16 => 2,
            Self::I32 | Self::U32 | Self::F32 => 4,
            _ => 8,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Void => "void",
            Self::Bool => "Bool",
            Self::I8 => "Int8",
            Self::U8 => "Uint8",
            Self::I16 => "Int16",
            Self::U16 => "Uint16",
            Self::I32 => "Int32",
            Self::U32 => "Uint32",
            Self::Long => "Int",
            Self::ULong => "Uint",
            Self::I64 => "Int64",
            Self::U64 => "Uint64",
            Self::F32 => "Float32",
            Self::F64 => "Float64",
        }
    }
    fn unsigned(self) -> bool {
        matches!(
            self,
            Self::Bool | Self::U8 | Self::U16 | Self::U32 | Self::ULong | Self::U64
        )
    }
    fn rank(self) -> u8 {
        match self {
            Self::Bool => 0,
            Self::I8 | Self::U8 => 1,
            Self::I16 | Self::U16 => 2,
            Self::I32 | Self::U32 => 3,
            Self::Long | Self::ULong => 4,
            Self::I64 | Self::U64 => 5,
            Self::F32 => 6,
            Self::F64 => 7,
            Self::Void => 0,
        }
    }
    fn promote(self) -> Self {
        if self.rank() < 3 { Self::I32 } else { self }
    }
    fn unsigned_version(self) -> Self {
        match self {
            Self::I32 => Self::U32,
            Self::Long => Self::ULong,
            Self::I64 => Self::U64,
            _ => self,
        }
    }
    fn bounds(self) -> (i128, i128) {
        if self.unsigned() {
            (0, (1i128 << (self.size() * 8)) - 1)
        } else {
            let half = 1i128 << (self.size() * 8 - 1);
            (-half, half - 1)
        }
    }
    fn convert(self, value: i128) -> i128 {
        if self == Self::Bool {
            return i128::from(value != 0);
        }
        let bits = self.size() * 8;
        let value = value & ((1i128 << bits) - 1);
        if !self.unsigned() && value >= 1i128 << (bits - 1) {
            value - (1i128 << bits)
        } else {
            value
        }
    }
}

#[derive(Clone, Debug)]
pub(super) enum Value {
    Int(i128, Scalar),
    Float(f64, Scalar),
    Bytes(Vec<u8>),
    Null(CType),
    Array(Vec<Value>, CType),
}

impl Value {
    fn truth(&self) -> Result<bool, String> {
        match self {
            Self::Int(n, _) => Ok(*n != 0),
            Self::Float(n, _) => Ok(*n != 0.0),
            Self::Null(_) => Ok(false),
            _ => Err("unsupported C condition".into()),
        }
    }
    fn scalar(&self) -> Result<Scalar, String> {
        match self {
            Self::Int(_, s) | Self::Float(_, s) => Ok(*s),
            _ => Err("expected C scalar constant".into()),
        }
    }
    fn number(&self) -> Result<f64, String> {
        match self {
            Self::Int(n, _) => Ok(*n as f64),
            Self::Float(n, _) => Ok(*n),
            _ => Err("expected C numeric constant".into()),
        }
    }
    pub fn cast(self, ty: &CType, unit: &Unit) -> Result<Self, String> {
        match unit.resolve(ty)? {
            CType::Scalar(s @ (Scalar::F32 | Scalar::F64)) => {
                let n = self.number()?;
                let n = if *s == Scalar::F32 {
                    n as f32 as f64
                } else {
                    n
                };
                if !n.is_finite() {
                    return Err("non-finite C constant is not representable in Solar source".into());
                }
                Ok(Self::Float(n, *s))
            }
            CType::Scalar(Scalar::Void) => Err("void C constant is not representable".into()),
            CType::Scalar(Scalar::Bool) => Ok(Self::Int(i128::from(self.truth()?), Scalar::Bool)),
            CType::Scalar(s) => {
                let n = match self {
                    Self::Int(n, _) => n,
                    Self::Float(n, _)
                        if n.is_finite()
                            && n >= s.bounds().0 as f64
                            && n < s.bounds().1 as f64 + 1.0 =>
                    {
                        n as i128
                    }
                    _ => return Err("C constant conversion is not representable".into()),
                };
                Ok(Self::Int(s.convert(n), *s))
            }
            CType::Pointer(inner) if matches!(self, Self::Int(0, _) | Self::Null(_)) => {
                Ok(Self::Null(*inner.clone()))
            }
            CType::Pointer(_) => {
                Err("non-null C pointer constant is not representable in Solar source".into())
            }
            CType::Array(inner, count) => {
                let count = unit.array_len(count)? as usize;
                let values = match self {
                    Self::Bytes(bytes) => bytes
                        .into_iter()
                        .map(|n| Self::Int(n.into(), Scalar::I32))
                        .collect(),
                    Self::Array(values, _) => values,
                    _ => return Err("unsupported C array initializer".into()),
                };
                if values.len() > count {
                    return Err("too many C array initializers".into());
                }
                if count > 1_000_000 {
                    return Err("C constant array is too large".into());
                }
                let mut result = values
                    .into_iter()
                    .map(|v| v.cast(inner, unit))
                    .collect::<Result<Vec<_>, _>>()?;
                while result.len() < count {
                    result.push(Self::Int(0, Scalar::I32).cast(inner, unit)?);
                }
                Ok(Self::Array(result, *inner.clone()))
            }
            _ => Err("C constant type is not representable in Solar source".into()),
        }
    }
}

fn common(a: Scalar, b: Scalar) -> Scalar {
    let (a, b) = (a.promote(), b.promote());
    if a == b {
        return a;
    }
    if matches!(a, Scalar::F32 | Scalar::F64) || matches!(b, Scalar::F32 | Scalar::F64) {
        return if a.rank() > b.rank() { a } else { b };
    }
    let (high, low) = if a.rank() > b.rank() { (a, b) } else { (b, a) };
    if high.unsigned() == low.unsigned() || high.unsigned() || high.size() > low.size() {
        high
    } else {
        high.unsigned_version()
    }
}

pub(super) fn evaluate(tokens: &[Token], unit: &Unit) -> Result<Value, String> {
    Evaluator {
        cursor: Cursor::new(tokens),
        unit,
        preprocessor: false,
        depth: 0,
        defining_enum: None,
    }
    .finish()
}

pub(super) fn condition(tokens: &[Token]) -> Result<bool, String> {
    Evaluator {
        cursor: Cursor::new(tokens),
        unit: &Unit::default(),
        preprocessor: true,
        depth: 0,
        defining_enum: None,
    }
    .finish()?
    .truth()
}

pub(super) fn constant(name: &str, unit: &Unit) -> Result<Value, String> {
    lookup(name, unit, false, 0, None)
}

fn lookup(
    name: &str,
    unit: &Unit,
    preprocessor: bool,
    depth: usize,
    defining_enum: Option<usize>,
) -> Result<Value, String> {
    if preprocessor {
        return Ok(Value::Int(0, Scalar::I64));
    }
    if depth > 100 {
        return Err(format!("recursive C constant `{name}`"));
    }
    let declaration = unit
        .constants
        .get(name)
        .ok_or_else(|| format!("cannot evaluate C constant `{name}`"))?;
    if !declaration.is_const {
        return Err(format!(
            "C variable `{name}` is not a constant; C FFI is not supported"
        ));
    }
    let cached = unit.enum_values.borrow().get(name).cloned();
    let value = if let Some(value) = cached {
        value
    } else {
        let value = if let Some(previous) = &declaration.previous_enumerator {
            let Value::Int(n, _) =
                lookup(previous, unit, false, depth + 1, declaration.enum_group)?
            else {
                return Err("non-integer C enumerator".into());
            };
            let n = n + 1;
            let ty = [Scalar::I32, Scalar::U32, Scalar::Long, Scalar::ULong]
                .into_iter()
                .find(|s| (s.bounds().0..=s.bounds().1).contains(&n))
                .ok_or("C enumerator overflow")?;
            Value::Int(n, ty)
        } else {
            Evaluator {
                cursor: Cursor::new(&declaration.value),
                unit,
                preprocessor,
                depth: depth + 1,
                defining_enum: declaration.enum_group,
            }
            .finish()?
        };
        if declaration.enum_group.is_some() {
            let Value::Int(n, s) = value else {
                return Err("non-integer C enumerator".into());
            };
            let value = Value::Int(
                n,
                if (i32::MIN as i128..=i32::MAX as i128).contains(&n) {
                    Scalar::I32
                } else {
                    s
                },
            );
            unit.enum_values
                .borrow_mut()
                .insert(name.into(), value.clone());
            value
        } else {
            value
        }
    };
    if let Some(ty) = &declaration.ty {
        value.cast(ty, unit)
    } else if let Some(group) = declaration
        .enum_group
        .filter(|group| Some(*group) != defining_enum)
    {
        let cached = unit.enum_types.borrow().get(&group).copied();
        let ty = if let Some(ty) = cached {
            ty
        } else {
            let mut min = 0;
            let mut max = 0;
            for member in &unit.enums[group] {
                let Value::Int(n, _) = lookup(member, unit, false, depth + 1, Some(group))? else {
                    return Err("non-integer C enumerator".into());
                };
                min = min.min(n);
                max = max.max(n);
            }
            // Enum values undergo integer promotion at the import boundary.
            // The compatible type is selected for the complete declaration.
            let ty = if min >= i32::MIN as i128 && max <= i32::MAX as i128 {
                Scalar::I32
            } else if min < 0 && max <= i64::MAX as i128 {
                Scalar::Long
            } else if min < 0 {
                return Err("C enum range is not representable".into());
            } else if max <= u32::MAX as i128 {
                Scalar::U32
            } else {
                Scalar::ULong
            };
            unit.enum_types.borrow_mut().insert(group, ty);
            ty
        };
        value.cast(&CType::Scalar(ty), unit)
    } else {
        Ok(value)
    }
}

struct Evaluator<'a> {
    cursor: Cursor<'a>,
    unit: &'a Unit,
    preprocessor: bool,
    depth: usize,
    defining_enum: Option<usize>,
}

impl Evaluator<'_> {
    fn finish(mut self) -> Result<Value, String> {
        let result = self.expression(0, true)?;
        if self.cursor.pos != self.cursor.tokens.len() {
            return Err(format!(
                "unsupported token `{}` in C constant expression",
                self.cursor.peek()
            ));
        }
        Ok(result)
    }

    fn expression(&mut self, minimum: u8, active: bool) -> Result<Value, String> {
        let mut lhs = self.prefix(active)?;
        loop {
            let op = self.cursor.peek();
            let priority = match op {
                "||" => 2,
                "&&" => 3,
                "|" => 4,
                "^" => 5,
                "&" => 6,
                "==" | "!=" => 7,
                "<" | "<=" | ">" | ">=" => 8,
                "<<" | ">>" => 9,
                "+" | "-" => 10,
                "*" | "/" | "%" => 11,
                "?" => 1,
                _ => break,
            };
            if priority < minimum {
                break;
            }
            let op = self.cursor.take()?.text.clone();
            if op == "?" {
                let condition = lhs.truth()?;
                let yes = self.expression(0, active && condition)?;
                self.cursor.expect(":")?;
                let no = self.expression(priority, active && !condition)?;
                let ty = common(yes.scalar()?, no.scalar()?);
                lhs = if condition { yes } else { no }.cast(&CType::Scalar(ty), self.unit)?;
            } else {
                let rhs_active = active
                    && match op.as_str() {
                        "&&" => lhs.truth()?,
                        "||" => !lhs.truth()?,
                        _ => true,
                    };
                let rhs = self.expression(priority + 1, rhs_active)?;
                lhs = binary(&op, lhs, rhs, active, self.unit)?;
                if self.preprocessor {
                    let ty = if lhs.scalar()?.unsigned() {
                        Scalar::U64
                    } else {
                        Scalar::I64
                    };
                    lhs = lhs.cast(&CType::Scalar(ty), self.unit)?;
                }
            }
        }
        Ok(lhs)
    }

    fn prefix(&mut self, active: bool) -> Result<Value, String> {
        let token = self.cursor.take()?;
        match token.text.as_str() {
            "+" | "-" | "!" | "~" => {
                let value = self.prefix(active)?;
                if token.text == "!" {
                    return Ok(Value::Int(
                        i128::from(!value.truth()?),
                        if self.preprocessor {
                            Scalar::I64
                        } else {
                            Scalar::I32
                        },
                    ));
                }
                let value = value
                    .clone()
                    .cast(&CType::Scalar(value.scalar()?.promote()), self.unit)?;
                match value {
                    Value::Int(n, s) => {
                        let n = match token.text.as_str() {
                            "-" => -n,
                            "~" => !n,
                            _ => n,
                        };
                        integer_result(n, s, active && token.text != "~")
                    }
                    Value::Float(n, s) if token.text != "~" => {
                        Ok(Value::Float(if token.text == "-" { -n } else { n }, s))
                    }
                    _ => Err("unsupported unary C operation".into()),
                }
            }
            "sizeof" | "_Alignof" | "__alignof__" if !self.preprocessor => {
                self.cursor.expect("(")?;
                let tokens = self.cursor.until(&[")"])?;
                self.cursor.expect(")")?;
                let ty = self.unit.type_name(tokens)?;
                let (size, align) = self.unit.layout(&ty)?;
                Ok(Value::Int(
                    if token.text == "sizeof" { size } else { align }.into(),
                    Scalar::ULong,
                ))
            }
            "(" => {
                let start = self.cursor.pos;
                let tokens = self.cursor.until(&[")"])?;
                if !self.preprocessor
                    && let Ok(ty) = self.unit.type_name(tokens)
                {
                    self.cursor.expect(")")?;
                    return self.prefix(active)?.cast(&ty, self.unit);
                }
                self.cursor.pos = start;
                let value = self.expression(0, active)?;
                self.cursor.expect(")")?;
                Ok(value)
            }
            "{" if !self.preprocessor => {
                let mut values = Vec::new();
                while !self.cursor.eat("}") {
                    values.push(self.expression(0, active)?);
                    if !self.cursor.eat(",") {
                        self.cursor.expect("}")?;
                        break;
                    }
                }
                Ok(Value::Array(values, CType::Scalar(Scalar::I32)))
            }
            text if text.starts_with('"') => {
                if self.preprocessor {
                    return Err("string in C preprocessing condition".into());
                }
                let mut bytes = decode(text)?;
                while self.cursor.peek().starts_with('"') {
                    bytes.extend(decode(&self.cursor.take()?.text)?);
                }
                bytes.push(0);
                Ok(Value::Bytes(bytes))
            }
            text if text.starts_with('\'') => {
                let bytes = decode(text)?;
                if bytes.len() != 1 {
                    return Err("multi-character C constants are not supported".into());
                }
                Ok(Value::Int(
                    (bytes[0] as i8).into(),
                    if self.preprocessor {
                        Scalar::I64
                    } else {
                        Scalar::I32
                    },
                ))
            }
            text if ident(text) => lookup(
                text,
                self.unit,
                self.preprocessor,
                self.depth,
                self.defining_enum,
            ),
            text => number(text, self.preprocessor),
        }
    }
}

fn integer_result(value: i128, ty: Scalar, check: bool) -> Result<Value, String> {
    if check && !ty.unsigned() && !(ty.bounds().0..=ty.bounds().1).contains(&value) {
        return Err("signed overflow in C constant expression".into());
    }
    Ok(Value::Int(ty.convert(value), ty))
}

fn binary(op: &str, lhs: Value, rhs: Value, active: bool, unit: &Unit) -> Result<Value, String> {
    if op == "&&" || op == "||" {
        return Ok(Value::Int(
            i128::from(if op == "&&" {
                lhs.truth()? && rhs.truth()?
            } else {
                lhs.truth()? || rhs.truth()?
            }),
            Scalar::I32,
        ));
    }
    if op == "<<" || op == ">>" {
        let (Value::Int(a, s), Value::Int(b, _)) = (lhs, rhs) else {
            return Err("non-integer C shift".into());
        };
        let s = s.promote();
        if !active {
            return Ok(Value::Int(0, s));
        }
        if b < 0 || b >= (s.size() * 8).into() {
            return Err("invalid shift count in C constant".into());
        }
        if op == "<<" {
            if a < 0 {
                return Err("left shift of negative C constant".into());
            }
            integer_result(a << b, s, true)
        } else {
            integer_result(s.convert(a) >> b, s, true)
        }
    } else {
        let ty = common(lhs.scalar()?, rhs.scalar()?);
        let lhs = lhs.cast(&CType::Scalar(ty), unit)?;
        let rhs = rhs.cast(&CType::Scalar(ty), unit)?;
        let comparison = |a: std::cmp::Ordering| match op {
            "==" => a.is_eq(),
            "!=" => !a.is_eq(),
            "<" => a.is_lt(),
            "<=" => !a.is_gt(),
            ">" => a.is_gt(),
            ">=" => !a.is_lt(),
            _ => false,
        };
        let is_comparison = matches!(op, "==" | "!=" | "<" | "<=" | ">" | ">=");
        match (lhs, rhs) {
            (Value::Int(a, _), Value::Int(b, _)) => {
                if is_comparison {
                    return Ok(Value::Int(i128::from(comparison(a.cmp(&b))), Scalar::I32));
                }
                if !active {
                    return Ok(Value::Int(0, ty));
                }
                let n = match op {
                    "+" => a.checked_add(b),
                    "-" => a.checked_sub(b),
                    "*" if ty.unsigned() => Some(a.wrapping_mul(b)),
                    "*" => a.checked_mul(b),
                    "/" => a.checked_div(b),
                    "%" => a.checked_rem(b),
                    "&" => Some(a & b),
                    "|" => Some(a | b),
                    "^" => Some(a ^ b),
                    _ => return Err("unsupported integer C operator".into()),
                }
                .ok_or("overflow or division by zero in C constant expression")?;
                integer_result(n, ty, true)
            }
            (Value::Float(a, _), Value::Float(b, _)) => {
                if is_comparison {
                    return Ok(Value::Int(
                        i128::from(comparison(
                            a.partial_cmp(&b).ok_or("non-finite C comparison")?,
                        )),
                        Scalar::I32,
                    ));
                }
                if !active {
                    return Ok(Value::Float(0.0, ty));
                }
                let n = match op {
                    "+" => a + b,
                    "-" => a - b,
                    "*" => a * b,
                    "/" => a / b,
                    _ => return Err("unsupported floating-point C operator".into()),
                };
                Value::Float(n, ty).cast(&CType::Scalar(ty), unit)
            }
            _ => Err("unsupported C constant expression".into()),
        }
    }
}

fn number(text: &str, preprocessor: bool) -> Result<Value, String> {
    let hex = text.starts_with("0x") || text.starts_with("0X");
    if text.contains('.')
        || if hex {
            text.contains(['p', 'P'])
        } else {
            text.contains(['e', 'E'])
        }
    {
        if preprocessor {
            return Err("floating-point C preprocessing condition".into());
        }
        let (text, ty) = if text.ends_with(['f', 'F']) {
            (&text[..text.len() - 1], Scalar::F32)
        } else {
            (text, Scalar::F64)
        };
        let value: f64 = text
            .parse()
            .map_err(|_| format!("unsupported C floating literal `{text}`"))?;
        return Value::Float(value, ty).cast(&CType::Scalar(ty), &Unit::default());
    }
    let digits = text.trim_end_matches(['u', 'U', 'l', 'L']);
    let suffix = text[digits.len()..].to_ascii_lowercase();
    let (radix, digits) = if hex {
        (16, &digits[2..])
    } else if digits.starts_with("0b") || digits.starts_with("0B") {
        (2, &digits[2..])
    } else if digits.starts_with('0') && digits.len() > 1 {
        (8, &digits[1..])
    } else {
        (10, digits)
    };
    let n = i128::from_str_radix(digits, radix)
        .map_err(|_| format!("unsupported C integer literal `{text}`"))?;
    let candidates: &[Scalar] = match (suffix.as_str(), radix == 10) {
        ("", true) => &[Scalar::I32, Scalar::Long, Scalar::I64],
        ("", false) => &[
            Scalar::I32,
            Scalar::U32,
            Scalar::Long,
            Scalar::ULong,
            Scalar::I64,
            Scalar::U64,
        ],
        ("u", _) => &[Scalar::U32, Scalar::ULong, Scalar::U64],
        ("l", true) => &[Scalar::Long, Scalar::I64],
        ("l", false) => &[Scalar::Long, Scalar::ULong, Scalar::I64, Scalar::U64],
        ("ul" | "lu", _) => &[Scalar::ULong, Scalar::U64],
        ("ll", true) => &[Scalar::I64],
        ("ll", false) => &[Scalar::I64, Scalar::U64],
        ("ull" | "llu", _) => &[Scalar::U64],
        _ => return Err(format!("unsupported C integer suffix `{suffix}`")),
    };
    let ty = candidates
        .iter()
        .copied()
        .find(|ty| n <= ty.bounds().1)
        .ok_or("C integer literal is too large")?;
    let ty = if preprocessor {
        if ty.unsigned() {
            Scalar::U64
        } else {
            Scalar::I64
        }
    } else {
        ty
    };
    Ok(Value::Int(n, ty))
}

fn decode(text: &str) -> Result<Vec<u8>, String> {
    let bytes = text.as_bytes();
    let mut result = Vec::new();
    let mut i = 1;
    while i < bytes.len() - 1 {
        let c = bytes[i];
        i += 1;
        if c != b'\\' {
            result.push(c);
            continue;
        }
        let c = bytes[i];
        i += 1;
        result.push(match c {
            b'a' => 7,
            b'b' => 8,
            b'f' => 12,
            b'n' => 10,
            b'r' => 13,
            b't' => 9,
            b'v' => 11,
            b'\\' | b'\'' | b'"' | b'?' => c,
            b'0'..=b'7' | b'x' => {
                let radix = if c == b'x' { 16 } else { 8 };
                let start = if c == b'x' { i } else { i - 1 };
                let max = if radix == 8 {
                    (start + 3).min(bytes.len() - 1)
                } else {
                    bytes.len() - 1
                };
                while i < max && (bytes[i] as char).is_digit(radix) {
                    i += 1;
                }
                u8::from_str_radix(&text[start..i], radix)
                    .map_err(|_| "unsupported C character escape")?
            }
            _ => return Err("unsupported C character escape".into()),
        });
    }
    Ok(result)
}
