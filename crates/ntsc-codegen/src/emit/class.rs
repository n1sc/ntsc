//! Class emission: struct layout, implicit init, and top-level variables.

use super::*;

/// Infer the type of a simple initializer expression (used for class fields
/// declared without an explicit type annotation).
pub(crate) fn expr_to_literal_ty(expr: &Expr) -> Option<Ty> {
    match expr {
        Expr::Literal { value, .. } => Some(match value {
            LiteralValue::String(_) => Ty::String,
            LiteralValue::Bool(_) => Ty::Bool,
            LiteralValue::Nil => Ty::Nil,
            LiteralValue::Number(n) => {
                if n.contains('.') {
                    Ty::Float
                } else {
                    Ty::Int
                }
            }
        }),

        Expr::ArrayLiteral { elements, .. } => Some(Ty::Array(Box::new(
            // The element type comes from the first element: an untyped `[]`
            // field and a heterogeneous literal stay `array[any]`, but
            // `[1, 2]` becomes `array[int]`, so reading an element back
            // yields an int rather than an opaque `any` that prints as
            // nothing.
            elements
                .first()
                .and_then(expr_to_literal_ty)
                .unwrap_or(Ty::Any),
        ))),
        Expr::ObjectLiteral { .. } => Some(Ty::Object),
        _ => None,
    }
}

pub(crate) fn emit_class<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    name: &ntsc_ast::token::Token,
    _parent: &Option<ntsc_ast::token::Token>,
    body: &[Stmt],
) -> Result<(), crate::CodegenError> {
    let class_name = name.lexeme();

    let fields: Vec<&Stmt> = body
        .iter()
        .filter(|s| matches!(s, Stmt::Var { .. }))
        .collect();
    let methods: Vec<&Stmt> = body
        .iter()
        .filter(|s| matches!(s, Stmt::Function { .. }))
        .collect();

    // Build the struct type: the *parent's* flattened fields come first (so
    // a base method sees a layout-compatible instance pointer), then this
    // class's own fields from `body`. `class_all_field_types` is not used
    // here because it already includes this class's own fields (the class
    // maps are populated for every class up front) — asking it would lay
    // every own field out twice.
    let mut field_types: Vec<inkwell::types::BasicTypeEnum<'ctx>> = Vec::new();
    if let Some(parent) = class_parent(class_name) {
        for inherited in class_all_field_types(&parent) {
            field_types.push(ty_to_llvm(&inherited, context));
        }
    }
    for field in &fields {
        let ty = match field {
            Stmt::Var {
                type_annotation,
                initializer,
                ..
            } => {
                if let Some(ann) = type_annotation {
                    type_annotation_to_ty(&Some(ann.clone()))
                } else if let Some(init) = initializer {
                    expr_to_literal_ty(init).unwrap_or(Ty::Any)
                } else {
                    Ty::Any
                }
            }
            _ => unreachable!(),
        };
        field_types.push(ty_to_llvm(&ty, context));
    }

    let struct_type = context.opaque_struct_type(class_name);
    if field_types.is_empty() {
        // Field-less classes still need a real (one-byte) LLVM type:
        // instances are allocated and addressed, and trait-object
        // machinery references the class type.
        field_types.push(context.i8_type().into());
    }
    struct_type.set_body(&field_types, false);

    for method in &methods {
        if let Stmt::Function {
            name: method_name,
            params,
            return_type: method_ret,
            body: method_body,
            ..
        } = method
        {
            // Methods are plain functions with the class struct pointer as
            // the first argument; `this` is just another parameter.
            let this_param = ntsc_ast::expr::FunctionParam {
                name: ntsc_ast::token::Token::new(
                    ntsc_ast::token::TokenKind::Identifier("this".to_string()),
                    ntsc_ast::span::Span::dummy(),
                ),
                type_annotation: Some(ntsc_ast::types::TypeAnnotation::Named(
                    ntsc_ast::token::Token::new(
                        ntsc_ast::token::TokenKind::Identifier(name.lexeme().to_string()),
                        ntsc_ast::span::Span::dummy(),
                    ),
                )),
            };
            let all_params: Vec<ntsc_ast::expr::FunctionParam> = std::iter::once(this_param)
                .chain(params.iter().cloned())
                .collect();

            let method_fn_name = format!("{class_name}.{}", method_name.lexeme());
            let (fn_ty, param_tys) = fn_type_from_params(context, &all_params, method_ret);
            let ret_ty = function_return_ty(method_ret);

            let function = module.add_function(
                &method_fn_name,
                fn_ty,
                Some(inkwell::module::Linkage::External),
            );

            if method_body.is_empty() {
                let entry = context.append_basic_block(function, "entry");
                let builder = context.create_builder();
                builder.position_at_end(entry);
                if ret_ty == Ty::Void {
                    builder.build_return(None)?;
                } else {
                    builder.build_return(Some(&default_llvm_value(&ret_ty, context)))?;
                }
                continue;
            }

            let builder = context.create_builder();
            let entry_bb = context.append_basic_block(function, "entry");
            builder.position_at_end(entry_bb);
            let entry_builder = context.create_builder();
            entry_builder.position_at_end(entry_bb);

            let mut fn_ctx = FunctionContext::new(
                function,
                &builder,
                &entry_builder,
                entry_bb,
                module,
                ret_ty.clone(),
                context,
            );

            for (i, param) in all_params.iter().enumerate() {
                let pty = &param_tys[i];
                let ptr = fn_ctx.alloca(param.name.lexeme(), pty)?;
                let arg_value = function.get_nth_param(i as u32).ok_or_else(|| {
                    crate::CodegenError::LLVMError(format!(
                        "missing parameter {}",
                        param.name.lexeme()
                    ))
                })?;
                fn_ctx.builder.build_store(ptr, arg_value)?;
                fn_ctx.define_var(param.name.lexeme(), ptr, pty.clone());

                // Owned parameters (arrays/strings, not `view` or `this`)
                // own the value passed by the caller and must drop it at
                // exit. `mark_owned_if_heap` reads the *declared* type, so
                // a `shared` parameter is recognized by its box type rather
                // than the wrapped inner type.
                fn_ctx.mark_owned_if_heap(param.name.lexeme(), pty);
            }

            for stmt in method_body {
                emit_statement_in_function(&mut fn_ctx, stmt)?;
            }

            emit_exception_return(&mut fn_ctx, &ret_ty, context)?;
            let current_block = fn_ctx.builder.get_insert_block().unwrap();
            if current_block.get_terminator().is_none() {
                emit_drop_all_owned(&mut fn_ctx)?;
                match &ret_ty {
                    Ty::Void => {
                        fn_ctx.builder.build_return(None)?;
                    }
                    _ => {
                        let default_val = default_llvm_value(&ret_ty, context);
                        fn_ctx.builder.build_return(Some(&default_val))?;
                    }
                }
            }
        }
    }

    Ok(())
}

// ── Implicit init function ──────────────────────────────────────────────

pub(crate) fn emit_implicit_init<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    top_expr: &Expr,
) -> Result<(), crate::CodegenError> {
    let void_ty = context.void_type();
    let fn_type = void_ty.fn_type(&[], false);
    let function = module.add_function(
        "__ntsc_init",
        fn_type,
        Some(inkwell::module::Linkage::Internal),
    );
    let entry = context.append_basic_block(function, "entry");
    let builder = context.create_builder();
    builder.position_at_end(entry);
    let entry_builder = context.create_builder();
    entry_builder.position_at_end(entry);

    let mut fn_ctx = FunctionContext::new(
        function,
        &builder,
        &entry_builder,
        entry,
        module,
        Ty::Void,
        context,
    );

    let val = emit_expression(&mut fn_ctx, top_expr)?;
    if expr_is_fresh(&fn_ctx, top_expr, &val) {
        emit_drop_value(&mut fn_ctx, &val)?;
    }
    emit_exception_return(&mut fn_ctx, &Ty::Void, context)?;
    emit_drop_all_owned(&mut fn_ctx)?;
    builder.build_return(None)?;

    Ok(())
}

// ── Top-level variable ──────────────────────────────────────────────────

pub(crate) fn emit_top_level_var<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    name: &ntsc_ast::token::Token,
    type_annotation: &Option<ntsc_ast::types::TypeAnnotation>,
    initializer: &Option<Expr>,
) -> Result<(), crate::CodegenError> {
    let void_ty = context.void_type();
    let fn_type = void_ty.fn_type(&[], false);
    let function = module.add_function(
        "__ntsc_init",
        fn_type,
        Some(inkwell::module::Linkage::Internal),
    );
    let entry = context.append_basic_block(function, "entry");
    let builder = context.create_builder();
    builder.position_at_end(entry);
    let entry_builder = context.create_builder();
    entry_builder.position_at_end(entry);

    let ty = type_annotation_to_ty(type_annotation);
    let mut fn_ctx = FunctionContext::new(
        function,
        &builder,
        &entry_builder,
        entry,
        module,
        Ty::Void,
        context,
    );

    let ptr = fn_ctx.alloca(name.lexeme(), &ty)?;
    if let Some(init_expr) = initializer {
        let init_val = emit_expression(&mut fn_ctx, init_expr)?;

        let store_ty = if type_annotation.is_some() {
            ty.clone()
        } else {
            init_val.ntsc_type.clone()
        };
        if store_into_owned_slot(&mut fn_ctx, ptr, &store_ty, init_expr, &init_val)? {
            fn_ctx.mark_owned_if_heap(name.lexeme(), &store_ty);
        }
    }
    fn_ctx.define_var(name.lexeme(), ptr, ty);

    emit_exception_return(&mut fn_ctx, &Ty::Void, context)?;
    emit_drop_all_owned(&mut fn_ctx)?;
    builder.build_return(None)?;
    Ok(())
}

/// ── Static const ────────────────────────────────────────────────────────
/// Emit a `static const var` as a true module-level constant: a global slot
/// of the value's type. Scalar literals get a constant LLVM initializer;
/// string literals are built lazily on first use (permanent handles, so the
/// global is only filled once). The name is registered in `STATIC_CONSTS`
/// so `emit_variable` resolves references from anywhere.
///
/// When the type checker has pre-evaluated the constant (folded arithmetic,
/// pure function calls, etc.), the folded value is used directly instead of
/// pattern-matching on the AST.
pub(crate) fn emit_static_const<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    name: &ntsc_ast::token::Token,
    type_annotation: &Option<ntsc_ast::types::TypeAnnotation>,
    initializer: &Option<Expr>,
) -> Result<(), crate::CodegenError> {
    let name_str = name.lexeme().to_string();

    // Check for a pre-evaluated constant value from the type checker.
    let pre_evaluated = CONST_EVAL_VALUES.with(|map| map.borrow().get(&name_str).cloned());

    let ty = if let Some(ref val) = pre_evaluated {
        // Infer type from the pre-evaluated value.
        match val {
            ntsc_typeck::ConstValue::Int(_) => Ty::Int,
            ntsc_typeck::ConstValue::Float(_) => Ty::Float,
            ntsc_typeck::ConstValue::Bool(_) => Ty::Bool,
            ntsc_typeck::ConstValue::String(_) => Ty::String,
        }
    } else {
        type_annotation
            .as_ref()
            .map(|ann| type_annotation_to_ty(&Some(ann.clone())))
            .or_else(|| initializer.as_ref().and_then(expr_to_literal_ty))
            .unwrap_or(Ty::Any)
    };

    let global_name = format!("ntsc_const_{name_str}");
    let llvm_ty = ty_to_llvm(&ty, context);
    let slot = module.add_global(
        llvm_ty,
        Some(inkwell::AddressSpace::default()),
        &global_name,
    );

    // Use pre-evaluated value when available, otherwise fall back to
    // pattern-matching on the AST for backward compatibility.
    if let Some(ref val) = pre_evaluated {
        match val {
            ntsc_typeck::ConstValue::Int(n) => {
                slot.set_initializer(&context.i64_type().const_int(*n as u64, true));
            }
            ntsc_typeck::ConstValue::Float(f) => {
                slot.set_initializer(&context.f64_type().const_float(*f));
            }
            ntsc_typeck::ConstValue::Bool(b) => {
                slot.set_initializer(&context.bool_type().const_int(*b as u64, false));
            }
            ntsc_typeck::ConstValue::String(_) => {
                // Strings are built lazily on first use.
                slot.set_initializer(&llvm_ty.const_zero());
            }
        }
    } else if let Some(init) = initializer {
        match (init, &ty) {
            (
                Expr::Literal {
                    value: LiteralValue::Number(n),
                    ..
                },
                Ty::Float,
            ) => {
                let val: f64 = n.parse().map_err(|e| {
                    crate::CodegenError::LLVMError(format!("invalid float constant: {e}"))
                })?;
                slot.set_initializer(&context.f64_type().const_float(val));
            }
            (
                Expr::Literal {
                    value: LiteralValue::Number(n),
                    ..
                },
                _,
            ) => {
                let val = ntsc_ast::expr::parse_int_literal(n).ok_or_else(|| {
                    crate::CodegenError::LLVMError(format!("invalid int constant: {n}"))
                })?;
                slot.set_initializer(&context.i64_type().const_int(val as u64, true));
            }
            (
                Expr::Literal {
                    value: LiteralValue::Bool(b),
                    ..
                },
                _,
            ) => {
                slot.set_initializer(&context.bool_type().const_int(*b as u64, false));
            }
            (
                Expr::Literal {
                    value: LiteralValue::Nil,
                    ..
                },
                _,
            ) => {
                slot.set_initializer(
                    &context
                        .ptr_type(inkwell::AddressSpace::default())
                        .const_null(),
                );
            }
            (Expr::Unary { op, right }, Ty::Int)
                if op.lexeme() == "-"
                    && matches!(
                        right.as_ref(),
                        Expr::Literal {
                            value: LiteralValue::Number(_),
                            ..
                        }
                    ) =>
            {
                if let Expr::Literal {
                    value: LiteralValue::Number(n),
                    ..
                } = right.as_ref()
                {
                    let val = ntsc_ast::expr::parse_int_literal(n).ok_or_else(|| {
                        crate::CodegenError::LLVMError(format!("invalid int constant: {n}"))
                    })?;
                    slot.set_initializer(
                        &context
                            .i64_type()
                            .const_int(val.wrapping_neg() as u64, true),
                    );
                }
            }
            // Strings are built lazily on first use; the slot starts zeroed.
            _ => {
                slot.set_initializer(&llvm_ty.const_zero());
            }
        }
    } else {
        slot.set_initializer(&llvm_ty.const_zero());
    }

    STATIC_CONST_TYPES.with(|map| {
        map.borrow_mut().insert(name_str.clone(), ty.clone());
    });
    STATIC_CONST_INITS.with(|map| {
        map.borrow_mut().insert(name_str, initializer.clone());
    });
    Ok(())
}
