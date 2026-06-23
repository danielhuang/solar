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

/// Refine each function's `param_noescape` (one fixpoint round); returns whether
/// any flag flipped `false` → `true`. `param_noescape[i] == true` means an
/// argument passed to parameter `i` cannot escape the call. Two cases, by type:
///
/// * **Value parameter** (`Int`, a struct, …): non-escaping iff its address is
///   never taken (no `Local(param)` beneath a `Ref`/`Unique`, closure captures
///   included). Copies of its value can never make its storage escape.
/// * **Reference parameter** (`&T`, `&?T`): the parameter *holds a pointer* that
///   must not escape (else a caller passing `&stack_local` would dangle).
///   Non-escaping iff (b) its own storage is never addressed (which would
///   re-expose the pointer, e.g. `(p@)&`) and (a) every `Local(param)` use is
///   either the direct operand of a `Deref` (only read/written through) **or** an
///   argument passed to *another* parameter already known non-escaping. The
///   second alternative is the **transitive** case (`f(p: &T) { g(p) }` is
///   non-escaping once `g`'s parameter is), and is why this must reach a fixpoint
///   — a chain like `get → find`/`key_hash → hash` resolves over several rounds.
///
/// Sound by monotone induction from the all-may-escape start: a param is marked
/// only when justified by facts already established. Anything uncertain stays
/// `false`, so the result never claims non-escape when escape is possible.
pub fn analyze_param_escapes(module: &mut Module) -> bool {
    let snapshot = param_noescape_snapshot(module);
    let mut changed = false;
    for func in &mut module.functions {
        let nodes = &func.nodes;
        // Variables whose address is taken (operand subtree of any Ref/Unique).
        let mut addr_taken: HashSet<VarId> = HashSet::new();
        // Node indices that are the direct operand of a `Deref` (read/written
        // through, rather than the pointer value itself being consumed).
        let mut deref_operand: HashSet<usize> = HashSet::new();
        for node in nodes {
            match &node.kind {
                NodeKind::Ref(op) | NodeKind::Unique(op) => {
                    collect_locals(nodes, *op, &mut addr_taken);
                }
                NodeKind::Deref(op) => {
                    deref_operand.insert(op.0);
                }
                _ => {}
            }
        }
        // Uses that forward the pointer into an already-non-escaping callee param.
        let good_use = good_call_args(nodes, &snapshot);

        let results: Vec<bool> = func
            .params
            .iter()
            .map(|p| {
                param_does_not_escape(nodes, p.var, &p.ty, &addr_taken, &deref_operand, &good_use)
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

fn is_reference_type(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Ref(_) | Type::RefUnsized(_) | Type::NullableRef(_) | Type::NullableRefUnsized(_)
    )
}

fn param_does_not_escape(
    nodes: &[Node],
    var: VarId,
    ty: &Type,
    addr_taken: &HashSet<VarId>,
    deref_operand: &HashSet<usize>,
    good_use: &HashSet<usize>,
) -> bool {
    if is_reference_type(ty) {
        // (b) storage never addressed, and (a) every value use is either a deref
        // or forwarded into a non-escaping callee param.
        !addr_taken.contains(&var)
            && nodes.iter().enumerate().all(|(idx, n)| match &n.kind {
                NodeKind::Local(v) if *v == var => {
                    deref_operand.contains(&idx) || good_use.contains(&idx)
                }
                _ => true,
            })
    } else {
        !addr_taken.contains(&var)
    }
}

/// Mark `Let` bindings that provably don't escape so codegen can place them on
/// the C stack. A binding `V` is non-escaping when **every** pointer to its
/// storage is `V&` taken *directly as a call argument* whose callee parameter is
/// itself non-escaping (`param_noescape`) — which is **vacuously true when `V`'s
/// address is never taken at all** (nothing points at it, so nothing can leak
/// it). Deliberately simple — anything more involved is treated as escaping:
///   * `^V` (unique pointer) or taking the address of a field/element of `V`;
///   * routing `V&` through another binding first (`let r = V&; f(r)`), since the
///     reference temp is then used as something other than a direct call arg.
///
/// Must run after `analyze_param_escapes` (it reads callees' `param_noescape`).
/// Returns whether any `Let` flag flipped `false` → `true`.
fn analyze_let_noescape(module: &mut Module) -> bool {
    let noescape_params = param_noescape_snapshot(module);
    let mut changed = false;
    for func in &mut module.functions {
        let to_mark = compute_noescape_lets(&func.nodes, &noescape_params);
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

/// Returns the node indices of the `Let` bindings in `nodes` that can be marked
/// non-escaping. Pure analysis over an immutable borrow.
fn compute_noescape_lets(
    nodes: &[Node],
    noescape_params: &HashMap<String, Vec<bool>>,
) -> Vec<usize> {
    // Node indices that appear as a call argument bound for a non-escaping param.
    let good_arg = good_call_args(nodes, noescape_params);

    // value node -> the `Let` var bound to it; var -> its defining `Let` node.
    let mut let_var_of_value: HashMap<usize, VarId> = HashMap::new();
    let mut let_node_of_var: HashMap<VarId, usize> = HashMap::new();
    // All `Local(var)` use sites, grouped by var.
    let mut local_uses: HashMap<VarId, Vec<usize>> = HashMap::new();
    for (idx, node) in nodes.iter().enumerate() {
        match &node.kind {
            NodeKind::Let { var, value, .. } => {
                let_var_of_value.insert(value.0, *var);
                let_node_of_var.insert(*var, idx);
            }
            NodeKind::Local(v) => local_uses.entry(*v).or_default().push(idx),
            _ => {}
        }
    }

    // For each binding V: the `Ref` nodes addressing V's own storage, plus a
    // "give up" set for addressing we don't handle.
    let mut direct_refs: HashMap<VarId, Vec<usize>> = HashMap::new();
    let mut bad: HashSet<VarId> = HashSet::new();
    for (idx, node) in nodes.iter().enumerate() {
        match &node.kind {
            NodeKind::Ref(op) => {
                // A ref of a place rooted at a local `V` (`V`, `V.field`, `V[i]`,
                // … but not reached through a deref) addresses `V`'s own storage,
                // so route-check it for `V`. A ref reached through a deref points
                // into a pointee — a different allocation — so it can't make any
                // local's storage escape and is ignored here.
                if let Some(v) = ref_root_local(nodes, *op) {
                    direct_refs.entry(v).or_default().push(idx);
                }
            }
            NodeKind::Unique(op) => {
                // `^place` moves/deep-copies; conservatively give up on its locals.
                let mut s = HashSet::new();
                collect_locals(nodes, *op, &mut s);
                bad.extend(s);
            }
            _ => {}
        }
    }

    let mut result = Vec::new();
    for (v, &idx) in &let_node_of_var {
        if bad.contains(v) {
            continue;
        }
        // Every direct `V&` must route to a non-escaping call argument. When the
        // address is never taken (`direct_refs` has no entry) this is vacuously
        // true — a binding nothing points at cannot escape.
        let refs = direct_refs.get(v).map(Vec::as_slice).unwrap_or(&[]);
        let ok = refs.iter().all(|&ref_idx| {
            // The `V&` result must be bound to a reference temp RT, and every use
            // of RT must be a non-escaping call argument — so the pointer only
            // ever flows into a non-escaping callee param, never elsewhere.
            let Some(&rt) = let_var_of_value.get(&ref_idx) else {
                return false;
            };
            match local_uses.get(&rt) {
                Some(uses) if !uses.is_empty() => uses.iter().all(|u| good_arg.contains(u)),
                _ => false,
            }
        });
        if ok {
            result.push(idx);
        }
    }
    result
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
    fn let_noescape_indirect_not_marked() {
        // Routing `b&` through a named binding first means the reference temp is
        // used as a `let` value, not a direct call arg -> `b` not marked.
        let m = ir_of(
            "fn reads(p: &Int) -> Int { p@ }\n\
             fn caller() -> Int { let b = 20; let rb = b&; reads(rb) }\n\
             fn main() { println(caller()); }\n",
        );
        assert!(!int_let_noescape(&m, "caller", 20));
    }

    #[test]
    fn let_noescape_escaping_callee_not_marked() {
        // `leaks` returns its pointer param -> the param escapes, so passing
        // `c&` to it does not make `c` non-escaping.
        let m = ir_of(
            "fn leaks(p: &Int) -> &Int { p }\n\
             fn caller() -> Int { let c = 30; leaks(c&)@ }\n\
             fn main() { println(caller()); }\n",
        );
        assert_eq!(noescape_of(&m, "leaks"), vec![false]);
        assert!(!int_let_noescape(&m, "caller", 30));
    }
}
