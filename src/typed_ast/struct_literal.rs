//! Resolve struct construction from its fields before emitting a concrete literal.

use super::*;

impl Lowerer<'_> {
    /// A matching contextual type can supply parameters which have no field.
    pub(super) fn struct_construction_args(
        &self,
        name: &DefId,
        args: &[ast::Type],
        expected: Option<&Type>,
    ) -> Vec<ast::Type> {
        if args.is_empty()
            && let Some(Type::Struct(id)) = expected
            && &id.def == name
        {
            return id
                .args
                .iter()
                .map(|ty| self.concrete_type_to_ast_type(ty))
                .collect();
        }
        args.to_vec()
    }

    pub(super) fn lower_tuple_constructor(
        &mut self,
        name: &DefId,
        args: &[ast::Type],
        values: &[ast::Expr],
        expected: Option<&Type>,
        span: ast::SourceSpan,
    ) -> Result<Expr, CompileError> {
        let args = self.struct_construction_args(name, args, expected);
        // A single reference to this tuple struct can select its copy constructor,
        // even when the struct itself has several fields.
        let mut single = None;
        if let [arg] = values
            && !Self::has_infer_params(arg)
        {
            let value = self.lower_expr(arg)?;
            if let Type::Ref(inner) = &value.ty
                && let Type::Struct(id) = inner.as_ref()
                && &id.def == name
                && (args.is_empty() || self.resolve_struct_name(name, &args)? == *id)
            {
                let owner = self.concrete_type_to_ast_type(inner);
                let entries = self.associated_call_entries(&owner, "", span)?;
                if !entries.is_empty() {
                    return self.resolve_overloaded_call(
                        CandidateSource::Entries(entries),
                        "copy constructor",
                        values,
                        &[],
                        &[],
                        span,
                    );
                }
            }
            single = Some(value);
        }
        let fields = &self
            .ast_struct_def(&TypeId::plain(name.clone()))
            .unwrap()
            .fields;
        let arity = fields.len();
        if values.len() != arity {
            return Err(CompileError::new(
                format!("{name}: expected {arity} arguments, got {}", values.len()),
                span,
            ));
        }
        let names: Vec<_> = fields.iter().map(|f| f.name.clone()).collect();
        let inputs = names
            .iter()
            .map(String::as_str)
            .zip(values)
            .collect::<Vec<_>>();
        self.lower_struct_literal(name, &args, &inputs, span, single)
    }

    pub(super) fn lower_struct_literal(
        &mut self,
        name: &DefId,
        args: &[ast::Type],
        inputs: &[(&str, &ast::Expr)],
        span: ast::SourceSpan,
        single: Option<Expr>,
    ) -> Result<Expr, CompileError> {
        let mut values = vec![None; inputs.len()];
        if single.is_some() {
            values[0] = single;
        }
        let id = if args.is_empty()
            && let Some(template) = self.struct_inference_template(name)
        {
            let args = self.infer_struct_args(&template, inputs, &mut values, span)?;
            self.resolve_struct_name(name, &args)?
        } else {
            self.resolve_struct_name(name, args)?
        };
        let fields = &self
            .lowered_structs
            .get(&id)
            .ok_or_else(|| CompileError::new(format!("undefined struct: {name}"), span))?
            .fields;
        let definition_span = self.ast_struct_def(&id).map(|s| s.span).unwrap_or_default();
        for field in fields {
            if !inputs.iter().any(|(n, _)| *n == field.name) {
                return Err(CompileError::new(
                    format!("missing field `{}` in {name} literal", field.name),
                    span,
                )
                .with_label("struct defined here", definition_span));
            }
        }
        let mut result = Vec::with_capacity(inputs.len());
        for (&(field, expr), value) in inputs.iter().zip(values) {
            let ty = self.lowered_structs[&id]
                .fields
                .iter()
                .find(|f| f.name == field)
                .ok_or_else(|| {
                    CompileError::new(
                        format!("unknown field `{field}` in {name} literal"),
                        expr.span,
                    )
                    .with_label("struct defined here", definition_span)
                })?
                .ty
                .clone();
            self.check_field_visibility(&id, field, span)?;
            let value = match value {
                Some(value) => value,
                None => self.lower_expr_with_expected(expr, &ty)?,
            };
            let value = self.try_coerce(value, &ty);
            if value.ty != ty {
                return Err(CompileError::new(
                    format!(
                        "type mismatch in field `{field}` of {name}: expected {ty}, got {}",
                        value.ty
                    ),
                    value.span,
                )
                .with_label("struct defined here", definition_span));
            }
            result.push(FieldInit {
                name: field.to_string(),
                value,
            });
        }
        Ok(Expr {
            ty: Type::Struct(id.clone()),
            kind: ExprKind::StructLiteral { id, fields: result },
            span,
        })
    }

    /// Express the fields of an aliased struct in terms of the alias's own
    /// parameters (which can reorder or specialize the underlying parameters).
    fn struct_inference_template(&self, name: &DefId) -> Option<GenericStructDef> {
        if let Some(template) = self.generic_structs.get(name) {
            return Some(template.clone());
        }
        let (params, target) = self.type_aliases.get(name)?;
        if params.is_empty() {
            return None;
        }
        let ast::Type::Generic {
            name: target,
            type_args,
        } = self.expand_type_aliases(target, 0)
        else {
            return None;
        };
        let mut template = self.generic_structs.get(&target)?.clone();
        if type_args.len() != template.type_params.len() {
            return None;
        }
        let subst = template.type_params.into_iter().zip(type_args).collect();
        for field in &mut template.ast_def.fields {
            field.ty = apply_subst_to_ast_type(&field.ty, &subst);
        }
        template.ast_def.name = name.name.to_string();
        template.type_params = params.clone();
        Some(template)
    }

    fn infer_struct_args(
        &mut self,
        template: &GenericStructDef,
        inputs: &[(&str, &ast::Expr)],
        values: &mut [Option<Expr>],
        span: ast::SourceSpan,
    ) -> Result<Vec<ast::Type>, CompileError> {
        let def = &template.ast_def;
        let name = &def.name;
        let mut bindings = HashMap::new();
        let mut pending = Vec::new();
        for (i, &(field, expr)) in inputs.iter().enumerate() {
            let pattern = &def
                .fields
                .iter()
                .find(|f| f.name == field)
                .ok_or_else(|| {
                    CompileError::new(
                        format!("unknown field `{field}` in {name} literal"),
                        expr.span,
                    )
                    .with_label("struct defined here", def.span)
                })?
                .ty;
            pending.push((i, expr, pattern));
        }

        // Delay closures with omitted parameter types until their inputs are
        // known. Their inferred return types can in turn determine other fields.
        // Store the lowered values at their original indices for evaluation.
        while !pending.is_empty() {
            let previous = pending.len();
            let previous_bindings = bindings.len();
            let mut deferred = Vec::new();
            for (i, expr, pattern) in pending {
                let value = if let Some(value) = values[i].take() {
                    value
                } else if let ast::ExprKind::Closure {
                    parameters,
                    return_type,
                    body,
                } = &expr.kind
                    && Self::has_infer_params(expr)
                {
                    let bound =
                        apply_subst_to_ast_type(&self.expand_type_aliases(pattern, 0), &bindings);
                    let ast::Type::Function { params, .. } = bound else {
                        deferred.push((i, expr, pattern));
                        continue;
                    };
                    // An annotation on one closure input can determine another.
                    for (parameter, (_, pat)) in parameters.iter().zip(&params) {
                        if !matches!(parameter.ty, ast::Type::Infer) {
                            let ty = self.resolve_ast_type_at(&parameter.ty, parameter.span)?;
                            if !self.try_unify_type(pat, &ty, &template.type_params, &mut bindings)
                            {
                                return Err(CompileError::new(
                                    format!(
                                        "conflicting closure parameter type for field `{}` of {name}",
                                        inputs[i].0
                                    ),
                                    parameter.span,
                                ));
                            }
                        }
                    }
                    if params.iter().any(|(_, ty)| {
                        template
                            .type_params
                            .iter()
                            .any(|p| !bindings.contains_key(p) && type_param_appears_in(ty, p))
                    }) {
                        deferred.push((i, expr, pattern));
                        continue;
                    }
                    let params = params
                        .iter()
                        .map(|(_, ty)| {
                            self.resolve_ast_type(&apply_subst_to_ast_type(ty, &bindings))
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    self.lower_closure_with_params(
                        expr.span,
                        parameters,
                        return_type.as_ref(),
                        body,
                        Some(&params),
                    )?
                } else {
                    self.lower_expr(expr)?
                };
                if !self.try_unify_type(pattern, &value.ty, &template.type_params, &mut bindings) {
                    return Err(CompileError::new(
                        format!(
                            "conflicting type arguments for struct `{name}` in field `{}` (got {})",
                            inputs[i].0, value.ty
                        ),
                        expr.span,
                    ));
                }
                values[i] = Some(value);
            }
            if deferred.len() == previous && previous_bindings == bindings.len() {
                break;
            }
            pending = deferred;
        }
        template
            .type_params
            .iter()
            .map(|p| {
                bindings.remove(p).ok_or_else(|| {
                    CompileError::new(
                        format!("could not infer type argument `{p}` for struct `{name}`"),
                        span,
                    )
                })
            })
            .collect()
    }
}
