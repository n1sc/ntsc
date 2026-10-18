//! Emission of user functions (`emit_function` and helpers).

use super::*;

pub(crate) fn fn_type_from_params<'ctx>(
    context: &'ctx Context,
    params: &[ntsc_ast::expr::FunctionParam],
    return_type: &Option<ntsc_ast::types::ReturnType>,
) -> (inkwell::types::FunctionType<'ctx>, Vec<Ty>) {
    let ret_ty = function_return_ty(return_type);

    let param_tys: Vec<Ty> = params
        .iter()
        .map(|p| type_annotation_to_ty(&p.type_annotation))
        .collect();
    let llvm_param_tys: Vec<inkwell::types::BasicMetadataTypeEnum<'ctx>> = param_tys
        .iter()
        .map(|p| ty_to_llvm(p, context).into())
        .collect();

    let fn_ty = if ret_ty == Ty::Void {
        context.void_type().fn_type(&llvm_param_tys, false)
    } else {
        ty_to_llvm(&ret_ty, context).fn_type(&llvm_param_tys, false)
    };
    (fn_ty, param_tys)
}

/// Emit the function-level exception-return block, if a pending exception
/// ever needed it (see [`FunctionContext::current_exception_handler`]). A
/// pending exception with no enclosing `try`/`retry` unwinds through here:
/// every owned local is dropped and the function returns a default value, so
/// the caller's own pending-exception check takes over.
pub(crate) fn emit_exception_return<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    ret_ty: &Ty,
    context: &'ctx Context,
) -> Result<(), crate::CodegenError> {
    if let Some(exc_bb) = fn_ctx.exception_return_bb {
        let resume_bb = fn_ctx.builder.get_insert_block();
        fn_ctx.builder.position_at_end(exc_bb);
        emit_drop_all_owned(fn_ctx)?;
        drop_return_escape_class_locals(fn_ctx)?;
        match ret_ty {
            Ty::Void => {
                fn_ctx.builder.build_return(None)?;
            }

            // A result-returning function reports the unwinding exception as
            // its Err value: the pending message is taken over (stringified
            // when the error type is not a string) and boxed into an Err
            // cell, so the caller's own pending-exception check sees a
            // normal return.
            Ty::Result { ok, err } => {
                let take_msg_fn = fn_ctx
                    .module
                    .get_function("ntsc_exception_take_message")
                    .ok_or_else(|| {
                        crate::CodegenError::LLVMError(
                            "ntsc_exception_take_message not declared".into(),
                        )
                    })?;
                let msg = fn_ctx
                    .builder
                    .build_call(take_msg_fn, &[], "exception_msg")?
                    .try_as_basic_value()
                    .unwrap_basic();
                let msg_val = TypedValue::new(msg, (**err).clone());
                let msg_val = if **err == Ty::String {
                    msg_val
                } else {
                    convert_to_string(fn_ctx, &msg_val)?
                };
                let boxed = super::result_cell::rebox_payload(fn_ctx, ok, err, false, msg_val)?;
                fn_ctx.builder.build_return(Some(&boxed.value))?;
            }
            _ => {
                let default_val = default_llvm_value(ret_ty, context);
                fn_ctx.builder.build_return(Some(&default_val))?;
            }
        }
        if let Some(resume_bb) = resume_bb {
            fn_ctx.builder.position_at_end(resume_bb);
        }
    }
    Ok(())
}

/// Drop the class instances whose only escape is a `return`. These are absent
/// from `owned_slots` because the normal path moves them to the caller, so
/// [`emit_drop_all_owned`] never touches them; on the exception-return path
/// the function returns a default instead, so their owned fields would leak.
/// Each slot is null-initialized and the class drop thunk is null-safe, so a
/// candidate not yet constructed when the throw occurred is dropped as a
/// no-op.
fn drop_return_escape_class_locals<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
) -> Result<(), crate::CodegenError> {
    if fn_ctx.future_base.is_some() {
        return Ok(());
    }
    let mut names: Vec<String> = fn_ctx.exception_class_drops.iter().cloned().collect();
    names.sort();
    for name in names {
        if let Some((ptr, ty)) = fn_ctx.variables.get(&name).map(|(p, t)| (*p, t.clone())) {
            emit_drop_slot_value(fn_ctx, ptr, &ty)?;
        }
    }
    Ok(())
}

pub(crate) fn emit_function<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    name_token: &ntsc_ast::token::Token,
    params: &[ntsc_ast::expr::FunctionParam],
    return_type: &Option<ntsc_ast::types::ReturnType>,
    body: &[Stmt],
) -> Result<FunctionValue<'ctx>, crate::CodegenError> {
    let fn_name = if name_token.lexeme() == "main" {
        "__ntsc_user_main"
    } else {
        name_token.lexeme()
    };

    let (fn_ty, param_tys) = fn_type_from_params(context, params, return_type);
    let ret_ty = function_return_ty(return_type);

    let function = match module.get_function(fn_name) {
        // Reuse the forward declaration emitted by the pre-pass when present.
        Some(function) => function,
        None => module.add_function(fn_name, fn_ty, Some(inkwell::module::Linkage::External)),
    };

    // A function with no statements still needs a definition: the call site
    // was already emitted against this symbol, so leaving it a declaration
    // fails to link. It returns immediately with the default for its return
    // type, exactly like an empty method body.
    if body.is_empty() {
        let entry = context.append_basic_block(function, "entry");
        let builder = context.create_builder();
        builder.position_at_end(entry);
        if ret_ty == Ty::Void {
            builder.build_return(None)?;
        } else {
            builder.build_return(Some(&default_llvm_value(&ret_ty, context)))?;
        }
        return Ok(function);
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
    fn_ctx.stack_allocated = analyze_stack_allocatable(body, module);
    fn_ctx.class_drops = analyze_class_drops(body, module, params);
    fn_ctx.exception_class_drops = analyze_return_escape_class_drops(body, module, params);

    // Owned parameters (arrays/strings, not `view`) own the value passed
    // by the caller and must drop it at exit.
    for (i, param) in params.iter().enumerate() {
        let param_ty = &param_tys[i];
        let ptr = fn_ctx.alloca(param.name.lexeme(), param_ty)?;
        let arg_value = function.get_nth_param(i as u32).ok_or_else(|| {
            crate::CodegenError::LLVMError(format!("missing parameter {}", param.name.lexeme()))
        })?;
        fn_ctx.builder.build_store(ptr, arg_value)?;
        fn_ctx.define_var(param.name.lexeme(), ptr, param_ty.clone());

        fn_ctx.mark_owned_if_heap(param.name.lexeme(), param_ty);
    }

    for stmt in body {
        emit_statement_in_function(&mut fn_ctx, stmt)?;
    }

    emit_exception_return(&mut fn_ctx, &ret_ty, context)?;
    let current_block = fn_ctx.builder.get_insert_block().unwrap();
    // Auto-insert the return if the body fell off the end.
    if current_block.get_terminator().is_none() {
        drop_return_escape_class_locals(&mut fn_ctx)?;
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

    Ok(function)
}
