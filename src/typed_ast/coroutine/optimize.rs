//! Keep storage that is confined to one dispatch block out of the coroutine.

use super::*;

/// A temporary used only within one block cannot be live across suspension.
/// Keep address-exposed storage in the frame, since its aliases may outlive
/// that block. Source locals retain their requested persistent representation.
pub(super) fn localize_temporaries(machine: &mut Machine<'_, '_>) {
    let mut uses: HashMap<String, HashSet<usize>> = HashMap::new();
    let mut addressed = HashSet::new();
    // Catch callbacks write these slots outside the driver's dispatch blocks.
    for handler in &mut machine.handlers {
        visit_expr(&mut handler.exception, &mut |e| {
            if let Some(name) = temporary_field(e, &machine.frame_name) {
                addressed.insert(name.to_owned());
            }
        });
    }
    for (index, block) in machine.blocks.iter_mut().enumerate() {
        for stmt in block {
            visit_statement(stmt, &mut |e| {
                if let Some(name) = temporary_field(e, &machine.frame_name) {
                    uses.entry(name.to_owned()).or_default().insert(index);
                }
                let mut mark_addressed = |place: &mut Expr| {
                    visit_expr(place, &mut |e| {
                        if let Some(name) = temporary_field(e, &machine.frame_name) {
                            addressed.insert(name.to_owned());
                        }
                    });
                };
                match &mut e.kind {
                    ExprKind::Reference(place) | ExprKind::Unique(place) => mark_addressed(place),
                    ExprKind::Closure { captures, .. } => {
                        for capture in captures {
                            if let Some(place) = &mut capture.place {
                                mark_addressed(place);
                            }
                        }
                    }
                    // Pattern bindings alias the scrutinee.
                    ExprKind::Match { scrutinee, .. } => mark_addressed(scrutinee),
                    _ => {}
                }
            });
        }
    }
    let mut promoted = HashMap::new();
    let mut declarations = vec![Vec::new(); machine.blocks.len()];
    machine.fields.retain(|field| {
        let Some(blocks) = uses.get(&field.name) else {
            return true;
        };
        if blocks.len() != 1 || addressed.contains(&field.name) {
            return true;
        }
        let block = *blocks.iter().next().unwrap();
        let name = ast::Ident::synthetic(format!("coroutine_{}", field.name));
        promoted.insert(field.name.clone(), name.clone());
        declarations[block].push(statement(
            StatementKind::Let {
                name,
                ty: field.value.ty.clone(),
                value: field.value.clone(),
            },
            machine.span,
        ));
        false
    });
    for (block, mut declarations) in machine.blocks.iter_mut().zip(declarations) {
        visit_body(block, &mut |e| {
            if let Some(name) = temporary_field(e, &machine.frame_name)
                && let Some(local) = promoted.get(name)
            {
                e.kind = ExprKind::Identifier(local.clone());
            }
        });
        declarations.append(block);
        *block = declarations;
    }
}

fn temporary_field<'a>(e: &'a Expr, frame_name: &ast::Ident) -> Option<&'a str> {
    if let ExprKind::FieldAccess { object, field } = &e.kind
        && field.starts_with("temp_")
        && let ExprKind::Deref(base) = &object.kind
        && matches!(&base.kind, ExprKind::Identifier(name) if name == frame_name)
    {
        Some(field)
    } else {
        None
    }
}

fn visit_body(body: &mut [Statement], visit: &mut impl FnMut(&mut Expr)) {
    for s in body {
        visit_statement(s, visit);
    }
}

fn visit_statement(s: &mut Statement, visit: &mut impl FnMut(&mut Expr)) {
    match &mut s.kind {
        StatementKind::Let { value, .. }
        | StatementKind::Expression(value)
        | StatementKind::Return(value) => visit_expr(value, visit),
        StatementKind::Assignment { target, value } => {
            visit_expr(target, visit);
            visit_expr(value, visit);
        }
        StatementKind::If {
            condition,
            body,
            else_body,
        } => {
            visit_expr(condition, visit);
            visit_body(body, visit);
            visit_body(else_body, visit);
        }
        StatementKind::While { condition, body } => {
            visit_expr(condition, visit);
            visit_body(body, visit);
        }
        StatementKind::CoroutineTry { body, handler, .. } => {
            visit_body(body, visit);
            visit_body(handler, visit);
        }
        StatementKind::Break(Some(value)) => visit_expr(value, visit),
        StatementKind::Break(None) | StatementKind::Continue => {}
    }
}

fn visit_expr(e: &mut Expr, visit: &mut impl FnMut(&mut Expr)) {
    match &mut e.kind {
        ExprKind::FieldAccess { object, .. }
        | ExprKind::Deref(object)
        | ExprKind::Reference(object)
        | ExprKind::Unique(object)
        | ExprKind::Not(object)
        | ExprKind::Yield(object)
        | ExprKind::Await {
            coroutine: object, ..
        }
        | ExprKind::ArraySizeCoerce { expr: object, .. } => visit_expr(object, visit),
        ExprKind::Call { arguments, .. } | ExprKind::IntrinsicCall { arguments, .. } => {
            for e in arguments {
                visit_expr(e, visit);
            }
        }
        ExprKind::CallIndirect { callee, arguments } => {
            visit_expr(callee, visit);
            for e in arguments {
                visit_expr(e, visit);
            }
        }
        ExprKind::StructLiteral { fields, .. } => {
            for f in fields {
                visit_expr(&mut f.value, visit);
            }
        }
        ExprKind::Index { object, index } => {
            visit_expr(object, visit);
            visit_expr(index, visit);
        }
        ExprKind::Slice { object, start, end } => {
            visit_expr(object, visit);
            visit_expr(start, visit);
            visit_expr(end, visit);
        }
        ExprKind::ArrayLiteral(values) => {
            for e in values {
                visit_expr(e, visit);
            }
        }
        ExprKind::ArrayRepeat { element, count } => {
            visit_expr(element, visit);
            visit_expr(count, visit);
        }
        ExprKind::ArrayInit { count, init } => {
            visit_expr(count, visit);
            visit_expr(init, visit);
        }
        ExprKind::BinaryOp { left, right, .. } => {
            visit_expr(left, visit);
            visit_expr(right, visit);
        }
        ExprKind::If {
            condition,
            then_body,
            else_body,
        } => {
            visit_expr(condition, visit);
            visit_body(then_body, visit);
            visit_body(else_body, visit);
        }
        ExprKind::Block(body) | ExprKind::Loop(body) => visit_body(body, visit),
        ExprKind::Closure { captures, .. } => {
            for c in captures {
                if let Some(place) = &mut c.place {
                    visit_expr(place, visit);
                }
            }
        }
        ExprKind::EnumVariant { value: Some(e), .. } => visit_expr(e, visit),
        ExprKind::Match { scrutinee, arms } => {
            visit_expr(scrutinee, visit);
            for arm in arms {
                visit_body(&mut arm.body, visit);
            }
        }
        ExprKind::Identifier(_)
        | ExprKind::FloatLiteral(_)
        | ExprKind::Global(_)
        | ExprKind::IntegerLiteral(_)
        | ExprKind::BooleanLiteral(_)
        | ExprKind::NullLiteral
        | ExprKind::FunctionRef(_)
        | ExprKind::EnumVariant { value: None, .. } => {}
    }
    visit(e);
}
