//! Selection of explicitly overlapping overloads without leaking failed lowerings.

use super::*;

pub(super) fn definition_location(definition: &ast::FunctionDef) -> (u32, u32, u32) {
    (
        definition.span.file_id,
        definition.span.start.line,
        definition.span.start.col,
    )
}

impl Lowerer<'_> {
    pub(super) fn overlap_index(&self, definition: &ast::FunctionDef) -> Option<usize> {
        definition.is_overlap.then(|| {
            self.overlap_indices[&(
                definition.span.file_id,
                definition.span.start.line,
                definition.span.start.col,
            )]
        })
    }

    /// Select an implicitly invoked function without manufacturing a runtime value.
    pub(super) fn resolve_call_with_parameter_types(
        &mut self,
        entries: Vec<FunctionEntry>,
        name: &str,
        parameters: &[Type],
        span: ast::SourceSpan,
    ) -> Result<Expr, CompileError> {
        self.push_scope();
        let arguments: Vec<_> = parameters
            .iter()
            .enumerate()
            .map(|(index, ty)| {
                let name = ast::Ident::synthetic(format!("overlap_argument_{index}"));
                self.define_var(name.clone(), ty.clone());
                ast::Expr {
                    kind: ast::ExprKind::Identifier(name),
                    span,
                }
            })
            .collect();
        let result = self.resolve_overloaded_call(
            CandidateSource::Entries(entries),
            name,
            &arguments,
            &[],
            &[],
            span,
        );
        self.pop_scope();
        result
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn resolve_overloaded_call(
        &mut self,
        source: CandidateSource,
        name: &str,
        arguments: &[ast::Expr],
        kwargs: &[(String, ast::Expr)],
        type_args: &[ast::Type],
        span: ast::SourceSpan,
    ) -> Result<Expr, CompileError> {
        let has_overlap = match &source {
            CandidateSource::Methods => self.method_defs.get(name).is_some_and(|entries| {
                entries.iter().any(|entry| {
                    entry.ast_def.is_overlap
                        && self.source.method_is_visible(&entry.ast_def, span.file_id)
                })
            }),
            CandidateSource::Entries(entries) => {
                entries.iter().any(|entry| entry.ast_def.is_overlap)
            }
            CandidateSource::MethodEntries(_) | CandidateSource::FunctionProbe(_) => false,
        };
        if !has_overlap {
            return self.resolve_overloaded_call_inner(
                source, name, arguments, kwargs, type_args, span, None,
            );
        }
        let is_method = matches!(source, CandidateSource::Methods);
        let mut entries = match source {
            CandidateSource::Methods => {
                let mut entries = self.method_generic_entries(name, span.file_id);
                entries.extend(self.method_concrete_entries(name, None, span.file_id));
                entries
            }
            CandidateSource::Entries(entries) => entries,
            _ => unreachable!(),
        };
        let overload_count = if is_method {
            self.method_defs[name]
                .iter()
                .filter(|entry| !entry.type_params.is_empty())
                .count()
        } else {
            entries
                .iter()
                .filter(|entry| !entry.type_params.is_empty())
                .count()
        };
        entries.sort_by(|a, b| {
            let a_params: Vec<_> = a.ast_def.parameters.iter().map(|p| p.ty.clone()).collect();
            let b_params: Vec<_> = b.ast_def.parameters.iter().map(|p| p.ty.clone()).collect();
            b.owner_specificity
                .cmp(&a.owner_specificity)
                .then_with(|| {
                    compare_overload_specificity(
                        &b_params,
                        &b.type_params,
                        &a_params,
                        &a.type_params,
                    )
                })
                .then_with(|| {
                    if a.ast_def.is_overlap && b.ast_def.is_overlap {
                        self.overlap_index(&b.ast_def)
                            .cmp(&self.overlap_index(&a.ast_def))
                    } else {
                        Ordering::Equal
                    }
                })
        });
        let attempt = |lowerer: &mut Self, entry: &FunctionEntry| {
            let candidates = if is_method {
                CandidateSource::MethodEntries(vec![entry.clone()])
            } else {
                CandidateSource::FunctionProbe(vec![entry.clone()])
            };
            lowerer.resolve_overloaded_call_inner(
                candidates,
                name,
                arguments,
                kwargs,
                type_args,
                span,
                Some(overload_count),
            )
        };

        // Overlap never gives an extension priority over an ordinary method.
        // Check conflicts even when an ordinary candidate would win the ranking.
        for extension in entries.iter().filter(|entry| entry.ast_def.is_extension) {
            if attempt(&mut self.clone(), extension).is_err() {
                continue;
            }
            for other in &entries {
                if !Rc::ptr_eq(&extension.ast_def, &other.ast_def)
                    && attempt(&mut self.clone(), other).is_ok()
                {
                    return Err(extension_conflict(
                        name,
                        span,
                        &extension.ast_def,
                        &other.ast_def,
                    ));
                }
            }
        }

        let mut failure = None;
        for entry in entries {
            let mut trial = self.clone();
            trial.failed_overload = None;
            match attempt(&mut trial, &entry) {
                Ok(result) => {
                    self.require_unsafe_access(&entry.ast_def, span)?;
                    *self = trial;
                    return Ok(result);
                }
                Err(error) => {
                    // Only marked bodies participate in substitution fallback.
                    // An ordinary overload can be skipped for a signature
                    // mismatch, but a failure in its selected body is an error.
                    if !entry.ast_def.is_overlap
                        && trial.failed_overload == Some(definition_location(&entry.ast_def))
                    {
                        return Err(error);
                    }
                    failure.get_or_insert(error);
                }
            }
        }
        Err(failure.unwrap())
    }

    pub(super) fn resolve_nested_overloads(
        &mut self,
        name: &str,
        entries: &[FunctionEntry],
        arguments: &[ast::Expr],
        type_args: &[ast::Type],
        span: ast::SourceSpan,
    ) -> Result<Expr, CompileError> {
        let mut entries = entries.to_vec();
        if entries.iter().any(|entry| entry.ast_def.is_overlap) {
            entries.sort_by(|a, b| {
                let a_params: Vec<_> = a.ast_def.parameters.iter().map(|p| p.ty.clone()).collect();
                let b_params: Vec<_> = b.ast_def.parameters.iter().map(|p| p.ty.clone()).collect();
                compare_overload_specificity(&b_params, &b.type_params, &a_params, &a.type_params)
                    .then_with(|| {
                        if a.ast_def.is_overlap && b.ast_def.is_overlap {
                            b.overload_index.cmp(&a.overload_index)
                        } else {
                            Ordering::Equal
                        }
                    })
            });
        }
        let mut failure = None;
        for entry in entries {
            if entry.ast_def.is_overlap {
                let mut trial = self.clone();
                match trial.lower_nested_candidate(name, &entry, arguments, type_args, span) {
                    Ok(Some(result)) => {
                        self.require_unsafe_access(&entry.ast_def, span)?;
                        *self = trial;
                        return Ok(result);
                    }
                    Ok(None) => {}
                    Err(error) => {
                        failure.get_or_insert(error);
                    }
                }
            } else if let Some(result) =
                self.lower_nested_candidate(name, &entry, arguments, type_args, span)?
            {
                return Ok(result);
            }
        }
        Err(failure.unwrap_or_else(|| {
            CompileError::new(
                format!("no matching overload for nested function `{name}` with given arguments"),
                span,
            )
        }))
    }

    fn lower_nested_candidate(
        &mut self,
        name: &str,
        entry: &FunctionEntry,
        arguments: &[ast::Expr],
        type_args: &[ast::Type],
        span: ast::SourceSpan,
    ) -> Result<Option<Expr>, CompileError> {
        let gdef = &entry.ast_def;
        if entry.type_params.is_empty() {
            // Concrete nested overload
            if gdef.parameters.len() != arguments.len() {
                return Ok(None);
            }
            let lowered_args: Vec<Expr> = arguments
                .iter()
                .map(|a| self.lower_expr(a))
                .collect::<Result<Vec<_>, _>>()?;

            let param_types: Vec<Type> = gdef
                .parameters
                .iter()
                .map(|p| self.resolve_ast_type(&p.ty))
                .collect::<Result<Vec<_>, _>>()?;
            let all_coercible = lowered_args
                .iter()
                .zip(param_types.iter())
                .all(|(arg, pty)| arg.ty == *pty || self.try_coerce(arg.clone(), pty).ty == *pty);
            if !all_coercible {
                return Ok(None);
            }

            if !gdef.is_overlap {
                self.require_unsafe_access(gdef, span)?;
            }
            let closure_expr = self.lower_closure(
                span,
                &gdef.parameters,
                gdef.return_type.as_ref(),
                &gdef.body,
            )?;
            let (cparams, return_type) = match &closure_expr.ty {
                Type::Function {
                    params,
                    return_type,
                } => (params.clone(), (**return_type).clone()),
                _ => unreachable!(),
            };
            let coerced_args: Vec<Expr> = lowered_args
                .into_iter()
                .zip(cparams.iter())
                .map(|(arg, pty)| self.try_coerce(arg, pty))
                .collect();

            Ok(Some(Expr {
                ty: return_type,
                kind: ExprKind::CallIndirect {
                    callee: Box::new(closure_expr),
                    arguments: coerced_args,
                },
                span,
            }))
        } else {
            // Generic nested function
            if gdef.parameters.len() != arguments.len() {
                return Ok(None);
            }
            let type_params = &entry.type_params;
            let (effective_type_args, pre_lowered_args) = if !type_args.is_empty() {
                if entry.out_type_param_count == 0 || type_args.len() != entry.out_type_param_count
                {
                    return Ok(None);
                }
                let Ok((effective, lowered)) =
                    self.bind_output_generic_call(name, entry, type_args, arguments)
                else {
                    return Ok(None);
                };
                (effective, Some(lowered))
            } else {
                if entry.out_type_param_count != 0 {
                    return Ok(None);
                }
                let param_ast_types: Vec<ast::Type> =
                    gdef.parameters.iter().map(|p| p.ty.clone()).collect();
                let lowered_args: Vec<Expr> = arguments
                    .iter()
                    .map(|a| self.lower_expr(a))
                    .collect::<Result<Vec<_>, _>>()?;
                let arg_types: Vec<Type> = lowered_args.iter().map(|a| a.ty.clone()).collect();
                match self.infer_type_args(name, type_params, &param_ast_types, &arg_types) {
                    Ok(args) => (args, Some(lowered_args)),
                    Err(_) => return Ok(None),
                }
            };

            let subst: HashMap<String, ast::Type> = type_params
                .iter()
                .zip(effective_type_args.iter())
                .map(|(p, a)| (p.clone(), a.clone()))
                .collect();

            let concrete_params: Vec<ast::Parameter> = gdef
                .parameters
                .iter()
                .map(|p| ast::Parameter {
                    pattern: p.pattern.clone(),
                    ty: apply_subst_to_ast_type(&p.ty, &subst),
                    default: p
                        .default
                        .as_ref()
                        .map(|d| Box::new(apply_subst_to_ast_expr(d, &subst))),
                    span: p.span,
                })
                .collect();
            let concrete_return_type = gdef
                .return_type
                .as_ref()
                .map(|t| apply_subst_to_ast_type(t, &subst));
            let concrete_body: Vec<ast::Statement> = gdef
                .body
                .iter()
                .map(|s| apply_subst_to_ast_statement(s, &subst))
                .collect();

            if !gdef.is_overlap {
                self.require_unsafe_access(gdef, span)?;
            }
            let closure_expr = self.lower_closure(
                span,
                &concrete_params,
                concrete_return_type.as_ref(),
                &concrete_body,
            )?;

            let (param_types, return_type) = match &closure_expr.ty {
                Type::Function {
                    params,
                    return_type,
                } => (params.clone(), (**return_type).clone()),
                _ => unreachable!(),
            };

            if arguments.len() != param_types.len() {
                return Ok(None);
            }

            let mut lowered_args: Vec<Expr> = Vec::new();
            let mut all_ok = true;
            for (index, (arg, pty)) in arguments.iter().zip(param_types.iter()).enumerate() {
                let lowered = if let Some(lowered) = &pre_lowered_args {
                    lowered[index].clone()
                } else {
                    self.lower_expr(arg)?
                };
                let coerced = self.try_coerce(lowered, pty);
                if coerced.ty != *pty {
                    all_ok = false;
                    break;
                }
                lowered_args.push(coerced);
            }
            if !all_ok {
                return Ok(None);
            }

            Ok(Some(Expr {
                ty: return_type,
                kind: ExprKind::CallIndirect {
                    callee: Box::new(closure_expr),
                    arguments: lowered_args,
                },
                span,
            }))
        }
    }
}
