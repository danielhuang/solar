//! Compiler intrinsic definitions and source-name lookup.

use crate::ast::NumericType;

/// A compiler intrinsic.
#[derive(Debug, Clone)]
pub enum Intrinsic {
    RefEq,
    Throw,
    Try,
    ArrayLen,
    AssertArrayLen,
    ThreadSpawn,
    AtomicLoad,
    AtomicStore,
    AtomicExchange,
    AtomicCompareExchange,
    FutexWait,
    FutexWake,
    FdFromRaw,
    FdToRaw,
    Syscall,
    FileClose,
    FileStdin,
    FileStdout,
    FileStderr,
    FileWritePartial,
    SocketCreate,
    SocketBind,
    SocketListen,
    SocketAccept,
    SocketConnect,
    SocketSetOption,
    SocketLocalAddr,
    SocketShutdown,
    Args,
    Env,
    MonotonicTime,
    SystemTime,
    NumCpus,
    Exit,
    Sqrt,
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Atan2,
    Pow,
    Exp,
    Log,
    Floor,
    Ceil,
    Round,
    Trunc,
    FloatAbs,
    CountTrailingZeros,
    CountLeadingZeros,
    CountOnes,
    CarryingMulAdd,
    U64FromLe,
    U32FromLe,
    SimdMatchByteX16,
    SimdMatchHighBitX16,
    Cast(NumericType, NumericType),
}

const INTRINSIC_NAMES: &[(&str, Intrinsic)] = &[
    ("ref_eq", Intrinsic::RefEq),
    ("throw", Intrinsic::Throw),
    ("try", Intrinsic::Try),
    ("array_len", Intrinsic::ArrayLen),
    ("assert_array_len", Intrinsic::AssertArrayLen),
    ("thread_spawn", Intrinsic::ThreadSpawn),
    ("atomic_load", Intrinsic::AtomicLoad),
    ("atomic_store", Intrinsic::AtomicStore),
    ("atomic_exchange", Intrinsic::AtomicExchange),
    ("atomic_compare_exchange", Intrinsic::AtomicCompareExchange),
    ("futex_wait", Intrinsic::FutexWait),
    ("futex_wake", Intrinsic::FutexWake),
    ("fd_from_raw", Intrinsic::FdFromRaw),
    ("fd_to_raw", Intrinsic::FdToRaw),
    ("syscall", Intrinsic::Syscall),
    ("file_close", Intrinsic::FileClose),
    ("file_stdin", Intrinsic::FileStdin),
    ("file_stdout", Intrinsic::FileStdout),
    ("file_stderr", Intrinsic::FileStderr),
    ("file_write_partial", Intrinsic::FileWritePartial),
    ("socket_create", Intrinsic::SocketCreate),
    ("socket_bind", Intrinsic::SocketBind),
    ("socket_listen", Intrinsic::SocketListen),
    ("socket_accept", Intrinsic::SocketAccept),
    ("socket_connect", Intrinsic::SocketConnect),
    ("socket_set_option", Intrinsic::SocketSetOption),
    ("socket_local_addr", Intrinsic::SocketLocalAddr),
    ("socket_shutdown", Intrinsic::SocketShutdown),
    ("args", Intrinsic::Args),
    ("env", Intrinsic::Env),
    ("monotonic_time", Intrinsic::MonotonicTime),
    ("system_time", Intrinsic::SystemTime),
    ("num_cpus", Intrinsic::NumCpus),
    ("exit", Intrinsic::Exit),
    ("sqrt", Intrinsic::Sqrt),
    ("sin", Intrinsic::Sin),
    ("cos", Intrinsic::Cos),
    ("tan", Intrinsic::Tan),
    ("asin", Intrinsic::Asin),
    ("acos", Intrinsic::Acos),
    ("atan", Intrinsic::Atan),
    ("atan2", Intrinsic::Atan2),
    ("pow", Intrinsic::Pow),
    ("exp", Intrinsic::Exp),
    ("log", Intrinsic::Log),
    ("floor", Intrinsic::Floor),
    ("ceil", Intrinsic::Ceil),
    ("round", Intrinsic::Round),
    ("trunc", Intrinsic::Trunc),
    ("float_abs", Intrinsic::FloatAbs),
    ("count_trailing_zeros", Intrinsic::CountTrailingZeros),
    ("count_leading_zeros", Intrinsic::CountLeadingZeros),
    ("count_ones", Intrinsic::CountOnes),
    ("carrying_mul_add", Intrinsic::CarryingMulAdd),
    ("u64_from_le", Intrinsic::U64FromLe),
    ("u32_from_le", Intrinsic::U32FromLe),
    ("simd_match_byte_x16", Intrinsic::SimdMatchByteX16),
    ("simd_match_high_bit_x16", Intrinsic::SimdMatchHighBitX16),
];

impl Intrinsic {
    /// Returns the intrinsic's source name.
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

    /// Looks up an intrinsic by source name.
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

    /// Whether calling this intrinsic requires an explicit `unsafe` block.
    pub fn is_unsafe(&self) -> bool {
        matches!(self, Intrinsic::FdFromRaw | Intrinsic::Syscall)
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
