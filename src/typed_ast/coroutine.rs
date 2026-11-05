//! Coroutine typing and suspension lowering. Each coroutine is a private struct
//! with a program counter, captured references, and MaybeZeroed local storage.
//! An explicit `next(&self, input)` method executes its state machine using
//! ordinary typed AST shared by all backends.

use super::*;

mod optimize;

#[derive(Clone)]
pub(super) struct Context {
    id: TypeId,
    resume: Type,
    yielded: Option<Type>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub(super) struct CoroutineKey {
    owner: Option<FuncId>,
    file: u32,
    line: u32,
    col: u32,
}

#[derive(Clone)]
pub(super) struct CoroutineInfo {
    resume: Option<Type>,
    yielded: Option<Type>,
    initial: Option<Expr>,
    captures: Vec<CapturedVar>,
    span: ast::SourceSpan,
}

impl Lowerer<'_> {
    fn coroutine_identity(&mut self, span: ast::SourceSpan, resume: Option<Type>) -> TypeId {
        let key = CoroutineKey {
            owner: self.current_function.clone(),
            file: span.file_id,
            line: span.start.line,
            col: span.start.col,
        };
        if let Some(id) = self.coroutine_instances.get(&key) {
            return id.clone();
        }
        let index = self.closure_counter;
        self.closure_counter += 1;
        let id = TypeId::plain(DefId::synthetic(format!(
            "Coroutine_{}_{}_{}_{index}",
            span.file_id, span.start.line, span.start.col
        )));
        self.coroutine_instances.insert(key, id.clone());
        self.coroutine_types.insert(
            id.clone(),
            CoroutineInfo {
                resume,
                yielded: None,
                initial: None,
                captures: Vec::new(),
                span,
            },
        );
        self.lowered_structs.insert(
            id.clone(),
            StructDef {
                id: id.clone(),
                repr_c: false,
                fields: vec![FieldDef {
                    name: "state".into(),
                    ty: Type::Int,
                }],
            },
        );
        self.register_coroutine_struct(&id);
        id
    }

    /// A coroutine's nominal identity is known before its body, permitting
    /// recursive factories. Layout validation decides whether their local
    /// storage is finite once all field types have been inferred.
    pub(super) fn predeclare_coroutine_return(
        &mut self,
        function: &ast::FunctionDef,
        fid: &FuncId,
        parameters: &[Parameter],
    ) -> Result<(), CompileError> {
        fn tail(body: &[ast::Statement]) -> Option<&ast::Expr> {
            match &body.last()?.kind {
                ast::StatementKind::Expression(e) => Some(e),
                _ => None,
            }
        }
        fn find(e: &ast::Expr) -> Option<(&ast::Expr, Vec<bool>)> {
            match &e.kind {
                ast::ExprKind::Coroutine { .. } => Some((e, Vec::new())),
                ast::ExprKind::Reference(inner) | ast::ExprKind::Unique(inner) => {
                    let (coroutine, mut refs) = find(inner)?;
                    refs.push(matches!(e.kind, ast::ExprKind::Unique(_)));
                    Some((coroutine, refs))
                }
                ast::ExprKind::Block(body) => find(tail(body)?),
                _ => None,
            }
        }
        if function.return_type.is_some() {
            return Ok(());
        }
        let Some((coroutine, references)) = tail(&function.body).and_then(find) else {
            return Ok(());
        };
        let ast::ExprKind::Coroutine { parameter, .. } = &coroutine.kind else {
            unreachable!()
        };
        let resume = parameter
            .as_ref()
            .map(|p| self.resolve_ast_type(&p.ty))
            .transpose()?;
        let id = self.coroutine_identity(coroutine.span, resume);
        let mut ty = Type::Struct(id);
        for unique in references {
            ty = if unique {
                Type::Unique(Box::new(ty))
            } else {
                Type::Ref(Box::new(ty))
            };
        }
        self.resolved_return_types.insert(fid.clone(), ty.clone());
        self.monomorphized_functions
            .entry(fid.clone())
            .or_insert_with(|| FunctionDef {
                id: fid.clone(),
                parameters: parameters.to_vec(),
                return_type: ty,
                body: Vec::new(),
                is_unsafe: function.is_unsafe,
                inline_hint: function.inline_hint,
                def_span: function.span,
            });
        Ok(())
    }

    fn coroutine_next_id(&self, id: &TypeId) -> FuncId {
        let mut args = vec![Type::Ref(Box::new(Type::Struct(id.clone())))];
        args.extend(self.coroutine_types[id].resume.iter().cloned());
        FuncId {
            def: DefId::new(ast::SYNTHETIC_FILE, "next"),
            args,
            overload: None,
            method: true,
            extension: false,
        }
    }

    fn register_coroutine_struct(&mut self, id: &TypeId) {
        let span = ast::SourceSpan {
            file_id: ast::SYNTHETIC_FILE,
            ..self.coroutine_types[id].span
        };
        let fields = self.lowered_structs[id]
            .fields
            .iter()
            .map(|f| ast::FieldDef {
                name: f.name.clone(),
                ty: self.concrete_type_to_ast_type(&f.ty),
                is_pub: false,
                span,
            })
            .collect();
        self.synthetic_structs.insert(
            id.clone(),
            ast::StructDef {
                name: id.def.name.to_string(),
                def_id: id.def.clone(),
                type_params: Vec::new(),
                fields,
                is_tuple: false,
                repr_c: false,
                opaque: true,
                is_pub: false,
                doc: Some("Compiler-generated suspended computation.".into()),
                span,
            },
        );
    }

    fn register_coroutine_method(&mut self, function: FunctionDef) {
        let span = ast::SourceSpan {
            file_id: ast::SYNTHETIC_FILE,
            ..function.def_span
        };
        let ast_def = Rc::new(ast::FunctionDef {
            associated_type_params: Vec::new(),
            associated_type: None,
            name: "next".into(),
            display_name: "next".into(),
            type_params: Vec::new(),
            out_type_params: Vec::new(),
            parameters: function
                .parameters
                .iter()
                .map(|p| ast::Parameter {
                    pattern: ast::DestructurePattern::Name(p.name.clone()),
                    ty: self.concrete_type_to_ast_type(&p.ty),
                    default: None,
                    span,
                })
                .collect(),
            return_type: Some(self.concrete_type_to_ast_type(&function.return_type)),
            return_type_span: None,
            body: Vec::new(),
            is_pub: true,
            is_unsafe: false,
            inline_hint: false,
            is_extension: false,
            doc: Some(
                "Advances the coroutine to its next yield, or returns None after completion."
                    .into(),
            ),
            span,
        });
        let entries = self.method_defs.entry("next".into()).or_default();
        entries.push(FunctionEntry {
            associated_type_params: Vec::new(),
            type_params: Vec::new(),
            out_type_param_count: 0,
            ast_def: ast_def.clone(),
            overload_index: entries.len(),
        });
        self.method_index.remove("next");
        self.concrete_ast_defs.insert(function.id.clone(), ast_def);
        self.resolved_return_types
            .insert(function.id.clone(), function.return_type.clone());
        self.monomorphized_functions
            .insert(function.id.clone(), function);
    }

    fn validate_coroutine_layout(&self, id: &TypeId) -> Result<(), CompileError> {
        fn visit(
            ty: &Type,
            lowerer: &Lowerer<'_>,
            active: &mut HashSet<Type>,
            complete: &mut HashSet<Type>,
        ) -> bool {
            if complete.contains(ty) {
                return true;
            }
            match ty {
                Type::Struct(_) | Type::Enum(_) | Type::Array(_) | Type::FixedArray(_, _) => {}
                _ => return true, // References and functions have a finite representation.
            }
            if !active.insert(ty.clone()) {
                return false;
            }
            let finite = match ty {
                Type::Struct(id) => lowerer.lowered_structs[id]
                    .fields
                    .iter()
                    .all(|f| visit(&f.ty, lowerer, active, complete)),
                Type::Enum(id) => lowerer.lowered_enums[id]
                    .variants
                    .iter()
                    .filter_map(|v| v.inner_type.as_ref())
                    .all(|t| visit(t, lowerer, active, complete)),
                Type::Array(inner) | Type::FixedArray(inner, _) => {
                    visit(inner, lowerer, active, complete)
                }
                _ => unreachable!(),
            };
            active.remove(ty);
            complete.insert(ty.clone());
            finite
        }
        if !visit(
            &Type::Struct(id.clone()),
            self,
            &mut HashSet::new(),
            &mut HashSet::new(),
        ) {
            return Err(CompileError::new("recursive coroutine has unbounded size: its stored fields contain the coroutine by value; put the recursive value behind a reference".into(), self.coroutine_types[id].span));
        }
        Ok(())
    }

    pub(super) fn lower_coroutine_try(
        &mut self,
        span: ast::SourceSpan,
        body: &[ast::Statement],
        binding: &ast::Ident,
        binding_type: Option<&ast::Type>,
        handler: &[ast::Statement],
    ) -> Result<Vec<Statement>, CompileError> {
        let exception_type = Type::Struct(self.exception_type()?);
        if let Some(ty) = binding_type {
            let ty = self.resolve_ast_type(ty)?;
            if ty != exception_type {
                return Err(CompileError::new(
                    format!("catch binding must have type {exception_type}, got {ty}"),
                    span,
                ));
            }
        }
        self.push_scope();
        let body = body
            .iter()
            .map(|s| self.lower_statement(s))
            .collect::<Result<Vec<_>, _>>();
        self.pop_scope();
        let body = body?.into_iter().flatten().collect();
        self.push_scope();
        self.define_var(binding.clone(), exception_type.clone());
        let handler = handler
            .iter()
            .map(|s| self.lower_statement(s))
            .collect::<Result<Vec<_>, _>>();
        self.pop_scope();
        let handler = handler?.into_iter().flatten().collect();
        Ok(vec![statement(
            StatementKind::CoroutineTry {
                body,
                binding: binding.clone(),
                exception_type,
                handler,
            },
            span,
        )])
    }

    pub(super) fn lower_yield(
        &mut self,
        span: ast::SourceSpan,
        value: &ast::Expr,
    ) -> Result<Expr, CompileError> {
        if self.coroutine_context.is_none() {
            return Err(CompileError::new(
                "yield is only allowed inside a coroutine body".into(),
                span,
            ));
        }
        let value = self.lower_expr(value)?;
        if !value.ty.is_sized(&self.lowered_structs) {
            return Err(CompileError::new(
                format!("coroutine cannot yield unsized type {}", value.ty),
                span,
            ));
        }
        let ctx = self.coroutine_context.as_mut().unwrap();
        if value.ty != Type::Never {
            if let Some(ty) = &ctx.yielded {
                if *ty != value.ty {
                    return Err(CompileError::new(
                        format!("yield type mismatch: expected {ty}, got {}", value.ty),
                        span,
                    ));
                }
            } else {
                ctx.yielded = Some(value.ty.clone());
            }
        }
        self.coroutine_types.get_mut(&ctx.id).unwrap().yielded = ctx.yielded.clone();
        Ok(Expr {
            ty: ctx.resume.clone(),
            kind: ExprKind::Yield(Box::new(value)),
            span,
        })
    }

    pub(super) fn lower_coroutine(
        &mut self,
        span: ast::SourceSpan,
        parameter: Option<&ast::Parameter>,
        body: &[ast::Statement],
    ) -> Result<Expr, CompileError> {
        let resume = parameter
            .map(|p| self.resolve_ast_type(&p.ty))
            .transpose()?
            .unwrap_or(Type::Unit);
        if !resume.is_sized(&self.lowered_structs) {
            return Err(CompileError::new(
                format!("coroutine parameter has unsized type {resume}"),
                span,
            ));
        }
        let id = self.coroutine_identity(span, parameter.map(|_| resume.clone()));
        if let Some(initial) = self.coroutine_types[&id].initial.clone() {
            for capture in self.coroutine_types[&id].captures.clone() {
                self.lookup_var(&capture.name);
            }
            return Ok(initial);
        }
        let saved_coroutine = self.coroutine_context.replace(Context {
            id: id.clone(),
            resume: resume.clone(),
            yielded: None,
        });
        let saved_loops = std::mem::take(&mut self.loop_ctx);
        let saved_try = self.in_try_block.take();
        let saved_return = self.current_return_type.replace(Type::Unit);
        let saved_return_span = self.current_return_type_span.take();
        let saved_unsafe = std::mem::replace(&mut self.unsafe_depth, 0);
        self.capture_contexts.push(CaptureContext {
            scope_depth_barrier: self.scopes.depth(),
            captures: Vec::new(),
            captured_names: HashSet::new(),
        });
        self.push_scope();
        self.nested_function_defs.push(HashMap::new());
        if let Some(p) = parameter {
            self.define_var(pattern_name(&p.pattern).clone(), resume.clone());
        }
        let lowered = body
            .iter()
            .map(|s| self.lower_statement(s))
            .collect::<Result<Vec<_>, _>>();
        self.nested_function_defs.pop();
        self.pop_scope();
        let captures = self.capture_contexts.pop().unwrap().captures;
        let context = std::mem::replace(&mut self.coroutine_context, saved_coroutine);
        self.loop_ctx = saved_loops;
        self.in_try_block = saved_try;
        self.current_return_type = saved_return;
        self.current_return_type_span = saved_return_span;
        self.unsafe_depth = saved_unsafe;
        let body: Vec<_> = lowered?.into_iter().flatten().collect();
        let context = context.unwrap();
        let yielded = context.yielded.unwrap_or(Type::Unit);
        let option_id = self.ensure_enum_monomorphized(
            &DefId::new(ast::STDLIB_FILE, "Option"),
            &[self.concrete_type_to_ast_type(&yielded)],
        )?;
        let option = self.lowered_enums[&option_id].clone();
        let input_name = ast::Ident::synthetic("coroutine_input");
        let self_name = ast::Ident::synthetic("coroutine_self");
        self.coroutine_types.get_mut(&id).unwrap().yielded = Some(yielded);
        let maybe_zeroed = self
            .generic_structs
            .keys()
            .filter(|id| id.name == "MaybeZeroed")
            .min_by_key(|id| id.file)
            .cloned()
            .ok_or_else(|| {
                CompileError::new(
                    "coroutines require the standard library MaybeZeroed type".into(),
                    span,
                )
            })?;
        let mut machine = Machine::new(
            self,
            id.clone(),
            self_name.clone(),
            input_name.clone(),
            option.clone(),
            span,
        );
        machine.maybe_zeroed = Some(maybe_zeroed);
        machine.input_type = parameter.map(|_| resume.clone());
        machine.boxed_locals = address_taken(&body);
        for capture in &captures {
            let place = expr(
                capture.ty.clone(),
                ExprKind::Identifier(capture.name.clone()),
                span,
            );
            let field = machine.add_field(
                machine.reference(place),
                &format!("capture_{}", capture.name),
            );
            machine.locals.insert(capture.name.clone(), deref(field));
        }
        if let Some(p) = parameter {
            let input = expr(
                resume.clone(),
                ExprKind::Identifier(input_name.clone()),
                span,
            );
            let name = pattern_name(&p.pattern).clone();
            let local = machine.local(&name, input);
            machine.locals.insert(name, local);
        }
        machine.body(&body, false);
        machine.complete();
        let (definition, initial, body, helpers) = machine.finish();
        self.lowered_structs.insert(id.clone(), definition);
        self.validate_coroutine_layout(&id)?;
        self.pending_closures.extend(helpers);
        let mut parameters = vec![Parameter {
            name: self_name,
            ty: Type::Ref(Box::new(Type::Struct(id.clone()))),
            span,
        }];
        if let Some(p) = parameter {
            parameters.push(Parameter {
                name: input_name,
                ty: resume,
                span: p.span,
            });
        }
        let next = FunctionDef {
            id: self.coroutine_next_id(&id),
            parameters,
            return_type: Type::Enum(option.id),
            body,
            is_unsafe: false,
            inline_hint: false,
            def_span: span,
        };
        self.register_coroutine_method(next);
        self.register_coroutine_struct(&id);
        let info = self.coroutine_types.get_mut(&id).unwrap();
        info.captures = captures;
        info.initial = Some(initial.clone());
        Ok(initial)
    }

    pub(super) fn lower_coroutine_method(
        &mut self,
        span: ast::SourceSpan,
        receiver: &Expr,
        method: &str,
        type_args: &[ast::Type],
        arguments: &[ast::Expr],
        kwargs: &[(String, ast::Expr)],
    ) -> Option<Result<Expr, CompileError>> {
        let mut object = receiver.clone();
        while matches!(object.ty, Type::Ref(_) | Type::Unique(_)) {
            object = deref(object);
        }
        let Type::Struct(id) = &object.ty else {
            return None;
        };
        let info = self.coroutine_types.get(id)?.clone();
        let id = id.clone();
        let params: Vec<Type> = info.resume.iter().cloned().collect();
        let result = (|| {
            if !type_args.is_empty() || !kwargs.is_empty() {
                return Err(CompileError::new(
                    "coroutine methods do not accept type or keyword arguments".into(),
                    span,
                ));
            }
            if method == "iter" {
                if !params.is_empty() || !arguments.is_empty() {
                    return Err(CompileError::new(
                        "only coroutines without a resume parameter can be iterated".into(),
                        span,
                    ));
                }
                return Ok(expr(
                    Type::Ref(Box::new(object.ty.clone())),
                    ExprKind::Reference(Box::new(object)),
                    span,
                ));
            }
            if arguments.len() != params.len() {
                return Err(CompileError::new(
                    format!(
                        "coroutine next expects {} arguments, got {}",
                        params.len(),
                        arguments.len()
                    ),
                    span,
                ));
            }
            let mut args = Vec::new();
            for (arg, ty) in arguments.iter().zip(&params) {
                let value = self.lower_expr_with_expected(arg, ty)?;
                let value = self.try_coerce(value, ty);
                if value.ty != *ty && value.ty != Type::Never {
                    return Err(CompileError::new(
                        format!("coroutine next expects {ty}, got {}", value.ty),
                        arg.span,
                    ));
                }
                args.push(value);
            }
            let yielded = info.yielded.ok_or_else(|| {
                CompileError::new(
                    "cannot infer a recursive coroutine's yielded type before its first yield"
                        .into(),
                    span,
                )
            })?;
            let option = self.ensure_enum_monomorphized(
                &DefId::new(ast::STDLIB_FILE, "Option"),
                &[self.concrete_type_to_ast_type(&yielded)],
            )?;
            args.insert(
                0,
                expr(
                    Type::Ref(Box::new(object.ty.clone())),
                    ExprKind::Reference(Box::new(object)),
                    span,
                ),
            );
            Ok(expr(
                Type::Enum(option),
                ExprKind::Call {
                    function: self.coroutine_next_id(&id),
                    arguments: args,
                },
                span,
            ))
        })();
        Some(result.and_then(|result| {
            // Coroutine iter/next have a dedicated lowering path, but an
            // applicable extension must still conflict with these methods.
            let extensions: Vec<_> = self
                .method_defs
                .get(method)
                .into_iter()
                .flatten()
                .filter(|entry| {
                    entry.ast_def.is_extension
                        && self.source.method_is_visible(&entry.ast_def, span.file_id)
                })
                .cloned()
                .collect();
            for extension in extensions {
                let mut trial = self.clone();
                trial.push_scope();
                let name = ast::Ident::synthetic("$extension_receiver");
                trial.define_var(name.clone(), receiver.ty.clone());
                let mut args = vec![ast::Expr {
                    kind: ast::ExprKind::Identifier(name),
                    span,
                }];
                args.extend_from_slice(arguments);
                if trial
                    .resolve_overloaded_call(
                        super::CandidateSource::MethodEntries(vec![extension.clone()]),
                        method,
                        &args,
                        kwargs,
                        type_args,
                        span,
                    )
                    .is_ok()
                {
                    return Err(CompileError::new(
                        format!("conflicting extension method `{method}`"),
                        span,
                    )
                    .with_label("extension method defined here", extension.ast_def.span)
                    .with_label("coroutine method defined here", info.span));
                }
            }
            Ok(result)
        }))
    }
}

fn expr(ty: Type, kind: ExprKind, span: ast::SourceSpan) -> Expr {
    Expr { ty, kind, span }
}
fn statement(kind: StatementKind, span: ast::SourceSpan) -> Statement {
    Statement { kind, span }
}
fn unit(span: ast::SourceSpan) -> Expr {
    expr(Type::Unit, ExprKind::Block(Vec::new()), span)
}
fn deref(value: Expr) -> Expr {
    if let ExprKind::Reference(place) = value.kind {
        return *place;
    }
    let ty = match &value.ty {
        Type::Ref(t)
        | Type::RefUnsized(t)
        | Type::NullableRef(t)
        | Type::NullableRefUnsized(t)
        | Type::Unique(t)
        | Type::UniqueUnsized(t) => *t.clone(),
        _ => unreachable!("expected reference, got {}", value.ty),
    };
    let span = value.span;
    expr(ty, ExprKind::Deref(Box::new(value)), span)
}

struct LoopTarget {
    start: usize,
    end: usize,
    result: Expr,
}

struct Handler {
    state: usize,
    exception: Expr,
}

struct Machine<'a, 'b> {
    lowerer: &'a mut Lowerer<'b>,
    frame_id: TypeId,
    frame_name: ast::Ident,
    input_name: ast::Ident,
    input_type: Option<Type>,
    maybe_zeroed: Option<DefId>,
    boxed_locals: HashSet<ast::Ident>,
    option: EnumDef,
    span: ast::SourceSpan,
    fields: Vec<FieldInit>,
    locals: HashMap<ast::Ident, Expr>,
    blocks: Vec<Vec<Statement>>,
    current: usize,
    loops: Vec<LoopTarget>,
    handlers: Vec<Handler>,
    active_handler: Option<usize>,
    block_handlers: Vec<Option<usize>>,
}

impl<'a, 'b> Machine<'a, 'b> {
    fn new(
        lowerer: &'a mut Lowerer<'b>,
        frame_id: TypeId,
        frame_name: ast::Ident,
        input_name: ast::Ident,
        option: EnumDef,
        span: ast::SourceSpan,
    ) -> Self {
        Self {
            lowerer,
            frame_id,
            frame_name,
            input_name,
            input_type: None,
            maybe_zeroed: None,
            boxed_locals: HashSet::new(),
            option,
            span,
            fields: vec![FieldInit {
                name: "state".into(),
                value: expr(Type::Int, ExprKind::IntegerLiteral(0), span),
            }],
            locals: HashMap::new(),
            blocks: vec![Vec::new()],
            current: 0,
            loops: Vec::new(),
            handlers: Vec::new(),
            active_handler: None,
            block_handlers: vec![None],
        }
    }
    fn self_type(&self) -> Type {
        Type::Ref(Box::new(Type::Struct(self.frame_id.clone())))
    }
    fn field(&self, name: &str, ty: Type) -> Expr {
        expr(
            ty,
            ExprKind::FieldAccess {
                object: Box::new(deref(expr(
                    self.self_type(),
                    ExprKind::Identifier(self.frame_name.clone()),
                    self.span,
                ))),
                field: name.into(),
            },
            self.span,
        )
    }
    fn add_field(&mut self, value: Expr, prefix: &str) -> Expr {
        let name = format!("{prefix}_{}", self.fields.len());
        let field = self.field(&name, value.ty.clone());
        self.fields.push(FieldInit { name, value });
        field
    }
    fn named_slot(&mut self, ty: &Type, name: &str) -> Expr {
        let id = TypeId {
            def: self.maybe_zeroed.clone().unwrap(),
            args: vec![ty.clone()],
        };
        self.lowerer
            .lowered_structs
            .entry(id.clone())
            .or_insert_with(|| StructDef {
                id: id.clone(),
                repr_c: false,
                fields: vec![FieldDef {
                    name: "value".into(),
                    ty: ty.clone(),
                }],
            });
        let storage = Type::Struct(id);
        let initial = expr(
            storage.clone(),
            ExprKind::IntrinsicCall {
                intrinsic: Intrinsic::Zeroed,
                type_args: vec![storage],
                arguments: Vec::new(),
            },
            self.span,
        );
        let field = self.add_field(initial, name);
        self.reference(expr(
            ty.clone(),
            ExprKind::FieldAccess {
                object: Box::new(field),
                field: "value".into(),
            },
            self.span,
        ))
    }
    fn slot(&mut self, ty: &Type) -> Expr {
        self.named_slot(ty, "temp")
    }
    fn reference(&self, value: Expr) -> Expr {
        let ty = self
            .lowerer
            .resolve_refs(Type::Ref(Box::new(value.ty.clone())));
        let span = value.span;
        expr(ty, ExprKind::Reference(Box::new(value)), span)
    }
    fn emit(&mut self, kind: StatementKind) {
        self.blocks[self.current].push(statement(kind, self.span));
    }
    fn assign(&mut self, target: Expr, mut value: Expr) {
        value = self.lowerer.try_coerce(value, &target.ty);
        self.emit(StatementKind::Assignment { target, value });
    }
    fn save(&mut self, value: Expr) -> Expr {
        if value.ty == Type::Never {
            self.emit(StatementKind::Expression(value.clone()));
            return value;
        }
        if !value.ty.is_sized(&self.lowerer.lowered_structs) {
            return self.boxed(value, "temp");
        }
        let slot = self.slot(&value.ty);
        self.store(slot.clone(), value);
        deref(slot)
    }
    fn local(&mut self, name: &ast::Ident, value: Expr) -> Expr {
        let field = format!("local_{name}");
        if self.boxed_locals.contains(name) || !value.ty.is_sized(&self.lowerer.lowered_structs) {
            self.boxed(value, &field)
        } else {
            let slot = self.named_slot(&value.ty, &field);
            self.store(slot.clone(), value);
            deref(slot)
        }
    }
    fn boxed(&mut self, value: Expr, prefix: &str) -> Expr {
        // Address-taken bindings need fresh storage on each declaration. This
        // preserves references and closures escaping earlier loop iterations.
        let name = ast::Ident::synthetic(format!("coroutine_value_{}", self.fields.len()));
        let local = expr(
            value.ty.clone(),
            ExprKind::Identifier(name.clone()),
            value.span,
        );
        self.emit(StatementKind::Let {
            name,
            ty: value.ty.clone(),
            value,
        });
        let reference = self.reference(local);
        let slot = self.named_slot(&reference.ty, prefix);
        self.store(slot.clone(), reference);
        deref(deref(slot))
    }
    fn store(&mut self, slot: Expr, value: Expr) {
        self.assign(deref(slot), value);
    }
    fn state(&self) -> Expr {
        self.field("state", Type::Int)
    }
    fn program_counter(&self) -> Expr {
        expr(
            Type::Int,
            ExprKind::Identifier(ast::Ident::synthetic("coroutine_pc")),
            self.span,
        )
    }
    fn set_state(&mut self, state: i64) {
        self.assign(
            self.state(),
            expr(Type::Int, ExprKind::IntegerLiteral(state), self.span),
        );
    }
    fn block(&mut self) -> usize {
        let id = self.blocks.len();
        self.blocks.push(Vec::new());
        self.block_handlers.push(self.active_handler);
        id
    }
    fn jump(&mut self, block: usize) {
        self.assign(
            self.program_counter(),
            expr(Type::Int, ExprKind::IntegerLiteral(block as i64), self.span),
        );
        self.emit(StatementKind::Continue);
    }
    fn jump_body(&self, block: usize) -> Vec<Statement> {
        vec![
            statement(
                StatementKind::Assignment {
                    target: self.program_counter(),
                    value: expr(Type::Int, ExprKind::IntegerLiteral(block as i64), self.span),
                },
                self.span,
            ),
            statement(StatementKind::Continue, self.span),
        ]
    }
    fn option(&self, value: Option<Expr>) -> Expr {
        let name = if value.is_some() { "Some" } else { "None" };
        let variant = self
            .option
            .variants
            .iter()
            .find(|v| v.name == name)
            .unwrap();
        expr(
            Type::Enum(self.option.id.clone()),
            ExprKind::EnumVariant {
                enum_id: self.option.id.clone(),
                variant_name: name.into(),
                variant_index: variant.index,
                value: value.map(Box::new),
            },
            self.span,
        )
    }
    fn complete(&mut self) {
        self.set_state(-1);
        for field in self.fields.clone() {
            if matches!(
                &field.value.kind,
                ExprKind::IntrinsicCall {
                    intrinsic: Intrinsic::Zeroed,
                    ..
                }
            ) {
                self.assign(self.field(&field.name, field.value.ty.clone()), field.value);
            }
        }
        self.emit(StatementKind::Return(self.option(None)));
    }
    fn body(&mut self, body: &[Statement], place: bool) -> Expr {
        let saved = self.locals.clone();
        let mut value = unit(self.span);
        for (index, stmt) in body.iter().enumerate() {
            if let StatementKind::Expression(e) = &stmt.kind {
                value = self.expression(e, place && index + 1 == body.len());
            } else {
                self.statement(stmt);
                value = unit(stmt.span);
            }
        }
        self.locals = saved;
        value
    }
    fn statement(&mut self, stmt: &Statement) {
        match &stmt.kind {
            StatementKind::CoroutineTry {
                body,
                binding,
                exception_type,
                handler,
            } => {
                let parent = self.active_handler;
                let end = self.block();
                let catch = self.block();
                let exception = self.slot(exception_type);
                let index = self.handlers.len();
                self.handlers.push(Handler {
                    state: catch,
                    exception: exception.clone(),
                });
                self.active_handler = Some(index);
                let start = self.block();
                self.jump(start);
                self.current = start;
                self.body(body, false);
                self.jump(end);
                self.active_handler = parent;
                self.current = catch;
                let saved = self.locals.clone();
                self.locals.insert(binding.clone(), deref(exception));
                self.body(handler, false);
                self.locals = saved;
                self.jump(end);
                self.current = end;
            }
            StatementKind::Let { name, value, .. } => {
                let value = self.expression(value, false);
                let local = self.local(name, value);
                self.locals.insert(name.clone(), local);
            }
            StatementKind::Assignment { target, value } => {
                let target = self.expression(target, true);
                let value = self.expression(value, false);
                self.assign(target, value);
            }
            StatementKind::Expression(e) => {
                self.expression(e, false);
            }
            StatementKind::Return(e) => {
                self.expression(e, false);
                self.complete();
            }
            StatementKind::If {
                condition,
                body,
                else_body,
            } => {
                self.branch(condition, body, else_body, &Type::Unit, false);
            }
            StatementKind::While { condition, body } => {
                let start = self.block();
                let run = self.block();
                let end = self.block();
                self.jump(start);
                self.current = start;
                let condition = self.expression(condition, false);
                self.emit(StatementKind::If {
                    condition,
                    body: self.jump_body(run),
                    else_body: self.jump_body(end),
                });
                self.current = run;
                self.loops.push(LoopTarget {
                    start,
                    end,
                    result: unit(self.span),
                });
                self.body(body, false);
                self.loops.pop();
                self.jump(start);
                self.current = end;
            }
            StatementKind::Break(value) => {
                if let Some(value) = value {
                    let value = self.expression(value, false);
                    let result = self.loops.last().unwrap().result.clone();
                    if result.ty != Type::Unit {
                        self.store(result, value);
                    }
                }
                self.jump(self.loops.last().unwrap().end);
            }
            StatementKind::Continue => self.jump(self.loops.last().unwrap().start),
        }
    }
    fn branch(
        &mut self,
        condition: &Expr,
        yes: &[Statement],
        no: &[Statement],
        ty: &Type,
        place: bool,
    ) -> Expr {
        let condition = self.expression(condition, false);
        let yes_id = self.block();
        let no_id = self.block();
        let end = self.block();
        let result = if *ty == Type::Unit || *ty == Type::Never {
            None
        } else {
            let stored = if place {
                self.lowerer.resolve_refs(Type::Ref(Box::new(ty.clone())))
            } else {
                ty.clone()
            };
            Some(self.slot(&stored))
        };
        self.emit(StatementKind::If {
            condition,
            body: self.jump_body(yes_id),
            else_body: self.jump_body(no_id),
        });
        for (id, body) in [(yes_id, yes), (no_id, no)] {
            self.current = id;
            let value = self.body(body, place);
            if let Some(slot) = &result
                && value.ty != Type::Never
            {
                if place {
                    self.store(slot.clone(), self.reference(value));
                } else {
                    self.store(slot.clone(), value);
                }
            }
            self.jump(end);
        }
        self.current = end;
        result
            .map(|slot| {
                let value = deref(slot);
                if place { deref(value) } else { value }
            })
            .unwrap_or_else(|| unit(self.span))
    }
    fn expression(&mut self, source: &Expr, place: bool) -> Expr {
        let span = source.span;
        let ty = source.ty.clone();
        let kind = match &source.kind {
            ExprKind::Yield(value) => {
                let value = self.expression(value, false);
                let next = self.block();
                self.set_state(next as i64);
                self.emit(StatementKind::Return(self.option(Some(value))));
                self.current = next;
                return self.save(if ty == Type::Unit {
                    unit(span)
                } else {
                    expr(ty, ExprKind::Identifier(self.input_name.clone()), span)
                });
            }
            ExprKind::Identifier(name) => {
                let value = self
                    .locals
                    .get(name)
                    .cloned()
                    .unwrap_or_else(|| source.clone());
                return if place { value } else { self.save(value) };
            }
            ExprKind::Global(_) => {
                return if place {
                    source.clone()
                } else {
                    self.save(source.clone())
                };
            }
            ExprKind::IntegerLiteral(_)
            | ExprKind::FloatLiteral(_)
            | ExprKind::BooleanLiteral(_)
            | ExprKind::NullLiteral
            | ExprKind::FunctionRef(_) => return source.clone(),
            ExprKind::Block(body) => return self.body(body, place),
            ExprKind::If {
                condition,
                then_body,
                else_body,
            } => return self.branch(condition, then_body, else_body, &ty, place),
            ExprKind::Loop(body) => {
                let start = self.block();
                let end = self.block();
                let result = if ty == Type::Unit || ty == Type::Never {
                    unit(span)
                } else {
                    self.slot(&ty)
                };
                self.jump(start);
                self.current = start;
                self.loops.push(LoopTarget {
                    start,
                    end,
                    result: result.clone(),
                });
                self.body(body, false);
                self.loops.pop();
                self.jump(start);
                self.current = end;
                return if result.ty == Type::Unit {
                    result
                } else {
                    deref(result)
                };
            }
            ExprKind::BinaryOp {
                op: ast::BinOp::And | ast::BinOp::Or,
                left,
                right,
            } => {
                let value = statement(StatementKind::Expression(*right.clone()), span);
                let is_and = matches!(
                    &source.kind,
                    ExprKind::BinaryOp {
                        op: ast::BinOp::And,
                        ..
                    }
                );
                let constant = statement(
                    StatementKind::Expression(expr(
                        Type::Bool,
                        ExprKind::BooleanLiteral(!is_and),
                        span,
                    )),
                    span,
                );
                let (yes, no) = if is_and {
                    (value, constant)
                } else {
                    (constant, value)
                };
                return self.branch(left, &[yes], &[no], &ty, false);
            }
            ExprKind::FieldAccess { object, field } => ExprKind::FieldAccess {
                object: Box::new(self.expression(object, true)),
                field: field.clone(),
            },
            // Dereferencing a unique reference accesses its existing storage;
            // materializing the reference as a value would deep-copy it first.
            ExprKind::Deref(value) => ExprKind::Deref(Box::new(self.expression(value, true))),
            ExprKind::Reference(value) => {
                ExprKind::Reference(Box::new(self.expression(value, true)))
            }
            ExprKind::Unique(value) => ExprKind::Unique(Box::new(self.expression(value, false))),
            ExprKind::Not(value) => ExprKind::Not(Box::new(self.expression(value, false))),
            ExprKind::Index { object, index } => ExprKind::Index {
                object: Box::new(self.expression(object, true)),
                index: Box::new(self.expression(index, false)),
            },
            ExprKind::Slice { object, start, end } => ExprKind::Slice {
                object: Box::new(self.expression(object, true)),
                start: Box::new(self.expression(start, false)),
                end: Box::new(self.expression(end, false)),
            },
            ExprKind::Call {
                function,
                arguments,
            } => ExprKind::Call {
                function: function.clone(),
                arguments: arguments
                    .iter()
                    .map(|a| self.expression(a, false))
                    .collect(),
            },
            ExprKind::CallIndirect { callee, arguments } => ExprKind::CallIndirect {
                callee: Box::new(self.expression(callee, false)),
                arguments: arguments
                    .iter()
                    .map(|a| self.expression(a, false))
                    .collect(),
            },
            ExprKind::StructLiteral { id, fields } => ExprKind::StructLiteral {
                id: id.clone(),
                fields: fields
                    .iter()
                    .map(|f| FieldInit {
                        name: f.name.clone(),
                        value: self.expression(&f.value, false),
                    })
                    .collect(),
            },
            ExprKind::ArrayLiteral(values) => {
                ExprKind::ArrayLiteral(values.iter().map(|v| self.expression(v, false)).collect())
            }
            ExprKind::ArrayRepeat { element, count } => ExprKind::ArrayRepeat {
                element: Box::new(self.expression(element, false)),
                count: Box::new(self.expression(count, false)),
            },
            ExprKind::ArrayInit { count, init } => ExprKind::ArrayInit {
                count: Box::new(self.expression(count, false)),
                init: Box::new(self.expression(init, false)),
            },
            ExprKind::ArraySizeCoerce { expr, size } => ExprKind::ArraySizeCoerce {
                expr: Box::new(self.expression(expr, place)),
                size: *size,
            },
            ExprKind::BinaryOp { op, left, right } => ExprKind::BinaryOp {
                op: *op,
                left: Box::new(self.expression(left, false)),
                right: Box::new(self.expression(right, false)),
            },
            ExprKind::Closure {
                synthetic_fn,
                captures,
            } => ExprKind::Closure {
                synthetic_fn: synthetic_fn.clone(),
                captures: captures
                    .iter()
                    .map(|c| CapturedVar {
                        name: c.name.clone(),
                        ty: c.ty.clone(),
                        place: Some(Box::new(self.expression(
                            c.place.as_deref().unwrap_or(&expr(
                                c.ty.clone(),
                                ExprKind::Identifier(c.name.clone()),
                                span,
                            )),
                            true,
                        ))),
                    })
                    .collect(),
            },
            ExprKind::EnumVariant {
                enum_id,
                variant_name,
                variant_index,
                value,
            } => ExprKind::EnumVariant {
                enum_id: enum_id.clone(),
                variant_name: variant_name.clone(),
                variant_index: *variant_index,
                value: value.as_ref().map(|v| Box::new(self.expression(v, false))),
            },
            ExprKind::Match { scrutinee, arms } => {
                return self.match_expr(scrutinee, arms, &ty, place);
            }
            ExprKind::IntrinsicCall {
                intrinsic,
                type_args,
                arguments,
            } => ExprKind::IntrinsicCall {
                intrinsic: intrinsic.clone(),
                type_args: type_args.clone(),
                arguments: arguments
                    .iter()
                    .map(|a| self.expression(a, false))
                    .collect(),
            },
        };
        let value = expr(ty, kind, span);
        if place {
            // Save the address before evaluating later operands, which may
            // mutate a reference, suspend, or replace a loop's binding.
            let reference = self.reference(value);
            deref(self.save(reference))
        } else {
            self.save(value)
        }
    }
    fn match_expr(
        &mut self,
        scrutinee: &Expr,
        arms: &[TypedMatchArm],
        ty: &Type,
        place: bool,
    ) -> Expr {
        let scrutinee = self.expression(scrutinee, true);
        let origin = self.current;
        let end = self.block();
        let result = if *ty == Type::Unit || *ty == Type::Never {
            None
        } else {
            let stored = if place {
                self.lowerer.resolve_refs(Type::Ref(Box::new(ty.clone())))
            } else {
                ty.clone()
            };
            Some(self.slot(&stored))
        };
        let mut dispatch = Vec::new();
        for arm in arms {
            let start = self.block();
            self.current = start;
            let saved = self.locals.clone();
            let binding = match &arm.pattern {
                TypedPattern::Variant { binding, .. } => binding.as_ref(),
                TypedPattern::Wildcard(name, ty) => Some(&(name.clone(), ty.clone())),
                _ => None,
            };
            let mut entry = Vec::new();
            if let Some((name, ty)) = binding {
                let ref_ty = self.lowerer.resolve_refs(Type::Ref(Box::new(ty.clone())));
                let slot = self.slot(&ref_ty);
                let reference = self.reference(expr(
                    ty.clone(),
                    ExprKind::Identifier(name.clone()),
                    self.span,
                ));
                entry.push(statement(
                    StatementKind::Assignment {
                        target: deref(slot.clone()),
                        value: reference,
                    },
                    self.span,
                ));
                self.locals.insert(name.clone(), deref(deref(slot)));
            }
            entry.extend(self.jump_body(start));
            dispatch.push(TypedMatchArm {
                pattern: arm.pattern.clone(),
                body: entry,
            });
            let value = self.body(&arm.body, place);
            if let Some(slot) = &result
                && value.ty != Type::Never
            {
                if place {
                    self.store(slot.clone(), self.reference(value));
                } else {
                    self.store(slot.clone(), value);
                }
            }
            self.jump(end);
            self.locals = saved;
        }
        self.current = origin;
        self.emit(StatementKind::Expression(expr(
            Type::Never,
            ExprKind::Match {
                scrutinee: Box::new(scrutinee),
                arms: dispatch,
            },
            self.span,
        )));
        self.current = end;
        result
            .map(|slot| {
                let value = deref(slot);
                if place { deref(value) } else { value }
            })
            .unwrap_or_else(|| unit(self.span))
    }
    fn finish(mut self) -> (StructDef, Expr, Vec<Statement>, Vec<FunctionDef>) {
        optimize::localize_temporaries(&mut self);
        let handler_field = if self.handlers.is_empty() {
            None
        } else {
            Some(self.add_field(
                expr(Type::Int, ExprKind::IntegerLiteral(-1), self.span),
                "handler",
            ))
        };
        let mut arms = Vec::new();
        for (index, block) in self.blocks.iter().enumerate() {
            let mut body = Vec::new();
            if let Some(field) = &handler_field {
                body.push(statement(
                    StatementKind::Assignment {
                        target: field.clone(),
                        value: expr(
                            Type::Int,
                            ExprKind::IntegerLiteral(
                                self.block_handlers[index].map_or(-1, |h| h as i64),
                            ),
                            self.span,
                        ),
                    },
                    self.span,
                ));
            }
            body.extend(block.clone());
            arms.push(TypedMatchArm {
                pattern: TypedPattern::IntegerLiteral(index as i64),
                body,
            });
        }
        arms.push(TypedMatchArm {
            pattern: TypedPattern::Wildcard(
                ast::Ident::synthetic("completed_coroutine"),
                Type::Int,
            ),
            body: vec![statement(
                StatementKind::Return(self.option(None)),
                self.span,
            )],
        });
        let dispatch = expr(
            Type::Never,
            ExprKind::Match {
                scrutinee: Box::new(self.program_counter()),
                arms,
            },
            self.span,
        );
        // Internal control flow belongs to this invocation. Only suspension
        // writes a continuation into the persistent state; poison it once so
        // exceptions and reentrant calls still observe a completed coroutine.
        let mut body = vec![
            statement(
                StatementKind::Let {
                    name: ast::Ident::synthetic("coroutine_pc"),
                    ty: Type::Int,
                    value: self.state(),
                },
                self.span,
            ),
            statement(
                StatementKind::Assignment {
                    target: self.state(),
                    value: expr(Type::Int, ExprKind::IntegerLiteral(-1), self.span),
                },
                self.span,
            ),
            statement(
                StatementKind::Expression(expr(
                    Type::Never,
                    ExprKind::Loop(vec![statement(
                        StatementKind::Expression(dispatch),
                        self.span,
                    )]),
                    self.span,
                )),
                self.span,
            ),
        ];
        let mut helpers = Vec::new();
        if let Some(handler) = handler_field {
            let (wrapped, functions) = self.wrap_exceptions(body, handler);
            body = wrapped;
            helpers = functions;
        }
        let frame = StructDef {
            id: self.frame_id.clone(),
            repr_c: false,
            fields: self
                .fields
                .iter()
                .map(|f| FieldDef {
                    name: f.name.clone(),
                    ty: f.value.ty.clone(),
                })
                .collect(),
        };
        let initial = expr(
            Type::Struct(self.frame_id.clone()),
            ExprKind::StructLiteral {
                id: self.frame_id.clone(),
                fields: self.fields,
            },
            self.span,
        );
        (frame, initial, body, helpers)
    }

    /// Exception handlers must be reinstalled on each resume: retaining a native
    /// stack or a setjmp target across a yield would leave dangling stack state.
    /// A driver runs until suspension. Its catch callback transfers control to
    /// the handler state; the outer loop then resumes the driver in this call.
    fn wrap_exceptions(
        &mut self,
        mut driver: Vec<Statement>,
        handler: Expr,
    ) -> (Vec<Statement>, Vec<FunctionDef>) {
        let result = self.add_field(self.option(None), "result");
        let paused = self.add_field(
            expr(Type::Bool, ExprKind::BooleanLiteral(false), self.span),
            "paused",
        );
        rewrite_returns(&mut driver, &result, &paused);
        let mut captures = vec![CapturedVar {
            name: self.frame_name.clone(),
            ty: self.self_type(),
            place: None,
        }];
        if let Some(ty) = &self.input_type {
            captures.push(CapturedVar {
                name: self.input_name.clone(),
                ty: ty.clone(),
                place: None,
            });
        }
        let (drive, driver_fn) = self.helper("drive", Vec::new(), driver, captures);
        let exception_ty = Type::Struct(self.lowerer.exception_type().unwrap());
        let exception_name = ast::Ident::synthetic("coroutine_exception");
        let exception = expr(
            exception_ty.clone(),
            ExprKind::Identifier(exception_name.clone()),
            self.span,
        );
        let mut arms = Vec::new();
        for (index, target) in self.handlers.iter().enumerate() {
            arms.push(TypedMatchArm {
                pattern: TypedPattern::IntegerLiteral(index as i64),
                body: vec![
                    statement(
                        StatementKind::Assignment {
                            target: deref(target.exception.clone()),
                            value: exception.clone(),
                        },
                        self.span,
                    ),
                    statement(
                        StatementKind::Assignment {
                            target: self.state(),
                            value: expr(
                                Type::Int,
                                ExprKind::IntegerLiteral(target.state as i64),
                                self.span,
                            ),
                        },
                        self.span,
                    ),
                ],
            });
        }
        arms.push(TypedMatchArm {
            pattern: TypedPattern::Wildcard(
                ast::Ident::synthetic("unhandled_coroutine_exception"),
                Type::Int,
            ),
            body: vec![statement(
                StatementKind::Expression(expr(
                    Type::Never,
                    ExprKind::IntrinsicCall {
                        intrinsic: Intrinsic::Throw,
                        type_args: Vec::new(),
                        arguments: vec![exception],
                    },
                    self.span,
                )),
                self.span,
            )],
        });
        let catch_body = vec![statement(
            StatementKind::Expression(expr(
                Type::Unit,
                ExprKind::Match {
                    scrutinee: Box::new(handler),
                    arms,
                },
                self.span,
            )),
            self.span,
        )];
        let (catch, catch_fn) = self.helper(
            "catch",
            vec![Parameter {
                name: exception_name,
                ty: exception_ty,
                span: self.span,
            }],
            catch_body,
            vec![CapturedVar {
                name: self.frame_name.clone(),
                ty: self.self_type(),
                place: None,
            }],
        );
        let body = vec![statement(
            StatementKind::Expression(expr(
                Type::Never,
                ExprKind::Loop(vec![
                    statement(
                        StatementKind::Assignment {
                            target: paused.clone(),
                            value: expr(Type::Bool, ExprKind::BooleanLiteral(false), self.span),
                        },
                        self.span,
                    ),
                    statement(
                        StatementKind::Assignment {
                            target: result.clone(),
                            value: self.option(None),
                        },
                        self.span,
                    ),
                    statement(
                        StatementKind::Expression(expr(
                            Type::Unit,
                            ExprKind::IntrinsicCall {
                                intrinsic: Intrinsic::Try,
                                type_args: Vec::new(),
                                arguments: vec![drive, catch],
                            },
                            self.span,
                        )),
                        self.span,
                    ),
                    statement(
                        StatementKind::If {
                            condition: paused,
                            body: vec![statement(StatementKind::Return(result), self.span)],
                            else_body: Vec::new(),
                        },
                        self.span,
                    ),
                ]),
                self.span,
            )),
            self.span,
        )];
        (body, vec![driver_fn, catch_fn])
    }

    fn helper(
        &self,
        suffix: &str,
        parameters: Vec<Parameter>,
        body: Vec<Statement>,
        captures: Vec<CapturedVar>,
    ) -> (Expr, FunctionDef) {
        let name = format!("__closure_{}_{}", self.frame_id.def.name, suffix);
        let value = expr(
            Type::Function {
                params: parameters.iter().map(|p| p.ty.clone()).collect(),
                return_type: Box::new(Type::Unit),
            },
            ExprKind::Closure {
                synthetic_fn: name.clone(),
                captures,
            },
            self.span,
        );
        let function = FunctionDef {
            id: FuncId::free(DefId::synthetic(name), Vec::new()),
            parameters,
            return_type: Type::Unit,
            body,
            is_unsafe: false,
            inline_hint: false,
            def_span: self.span,
        };
        (value, function)
    }
}

/// Conservatively identify locals whose storage can escape. Their frame field
/// holds a reference to fresh declaration storage; other sized locals live inline.
fn address_taken(body: &[Statement]) -> HashSet<ast::Ident> {
    fn expression(e: &Expr, address: bool, names: &mut HashSet<ast::Ident>) {
        match &e.kind {
            ExprKind::Identifier(name) => {
                if address {
                    names.insert(name.clone());
                }
            }
            ExprKind::Reference(value) => expression(value, true, names),
            ExprKind::FieldAccess { object, .. } => expression(object, address, names),
            ExprKind::Deref(value)
            | ExprKind::Unique(value)
            | ExprKind::Not(value)
            | ExprKind::Yield(value) => expression(value, false, names),
            ExprKind::ArraySizeCoerce { expr, .. } => expression(expr, address, names),
            ExprKind::Index { object, index } => {
                expression(object, address, names);
                expression(index, false, names);
            }
            ExprKind::Slice { object, start, end } => {
                expression(object, address, names);
                expression(start, false, names);
                expression(end, false, names);
            }
            ExprKind::Call { arguments, .. } | ExprKind::IntrinsicCall { arguments, .. } => {
                for value in arguments {
                    expression(value, false, names);
                }
            }
            ExprKind::CallIndirect { callee, arguments } => {
                expression(callee, false, names);
                for value in arguments {
                    expression(value, false, names);
                }
            }
            ExprKind::StructLiteral { fields, .. } => {
                for field in fields {
                    expression(&field.value, false, names);
                }
            }
            ExprKind::ArrayLiteral(values) => {
                for value in values {
                    expression(value, false, names);
                }
            }
            ExprKind::ArrayRepeat { element, count }
            | ExprKind::ArrayInit {
                init: element,
                count,
            } => {
                expression(element, false, names);
                expression(count, false, names);
            }
            ExprKind::BinaryOp { left, right, .. } => {
                expression(left, false, names);
                expression(right, false, names);
            }
            ExprKind::If {
                condition,
                then_body,
                else_body,
            } => {
                expression(condition, false, names);
                statements(then_body, address, names);
                statements(else_body, address, names);
            }
            ExprKind::Block(body) | ExprKind::Loop(body) => statements(body, address, names),
            ExprKind::Match { scrutinee, arms } => {
                // Pattern bindings alias the scrutinee and can escape an arm.
                expression(scrutinee, true, names);
                for arm in arms {
                    statements(&arm.body, address, names);
                }
            }
            ExprKind::Closure { captures, .. } => {
                for capture in captures {
                    if let Some(place) = &capture.place {
                        expression(place, true, names);
                    } else {
                        names.insert(capture.name.clone());
                    }
                }
            }
            ExprKind::EnumVariant { value, .. } => {
                if let Some(value) = value {
                    expression(value, false, names);
                }
            }
            ExprKind::Global(_)
            | ExprKind::IntegerLiteral(_)
            | ExprKind::FloatLiteral(_)
            | ExprKind::BooleanLiteral(_)
            | ExprKind::NullLiteral
            | ExprKind::FunctionRef(_) => {}
        }
    }
    fn statements(body: &[Statement], address: bool, names: &mut HashSet<ast::Ident>) {
        for stmt in body {
            match &stmt.kind {
                StatementKind::Let { value, .. } => expression(value, false, names),
                StatementKind::Assignment { target, value } => {
                    expression(target, false, names);
                    expression(value, false, names);
                }
                StatementKind::Expression(value) => expression(value, address, names),
                StatementKind::Return(value) => expression(value, false, names),
                StatementKind::Break(value) => {
                    if let Some(value) = value {
                        expression(value, address, names);
                    }
                }
                StatementKind::If {
                    condition,
                    body,
                    else_body,
                } => {
                    expression(condition, false, names);
                    statements(body, address, names);
                    statements(else_body, address, names);
                }
                StatementKind::While { condition, body } => {
                    expression(condition, false, names);
                    statements(body, address, names);
                }
                StatementKind::CoroutineTry { body, handler, .. } => {
                    statements(body, address, names);
                    statements(handler, address, names);
                }
                StatementKind::Continue => {}
            }
        }
    }
    let mut names = HashSet::new();
    statements(body, false, &mut names);
    names
}

fn rewrite_returns(body: &mut Vec<Statement>, result: &Expr, paused: &Expr) {
    let mut output = Vec::new();
    for mut stmt in std::mem::take(body) {
        match &mut stmt.kind {
            StatementKind::Return(value) => {
                output.push(statement(
                    StatementKind::Assignment {
                        target: result.clone(),
                        value: value.clone(),
                    },
                    stmt.span,
                ));
                output.push(statement(
                    StatementKind::Assignment {
                        target: paused.clone(),
                        value: expr(Type::Bool, ExprKind::BooleanLiteral(true), stmt.span),
                    },
                    stmt.span,
                ));
                *value = unit(stmt.span);
            }
            StatementKind::If {
                body, else_body, ..
            } => {
                rewrite_returns(body, result, paused);
                rewrite_returns(else_body, result, paused);
            }
            StatementKind::Expression(value) => match &mut value.kind {
                ExprKind::Loop(body) | ExprKind::Block(body) => {
                    rewrite_returns(body, result, paused)
                }
                ExprKind::Match { arms, .. } => {
                    for arm in arms {
                        rewrite_returns(&mut arm.body, result, paused);
                    }
                }
                _ => {}
            },
            _ => {}
        }
        output.push(stmt);
    }
    *body = output;
}
