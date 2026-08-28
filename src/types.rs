//! Types shared by the typed and mangled compiler stages.

use crate::ast::NumericType;
use std::fmt;

/// A Solar type parameterized by its struct and enum identity representation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Type<I> {
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
    Struct(I),
    Enum(I),
    Array(Box<Type<I>>),
    FixedArray(Box<Type<I>>, u64),
    Ref(Box<Type<I>>),
    RefUnsized(Box<Type<I>>),
    /// `&?T` — a nullable reference to a sized `T` (8-byte pointer, may be null).
    NullableRef(Box<Type<I>>),
    /// `&?T` — a nullable reference to an unsized `T` (16-byte fat pointer, may be null).
    NullableRefUnsized(Box<Type<I>>),
    Unique(Box<Type<I>>),
    UniqueUnsized(Box<Type<I>>),
    Function {
        params: Vec<Type<I>>,
        return_type: Box<Type<I>>,
    },
    /// An open file descriptor. A built-in opaque handle with the byte
    /// representation of `&Int32`: an 8-byte pointer into the GC-traced fd
    /// arena. The collector closes the file once no live `FileDesc` remains.
    FileDesc,
    Unit,
    Never,
}

/// Provides struct field layouts for type size classification.
pub trait StructDefinitions<I> {
    /// Returns the last field type for a struct, distinguishing a missing
    /// definition from a struct with no fields.
    fn last_field_type<'a>(&'a self, id: &I) -> Option<Option<&'a Type<I>>>;
}

/// Packs aligned, non-overlapping fields around an occupied byte prefix.
///
/// Returns one byte offset per `(size, alignment)` input, in input order, plus
/// the first byte after every occupied field. Fields are considered from
/// strictest to loosest alignment and placed in the first aligned gap where
/// they fit, so later small fields can consume padding left by earlier fields.
pub(crate) fn pack_fields(
    fields: &[(usize, usize)],
    occupied_prefix: usize,
) -> (Vec<usize>, usize) {
    let mut order: Vec<usize> = (0..fields.len()).collect();
    order.sort_by_key(|&index| std::cmp::Reverse(fields[index].1));

    let mut occupied = if occupied_prefix == 0 {
        Vec::new()
    } else {
        vec![(0usize, occupied_prefix)]
    };
    let mut offsets = vec![0usize; fields.len()];
    let mut extent = occupied_prefix;

    for index in order {
        let (size, align) = fields[index];
        assert!(
            align.is_power_of_two(),
            "field alignment must be a power of two"
        );

        let mut offset = 0usize;
        for &(start, end) in &occupied {
            offset = (offset + align - 1) & !(align - 1);
            if offset + size <= start {
                break;
            }
            if offset < end {
                offset = end;
            }
        }
        offset = (offset + align - 1) & !(align - 1);
        offsets[index] = offset;
        extent = extent.max(offset + size);

        if size != 0 {
            let insert_at = occupied.partition_point(|&(start, _)| start < offset);
            occupied.insert(insert_at, (offset, offset + size));
        }
    }

    (offsets, extent)
}

/// Lays out fields in declaration order using C struct alignment rules.
pub(crate) fn layout_fields_in_order(fields: &[(usize, usize)]) -> (Vec<usize>, usize) {
    let mut extent = 0usize;
    let offsets = fields
        .iter()
        .map(|&(size, align)| {
            assert!(
                align.is_power_of_two(),
                "field alignment must be a power of two"
            );
            extent = (extent + align - 1) & !(align - 1);
            let offset = extent;
            extent += size;
            offset
        })
        .collect();
    (offsets, extent)
}

impl<I> From<&NumericType> for Type<I> {
    fn from(nt: &NumericType) -> Type<I> {
        match nt {
            NumericType::Int8 => Type::Int8,
            NumericType::Int16 => Type::Int16,
            NumericType::Int32 => Type::Int32,
            NumericType::Int64 => Type::Int64,
            NumericType::Int => Type::Int,
            NumericType::Uint8 => Type::Uint8,
            NumericType::Uint16 => Type::Uint16,
            NumericType::Uint32 => Type::Uint32,
            NumericType::Uint64 => Type::Uint64,
            NumericType::Uint => Type::Uint,
            NumericType::Float32 => Type::Float32,
            NumericType::Float64 => Type::Float64,
        }
    }
}

impl<I> Type<I> {
    /// Replaces every struct and enum identity while preserving type shape.
    pub fn map_id<J>(&self, mut map: impl FnMut(&I) -> J) -> Type<J> {
        self.map_id_with(&mut map)
    }

    fn map_id_with<J>(&self, map: &mut impl FnMut(&I) -> J) -> Type<J> {
        match self {
            Type::Int8 => Type::Int8,
            Type::Int16 => Type::Int16,
            Type::Int32 => Type::Int32,
            Type::Int64 => Type::Int64,
            Type::Int => Type::Int,
            Type::Uint8 => Type::Uint8,
            Type::Uint16 => Type::Uint16,
            Type::Uint32 => Type::Uint32,
            Type::Uint64 => Type::Uint64,
            Type::Uint => Type::Uint,
            Type::Float32 => Type::Float32,
            Type::Float64 => Type::Float64,
            Type::Bool => Type::Bool,
            Type::Struct(id) => Type::Struct(map(id)),
            Type::Enum(id) => Type::Enum(map(id)),
            Type::Array(inner) => Type::Array(Box::new(inner.map_id_with(map))),
            Type::FixedArray(inner, len) => {
                Type::FixedArray(Box::new(inner.map_id_with(map)), *len)
            }
            Type::Ref(inner) => Type::Ref(Box::new(inner.map_id_with(map))),
            Type::RefUnsized(inner) => Type::RefUnsized(Box::new(inner.map_id_with(map))),
            Type::NullableRef(inner) => Type::NullableRef(Box::new(inner.map_id_with(map))),
            Type::NullableRefUnsized(inner) => {
                Type::NullableRefUnsized(Box::new(inner.map_id_with(map)))
            }
            Type::Unique(inner) => Type::Unique(Box::new(inner.map_id_with(map))),
            Type::UniqueUnsized(inner) => Type::UniqueUnsized(Box::new(inner.map_id_with(map))),
            Type::Function {
                params,
                return_type,
            } => Type::Function {
                params: params.iter().map(|ty| ty.map_id_with(map)).collect(),
                return_type: Box::new(return_type.map_id_with(map)),
            },
            Type::FileDesc => Type::FileDesc,
            Type::Unit => Type::Unit,
            Type::Never => Type::Never,
        }
    }

    /// Returns whether this is an integer type.
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

    /// Returns whether this is a floating-point type.
    pub fn is_float(&self) -> bool {
        matches!(self, Type::Float32 | Type::Float64)
    }

    /// Returns whether this is numeric.
    pub fn is_numeric(&self) -> bool {
        self.is_integer() || self.is_float()
    }

    /// Returns whether this is an unsigned integer type.
    pub fn is_unsigned(&self) -> bool {
        matches!(
            self,
            Type::Uint8 | Type::Uint16 | Type::Uint32 | Type::Uint64 | Type::Uint
        )
    }

    /// Bit width of an integer type (`Int`/`Uint` are pointer-width 64).
    ///
    /// Panics on non-integer types.
    pub fn int_bit_width(&self) -> u32
    where
        I: fmt::Display,
    {
        match self {
            Type::Int8 | Type::Uint8 => 8,
            Type::Int16 | Type::Uint16 => 16,
            Type::Int32 | Type::Uint32 => 32,
            Type::Int64 | Type::Uint64 | Type::Int | Type::Uint => 64,
            other => panic!("int_bit_width on non-integer type {other}"),
        }
    }

    /// Returns whether this is a nullable reference type.
    pub fn is_nullable_ref(&self) -> bool {
        matches!(self, Type::NullableRef(_) | Type::NullableRefUnsized(_))
    }

    /// Returns whether values of this type have a compile-time size.
    pub fn is_sized<S>(&self, structs: &S) -> bool
    where
        I: fmt::Display,
        S: StructDefinitions<I>,
    {
        match self {
            Type::Array(_) => false,
            Type::FixedArray(_, _) | Type::Function { .. } => true,
            Type::Enum(_) => true,
            Type::Struct(id) => {
                let last_field = structs.last_field_type(id).unwrap_or_else(|| {
                    panic!(
                        "is_sized: missing struct `{id}` — the name resolved to no \
                         definition. Check for a module-qualified type whose module does \
                         not export it (e.g. a typo in `alias::Name`), or a generic type \
                         used only in a position that never triggered monomorphization."
                    )
                });
                last_field.is_none_or(|ty| ty.is_sized(structs))
            }
            _ => true,
        }
    }
}

impl<I: fmt::Display> fmt::Display for Type<I> {
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
            Type::Struct(id) | Type::Enum(id) => write!(f, "{id}"),
            Type::Array(inner) => write!(f, "[{inner}]"),
            Type::FixedArray(inner, n) => write!(f, "[{inner}; {n}]"),
            Type::Ref(inner) | Type::RefUnsized(inner) => write!(f, "&{inner}"),
            Type::NullableRef(inner) | Type::NullableRefUnsized(inner) => {
                write!(f, "&?{inner}")
            }
            Type::Unique(inner) | Type::UniqueUnsized(inner) => write!(f, "^{inner}"),
            Type::Function {
                params,
                return_type,
            } => {
                write!(f, "fn(")?;
                for (i, param) in params.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{param}")?;
                }
                write!(f, ")")?;
                if !matches!(return_type.as_ref(), Type::Unit) {
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
