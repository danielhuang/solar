//! In-place optimization passes over the lowered IR (`ir::Module`).
//!
//! These run after `ir::lower` and before codegen / the interpreters, mutating
//! the module in place. Each pass is a transformation/analysis that preserves
//! the program's observable behavior while making the IR cheaper to execute or
//! easier for the downstream LLVM pipeline to optimize.

use std::collections::{HashMap, HashSet};

use crate::ir::{Module, Node, NodeId, NodeKind, Type, VarId};

/// Run all IR optimization passes over `module` to a fixpoint, mutating it in
/// place. Both passes only ever flip flags `false` → `true` (monotonic) and
/// report whether they changed anything, so this terminates: `analyze_param_escapes`
/// iterates until parameter-escape facts stabilize (its transitive rule means
/// one param's result can depend on another's), then `analyze_let_noescape`
/// consumes the stable facts.
pub fn optimize(module: &mut Module) {
    while analyze_param_escapes(module) || analyze_let_noescape(module) {}
}

/// Snapshot of each function's `param_noescape`, for cross-function lookup
/// during a pass (so results don't depend on iteration order within the pass).
fn param_noescape_snapshot(module: &Module) -> HashMap<String, Vec<bool>> {
    module
        .functions
        .iter()
        .map(|f| (f.name.clone(), f.param_noescape.clone()))
        .collect()
}

/// Node indices that appear as a call argument bound for a parameter currently
/// known to be non-escaping (per `noescape_params`). Passing a pointer to such a
/// position cannot leak it.
fn good_call_args(nodes: &[Node], noescape_params: &HashMap<String, Vec<bool>>) -> HashSet<usize> {
    let mut good: HashSet<usize> = HashSet::new();
    for node in nodes {
        if let NodeKind::Call { function, args } = &node.kind
            && let Some(pn) = noescape_params.get(function)
        {
            for (i, a) in args.iter().enumerate() {
                if pn.get(i).copied().unwrap_or(false) {
                    good.insert(a.0);
                }
            }
        }
    }
    good
}

fn is_reference_type(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Ref(_) | Type::RefUnsized(_) | Type::NullableRef(_) | Type::NullableRefUnsized(_)
    )
}

/// Structural escape facts for one function, independent of the cross-function
/// `param_noescape` snapshot (so they're gathered once per function per round,
/// shared by both passes; only the snapshot-dependent `contained` set below is
/// derived on top).
struct FnFacts {
    /// Node indices that are the direct operand of a `Deref` (read/written
    /// through, so a pointer used there isn't itself leaked).
    deref_operand: HashSet<usize>,
    /// `value` node index -> the `Let` var bound to it. Identifies the reference
    /// temp a `Ref` result flows into.
    let_var_of_value: HashMap<usize, VarId>,
    /// `Let` var -> its defining node index.
    let_node_of_var: HashMap<VarId, usize>,
    /// Reference-typed `Let` vars — the candidate pointer-holders for `contained`.
    ref_let_vars: Vec<VarId>,
    /// Every `Local(var)` use site, grouped by var.
    local_uses: HashMap<VarId, Vec<usize>>,
    /// `Ref` node indices addressing a var's *own* storage (`var&`, `var.f&`, …,
    /// not through a deref), grouped by that var.
    direct_refs: HashMap<VarId, Vec<usize>>,
    /// Vars to give up on (their address reaches a `^` move/deep-copy).
    bad: HashSet<VarId>,
    /// Vars whose address is taken anywhere in a `Ref`/`Unique` operand subtree.
    addr_taken: HashSet<VarId>,
}

/// Gather the per-function structural facts in a single pass over `nodes`.
fn collect_fn_facts(nodes: &[Node]) -> FnFacts {
    let mut f = FnFacts {
        deref_operand: HashSet::new(),
        let_var_of_value: HashMap::new(),
        let_node_of_var: HashMap::new(),
        ref_let_vars: Vec::new(),
        local_uses: HashMap::new(),
        direct_refs: HashMap::new(),
        bad: HashSet::new(),
        addr_taken: HashSet::new(),
    };
    for (idx, node) in nodes.iter().enumerate() {
        match &node.kind {
            NodeKind::Let { var, value, .. } => {
                f.let_var_of_value.insert(value.0, *var);
                f.let_node_of_var.insert(*var, idx);
                if is_reference_type(&nodes[value.0].ty) {
                    f.ref_let_vars.push(*var);
                }
            }
            NodeKind::Local(v) => f.local_uses.entry(*v).or_default().push(idx),
            NodeKind::Deref(op) => {
                f.deref_operand.insert(op.0);
            }
            NodeKind::Ref(op) => {
                // A ref of a place rooted at a local `V` (`V`, `V.field`, `V[i]`,
                // … but not through a deref) addresses `V`'s own storage. A ref
                // reached through a deref points into a pointee — a different
                // allocation — so it can't make any local's storage escape.
                collect_locals(nodes, *op, &mut f.addr_taken);
                if let Some(v) = ref_root_local(nodes, *op) {
                    f.direct_refs.entry(v).or_default().push(idx);
                }
            }
            NodeKind::Unique(op) => {
                // `^place` moves/deep-copies; conservatively give up on its locals.
                let mut s = HashSet::new();
                collect_locals(nodes, *op, &mut s);
                f.addr_taken.extend(&s);
                f.bad.extend(s);
            }
            _ => {}
        }
    }
    f
}

/// Fixpoint over reference-holding `Let` bindings that are "contained" — analogous
/// to a scope-local lifetime. `r` is contained iff (1) no reference to `r`'s
/// storage escapes (`refs_to_storage_contained`) and (2) `r`'s *contents* (the
/// pointer it holds) only reach non-escaping places: every `Local(r)` use is the
/// operand of a `Deref`, an argument to a non-escaping callee param (`good_use`),
/// or copied into another contained binding (`let w = r` with `w` contained).
/// Storing a reference into a contained binding therefore doesn't let it escape.
/// Monotonic (`contained` only grows), so this terminates.
fn compute_contained(facts: &FnFacts, good_use: &HashSet<usize>) -> HashSet<VarId> {
    let mut contained: HashSet<VarId> = HashSet::new();
    loop {
        let mut changed = false;
        for &r in &facts.ref_let_vars {
            if contained.contains(&r) || facts.bad.contains(&r) {
                continue;
            }
            if !refs_to_storage_contained(
                r,
                &facts.direct_refs,
                &facts.let_var_of_value,
                &contained,
            ) {
                continue;
            }
            let contents_ok = facts.local_uses.get(&r).is_none_or(|uses| {
                uses.iter().all(|&u| {
                    facts.deref_operand.contains(&u)
                        || good_use.contains(&u)
                        || facts
                            .let_var_of_value
                            .get(&u)
                            .is_some_and(|w| contained.contains(w))
                })
            });
            if contents_ok {
                contained.insert(r);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    contained
}

/// Whether a binding's own storage can sit on the stack: it isn't given up on,
/// and every reference to its storage is a contained temp (no escaping reference
/// to it). Vacuously true when its address is never taken. Used for both `Let`
/// bindings and **value** parameters — a value param whose address is taken (e.g.
/// `key&` forwarded to a non-escaping `key_hash`/`find`) qualifies here even
/// though it once didn't, eliminating the per-call box.
fn storage_noescape(var: VarId, facts: &FnFacts, contained: &HashSet<VarId>) -> bool {
    !facts.bad.contains(&var)
        && refs_to_storage_contained(var, &facts.direct_refs, &facts.let_var_of_value, contained)
}

/// Whether a **reference** parameter's held pointer can't escape: (b) its own
/// storage is never addressed (which would re-expose the pointer, e.g. `(p@)&`)
/// and (a) every `Local(param)` use is either the direct operand of a `Deref` or
/// forwarded into an already-non-escaping callee param (the transitive case, e.g.
/// `f(p: &T) { g(p) }` once `g`'s param is non-escaping).
fn reference_param_noescape(var: VarId, facts: &FnFacts, good_use: &HashSet<usize>) -> bool {
    !facts.addr_taken.contains(&var)
        && facts.local_uses.get(&var).is_none_or(|uses| {
            uses.iter()
                .all(|&u| facts.deref_operand.contains(&u) || good_use.contains(&u))
        })
}

/// Refine each function's `param_noescape` (one fixpoint round); returns whether
/// any flag flipped `false` → `true`. `param_noescape[i] == true` means an
/// argument passed to parameter `i` cannot escape the call. Two cases, by type:
///
/// * **Value parameter** (`Int`, a struct, …): non-escaping when its storage
///   doesn't escape (`storage_noescape`) — its address is never taken, or every
///   `param&` is a contained temp flowing only into non-escaping callees.
/// * **Reference parameter** (`&T`, `&?T`): the parameter *holds a pointer* that
///   must not escape (`reference_param_noescape`).
///
/// Sound by monotone induction from the all-may-escape start: a param is marked
/// only when justified by facts already established. Anything uncertain stays
/// `false`, so the result never claims non-escape when escape is possible.
pub fn analyze_param_escapes(module: &mut Module) -> bool {
    let snapshot = param_noescape_snapshot(module);
    let mut changed = false;
    for func in &mut module.functions {
        let facts = collect_fn_facts(&func.nodes);
        let good_use = good_call_args(&func.nodes, &snapshot);
        let contained = compute_contained(&facts, &good_use);
        let results: Vec<bool> = func
            .params
            .iter()
            .map(|p| {
                if is_reference_type(&p.ty) {
                    reference_param_noescape(p.var, &facts, &good_use)
                } else {
                    storage_noescape(p.var, &facts, &contained)
                }
            })
            .collect();
        for (i, r) in results.into_iter().enumerate() {
            // Monotonic: only ever upgrade may-escape → non-escape.
            if r && !func.param_noescape[i] {
                func.param_noescape[i] = true;
                changed = true;
            }
        }
    }
    changed
}

/// Mark `Let` bindings that provably don't escape so codegen can place them on
/// the C stack. A binding `V` is non-escaping when **every** pointer to its
/// storage is `V&` taken *directly as a call argument* whose callee parameter is
/// itself non-escaping (`param_noescape`) — which is **vacuously true when `V`'s
/// address is never taken at all** (nothing points at it, so nothing can leak
/// it). Deliberately simple — anything more involved is treated as escaping:
///   * `^V` (unique reference) or taking the address of a field/element of `V`;
///   * routing `V&` through another binding first (`let r = V&; f(r)`), since the
///     reference temp is then used as something other than a direct call arg.
///
/// Must run after `analyze_param_escapes` (it reads callees' `param_noescape`).
/// Returns whether any `Let` flag flipped `false` → `true`. A `Let` is marked
/// when its storage doesn't escape — the same `storage_noescape` test the value-
/// parameter case uses, over the shared per-function `FnFacts`/`contained`.
fn analyze_let_noescape(module: &mut Module) -> bool {
    let noescape_params = param_noescape_snapshot(module);
    let mut changed = false;
    for func in &mut module.functions {
        let facts = collect_fn_facts(&func.nodes);
        let good_use = good_call_args(&func.nodes, &noescape_params);
        let contained = compute_contained(&facts, &good_use);
        let to_mark: Vec<usize> = facts
            .let_node_of_var
            .iter()
            .filter_map(|(&v, &idx)| storage_noescape(v, &facts, &contained).then_some(idx))
            .collect();
        for idx in to_mark {
            if let NodeKind::Let { noescape, .. } = &mut func.nodes[idx].kind
                && !*noescape
            {
                *noescape = true;
                changed = true;
            }
        }
    }
    changed
}

/// The local variable whose *own storage* the place at `id` is part of, if any:
/// descends `FieldAccess`/`Index`/`Slice` objects down to a `Local`. Stops
/// (returns `None`) at a `Deref` — past a deref the place lives in a pointee, a
/// separate allocation, not in any local's storage. (Index/slice subscripts are
/// by-value reads, not address-takings, so they're not followed.)
fn ref_root_local(nodes: &[Node], id: NodeId) -> Option<VarId> {
    match &nodes[id.0].kind {
        NodeKind::Local(v) => Some(*v),
        NodeKind::FieldAccess { object, .. }
        | NodeKind::Index { object, .. }
        | NodeKind::Slice { object, .. } => ref_root_local(nodes, *object),
        _ => None,
    }
}

/// Whether every pointer to `v`'s own storage (`v&`, `v.field&`, …) is a
/// reference temp that is itself `contained` — i.e. no reference to `v` escapes.
/// Vacuously true when `v`'s address is never taken.
fn refs_to_storage_contained(
    v: VarId,
    direct_refs: &HashMap<VarId, Vec<usize>>,
    let_var_of_value: &HashMap<usize, VarId>,
    contained: &HashSet<VarId>,
) -> bool {
    direct_refs.get(&v).is_none_or(|refs| {
        refs.iter().all(|ref_idx| {
            // The `v&` result must be bound to a reference temp that is contained.
            let_var_of_value
                .get(ref_idx)
                .is_some_and(|rt| contained.contains(rt))
        })
    })
}

/// Collect every `Local(var)` reachable from `id`'s subtree into `out`. Used on
/// the operand of a `Ref`/`Unique` to find which variables have their address
/// taken. The match is exhaustive (no wildcard) on purpose: if a new `NodeKind`
/// is added, this fails to compile rather than silently missing an address-take
/// and unsoundly marking a parameter as non-escaping.
fn collect_locals(nodes: &[Node], id: NodeId, out: &mut HashSet<VarId>) {
    match &nodes[id.0].kind {
        NodeKind::Local(var) => {
            out.insert(*var);
        }
        // A static's storage is global, not any local's — taking a reference
        // rooted at it involves no local variable.
        NodeKind::Global(_) => {}
        NodeKind::FieldAccess { object, .. } => collect_locals(nodes, *object, out),
        NodeKind::Deref(n)
        | NodeKind::Ref(n)
        | NodeKind::Unique(n)
        | NodeKind::Not(n)
        | NodeKind::Expr(n)
        | NodeKind::Return(n)
        | NodeKind::ArraySizeCoerce { value: n, .. } => collect_locals(nodes, *n, out),
        NodeKind::Index { object, index } => {
            collect_locals(nodes, *object, out);
            collect_locals(nodes, *index, out);
        }
        NodeKind::Slice { object, start, end } => {
            collect_locals(nodes, *object, out);
            collect_locals(nodes, *start, out);
            collect_locals(nodes, *end, out);
        }
        NodeKind::ArrayRepeat { element, count } => {
            collect_locals(nodes, *element, out);
            collect_locals(nodes, *count, out);
        }
        NodeKind::ArrayInit { count, init } => {
            collect_locals(nodes, *count, out);
            collect_locals(nodes, *init, out);
        }
        NodeKind::BinaryOp { left, right, .. } => {
            collect_locals(nodes, *left, out);
            collect_locals(nodes, *right, out);
        }
        NodeKind::Call { args, .. } | NodeKind::IntrinsicCall { args, .. } => {
            for a in args {
                collect_locals(nodes, *a, out);
            }
        }
        NodeKind::CallIndirect { callee, args } => {
            collect_locals(nodes, *callee, out);
            for a in args {
                collect_locals(nodes, *a, out);
            }
        }
        NodeKind::MakeClosure { captures, .. } => {
            for c in captures {
                collect_locals(nodes, *c, out);
            }
        }
        NodeKind::ArrayLiteral(elems) => {
            for e in elems {
                collect_locals(nodes, *e, out);
            }
        }
        NodeKind::StructLiteral { fields, .. } => {
            for (_, f) in fields {
                collect_locals(nodes, *f, out);
            }
        }
        NodeKind::EnumVariant { value, .. } => {
            if let Some(v) = value {
                collect_locals(nodes, *v, out);
            }
        }
        NodeKind::Let { value, .. } => collect_locals(nodes, *value, out),
        NodeKind::Assign { target, value } => {
            collect_locals(nodes, *target, out);
            collect_locals(nodes, *value, out);
        }
        NodeKind::Break(v) => {
            if let Some(v) = v {
                collect_locals(nodes, *v, out);
            }
        }
        NodeKind::If {
            condition,
            then_body,
            else_body,
        }
        | NodeKind::IfExpr {
            condition,
            then_body,
            else_body,
        } => {
            collect_locals(nodes, *condition, out);
            for s in then_body.iter().chain(else_body) {
                collect_locals(nodes, *s, out);
            }
        }
        NodeKind::Loop { body } => {
            for s in body {
                collect_locals(nodes, *s, out);
            }
        }
        NodeKind::Match { scrutinee, arms } => {
            collect_locals(nodes, *scrutinee, out);
            for arm in arms {
                for s in &arm.body {
                    collect_locals(nodes, *s, out);
                }
            }
        }
        NodeKind::IntegerLiteral(_)
        | NodeKind::BooleanLiteral(_)
        | NodeKind::Null
        | NodeKind::FunctionRef(_)
        | NodeKind::Continue => {}
    }
}

#[cfg(test)]
mod tests {
    use crate::{ir, pipeline};

    /// Compile `src` (a whole program) through to optimized IR. `to_ir` runs
    /// `optimized()` runs `analyze_param_escapes`, so the returned module has
    /// `param_noescape` populated (matching the release pipeline).
    fn ir_of(src: &str) -> ir::Module {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let uniq = N.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("ir_opt_{}_{uniq}.solar", std::process::id()));
        std::fs::write(&path, src).unwrap();
        let result = pipeline::compile(&path);
        let _ = std::fs::remove_file(&path);
        let typed = result.unwrap_or_else(|(errs, _)| panic!("compile failed: {errs:?}"));
        typed.to_ir().optimized().ir
    }

    /// Find a (root-file) function by its original name. IR names are
    /// length-prefix mangled (`noesc` -> `5_noescG2_...`); a zero-parameter
    /// function keeps its plain name (`caller`). Accept either form.
    fn find_func<'a>(m: &'a ir::Module, name: &str) -> &'a ir::Function {
        let needle = format!("{}_{}G", name.len(), name);
        m.functions
            .iter()
            .find(|f| f.name == name || f.name.contains(&needle))
            .unwrap_or_else(|| panic!("function `{name}` not found"))
    }

    fn noescape_of(m: &ir::Module, name: &str) -> Vec<bool> {
        find_func(m, name).param_noescape.clone()
    }

    #[test]
    fn noescape_basic() {
        let m = ir_of(
            "fn noesc(x: Int, y: Int) -> Int { x + y }\n\
             fn addrfn(x: Int) -> &Int { x& }\n\
             fn main() { println(noesc(1, 2)); println(addrfn(5)@); }\n",
        );
        // Neither param's address is taken -> proven non-escaping.
        assert_eq!(noescape_of(&m, "noesc"), vec![true, true]);
        // `x&` takes the address of `x` -> must conservatively escape.
        assert_eq!(noescape_of(&m, "addrfn"), vec![false]);
    }

    #[test]
    fn noescape_partial() {
        // Only `b`'s address is taken; `a` and `c` are clean.
        let m = ir_of(
            "fn partialfn(a: Int, b: Int, c: Int) -> &Int { let _ = a + c; b& }\n\
             fn main() { println(partialfn(1, 2, 3)@); }\n",
        );
        assert_eq!(noescape_of(&m, "partialfn"), vec![true, false, true]);
    }

    #[test]
    fn noescape_closure_capture() {
        // A parameter captured by a closure lowers to a `Ref`, so it must be
        // treated as escaping.
        let m = ir_of(
            "fn capfn(captured: Int, plain: Int) -> fn() -> Int { let _ = plain; \\ captured }\n\
             fn main() { println(capfn(7, 9)()); }\n",
        );
        assert_eq!(noescape_of(&m, "capfn"), vec![false, true]);
    }

    #[test]
    fn noescape_transitive() {
        // `fwd` only forwards its pointer to `reads` (which just derefs), so the
        // fixpoint should mark `fwd`'s param non-escaping too. `leakfwd` forwards
        // to `leaks` (which returns it), so it stays escaping.
        let m = ir_of(
            "fn reads(p: &Int) -> Int { p@ }\n\
             fn leaks(p: &Int) -> &Int { p }\n\
             fn fwd(q: &Int) -> Int { reads(q) }\n\
             fn leakfwd(q: &Int) -> &Int { leaks(q) }\n\
             fn main() {\n\
               let a = 1; let b = 2;\n\
               println(fwd(a&)); println(leakfwd(b&)@);\n\
             }\n",
        );
        assert_eq!(noescape_of(&m, "reads"), vec![true]);
        assert_eq!(noescape_of(&m, "fwd"), vec![true]); // transitive
        assert_eq!(noescape_of(&m, "leaks"), vec![false]);
        assert_eq!(noescape_of(&m, "leakfwd"), vec![false]);
        // `a` flows fwd -> reads (both non-escaping) -> stack; `b` reaches leaks.
        assert!(int_let_noescape(&m, "main", 1));
        assert!(!int_let_noescape(&m, "main", 2));
    }

    #[test]
    fn noescape_value_param_addr_to_noescape_callee() {
        // A *value* parameter whose address is taken only to forward it into a
        // non-escaping callee is itself non-escaping — the `insert(key) ->
        // key_hash(key&)/find(key&)` pattern. `x&` flows to `reads` (which only
        // derefs), so `x` needs no per-call box. `leaktaker` forwards `y&` to
        // `leaks` (which returns it), so `y` stays escaping.
        let m = ir_of(
            "fn reads(p: &Int) -> Int { p@ }\n\
             fn leaks(p: &Int) -> &Int { p }\n\
             fn taker(x: Int) -> Int { reads(x&) }\n\
             fn leaktaker(y: Int) -> &Int { leaks(y&) }\n\
             fn main() { println(taker(3)); println(leaktaker(4)@); }\n",
        );
        assert_eq!(noescape_of(&m, "taker"), vec![true]);
        assert_eq!(noescape_of(&m, "leaktaker"), vec![false]);
    }

    /// Whether the `let = <n>` binding (the `Let` whose value is the integer
    /// literal `n`) in function `name` is marked non-escaping. Targets the data
    /// binding specifically, ignoring compiler-generated reference temps.
    fn int_let_noescape(m: &ir::Module, name: &str, n: i64) -> bool {
        let f = find_func(m, name);
        f.nodes
            .iter()
            .find_map(|node| {
                if let ir::NodeKind::Let {
                    value, noescape, ..
                } = &node.kind
                    && matches!(f.nodes[value.0].kind, ir::NodeKind::IntegerLiteral(v) if v == n)
                {
                    return Some(*noescape);
                }
                None
            })
            .unwrap_or_else(|| panic!("no `let = {n}` in `{name}`"))
    }

    #[test]
    fn let_noescape_address_never_taken() {
        // `z`'s address is never taken -> vacuously non-escaping.
        let m = ir_of(
            "fn f() -> Int { let z = 99; z + 1 }\n\
             fn main() { println(f()); }\n",
        );
        assert!(int_let_noescape(&m, "f", 99));
    }

    #[test]
    fn let_noescape_direct_call() {
        // `a&` passed directly to `reads`, whose `&Int` param is only deref'd
        // (non-escaping) -> `a` is stack-eligible.
        let m = ir_of(
            "fn reads(p: &Int) -> Int { p@ }\n\
             fn caller() -> Int { let a = 10; reads(a&) }\n\
             fn main() { println(caller()); }\n",
        );
        assert_eq!(noescape_of(&m, "reads"), vec![true]);
        assert!(int_let_noescape(&m, "caller", 10));
    }

    #[test]
    fn let_noescape_indirect_via_contained_binding() {
        // Routing `b&` through a named binding `rb` is fine: `rb` is contained —
        // its only use forwards the pointer to `reads`' non-escaping param — so
        // storing `b&` into `rb` doesn't let `b` escape. `b` and the two-hop `c`
        // are both stack-allocatable.
        let m = ir_of(
            "fn reads(p: &Int) -> Int { p@ }\n\
             fn caller() -> Int {\n\
               let b = 20; let rb = b&; let x = reads(rb);\n\
               let c = 30; let r1 = c&; let r2 = r1; let y = reads(r2);\n\
               x + y\n\
             }\n\
             fn main() { println(caller()); }\n",
        );
        assert!(int_let_noescape(&m, "caller", 20));
        assert!(int_let_noescape(&m, "caller", 30));
    }

    #[test]
    fn let_noescape_indirect_escaping_not_marked() {
        // `leaks` returns its pointer param (escapes). Routing `c&` through a
        // binding `rc` that then feeds `leaks` does NOT make `c` non-escaping —
        // `rc` isn't contained (its contents reach an escaping place).
        let m = ir_of(
            "fn leaks(p: &Int) -> &Int { p }\n\
             fn caller() -> Int { let c = 30; let rc = c&; leaks(rc)@ }\n\
             fn main() { println(caller()); }\n",
        );
        assert_eq!(noescape_of(&m, "leaks"), vec![false]);
        assert!(!int_let_noescape(&m, "caller", 30));
    }

    #[test]
    fn let_noescape_direct_escaping_callee_not_marked() {
        // Direct `c&` to an escaping callee is still not marked.
        let m = ir_of(
            "fn leaks(p: &Int) -> &Int { p }\n\
             fn caller() -> Int { let c = 30; leaks(c&)@ }\n\
             fn main() { println(caller()); }\n",
        );
        assert!(!int_let_noescape(&m, "caller", 30));
    }
}
