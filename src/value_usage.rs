//! Conservative, control-flow-aware last-use moves, shared by every backend.

use crate::mangled_ast::{
    Expr, ExprKind as E, SourceFile, Statement, StatementKind as S, Type, TypedPattern,
};
use crate::tree_ir::{self, Module};
use std::collections::{HashMap, HashSet};

type Live = HashSet<String>;

pub(crate) fn insert_moves(source: &mut SourceFile) {
    // Reuse the IR's interprocedural escape proof, before adding moves. This
    // also accounts for references returned/stored by implicit constructors.
    let mut module = tree_ir::lower(source);
    while crate::tree_ir_opt::analyze_param_escapes(&mut module) {}
    let noescape: HashMap<_, _> = module
        .functions
        .iter()
        .map(|f| (f.name.clone(), f.param_noescape.clone()))
        .collect();
    source.copy_noescape = source
        .copy_constructors
        .iter()
        .filter(|(_, f)| noescape.get(*f).is_some_and(|p| p.first() == Some(&true)))
        .map(|(ty, _)| ty.clone())
        .collect();
    let mut captures: HashMap<String, Live> = HashMap::new();
    for function in source.functions.values_mut() {
        walk_body(&mut function.body, &mut |e| {
            if let E::Closure {
                synthetic_fn,
                captures: vars,
            } = &e.kind
            {
                captures
                    .entry(synthetic_fn.clone())
                    .or_default()
                    .extend(vars.iter().map(|v| v.name.clone()));
            }
        });
    }
    for function in source.functions.values_mut() {
        let mut pinned = captures.remove(&function.name).unwrap_or_default();
        let mut declared: Live = function.parameters.iter().map(|p| p.name.clone()).collect();
        // Shadowed names are conservatively excluded; liveness can then use
        // source names without conflating two movable bindings.
        collect_declarations(&function.body, &mut declared, &mut pinned);
        collect_aliases(&mut function.body, &noescape, &mut pinned);
        let mut analysis = Analysis {
            module: &module,
            noescape: &noescape,
            pinned,
            escaping_copies: Live::new(),
            breaks: Live::new(),
            continues: Live::new(),
            mark: false,
        };
        analysis.body(&mut function.body, &mut Live::new(), true);
        analysis
            .pinned
            .extend(std::mem::take(&mut analysis.escaping_copies));
        analysis.mark = true;
        analysis.body(&mut function.body, &mut Live::new(), true);
    }
}

struct Analysis<'a> {
    module: &'a Module,
    noescape: &'a HashMap<String, Vec<bool>>,
    pinned: Live,
    escaping_copies: Live,
    breaks: Live,
    continues: Live,
    mark: bool,
}

impl Analysis<'_> {
    fn body(&mut self, body: &mut [Statement], live: &mut Live, value: bool) {
        let last = body.len().saturating_sub(1);
        for (i, statement) in body.iter_mut().enumerate().rev() {
            match &mut statement.kind {
                S::Let { name, value, .. } => {
                    live.remove(name);
                    self.expr(value, live, true);
                }
                S::Assignment { target, value } => {
                    // A whole-local write ends the previous value's lifetime.
                    // Interior writes still require the containing value.
                    if let E::Identifier(name) = &target.kind {
                        live.remove(name);
                    } else {
                        self.expr(target, live, false);
                    }
                    self.expr(value, live, true);
                }
                S::Expression(e) => self.expr(e, live, value || i != last),
                S::Return(e) => {
                    live.clear();
                    self.expr(e, live, true);
                }
                S::Break(e) => {
                    *live = self.breaks.clone();
                    if let Some(e) = e {
                        self.expr(e, live, true);
                    }
                }
                S::Continue => *live = self.continues.clone(),
                S::If {
                    condition,
                    body,
                    else_body,
                } => {
                    self.branches(condition, body, else_body, live, true);
                }
                S::While { condition, body } => self.loop_body(Some(condition), body, live),
            }
        }
    }

    fn branches(
        &mut self,
        condition: &mut Expr,
        a: &mut [Statement],
        b: &mut [Statement],
        live: &mut Live,
        value: bool,
    ) {
        let mut other = live.clone();
        self.body(a, live, value);
        self.body(b, &mut other, value);
        live.extend(other);
        self.expr(condition, live, true);
    }

    fn loop_body(
        &mut self,
        mut condition: Option<&mut Expr>,
        body: &mut [Statement],
        live: &mut Live,
    ) {
        let outer_breaks = std::mem::replace(&mut self.breaks, live.clone());
        let outer_continues = self.continues.clone();
        let mark = self.mark;
        self.mark = false;
        let mut entry = live.clone();
        loop {
            self.continues = entry.clone();
            let mut before = entry.clone();
            self.body(body, &mut before, true);
            before.extend(live.iter().cloned());
            if let Some(c) = condition.as_deref_mut() {
                self.expr(c, &mut before, true);
            }
            let mut next = entry.clone();
            next.extend(before);
            if next == entry {
                break;
            }
            entry = next;
        }
        self.mark = mark;
        self.continues = entry.clone();
        let mut before = entry;
        self.body(body, &mut before, true);
        before.extend(live.iter().cloned());
        if let Some(c) = condition {
            self.expr(c, &mut before, true);
        }
        *live = before;
        self.breaks = outer_breaks;
        self.continues = outer_continues;
    }

    fn expr(&mut self, expr: &mut Expr, live: &mut Live, value: bool) {
        let nontrivial = tree_ir::type_contains_unique(&expr.ty, &self.module.datatypes)
            || tree_ir::type_contains_copy_constructor(&expr.ty, self.module);
        let last_use = owned_root(expr)
            .is_some_and(|root| !live.contains(root) && !self.pinned.contains(root))
            || fresh_place(expr);
        if value
            && nontrivial
            && !last_use
            && copy_escapes(&expr.ty, self.module, self.noescape, &mut HashSet::new())
            && let Some(root) = owned_root(expr)
        {
            self.escaping_copies.insert(root.to_string());
        }
        let movable = value && self.mark && nontrivial && last_use;
        match &mut expr.kind {
            E::Identifier(name) => {
                live.insert(name.clone());
            }
            E::Move(inner) => self.expr(inner, live, false),
            E::FieldAccess { object, .. } => self.expr(object, live, false),
            E::Index { object, index } => {
                self.expr(index, live, true);
                self.expr(object, live, false);
            }
            E::Slice { object, start, end } => {
                self.expr(end, live, true);
                self.expr(start, live, true);
                self.expr(object, live, false);
            }
            E::Deref(inner) | E::Reference(inner) => self.expr(inner, live, false),
            E::Unique(inner) | E::Not(inner) | E::ArraySizeCoerce { expr: inner, .. } => {
                self.expr(inner, live, true)
            }
            E::Call { arguments, .. } | E::IntrinsicCall { arguments, .. } => {
                // A borrowed argument stays live until the entire call ends,
                // even if a later argument consumes the same local.
                for arg in arguments.iter() {
                    if let E::Reference(inner) = &arg.kind
                        && let Some(root) = owned_root(inner)
                    {
                        live.insert(root.to_string());
                    }
                }
                for arg in arguments.iter_mut().rev() {
                    self.expr(arg, live, true);
                }
            }
            E::CallIndirect { callee, arguments } => {
                // A borrowed argument stays live until the entire call ends,
                // even if a later argument consumes the same local.
                for arg in arguments.iter() {
                    if let E::Reference(inner) = &arg.kind
                        && let Some(root) = owned_root(inner)
                    {
                        live.insert(root.to_string());
                    }
                }
                for arg in arguments.iter_mut().rev() {
                    self.expr(arg, live, true);
                }
                self.expr(callee, live, true);
            }
            E::StructLiteral { fields, .. } => {
                for field in fields.iter_mut().rev() {
                    self.expr(&mut field.value, live, true);
                }
            }
            E::ArrayLiteral(elements) => {
                for element in elements.iter_mut().rev() {
                    self.expr(element, live, true);
                }
            }
            E::ArrayRepeat { element, count } => {
                self.expr(count, live, true);
                self.expr(element, live, true);
            }
            E::ArrayInit { count, init } => {
                self.expr(init, live, true);
                self.expr(count, live, true);
            }
            E::BinaryOp { left, right, op } => {
                let after = live.clone();
                self.expr(right, live, true);
                if matches!(op, crate::ast::BinOp::And | crate::ast::BinOp::Or) {
                    live.extend(after);
                }
                self.expr(left, live, true);
            }
            E::If {
                condition,
                then_body,
                else_body,
            } => self.branches(condition, then_body, else_body, live, value),
            E::Block(body) => self.body(body, live, value),
            E::Loop(body) => self.loop_body(None, body, live),
            E::EnumVariant { value, .. } => {
                if let Some(value) = value {
                    self.expr(value, live, true);
                }
            }
            E::Match { scrutinee, arms } => {
                let after = live.clone();
                for arm in arms {
                    let mut branch = after.clone();
                    self.body(&mut arm.body, &mut branch, value);
                    live.extend(branch);
                }
                self.expr(scrutinee, live, false);
            }
            E::Closure { captures, .. } => {
                for c in captures {
                    self.expr(&mut c.place, live, false);
                }
            }
            E::Global(_)
            | E::IntegerLiteral(_)
            | E::FloatLiteral(_)
            | E::BooleanLiteral(_)
            | E::NullLiteral
            | E::FunctionRef(_) => {}
        }
        if movable {
            let inner = expr.clone();
            expr.kind = E::Move(Box::new(inner));
        }
    }
}

fn owned_root(expr: &Expr) -> Option<&str> {
    match &expr.kind {
        E::Identifier(name) => Some(name),
        E::FieldAccess { object, .. } | E::Index { object, .. } | E::Slice { object, .. } => {
            owned_root(object)
        }
        E::Deref(inner) if matches!(inner.ty, Type::Unique(_) | Type::UniqueUnsized(_)) => {
            owned_root(inner)
        }
        _ => None,
    }
}

// A projection of a freshly produced value has no older owner to preserve.
fn fresh_place(expr: &Expr) -> bool {
    fn temporary(expr: &Expr) -> bool {
        matches!(
            expr.kind,
            E::Call { .. }
                | E::CallIndirect { .. }
                | E::StructLiteral { .. }
                | E::ArrayLiteral(_)
                | E::ArrayRepeat { .. }
                | E::ArrayInit { .. }
                | E::Unique(_)
                | E::EnumVariant { .. }
        ) || fresh_place(expr)
    }
    match &expr.kind {
        E::FieldAccess { object, .. } | E::Index { object, .. } | E::Slice { object, .. } => {
            temporary(object)
        }
        E::Deref(inner) if matches!(inner.ty, Type::Unique(_) | Type::UniqueUnsized(_)) => {
            temporary(inner)
        }
        _ => false,
    }
}

fn copy_escapes(
    ty: &Type,
    module: &Module,
    noescape: &HashMap<String, Vec<bool>>,
    seen: &mut HashSet<Type>,
) -> bool {
    if !seen.insert(ty.clone()) {
        return false;
    }
    if let Some(f) = module.copy_constructors.get(ty) {
        return !noescape.get(f).is_some_and(|p| p.first() == Some(&true));
    }
    match ty {
        Type::Struct(name) | Type::Enum(name) => module.datatypes[name]
            .fields
            .iter()
            .any(|f| copy_escapes(&f.ty, module, noescape, seen)),
        Type::Unique(t) | Type::UniqueUnsized(t) | Type::Array(t) | Type::FixedArray(t, _) => {
            copy_escapes(t, module, noescape, seen)
        }
        _ => false,
    }
}

fn collect_aliases(
    body: &mut [Statement],
    noescape: &HashMap<String, Vec<bool>>,
    pinned: &mut Live,
) {
    // References passed directly to a proven non-escaping parameter have no
    // surviving alias. All other references and closure/match aliases pin the
    // containing local, including references into unique pointees.
    let mut safe_refs = HashSet::new();
    walk_body(body, &mut |expr| {
        if let E::Call {
            function,
            arguments,
        } = &expr.kind
        {
            for (i, arg) in arguments.iter().enumerate() {
                if noescape
                    .get(function)
                    .is_some_and(|p| p.get(i) == Some(&true))
                    && matches!(arg.kind, E::Reference(_))
                {
                    safe_refs.insert(arg as *const Expr as usize);
                }
            }
        }
    });
    walk_body(body, &mut |expr| {
        if safe_refs.contains(&(expr as *const Expr as usize)) {
            return;
        }
        match &expr.kind {
            E::Reference(inner) => {
                if let Some(root) = owned_root(inner) {
                    pinned.insert(root.to_string());
                }
            }
            E::Closure { captures, .. } => {
                for c in captures {
                    if let Some(root) = owned_root(&c.place) {
                        pinned.insert(root.to_string());
                    }
                }
            }
            E::Match { scrutinee, arms } => {
                if let Some(root) = owned_root(scrutinee) {
                    pinned.insert(root.to_string());
                }
                for arm in arms {
                    match &arm.pattern {
                        TypedPattern::Variant {
                            binding: Some((name, _)),
                            ..
                        }
                        | TypedPattern::Wildcard(name, _) => {
                            pinned.insert(name.clone());
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    });
    // Keep this proof deliberately conservative for stored reference aliases.
}

fn collect_declarations(body: &[Statement], seen: &mut Live, pinned: &mut Live) {
    let mut body = body.to_vec();
    fn declare(body: &[Statement], seen: &mut Live, pinned: &mut Live) {
        for s in body {
            match &s.kind {
                S::Let { name, .. } if !seen.insert(name.clone()) => {
                    pinned.insert(name.clone());
                }
                S::If {
                    body, else_body, ..
                } => {
                    declare(body, seen, pinned);
                    declare(else_body, seen, pinned);
                }
                S::While { body, .. } => declare(body, seen, pinned),
                _ => {}
            }
        }
    }
    declare(&body, seen, pinned);
    walk_body(&mut body, &mut |e| match &e.kind {
        E::Block(b) | E::Loop(b) => declare(b, seen, pinned),
        E::If {
            then_body,
            else_body,
            ..
        } => {
            declare(then_body, seen, pinned);
            declare(else_body, seen, pinned);
        }
        E::Match { arms, .. } => {
            for a in arms {
                declare(&a.body, seen, pinned);
            }
        }
        _ => {}
    });
}

fn walk_body(body: &mut [Statement], visit: &mut impl FnMut(&mut Expr)) {
    for s in body {
        match &mut s.kind {
            S::Let { value, .. } | S::Expression(value) | S::Return(value) => {
                walk_expr(value, visit)
            }
            S::Assignment { target, value } => {
                walk_expr(value, visit);
                walk_expr(target, visit);
            }
            S::If {
                condition,
                body,
                else_body,
            } => {
                walk_expr(condition, visit);
                walk_body(body, visit);
                walk_body(else_body, visit);
            }
            S::While { condition, body } => {
                walk_expr(condition, visit);
                walk_body(body, visit);
            }
            S::Break(Some(e)) => walk_expr(e, visit),
            S::Break(None) | S::Continue => {}
        }
    }
}

fn walk_expr(expr: &mut Expr, visit: &mut impl FnMut(&mut Expr)) {
    visit(expr);
    match &mut expr.kind {
        E::Move(e)
        | E::Deref(e)
        | E::Reference(e)
        | E::Unique(e)
        | E::Not(e)
        | E::ArraySizeCoerce { expr: e, .. } => walk_expr(e, visit),
        E::FieldAccess { object, .. } => walk_expr(object, visit),
        E::Index { object, index } => {
            walk_expr(object, visit);
            walk_expr(index, visit);
        }
        E::Slice { object, start, end } => {
            walk_expr(object, visit);
            walk_expr(start, visit);
            walk_expr(end, visit);
        }
        E::Call { arguments, .. } | E::IntrinsicCall { arguments, .. } => {
            for e in arguments {
                walk_expr(e, visit);
            }
        }
        E::CallIndirect { callee, arguments } => {
            walk_expr(callee, visit);
            for e in arguments {
                walk_expr(e, visit);
            }
        }
        E::StructLiteral { fields, .. } => {
            for f in fields {
                walk_expr(&mut f.value, visit);
            }
        }
        E::ArrayLiteral(elements) => {
            for e in elements {
                walk_expr(e, visit);
            }
        }
        E::ArrayRepeat { element, count } => {
            walk_expr(element, visit);
            walk_expr(count, visit);
        }
        E::ArrayInit { count, init } => {
            walk_expr(count, visit);
            walk_expr(init, visit);
        }
        E::BinaryOp { left, right, .. } => {
            walk_expr(left, visit);
            walk_expr(right, visit);
        }
        E::If {
            condition,
            then_body,
            else_body,
        } => {
            walk_expr(condition, visit);
            walk_body(then_body, visit);
            walk_body(else_body, visit);
        }
        E::Block(b) | E::Loop(b) => walk_body(b, visit),
        E::EnumVariant { value: Some(e), .. } => walk_expr(e, visit),
        E::Match { scrutinee, arms } => {
            walk_expr(scrutinee, visit);
            for a in arms {
                walk_body(&mut a.body, visit);
            }
        }
        E::Closure { captures, .. } => {
            for c in captures {
                walk_expr(&mut c.place, visit);
            }
        }
        _ => {}
    }
}
