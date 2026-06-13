//! Post-optimization GC write barrier insertion.
//!
//! Runs on the textual LLVM IR of the fully merged and `opt -O3`-optimized
//! module (see `pipeline::compile_release`). Inserting barriers *after*
//! optimization is deliberate: a barrier call is a use of the object pointer,
//! so barriers emitted by the C codegen would block LLVM's dead-allocation
//! elision (`allockind` on `sol_alloc`) and SROA. After optimization, only
//! allocations and stores that survived are instrumented — the same late
//! barrier insertion strategy used by ZGC/Shenandoah in HotSpot.
//!
//! The barrier is Dijkstra-style (insertion): after each store of a pointer
//! value into a potentially-heap destination, `sol_write_barrier(dst, val)`
//! is called so concurrent marking can shade `val`. The runtime fast path is
//! a single flag check and is currently always a no-op (the GC is still
//! stop-the-world).
//!
//! What gets instrumented:
//! - only function bodies of generated Solar code (`@solar_*` and `@main`);
//!   runtime Rust code never inlines Solar code (all runtime entry points
//!   have external linkage and receive Solar functions only through opaque
//!   function pointers), so Solar stores cannot migrate elsewhere
//! - `store ptr %v, ptr %dst` (incl. `atomic`/`volatile`) where `%v` is an
//!   SSA value and `%dst` does not provably derive from an `alloca`
//! - `store <N x ptr>` vector stores, conservatively with a null `val`
//!
//! What is deliberately skipped (correctness relies on the listed invariant):
//! - stored values that are `null`/`undef`/`poison` or globals/constexprs —
//!   never GC-heap pointers, nothing to shade
//! - destinations derived from `alloca` — stacks are rescanned during the
//!   STW remark phase that terminates concurrent marking
//! - destinations that are globals or constant expressions — global roots
//!   are likewise rescanned at remark
//! - `llvm.memcpy`/`memmove` and `sol_memcpy` aggregate copies —
//!   TODO(concurrent-gc): these need a bulk barrier in the runtime before
//!   marking can run concurrently; the conservative small-slot heap scan
//!   makes shading every pointer-aligned word of the copy sufficient
//! - pointer stores re-written by LLVM as integer stores (`ptrtoint`) —
//!   not observed in practice for this codegen; the stats printed by the
//!   pipeline make regressions visible for auditing

/// Summary of one instrumentation run, printed by the pipeline.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Functions whose bodies were scanned (`@solar_*` and `@main`).
    pub functions: usize,
    /// Barriers inserted after scalar `store ptr` instructions.
    pub barriers: usize,
    /// Barriers inserted conservatively (null `val`) after vector ptr stores.
    pub vector_barriers: usize,
    /// Pointer stores skipped because the destination derives from an alloca.
    pub stack_skipped: usize,
}

pub fn insert_write_barriers(ll: &str) -> (String, Stats) {
    let mut stats = Stats::default();
    let mut out = String::with_capacity(ll.len() + ll.len() / 8);
    let lines: Vec<&str> = ll.lines().collect();

    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if let Some(name) = define_name(line)
            && (name.starts_with("solar_") || name == "main")
        {
            // Function bodies end at a `}` in column 0.
            let mut end = i + 1;
            while end < lines.len() && lines[end] != "}" {
                end += 1;
            }
            instrument_function(&lines[i..=end.min(lines.len() - 1)], &mut out, &mut stats);
            stats.functions += 1;
            i = end + 1;
            continue;
        }
        out.push_str(line);
        out.push('\n');
        i += 1;
    }

    if stats.barriers + stats.vector_barriers > 0 {
        assert!(
            ll.contains("@sol_write_barrier("),
            "sol_write_barrier not found in module — runtime bitcode missing the barrier definition"
        );
    }
    (out, stats)
}

/// Returns the function name (without `@`) if this line opens a definition.
fn define_name(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("define ")?;
    let at = rest.find('@')?;
    let name = &rest[at + 1..];
    let end = name.find('(')?;
    Some(&name[..end])
}

fn instrument_function(body: &[&str], out: &mut String, stats: &mut Stats) {
    let stack = stack_derived(body);

    for line in body {
        out.push_str(line);
        out.push('\n');

        let trimmed = line.trim_start();
        let Some(store) = parse_store(trimmed) else {
            continue;
        };
        let indent = &line[..line.len() - trimmed.len()];

        match store {
            Store::Scalar { val, dst } => {
                if stack.contains(dst) {
                    stats.stack_skipped += 1;
                } else {
                    out.push_str(&format!(
                        "{indent}call void @sol_write_barrier(ptr {dst}, ptr {val})\n"
                    ));
                    stats.barriers += 1;
                }
            }
            Store::Vector { dst } => {
                if stack.contains(dst) {
                    stats.stack_skipped += 1;
                } else {
                    out.push_str(&format!(
                        "{indent}call void @sol_write_barrier(ptr {dst}, ptr null)\n"
                    ));
                    stats.vector_barriers += 1;
                }
            }
        }
    }
}

enum Store<'a> {
    /// `store [atomic] [volatile] ptr %val, ptr %dst ...` with an SSA value.
    Scalar { val: &'a str, dst: &'a str },
    /// `store [atomic] [volatile] <N x ptr> ..., ptr %dst ...` — the stored
    /// pointers can't be named without synthesizing extractelement
    /// instructions, so the barrier gets a null `val`.
    Vector { dst: &'a str },
}

/// Parses a (trimmed) line if it is a pointer store that may need a barrier.
/// Returns `None` for non-stores, non-pointer stores, and stores whose value
/// or destination is provably not a GC-heap pointer (constants, globals).
fn parse_store(line: &str) -> Option<Store<'_>> {
    let mut rest = line.strip_prefix("store ")?;
    rest = rest.strip_prefix("atomic ").unwrap_or(rest);
    rest = rest.strip_prefix("volatile ").unwrap_or(rest);

    if let Some(after_ty) = rest.strip_prefix("ptr ") {
        // Values that are not SSA registers (null, undef, poison, @globals,
        // constant expressions) are never GC-heap pointers.
        if !after_ty.starts_with('%') {
            return None;
        }
        let val = after_ty[..after_ty.find(',')?].trim_end();
        let dst = parse_dst(&after_ty[val.len()..])?;
        Some(Store::Scalar { val, dst })
    } else if rest.starts_with('<') {
        let close = rest.find('>')?;
        let elem = rest[1..close].split_whitespace().last()?;
        if elem != "ptr" {
            return None;
        }
        // Skip the stored value: either an SSA register/`zeroinitializer`
        // (ends at the next comma) or a vector literal `<ptr %a, ptr %b>`
        // (ends at its closing `>`). The value may mix constants and SSA
        // registers; treat any vector-of-ptr store conservatively.
        let val = rest[close + 1..].trim_start();
        let val_end = if val.starts_with('<') {
            val.find('>')? + 1
        } else {
            val.find(',')?
        };
        let dst = parse_dst(&val[val_end..])?;
        Some(Store::Vector { dst })
    } else {
        None
    }
}

/// Extracts the destination SSA register from `, ptr %dst[ ordering][, align]`.
/// Global or constexpr destinations return `None`: those are GC roots,
/// rescanned during the STW remark phase, and need no barrier.
fn parse_dst(rest: &str) -> Option<&str> {
    let after = rest.trim_start().strip_prefix(',')?.trim_start();
    let dst = after.strip_prefix("ptr ")?;
    if !dst.starts_with('%') {
        return None;
    }
    let end = dst.find([',', ' ']).unwrap_or(dst.len());
    Some(&dst[..end])
}

/// Collects SSA names that provably derive from an `alloca` via
/// `getelementptr` chains. Runs to a fixpoint because blocks (and therefore
/// definitions) may appear in any textual order. `phi`/`select` of stack
/// pointers are not tracked — that is conservative (extra barriers), never
/// unsound.
fn stack_derived<'a>(body: &[&'a str]) -> std::collections::HashSet<&'a str> {
    let mut stack = std::collections::HashSet::new();
    loop {
        let mut changed = false;
        for line in body {
            let trimmed = line.trim_start();
            let Some((name, def)) = trimmed.split_once(" = ") else {
                continue;
            };
            if !name.starts_with('%') || stack.contains(name) {
                continue;
            }
            let derived = if def.starts_with("alloca ") {
                true
            } else if def.starts_with("getelementptr ") {
                gep_base(def).is_some_and(|base| stack.contains(base))
            } else {
                false
            };
            if derived {
                stack.insert(name);
                changed = true;
            }
        }
        if !changed {
            return stack;
        }
    }
}

/// Finds the base pointer operand of a `getelementptr` definition: the first
/// `, ptr %name` whose operand is an SSA register. Earlier `, ptr ` matches
/// can occur inside the source element type (e.g. `{ i64, ptr }`), but those
/// are never followed by `%`.
fn gep_base(def: &str) -> Option<&str> {
    let pos = def.find(", ptr %")?;
    let operand = &def[pos + 6..];
    let end = operand.find([',', ' ', ')']).unwrap_or(operand.len());
    Some(&operand[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(ll: &str) -> (String, Stats) {
        insert_write_barriers(ll)
    }

    const BARRIER_DEF: &str = "define void @sol_write_barrier(ptr %0, ptr %1) {\n  ret void\n}\n";

    fn with_def(body: &str) -> String {
        format!("{BARRIER_DEF}{body}")
    }

    #[test]
    fn plain_pointer_store_gets_barrier() {
        let ll = with_def(
            "define internal void @solar_f(ptr %a, ptr %b) {\n  store ptr %a, ptr %b, align 8\n  ret void\n}\n",
        );
        let (out, stats) = run(&ll);
        assert!(out.contains(
            "store ptr %a, ptr %b, align 8\n  call void @sol_write_barrier(ptr %b, ptr %a)\n"
        ));
        assert_eq!(stats.barriers, 1);
        assert_eq!(stats.functions, 1);
    }

    #[test]
    fn atomic_and_volatile_stores_get_barriers() {
        let ll = with_def(
            "define internal void @solar_f(ptr %a, ptr %b) {\n  store atomic ptr %a, ptr %b release, align 8\n  store volatile ptr %a, ptr %b, align 8\n  ret void\n}\n",
        );
        let (out, stats) = run(&ll);
        assert_eq!(stats.barriers, 2);
        assert!(out.contains("store atomic ptr %a, ptr %b release, align 8\n  call void @sol_write_barrier(ptr %b, ptr %a)\n"));
    }

    #[test]
    fn non_pointer_and_constant_stores_skipped() {
        let ll = with_def(
            "define internal void @solar_f(ptr %b) {\n  store i64 42, ptr %b, align 8\n  store ptr null, ptr %b, align 8\n  store ptr undef, ptr %b, align 8\n  store ptr @some_global, ptr %b, align 8\n  ret void\n}\n",
        );
        let (out, stats) = run(&ll);
        assert_eq!(stats.barriers, 0);
        assert!(!out.contains("sol_write_barrier(ptr %b"));
    }

    #[test]
    fn stores_to_alloca_and_gep_chains_skipped() {
        let ll = with_def(
            "define internal void @solar_f(ptr %a) {\n  %slot = alloca [4 x i64], align 8\n  %f = getelementptr inbounds i8, ptr %slot, i64 8\n  %g = getelementptr inbounds { i64, ptr }, ptr %f, i64 0, i32 1\n  store ptr %a, ptr %slot, align 8\n  store ptr %a, ptr %f, align 8\n  store ptr %a, ptr %g, align 8\n  ret void\n}\n",
        );
        let (out, stats) = run(&ll);
        assert_eq!(stats.barriers, 0);
        assert_eq!(stats.stack_skipped, 3);
        assert!(
            !out.contains("call void @sol_write_barrier"),
            "no barrier expected:\n{out}"
        );
    }

    #[test]
    fn gep_defined_after_use_is_still_stack() {
        // Blocks can appear in any order; the fixpoint must catch a GEP whose
        // textual definition precedes the alloca line it derives from.
        let ll = with_def(
            "define internal void @solar_f(ptr %a) {\nbb1:\n  %f = getelementptr inbounds i8, ptr %slot, i64 8\n  store ptr %a, ptr %f, align 8\n  ret void\nbb0:\n  %slot = alloca i64, align 8\n  br label %bb1\n}\n",
        );
        let (_, stats) = run(&ll);
        assert_eq!(stats.barriers, 0);
        assert_eq!(stats.stack_skipped, 1);
    }

    #[test]
    fn store_to_global_destination_skipped() {
        let ll = with_def(
            "define internal void @solar_f(ptr %a) {\n  store ptr %a, ptr @g, align 8\n  ret void\n}\n",
        );
        let (_, stats) = run(&ll);
        assert_eq!(stats.barriers, 0);
    }

    #[test]
    fn vector_pointer_store_gets_conservative_barrier() {
        let ll = with_def(
            "define internal void @solar_f(<2 x ptr> %v, ptr %b) {\n  store <2 x ptr> %v, ptr %b, align 16\n  store <4 x i64> zeroinitializer, ptr %b, align 16\n  ret void\n}\n",
        );
        let (out, stats) = run(&ll);
        assert_eq!(stats.vector_barriers, 1);
        assert_eq!(stats.barriers, 0);
        assert!(out.contains("call void @sol_write_barrier(ptr %b, ptr null)"));
    }

    #[test]
    fn runtime_functions_untouched() {
        let ll = "define void @sol_alloc(ptr %a, ptr %b) {\n  store ptr %a, ptr %b, align 8\n  ret void\n}\n";
        let (out, stats) = run(ll);
        assert_eq!(stats.functions, 0);
        assert_eq!(stats.barriers, 0);
        assert_eq!(out, ll);
    }

    #[test]
    fn main_is_instrumented() {
        let ll = with_def(
            "define i32 @main(ptr %a, ptr %b) {\n  store ptr %a, ptr %b, align 8\n  ret i32 0\n}\n",
        );
        let (_, stats) = run(&ll);
        assert_eq!(stats.functions, 1);
        assert_eq!(stats.barriers, 1);
    }

    #[test]
    fn missing_barrier_definition_panics() {
        let ll = "define internal void @solar_f(ptr %a, ptr %b) {\n  store ptr %a, ptr %b, align 8\n  ret void\n}\n";
        assert!(std::panic::catch_unwind(|| insert_write_barriers(ll)).is_err());
    }
}
