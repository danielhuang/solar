//! Compile-time C imports, read directly from headers without a C compiler.
//! This supports the Linux LP64 header subset used by the standard library:
//! preprocessing, scalar constant expressions, typedefs, and ordinary structs.
//! Unsupported requested declarations are errors; C FFI is not introduced.

mod expr;
mod lex;
mod parse;
mod preprocess;
#[cfg(test)]
mod tests;

use crate::ast::*;
use expr::{Scalar, Value};
use parse::{CType, Unit};
use preprocess::Preprocessor;
use std::collections::{BTreeSet, HashMap};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

fn named(name: &str) -> Type {
    Type::Named(DefId::new(0, name))
}

struct Importer<'a> {
    unit: &'a Unit,
    items: Vec<TopLevelItem>,
    records: HashMap<String, String>,
}

impl Importer<'_> {
    fn ty(&mut self, ty: &CType) -> Result<Type, String> {
        match self.unit.resolve(ty)? {
            CType::Scalar(Scalar::Void) => Ok(Type::Tuple(Vec::new())),
            CType::Scalar(scalar) => Ok(named(scalar.name())),
            CType::Pointer(inner) => Ok(Type::NullableReference(Box::new(self.ty(inner)?))),
            CType::Array(inner, count) => Ok(Type::FixedArray(
                Box::new(self.ty(inner)?),
                self.unit.array_len(count)?,
            )),
            CType::Record(key) => {
                if let Some(name) = self.records.get(key) {
                    return Ok(named(name));
                }
                self.unit.layout(ty)?;
                let record = self.unit.records.get(key).ok_or_else(|| {
                    format!("incomplete C struct `{key}` is not representable in Solar source")
                })?;
                let mut hash = std::collections::hash_map::DefaultHasher::new();
                record.origin.hash(&mut hash);
                key.hash(&mut hash);
                let name = format!("__SolarCRecord{:016x}", hash.finish());
                self.records.insert(key.clone(), name.clone());
                let mut fields = Vec::new();
                for (name, ty) in record.fields.as_ref().map_err(Clone::clone)? {
                    fields.push(FieldDef {
                        name: name.clone(),
                        ty: self.ty(ty)?,
                        is_pub: true,
                        span: SourceSpan::default(),
                    });
                }
                self.items.push(TopLevelItem::Struct(StructDef {
                    name: name.clone(),
                    def_id: DefId::new(0, &name),
                    type_params: Vec::new(),
                    fields,
                    is_tuple: false,
                    repr_c: true,
                    opaque: false,
                    is_pub: true,
                    doc: Some(format!("Imported C struct `{key}`.")),
                    span: SourceSpan::default(),
                }));
                Ok(named(&name))
            }
            _ => Err("C type is not representable in Solar source".into()),
        }
    }

    fn literal(&mut self, value: Value) -> Result<(Type, ExprKind), String> {
        Ok(match value {
            Value::Int(n, Scalar::Bool) => (named("Bool"), ExprKind::BooleanLiteral(n != 0)),
            Value::Int(n, scalar) => {
                let ty = match scalar {
                    Scalar::I8 => IntegerType::Int8,
                    Scalar::U8 => IntegerType::Uint8,
                    Scalar::I16 => IntegerType::Int16,
                    Scalar::U16 => IntegerType::Uint16,
                    Scalar::I32 => IntegerType::Int32,
                    Scalar::U32 => IntegerType::Uint32,
                    Scalar::Long => IntegerType::Int,
                    Scalar::ULong => IntegerType::Uint,
                    Scalar::I64 => IntegerType::Int64,
                    Scalar::U64 => IntegerType::Uint64,
                    _ => return Err("invalid C integer constant type".into()),
                };
                (named(scalar.name()), ExprKind::IntegerLiteral(n, ty))
            }
            Value::Float(n, scalar) => (
                named(scalar.name()),
                ExprKind::FloatLiteral(
                    n,
                    if scalar == Scalar::F32 {
                        FloatType::Float32
                    } else {
                        FloatType::Float64
                    },
                ),
            ),
            Value::Null(inner) => {
                let inner = self.ty(&inner)?;
                (
                    Type::NullableReference(Box::new(inner.clone())),
                    ExprKind::NullLiteral(inner),
                )
            }
            Value::Bytes(bytes) => {
                let values = bytes
                    .into_iter()
                    .map(|b| Value::Int((b as i8).into(), Scalar::I8))
                    .collect();
                return self.literal(Value::Array(values, CType::Scalar(Scalar::I8)));
            }
            Value::Array(values, inner) => {
                let inner = self.ty(&inner)?;
                let ty = Type::FixedArray(Box::new(inner.clone()), values.len() as u64);
                let values = values
                    .into_iter()
                    .map(|v| {
                        Ok(Expr {
                            kind: self.literal(v)?.1,
                            span: SourceSpan::default(),
                        })
                    })
                    .collect::<Result<_, String>>()?;
                (ty, ExprKind::ArrayLiteral(values, Some(inner)))
            }
        })
    }
}

fn import_uncached(path: &str, kind: &ImportKind) -> Result<CachedImport, String> {
    let mut preprocessor = Preprocessor::new()?;
    let tokens = preprocessor.header(path)?;
    let unit = Unit::parse(&tokens)?;
    let mut importer = Importer {
        unit: &unit,
        items: Vec::new(),
        records: HashMap::new(),
    };
    let names: BTreeSet<String> = match kind {
        ImportKind::Named(names) => {
            if names.iter().any(ImportName::is_path) {
                return Err("C imports require unqualified declaration names".into());
            }
            names
                .iter()
                .map(|name| name.local_name().to_owned())
                .collect()
        }
        ImportKind::Module(_) | ImportKind::Wildcard => unit
            .types
            .keys()
            .chain(unit.constants.keys())
            .chain(
                preprocessor
                    .macros
                    .iter()
                    .filter(|(_, m)| m.parameters.is_none() && !m.body.is_empty())
                    .map(|(name, _)| name),
            )
            .filter(|name| !name.starts_with('_'))
            .cloned()
            .collect(),
    };
    for name in names {
        let result = (|| {
            let mac = preprocessor.macros.get(&name);
            if mac.is_none()
                && let Some(ty) = unit.types.get(&name)
            {
                let ty = importer.ty(ty)?;
                importer.items.push(TopLevelItem::TypeAlias(TypeAliasDef {
                    name: name.clone(),
                    type_params: Vec::new(),
                    target_type: ty,
                    is_pub: true,
                    doc: Some(format!("Imported C type `{name}`.")),
                    span: SourceSpan::default(),
                }));
                return Ok(());
            }
            if mac.is_some_and(|m| m.parameters.is_some())
                || mac.is_none() && unit.functions.contains(&name)
            {
                return Err(format!(
                    "cannot import C function `{name}`: C FFI is not supported"
                ));
            }
            let value = if mac.is_some() {
                let tokens = preprocessor.expand(&lex::lex(&name, "<C import>".into())?)?;
                expr::evaluate(&tokens, &unit)?
            } else if unit.constants.contains_key(&name) {
                expr::constant(&name, &unit)?
            } else {
                return Err(format!(
                    "C header `{path}` has no constant or type named `{name}`"
                ));
            };
            let (ty, kind) = importer.literal(value)?;
            importer.items.push(TopLevelItem::Const(ConstDef {
                name: name.clone(),
                ty: Some(ty),
                value: Box::new(Expr {
                    kind,
                    span: SourceSpan::default(),
                }),
                is_pub: true,
                doc: Some(format!("Imported C constant `{name}`.")),
                span: SourceSpan::default(),
            }));
            Ok(())
        })();
        result.map_err(|e: String| format!("cannot import C declaration `{name}`: {e}"))?;
    }
    let dependencies = preprocessor
        .dependencies
        .into_iter()
        .map(|path| {
            let metadata = std::fs::metadata(&path).map_err(|e| e.to_string())?;
            Ok((
                path,
                metadata.modified().map_err(|e| e.to_string())?,
                metadata.len(),
            ))
        })
        .collect::<Result<_, String>>()?;
    Ok(CachedImport {
        items: importer.items,
        dependencies,
    })
}

#[derive(Clone)]
struct CachedImport {
    items: Vec<TopLevelItem>,
    dependencies: Vec<(PathBuf, SystemTime, u64)>,
}

/// Reads requested C declarations. System headers are reused within the compiler
/// process while their transitive dependencies and include paths remain unchanged.
pub(crate) fn import(path: &str, kind: &ImportKind) -> Result<Vec<TopLevelItem>, String> {
    static CACHE: OnceLock<Mutex<HashMap<String, CachedImport>>> = OnceLock::new();
    if path.starts_with("./") {
        return Ok(import_uncached(path, kind)?.items);
    }
    let key = format!(
        "{path}:{kind:?}:{:?}:{:?}",
        ["CPATH", "C_INCLUDE_PATH"].map(std::env::var_os),
        std::env::current_dir()
    );
    let cache = CACHE.get_or_init(Mutex::default);
    if let Some(cached) = cache.lock().unwrap().get(&key)
        && cached.dependencies.iter().all(|(path, modified, size)| {
            std::fs::metadata(path).is_ok_and(|metadata| {
                metadata.modified().ok() == Some(*modified) && metadata.len() == *size
            })
        })
    {
        return Ok(cached.items.clone());
    }
    let result = import_uncached(path, kind)?;
    let items = result.items.clone();
    cache.lock().unwrap().insert(key, result);
    Ok(items)
}
