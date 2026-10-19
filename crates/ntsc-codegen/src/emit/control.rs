//! Control flow: if/while/do-while/for/for-in and the ternary.

use super::*;

pub(crate) fn emit_ternary<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    condition: &Expr,
    then_branch: &Expr,
    else_branch: &Expr,
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    let cond_val = emit_expression(fn_ctx, condition)?;
    let current_fn = fn_ctx.function;

    let then_bb = fn_ctx.context.append_basic_block(current_fn, "then");
    let else_bb = fn_ctx.context.append_basic_block(current_fn, "else");
    let merge_bb = fn_ctx.context.append_basic_block(current_fn, "merge");

    fn_ctx
        .builder
        .build_conditional_branch(cond_val.value.into_int_value(), then_bb, else_bb)?;

    fn_ctx.builder.position_at_end(then_bb);
    let then_val = emit_expression(fn_ctx, then_branch)?;
    let then_ty = then_val.ntsc_type.clone();

    // Emitting a branch can itself open new blocks (a string literal builds
    // its handle in one), so the edge into the merge leaves from wherever the
    // branch actually ended — naming `then_bb` would produce a phi whose
    // predecessors do not match.
    let then_end = fn_ctx
        .builder
        .get_insert_block()
        .ok_or_else(|| crate::CodegenError::LLVMError("empty block in ternary branch".into()))?;
    if then_end.get_terminator().is_none() {
        fn_ctx.builder.build_unconditional_branch(merge_bb)?;
    }

    fn_ctx.builder.position_at_end(else_bb);
    let else_val = emit_expression(fn_ctx, else_branch)?;
    let else_end = fn_ctx
        .builder
        .get_insert_block()
        .ok_or_else(|| crate::CodegenError::LLVMError("empty block in ternary branch".into()))?;
    if else_end.get_terminator().is_none() {
        fn_ctx.builder.build_unconditional_branch(merge_bb)?;
    }

    fn_ctx.builder.position_at_end(merge_bb);
    let llvm_ty = ty_to_llvm(&then_ty, fn_ctx.context);
    let phi = fn_ctx.builder.build_phi(llvm_ty, "ternary")?;
    phi.add_incoming(&[(&then_val.value, then_end), (&else_val.value, else_end)]);

    Ok(TypedValue::new(phi.as_basic_value(), then_ty))
}

// ── Control flow: if/else ───────────────────────────────────────────────

pub(crate) fn emit_if<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    condition: &Expr,
    then_branch: &Stmt,
    elif_branches: &[ntsc_ast::stmt::ElifBranch],
    else_branch: &Option<Box<Stmt>>,
) -> Result<(), crate::CodegenError> {
    let current_fn = fn_ctx.function;
    let cond_val = emit_expression(fn_ctx, condition)?;
    let cond_i1 = bool_to_i1(fn_ctx, cond_val);
    let then_bb = fn_ctx.context.append_basic_block(current_fn, "if.then");
    let merge_bb = fn_ctx.context.append_basic_block(current_fn, "if.merge");

    let false_bb = fn_ctx.context.append_basic_block(current_fn, "if.false");
    fn_ctx
        .builder
        .build_conditional_branch(cond_i1, then_bb, false_bb)?;

    fn_ctx.builder.position_at_end(then_bb);
    emit_statement_in_function(fn_ctx, then_branch)?;
    if fn_ctx
        .builder
        .get_insert_block()
        .unwrap()
        .get_terminator()
        .is_none()
    {
        fn_ctx.builder.build_unconditional_branch(merge_bb)?;
    }

    emit_if_false_chain(fn_ctx, false_bb, elif_branches, else_branch, merge_bb)?;

    fn_ctx.builder.position_at_end(merge_bb);
    Ok(())
}

fn emit_if_false_chain<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    false_bb: inkwell::basic_block::BasicBlock<'ctx>,
    elif_branches: &[ntsc_ast::stmt::ElifBranch],
    else_branch: &Option<Box<Stmt>>,
    merge_bb: inkwell::basic_block::BasicBlock<'ctx>,
) -> Result<(), crate::CodegenError> {
    let current_fn = fn_ctx.function;
    fn_ctx.builder.position_at_end(false_bb);

    if let Some(elif) = elif_branches.first() {
        let cond_val = emit_expression(fn_ctx, &elif.condition)?;
        let cond_i1 = bool_to_i1(fn_ctx, cond_val);
        let then_bb = fn_ctx.context.append_basic_block(current_fn, "elif.then");
        let next_false = fn_ctx.context.append_basic_block(current_fn, "elif.false");
        fn_ctx
            .builder
            .build_conditional_branch(cond_i1, then_bb, next_false)?;

        fn_ctx.builder.position_at_end(then_bb);
        emit_statement_in_function(fn_ctx, &elif.body)?;
        if fn_ctx
            .builder
            .get_insert_block()
            .unwrap()
            .get_terminator()
            .is_none()
        {
            fn_ctx.builder.build_unconditional_branch(merge_bb)?;
        }

        return emit_if_false_chain(
            fn_ctx,
            next_false,
            &elif_branches[1..],
            else_branch,
            merge_bb,
        );
    }

    if let Some(else_branch) = else_branch {
        emit_statement_in_function(fn_ctx, else_branch)?;
    }
    if fn_ctx
        .builder
        .get_insert_block()
        .unwrap()
        .get_terminator()
        .is_none()
    {
        fn_ctx.builder.build_unconditional_branch(merge_bb)?;
    }
    Ok(())
}

// ── Control flow: while ─────────────────────────────────────────────────

pub(crate) fn emit_while<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    condition: &Expr,
    body: &Stmt,
) -> Result<(), crate::CodegenError> {
    let current_fn = fn_ctx.function;

    let cond_bb = fn_ctx.context.append_basic_block(current_fn, "while.cond");
    let body_bb = fn_ctx.context.append_basic_block(current_fn, "while.body");
    let merge_bb = fn_ctx.context.append_basic_block(current_fn, "while.merge");

    fn_ctx.push_loop_targets(merge_bb, cond_bb);

    fn_ctx.builder.build_unconditional_branch(cond_bb)?;

    fn_ctx.builder.position_at_end(cond_bb);
    let cond_val = emit_expression(fn_ctx, condition)?;
    let cond_i1 = bool_to_i1(fn_ctx, cond_val);
    fn_ctx
        .builder
        .build_conditional_branch(cond_i1, body_bb, merge_bb)?;

    fn_ctx.builder.position_at_end(body_bb);
    emit_statement_in_function(fn_ctx, body)?;
    if fn_ctx
        .builder
        .get_insert_block()
        .unwrap()
        .get_terminator()
        .is_none()
    {
        // Release values bound by the loop itself (never names from
        // outside it) before the next iteration overwrites their slots.
        fn_ctx.drop_loop_locals()?;
        fn_ctx.builder.build_unconditional_branch(cond_bb)?;
    }

    fn_ctx.builder.position_at_end(merge_bb);
    fn_ctx.pop_loop_targets();
    Ok(())
}

// ── Control flow: do-while ──────────────────────────────────────────────

pub(crate) fn emit_do_while<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    condition: &Expr,
    body: &Stmt,
) -> Result<(), crate::CodegenError> {
    let current_fn = fn_ctx.function;

    let body_bb = fn_ctx
        .context
        .append_basic_block(current_fn, "dowhile.body");
    let cond_bb = fn_ctx
        .context
        .append_basic_block(current_fn, "dowhile.cond");
    let merge_bb = fn_ctx
        .context
        .append_basic_block(current_fn, "dowhile.merge");

    fn_ctx.push_loop_targets(merge_bb, cond_bb);

    fn_ctx.builder.build_unconditional_branch(body_bb)?;
    fn_ctx.builder.position_at_end(body_bb);
    emit_statement_in_function(fn_ctx, body)?;
    if fn_ctx
        .builder
        .get_insert_block()
        .unwrap()
        .get_terminator()
        .is_none()
    {
        // Release values bound by the loop itself (never names from
        // outside it) before the next iteration overwrites their slots.
        fn_ctx.drop_loop_locals()?;
        fn_ctx.builder.build_unconditional_branch(cond_bb)?;
    }

    fn_ctx.builder.position_at_end(cond_bb);
    let cond_val = emit_expression(fn_ctx, condition)?;
    let cond_i1 = bool_to_i1(fn_ctx, cond_val);
    fn_ctx
        .builder
        .build_conditional_branch(cond_i1, body_bb, merge_bb)?;

    fn_ctx.builder.position_at_end(merge_bb);
    fn_ctx.pop_loop_targets();
    Ok(())
}

// ── Control flow: for ───────────────────────────────────────────────────

pub(crate) fn emit_for<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    init: &Option<Box<Stmt>>,
    condition: &Option<Expr>,
    update: &Option<Expr>,
    body: &Stmt,
) -> Result<(), crate::CodegenError> {
    let current_fn = fn_ctx.function;

    if let Some(init) = init {
        emit_statement_in_function(fn_ctx, init)?;
    }

    let cond_bb = fn_ctx.context.append_basic_block(current_fn, "for.cond");
    let body_bb = fn_ctx.context.append_basic_block(current_fn, "for.body");
    let update_bb = fn_ctx.context.append_basic_block(current_fn, "for.update");
    let merge_bb = fn_ctx.context.append_basic_block(current_fn, "for.merge");

    fn_ctx.push_loop_targets(merge_bb, update_bb);

    fn_ctx.builder.build_unconditional_branch(cond_bb)?;

    fn_ctx.builder.position_at_end(cond_bb);
    if let Some(cond) = condition {
        let cond_val = emit_expression(fn_ctx, cond)?;
        let cond_i1 = bool_to_i1(fn_ctx, cond_val);
        fn_ctx
            .builder
            .build_conditional_branch(cond_i1, body_bb, merge_bb)?;
    } else {
        // No condition → infinite loop.
        fn_ctx.builder.build_unconditional_branch(body_bb)?;
    }

    fn_ctx.builder.position_at_end(body_bb);
    emit_statement_in_function(fn_ctx, body)?;
    if fn_ctx
        .builder
        .get_insert_block()
        .unwrap()
        .get_terminator()
        .is_none()
    {
        fn_ctx.drop_loop_locals()?;
        fn_ctx.builder.build_unconditional_branch(update_bb)?;
    }

    fn_ctx.builder.position_at_end(update_bb);
    if let Some(update) = update {
        let _ = emit_expression(fn_ctx, update)?;
    }
    fn_ctx.builder.build_unconditional_branch(cond_bb)?;

    fn_ctx.builder.position_at_end(merge_bb);
    fn_ctx.pop_loop_targets();
    Ok(())
}

// ── Control flow: for-in ────────────────────────────────────────────────

pub(crate) fn emit_for_in<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    variable: &ntsc_ast::token::Token,
    iterable: &Expr,
    body: &Stmt,
) -> Result<(), crate::CodegenError> {
    let current_fn = fn_ctx.function;

    let iter_val = emit_expression(fn_ctx, iterable)?;
    let iter_slot = if expr_is_fresh(fn_ctx, iterable, &iter_val) {
        // Register before the loop snapshot: iteration cleanup must preserve
        // the iterable, while returns and exception exits still own its drop.
        let name = format!("forin.iterable.{}", current_fn.count_basic_blocks());
        let ptr = fn_ctx.alloca(&name, &iter_val.ntsc_type)?;
        // A caught throw can skip the merge and re-enter this same loop.
        emit_drop_slot_value(fn_ctx, ptr, &iter_val.ntsc_type)?;
        fn_ctx.builder.build_store(ptr, iter_val.value)?;
        fn_ctx.define_var(&name, ptr, iter_val.ntsc_type.clone());
        fn_ctx.owned_slots.insert(name);
        Some(ptr)
    } else {
        None
    };

    // A view iterable borrows the underlying heap value: the view value is
    // that value's handle, so for-in iterates it directly.
    let iter_ty = match &iter_val.ntsc_type {
        Ty::View(inner, _) => inner.as_ref(),
        other => other,
    };

    // Class iterables implement the index protocol: `length()` returns the
    // element count and `get(i)` the i-th element. Arrays iterate through
    // the runtime's array accessors. Everything else was previously emitted
    // against the array accessors regardless, which either panicked in
    // codegen (tuples) or silently ran zero iterations on a non-array
    // handle (strings, slices), so non-iterable values are rejected here.
    let class_iter = match iter_ty {
        Ty::Class(name) => {
            let length_fn = fn_ctx.module.get_function(&format!("{name}.length"));
            let get_fn = fn_ctx.module.get_function(&format!("{name}.get"));
            match (length_fn, get_fn) {
                (Some(length_fn), Some(get_fn)) => Some((name.clone(), length_fn, get_fn)),
                _ => {
                    return Err(crate::CodegenError::LLVMError(format!(
                        "for-in over `{name}` requires `{name}.length()` and `{name}.get(i)` methods"
                    )));
                }
            }
        }
        Ty::Array(_) => None,
        _ => {
            return Err(crate::CodegenError::LLVMError(format!(
                "for-in is not supported on `{iter_ty}`; only arrays and classes with `length()`/`get(i)` iterator methods can be iterated"
            )));
        }
    };

    let elem_ty = if let Some((class_name, _, _)) = &class_iter {
        class_method_ret_ty(class_name, "get").unwrap_or(Ty::Any)
    } else {
        match iter_ty {
            Ty::Array(inner) => *inner.clone(),
            Ty::String => Ty::String,
            _ => Ty::Any,
        }
    };
    let is_untyped_array = matches!(iter_ty, Ty::Array(inner) if **inner == Ty::Any);

    let idx_ptr = fn_ctx.alloca(&format!("forin_idx_{}", variable.lexeme()), &Ty::Int)?;
    let zero = fn_ctx.context.i64_type().const_zero();
    fn_ctx.builder.build_store(idx_ptr, zero)?;

    let len_val = if let Some((_, length_fn, _)) = &class_iter {
        fn_ctx
            .builder
            .build_call(
                *length_fn,
                &[BasicMetadataValueEnum::PointerValue(
                    iter_val.value.into_pointer_value(),
                )],
                "forin_len",
            )?
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value()
    } else {
        let len_fn = fn_ctx
            .module
            .get_function("ntsc_array_len")
            .ok_or_else(|| crate::CodegenError::LLVMError("ntsc_array_len not declared".into()))?;
        fn_ctx
            .builder
            .build_call(
                len_fn,
                &[BasicMetadataValueEnum::IntValue(
                    iter_val.value.into_int_value(),
                )],
                "forin_len",
            )?
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value()
    };

    let cond_bb = fn_ctx.context.append_basic_block(current_fn, "forin.cond");
    let body_bb = fn_ctx.context.append_basic_block(current_fn, "forin.body");
    let incr_bb = fn_ctx.context.append_basic_block(current_fn, "forin.incr");
    let merge_bb = fn_ctx.context.append_basic_block(current_fn, "forin.merge");

    fn_ctx.push_loop_targets(merge_bb, incr_bb);

    fn_ctx.builder.build_unconditional_branch(cond_bb)?;

    fn_ctx.builder.position_at_end(cond_bb);
    let idx_val = fn_ctx
        .builder
        .build_load(fn_ctx.context.i64_type(), idx_ptr, "forin_idx")?
        .into_int_value();
    let cond =
        fn_ctx
            .builder
            .build_int_compare(IntPredicate::SLT, idx_val, len_val, "forin_cond")?;
    fn_ctx
        .builder
        .build_conditional_branch(cond, body_bb, merge_bb)?;

    fn_ctx.builder.position_at_end(body_bb);
    let elem_tv = if let Some((_, _, get_fn)) = &class_iter {
        let call_val = fn_ctx.builder.build_call(
            *get_fn,
            &[
                BasicMetadataValueEnum::PointerValue(iter_val.value.into_pointer_value()),
                BasicMetadataValueEnum::IntValue(idx_val),
            ],
            "forin_get",
        )?;
        let ret_val = call_result_to_value(fn_ctx, &call_val);
        TypedValue::new(ret_val, elem_ty.clone())
    } else if is_untyped_array {
        emit_untyped_array_element(fn_ctx, iter_val.value.into_int_value(), idx_val)?
    } else {
        let get_fn = fn_ctx
            .module
            .get_function("ntsc_array_get")
            .ok_or_else(|| crate::CodegenError::LLVMError("ntsc_array_get not declared".into()))?;
        let raw = fn_ctx
            .builder
            .build_call(
                get_fn,
                &[
                    BasicMetadataValueEnum::IntValue(iter_val.value.into_int_value()),
                    BasicMetadataValueEnum::IntValue(idx_val),
                ],
                "forin_get",
            )?
            .try_as_basic_value()
            .unwrap_basic();
        let elem_val = decode_array_scalar(fn_ctx, raw, &elem_ty)?;
        TypedValue::new(elem_val, elem_ty)
    };
    let var_ptr = fn_ctx.alloca(variable.lexeme(), &elem_tv.ntsc_type)?;
    fn_ctx.builder.build_store(var_ptr, elem_tv.value)?;
    fn_ctx.define_var(variable.lexeme(), var_ptr, elem_tv.ntsc_type);

    emit_statement_in_function(fn_ctx, body)?;
    if fn_ctx
        .builder
        .get_insert_block()
        .unwrap()
        .get_terminator()
        .is_none()
    {
        fn_ctx.drop_loop_locals()?;
        fn_ctx.builder.build_unconditional_branch(incr_bb)?;
    }

    fn_ctx.builder.position_at_end(incr_bb);
    let cur = fn_ctx
        .builder
        .build_load(fn_ctx.context.i64_type(), idx_ptr, "forin_cur")?
        .into_int_value();
    let next = fn_ctx.builder.build_int_add(
        cur,
        fn_ctx.context.i64_type().const_int(1, false),
        "forin_next",
    )?;
    fn_ctx.builder.build_store(idx_ptr, next)?;
    fn_ctx.builder.build_unconditional_branch(cond_bb)?;

    fn_ctx.builder.position_at_end(merge_bb);
    fn_ctx.pop_loop_targets();

    if let Some(ptr) = iter_slot {
        emit_drop_slot_value(fn_ctx, ptr, &iter_val.ntsc_type)?;
        fn_ctx
            .builder
            .build_store(ptr, default_llvm_value(&iter_val.ntsc_type, fn_ctx.context))?;
    }
    Ok(())
}

/// `for await x in producer { body }` — evaluate the producer (which
/// returns an array), then iterate over it.
pub(crate) fn emit_for_await<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    variable: &ntsc_ast::token::Token,
    producer: &Expr,
    body: &Stmt,
) -> Result<(), crate::CodegenError> {
    let producer_val = emit_expression(fn_ctx, producer)?;
    emit_drop_borrowed_fresh_args(
        fn_ctx,
        std::slice::from_ref(producer),
        std::slice::from_ref(&producer_val),
        &[],
    )?;
    emit_for_in(fn_ctx, variable, producer, body)
}
