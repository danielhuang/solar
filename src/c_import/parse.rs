//! Declaration parsing is deliberately shallow for unrequested declarations.
//! Unsupported field/type syntax is retained as an error, never a guessed ABI.

use super::expr::{Scalar, Value, evaluate};
use super::lex::{Cursor, Token, ident};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};

#[derive(Clone, Debug)]
pub(super) enum CType {
    Scalar(Scalar),
    Named(String),
    Record(String),
    Pointer(Box<CType>),
    Array(Box<CType>, Vec<Token>),
    Const(Box<CType>),
    Unsupported(String),
    Function,
}

impl CType {
    pub fn unqualified(&self) -> &Self {
        if let Self::Const(inner) = self {
            inner.unqualified()
        } else {
            self
        }
    }
}

#[derive(Clone)]
pub(super) struct Record {
    pub origin: String,
    pub fields: Result<Vec<(String, CType)>, String>,
}

#[derive(Clone)]
pub(super) struct Constant {
    pub ty: Option<CType>,
    pub value: Vec<Token>,
    pub is_const: bool,
    pub enum_group: Option<usize>,
    pub previous_enumerator: Option<String>,
}

#[derive(Clone, Default)]
pub(super) struct Unit {
    pub types: BTreeMap<String, CType>,
    pub records: HashMap<String, Record>,
    pub constants: BTreeMap<String, Constant>,
    pub functions: Vec<String>,
    pub enums: Vec<Vec<String>>,
    pub enum_values: RefCell<HashMap<String, Value>>,
    pub enum_types: RefCell<HashMap<usize, Scalar>>,
    packed: bool,
}

impl Unit {
    pub fn parse(tokens: &[Token]) -> Result<Self, String> {
        let mut unit = Self::default();
        let mut cursor = Cursor::new(tokens);
        while cursor.pos < tokens.len() {
            if cursor.eat(";") {
                continue;
            }
            if cursor.eat("__solar_unsupported_pack") {
                unit.packed = true;
                cursor.expect(";")?;
                continue;
            }
            let start = cursor.pos;
            let declaration = declaration(&mut cursor, &mut unit);
            match declaration {
                Ok((is_typedef, declarations)) => {
                    for (name, ty, value) in declarations {
                        if name.is_empty() {
                            continue;
                        }
                        if matches!(ty.unqualified(), CType::Function) {
                            unit.functions.push(name);
                        } else if is_typedef {
                            unit.types.insert(name, ty);
                        } else {
                            let is_const = const_type(&ty, &unit, 0);
                            unit.constants.insert(
                                name,
                                Constant {
                                    ty: Some(ty),
                                    value,
                                    is_const,
                                    enum_group: None,
                                    previous_enumerator: None,
                                },
                            );
                        }
                    }
                }
                Err(error) => {
                    // Function prototypes/bodies may contain syntax irrelevant
                    // to imports. Never recover inside a requested record: its
                    // field parser saves the error on the record itself.
                    cursor.pos = start;
                    let skipped = cursor.until(&[";"])?;
                    for window in skipped.windows(2) {
                        if ident(&window[0].text) && window[1].text == "(" {
                            unit.functions.push(window[0].text.clone());
                        }
                    }
                    if skipped.first().is_some_and(|t| t.text == "typedef") {
                        return Err(format!("{}: {error}", skipped[0].location()));
                    }
                    cursor.eat(";");
                }
            }
            if cursor.pos == start {
                return Err("cannot parse C declaration".into());
            }
        }
        Ok(unit)
    }

    pub fn resolve<'a>(&'a self, mut ty: &'a CType) -> Result<&'a CType, String> {
        for _ in 0..100 {
            match ty {
                CType::Named(name) => {
                    ty = self
                        .types
                        .get(name)
                        .ok_or_else(|| format!("unknown C type `{name}`"))?
                }
                CType::Const(inner) => ty = inner,
                CType::Unsupported(error) => return Err(error.clone()),
                _ => return Ok(ty),
            }
        }
        Err("recursive C typedef is not representable in Solar source".into())
    }

    pub fn type_name(&self, tokens: &[Token]) -> Result<CType, String> {
        let mut cursor = Cursor::new(tokens);
        let mut words = Vec::new();
        let mut qualifiers = Vec::new();
        while matches!(
            cursor.peek(),
            "const"
                | "__const"
                | "__const__"
                | "volatile"
                | "restrict"
                | "__restrict"
                | "__restrict__"
        ) {
            qualifiers.push(cursor.take()?.text.clone());
        }
        let mut ty = if matches!(cursor.peek(), "struct" | "union" | "enum") {
            let tag = cursor.take()?.text.clone();
            let name = cursor.take()?.text.clone();
            CType::Record(format!("{tag} {name}"))
        } else {
            while scalar_word(cursor.peek()) {
                words.push(cursor.take()?.text.clone());
            }
            if words.is_empty() {
                let name = cursor.take()?.text.clone();
                if !self.types.contains_key(&name) {
                    return Err(format!("unknown C type `{name}`"));
                }
                CType::Named(name)
            } else {
                scalar_type(&words)
            }
        };
        for qualifier in qualifiers {
            ty = qualify(ty, &qualifier);
        }
        let declarator = declarator(&mut cursor)?;
        let (name, ty) = declarator.apply(ty);
        if !name.is_empty() || cursor.pos != tokens.len() {
            return Err("unsupported C type name".into());
        }
        Ok(ty)
    }

    pub fn array_len(&self, tokens: &[Token]) -> Result<u64, String> {
        if tokens.is_empty() {
            return Err("flexible C arrays are not representable in Solar source".into());
        }
        let Value::Int(value, _) = evaluate(tokens, self)? else {
            return Err("C array bound must be an integer".into());
        };
        let count = u64::try_from(value).map_err(|_| "invalid C array bound")?;
        if count == 0 {
            return Err("zero-sized C arrays are not representable in Solar source".into());
        }
        Ok(count)
    }

    pub fn layout(&self, ty: &CType) -> Result<(u64, u64), String> {
        self.layout_inner(ty, &mut Vec::new())
    }

    fn layout_inner(&self, ty: &CType, visiting: &mut Vec<String>) -> Result<(u64, u64), String> {
        match self.resolve(ty)? {
            CType::Scalar(Scalar::Void) => Err("void has no C object layout".into()),
            CType::Scalar(scalar) => Ok((scalar.size(), scalar.size())),
            CType::Pointer(_) => Ok((8, 8)),
            CType::Array(element, count) => {
                let (size, align) = self.layout_inner(element, visiting)?;
                Ok((
                    size.checked_mul(self.array_len(count)?)
                        .ok_or("C array size overflow")?,
                    align,
                ))
            }
            CType::Record(key) => {
                if visiting.contains(key) {
                    return Err("recursive C struct layout is not representable".into());
                }
                visiting.push(key.clone());
                let record = self.records.get(key).ok_or_else(|| {
                    format!("incomplete C struct `{key}` is not representable in Solar source")
                })?;
                let fields = record.fields.as_ref().map_err(Clone::clone)?;
                if fields.is_empty() {
                    return Err("empty C structs are not representable in Solar source".into());
                }
                let mut size = 0u64;
                let mut align = 1;
                for (_, ty) in fields {
                    let (field_size, field_align) = self.layout_inner(ty, visiting)?;
                    align = align.max(field_align);
                    size = round_up(size, field_align)?
                        .checked_add(field_size)
                        .ok_or("C struct size overflow")?;
                }
                visiting.pop();
                Ok((round_up(size, align)?, align))
            }
            _ => Err("C type is not representable in Solar source".into()),
        }
    }
}

fn round_up(value: u64, align: u64) -> Result<u64, String> {
    value
        .checked_add(align - 1)
        .map(|v| v / align * align)
        .ok_or_else(|| "C struct size overflow".into())
}

fn const_type(ty: &CType, unit: &Unit, depth: usize) -> bool {
    if depth > 100 {
        return false;
    }
    match ty {
        CType::Const(_) => true,
        CType::Array(inner, _) => const_type(inner, unit, depth + 1),
        CType::Named(name) => unit
            .types
            .get(name)
            .is_some_and(|t| const_type(t, unit, depth + 1)),
        _ => false,
    }
}

fn scalar_word(word: &str) -> bool {
    matches!(
        word,
        "void"
            | "_Bool"
            | "char"
            | "short"
            | "int"
            | "long"
            | "signed"
            | "__signed__"
            | "__signed"
            | "unsigned"
            | "float"
            | "double"
            | "__int128"
            | "_Complex"
    )
}

fn scalar_type(words: &[String]) -> CType {
    let has = |s| words.iter().any(|w| w == s);
    let unsigned = has("unsigned");
    let longs = words.iter().filter(|w| *w == "long").count();
    let scalar = if has("__int128") || has("_Complex") || longs > 2 || has("double") && longs > 0 {
        return CType::Unsupported(format!(
            "C type `{}` is not representable in Solar source",
            words.join(" ")
        ));
    } else if has("void") {
        Scalar::Void
    } else if has("_Bool") {
        Scalar::Bool
    } else if has("float") {
        Scalar::F32
    } else if has("double") {
        Scalar::F64
    } else if has("char") {
        if unsigned { Scalar::U8 } else { Scalar::I8 }
    } else if has("short") {
        if unsigned { Scalar::U16 } else { Scalar::I16 }
    } else if longs == 2 {
        if unsigned { Scalar::U64 } else { Scalar::I64 }
    } else if longs == 1 {
        if unsigned {
            Scalar::ULong
        } else {
            Scalar::Long
        }
    } else if unsigned {
        Scalar::U32
    } else {
        Scalar::I32
    };
    CType::Scalar(scalar)
}

fn qualify(ty: CType, word: &str) -> CType {
    match word {
        "const" | "__const" | "__const__" => CType::Const(Box::new(ty)),
        "restrict" | "__restrict" | "__restrict__" => ty,
        _ => CType::Unsupported(format!(
            "C qualifiers `{word}` are not representable in Solar source"
        )),
    }
}

fn attribute(cursor: &mut Cursor<'_>) -> Result<bool, String> {
    if !matches!(
        cursor.peek(),
        "__attribute__" | "__attribute" | "_Alignas" | "alignas"
    ) {
        return Ok(false);
    }
    cursor.take()?;
    cursor.expect("(")?;
    cursor.until(&[")"])?;
    cursor.expect(")")?;
    Ok(true)
}

fn attributed(ty: CType, has_attribute: bool) -> CType {
    if has_attribute && !matches!(ty, CType::Function) {
        CType::Unsupported("C attribute is not representable in Solar source".into())
    } else {
        ty
    }
}

type Declarations = Vec<(String, CType, Vec<Token>)>;

fn declaration(cursor: &mut Cursor<'_>, unit: &mut Unit) -> Result<(bool, Declarations), String> {
    let mut is_typedef = false;
    let mut qualifiers = Vec::new();
    let mut words = Vec::new();
    let mut base = None;
    let mut has_attribute = false;
    loop {
        match cursor.peek() {
            "typedef" => {
                cursor.take()?;
                is_typedef = true;
            }
            "extern" | "static" | "inline" | "__inline" | "__inline__" | "__extension__"
            | "register" => {
                cursor.take()?;
            }
            "const" | "__const" | "__const__" | "volatile" | "__volatile__" | "restrict"
            | "__restrict" | "__restrict__" | "_Atomic" => {
                qualifiers.push(cursor.take()?.text.clone());
            }
            "__attribute__" | "__attribute" | "_Alignas" | "alignas" => {
                has_attribute |= attribute(cursor)?;
            }
            "struct" | "union" | "enum" if base.is_none() && words.is_empty() => {
                base = Some(record(cursor, unit)?);
            }
            word if scalar_word(word) && base.is_none() => {
                words.push(cursor.take()?.text.clone());
            }
            word if ident(word) && base.is_none() && words.is_empty() => {
                base = Some(CType::Named(cursor.take()?.text.clone()));
            }
            _ => break,
        }
    }
    let mut base = base.unwrap_or_else(|| scalar_type(&words));
    for qualifier in qualifiers {
        base = qualify(base, &qualifier);
    }
    base = attributed(base, has_attribute);
    if cursor.eat(";") {
        return Ok((is_typedef, Vec::new()));
    }
    let mut result = Vec::new();
    loop {
        let decl = declarator(cursor)?;
        let (name, mut ty) = decl.apply(base.clone());
        while attribute(cursor)? {
            ty = attributed(ty, true);
        }
        if cursor.eat(":") {
            cursor.until(&[",", ";"])?;
            ty = CType::Unsupported("C bitfields are not representable in Solar source".into());
        }
        // Assembly aliases only decorate functions; no C function is imported.
        if matches!(cursor.peek(), "__asm__" | "__asm" | "asm") {
            cursor.take()?;
            cursor.expect("(")?;
            cursor.until(&[")"])?;
            cursor.expect(")")?;
            while attribute(cursor)? {}
        }
        let value = if cursor.eat("=") {
            cursor.until(&[",", ";"])?.to_vec()
        } else {
            Vec::new()
        };
        let function = matches!(ty.unqualified(), CType::Function);
        result.push((name, ty, value));
        if function && cursor.eat("{") {
            cursor.until(&["}"])?;
            cursor.expect("}")?;
            break;
        }
        if cursor.eat(";") {
            break;
        }
        cursor.expect(",")?;
    }
    Ok((is_typedef, result))
}

fn record(cursor: &mut Cursor<'_>, unit: &mut Unit) -> Result<CType, String> {
    let start = cursor.take()?.clone();
    let mut has_attribute = false;
    while attribute(cursor)? {
        has_attribute = true;
    }
    let name = if ident(cursor.peek()) {
        cursor.take()?.text.clone()
    } else {
        String::new()
    };
    let key = if name.is_empty() {
        format!("{}@{}", start.text, start.location())
    } else {
        format!("{} {name}", start.text)
    };
    if cursor.eat("{") {
        let body = cursor.until(&["}"])?;
        cursor.expect("}")?;
        while attribute(cursor)? {
            has_attribute = true;
        }
        if start.text == "enum" {
            let mut members = Cursor::new(body);
            let mut previous: Option<String> = None;
            let group = unit.enums.len();
            unit.enums.push(Vec::new());
            while members.pos < body.len() {
                let token = members.take()?;
                if !ident(&token.text) {
                    return Err("unsupported C enumerator".into());
                }
                let explicit = members.eat("=");
                let value = if explicit {
                    members.until(&[","])?.to_vec()
                } else if let Some(previous) = &previous {
                    vec![
                        token.with_text(previous),
                        token.with_text("+"),
                        token.with_text("1"),
                    ]
                } else {
                    vec![token.with_text("0")]
                };
                unit.constants.insert(
                    token.text.clone(),
                    Constant {
                        ty: None,
                        value,
                        is_const: true,
                        enum_group: Some(group),
                        previous_enumerator: if explicit { None } else { previous.clone() },
                    },
                );
                unit.enums[group].push(token.text.clone());
                previous = Some(token.text.clone());
                if !members.eat(",") {
                    break;
                }
            }
        } else {
            let fields = if start.text == "union" {
                Err("C unions are not representable in Solar source".into())
            } else if has_attribute {
                Err("C attribute is not representable in Solar source".into())
            } else if unit.packed {
                Err("C pragma pack layout is not representable in Solar source".into())
            } else {
                fields(body, unit)
            };
            unit.records.insert(
                key.clone(),
                Record {
                    origin: start.location(),
                    fields,
                },
            );
        }
    }
    let ty = if start.text == "enum" {
        CType::Unsupported("C enum types are not representable in Solar source".into())
    } else {
        CType::Record(key)
    };
    let ty = attributed(ty, has_attribute);
    if !name.is_empty() {
        unit.types.entry(name).or_insert_with(|| ty.clone());
    }
    Ok(ty)
}

fn fields(tokens: &[Token], unit: &mut Unit) -> Result<Vec<(String, CType)>, String> {
    let mut cursor = Cursor::new(tokens);
    let mut fields = Vec::new();
    while cursor.pos < tokens.len() {
        let (is_typedef, declarations) = declaration(&mut cursor, unit)?;
        if is_typedef || declarations.is_empty() {
            return Err("anonymous C fields are not representable in Solar source".into());
        }
        for (name, ty, initializer) in declarations {
            if let CType::Unsupported(error) = &ty {
                return Err(error.clone());
            }
            if name.is_empty() {
                return Err("anonymous C fields are not representable in Solar source".into());
            }
            if !initializer.is_empty() {
                return Err("C field initializers are not representable".into());
            }
            fields.push((name, ty));
        }
    }
    Ok(fields)
}

enum Declarator {
    Name(String),
    Pointer(Box<Self>, Vec<String>),
    Array(Box<Self>, Vec<Token>),
    Function(Box<Self>),
}

impl Declarator {
    fn apply(self, ty: CType) -> (String, CType) {
        match self {
            Self::Name(name) => (name, ty),
            Self::Pointer(inner, qualifiers) => {
                let mut ty = CType::Pointer(Box::new(ty));
                for qualifier in qualifiers {
                    ty = qualify(ty, &qualifier);
                }
                inner.apply(ty)
            }
            Self::Array(inner, count) => inner.apply(CType::Array(Box::new(ty), count)),
            Self::Function(inner) => inner.apply(CType::Function),
        }
    }
}

fn declarator(cursor: &mut Cursor<'_>) -> Result<Declarator, String> {
    if cursor.eat("*") {
        let mut qualifiers = Vec::new();
        while matches!(
            cursor.peek(),
            "const"
                | "__const"
                | "__const__"
                | "volatile"
                | "restrict"
                | "__restrict"
                | "__restrict__"
        ) {
            qualifiers.push(cursor.take()?.text.clone());
        }
        return Ok(Declarator::Pointer(
            Box::new(declarator(cursor)?),
            qualifiers,
        ));
    }
    let mut result = if cursor.eat("(") {
        let result = declarator(cursor)?;
        cursor.expect(")")?;
        result
    } else if ident(cursor.peek()) {
        Declarator::Name(cursor.take()?.text.clone())
    } else {
        Declarator::Name(String::new())
    };
    loop {
        if cursor.eat("[") {
            let count = cursor.until(&["]"])?.to_vec();
            cursor.expect("]")?;
            result = Declarator::Array(Box::new(result), count);
        } else if cursor.eat("(") {
            cursor.until(&[")"])?;
            cursor.expect(")")?;
            result = Declarator::Function(Box::new(result));
        } else {
            break;
        }
    }
    Ok(result)
}
