//! try/catch/finally, retry, and match emission.

use super::*;

/// Emit a protected region using the runtime's return-check exception
/// model.
///
/// Calls inside the try body are followed by a pending-exception check that
/// branches to the handler block when a call (or an explicit `throw`) set
/// the runtime's pending flag. The handler binds the catch variable (an
/// owned clone of the message, after which the runtime's copy is freed),
/// runs the catch body, and falls through to the finally block. The finally
/// runs on both paths; the normal path clears the (empty) pending slot
/// first, and an exception still active afterwards — a finally with no
/// catch, or a throw inside the finally itself — propagates outward through
/// the rethrow block.
pub(crate) fn emit_try_catch<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    try_block: &Stmt,
    catch_var: &Option<ntsc_ast::token::Token>,
    catch_block: &Option<Box<Stmt>>,
    finally_block: &Option<Box<Stmt>>,
) -> Result<(), crate::CodegenError> {
    let has_catch = catch_block.is_some();
    let has_finally = finally_block.is_some();

    if !has_catch && !has_finally {
        return emit_statement_in_function(fn_ctx, try_block);
    }

    let current_fn = fn_ctx.function;
    let clear_fn = fn_ctx
        .module
        .get_function("ntsc_exception_clear")
        .ok_or_else(|| {
            crate::CodegenError::LLVMError("ntsc_exception_clear not declared".into())
        })?;
    let get_msg_fn = fn_ctx
        .module
        .get_function("ntsc_exception_get_message")
        .ok_or_else(|| {
            crate::CodegenError::LLVMError("ntsc_exception_get_message not declared".into())
        })?;
    let clone_fn = fn_ctx
        .module
        .get_function("ntsc_string_clone")
        .ok_or_else(|| crate::CodegenError::LLVMError("ntsc_string_clone not declared".into()))?;

    let handler_bb = fn_ctx.context.append_basic_block(current_fn, "try.handler");
    let finally_bb =
        has_finally.then(|| fn_ctx.context.append_basic_block(current_fn, "try.finally"));

    let rethrow_bb = (!has_catch || has_finally)
        .then(|| fn_ctx.context.append_basic_block(current_fn, "try.rethrow"));
    let merge_bb = fn_ctx.context.append_basic_block(current_fn, "try.merge");

    // A finally body can throw even when a catch handled the try body, so
    // it always needs a propagation target. Register the handler for the
    // try body, then fall into it.
    fn_ctx.exception_targets.push(handler_bb);
    emit_statement_in_function(fn_ctx, try_block)?;
    fn_ctx.exception_targets.pop();

    // Normal completion of the try body: the finally runs with a clean
    // pending slot.
    if fn_ctx
        .builder
        .get_insert_block()
        .unwrap()
        .get_terminator()
        .is_none()
    {
        fn_ctx
            .builder
            .build_call(clear_fn, &[], "try_normal_clear")?;
        let target = finally_bb.unwrap_or(merge_bb);
        fn_ctx.builder.build_unconditional_branch(target)?;
    }

    fn_ctx.builder.position_at_end(handler_bb);
    if let (Some(var), Some(catch_body)) = (catch_var, catch_block) {
        // Bind an owned copy of the message, then free the runtime's copy so
        // the catch body runs with a clean pending slot: its own throws then
        // propagate outward instead of re-triggering this handler.
        let msg = fn_ctx
            .builder
            .build_call(get_msg_fn, &[], "catch_msg")?
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let owned = fn_ctx
            .builder
            .build_call(
                clone_fn,
                &[inkwell::values::BasicMetadataValueEnum::IntValue(msg)],
                "catch_msg_owned",
            )?
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        fn_ctx.builder.build_call(clear_fn, &[], "catch_clear")?;
        let ptr = fn_ctx.alloca(var.lexeme(), &Ty::String)?;

        // The binding owns a clone of the message, and one entry-block slot
        // backs it on every pass, so a `try` inside a loop must release the
        // previous iteration's message here. The slot starts null, so the
        // first pass drops a null handle.
        emit_drop_slot_value(fn_ctx, ptr, &Ty::String)?;
        fn_ctx.builder.build_store(ptr, owned)?;
        fn_ctx.define_var(var.lexeme(), ptr, Ty::String);
        fn_ctx.mark_owned_if_heap(var.lexeme(), &Ty::String);
        emit_statement_in_function(fn_ctx, catch_body)?;
        if fn_ctx
            .builder
            .get_insert_block()
            .unwrap()
            .get_terminator()
            .is_none()
        {
            let target = finally_bb.unwrap_or(merge_bb);
            fn_ctx.builder.build_unconditional_branch(target)?;
        }
    } else {
        let target = finally_bb.unwrap_or_else(|| rethrow_bb.expect("rethrow block present"));
        fn_ctx.builder.build_unconditional_branch(target)?;
    }

    if let Some(finally_bb) = finally_bb {
        fn_ctx.builder.position_at_end(finally_bb);
        if let Some(finally_body) = finally_block {
            emit_statement_in_function(fn_ctx, finally_body)?;
        }
        if fn_ctx
            .builder
            .get_insert_block()
            .unwrap()
            .get_terminator()
            .is_none()
        {
            // Finally: runs on both paths; propagate an exception that is
            // still active afterwards (no catch, or the finally itself
            // threw).
            let is_active_fn = fn_ctx
                .module
                .get_function("ntsc_exception_is_active")
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError("ntsc_exception_is_active not declared".into())
                })?;
            let active = fn_ctx
                .builder
                .build_call(is_active_fn, &[], "finally_active")?
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            let active_bool = fn_ctx.builder.build_int_compare(
                IntPredicate::NE,
                active,
                fn_ctx.context.i8_type().const_zero(),
                "finally_has_exception",
            )?;
            let rethrow = rethrow_bb.expect("rethrow block present");
            fn_ctx
                .builder
                .build_conditional_branch(active_bool, rethrow, merge_bb)?;
        }
    }

    if let Some(rethrow_bb) = rethrow_bb {
        // Rethrow (no catch): hand the still-pending message to the outer
        // handler.
        fn_ctx.builder.position_at_end(rethrow_bb);
        let take_msg_fn = fn_ctx
            .module
            .get_function("ntsc_exception_take_message")
            .ok_or_else(|| {
                crate::CodegenError::LLVMError("ntsc_exception_take_message not declared".into())
            })?;
        let msg = fn_ctx
            .builder
            .build_call(take_msg_fn, &[], "rethrow_msg")?
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let rethrow_fn = fn_ctx
            .module
            .get_function("ntsc_rethrow")
            .ok_or_else(|| crate::CodegenError::LLVMError("ntsc_rethrow not declared".into()))?;
        fn_ctx.builder.build_call(
            rethrow_fn,
            &[inkwell::values::BasicMetadataValueEnum::IntValue(msg)],
            "rethrow",
        )?;
        let outer_handler = fn_ctx.current_exception_handler();
        fn_ctx.builder.build_unconditional_branch(outer_handler)?;
    }

    fn_ctx.builder.position_at_end(merge_bb);
    Ok(())
}

/// Emit a `retry count body [catch (var) body]` statement.
///
/// The body is attempted up to `count` times. A failed attempt (a pending
/// exception detected after a call, or an explicit `throw`) branches to the
/// failure handler, which counts the attempt and loops back while retries
/// remain; once exhausted, control passes to the `catch` handler, or the
/// last exception propagates outward when there is no handler. The message
/// binding follows the same return-check model as [`emit_try_catch`].
pub(crate) fn emit_retry<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    count: &Expr,
    body: &Stmt,
    catch_var: &Option<ntsc_ast::token::Token>,
    catch_block: &Option<Box<Stmt>>,
) -> Result<(), crate::CodegenError> {
    let has_catch = catch_block.is_some();
    let current_fn = fn_ctx.function;

    let clear_fn = fn_ctx
        .module
        .get_function("ntsc_exception_clear")
        .ok_or_else(|| {
            crate::CodegenError::LLVMError("ntsc_exception_clear not declared".into())
        })?;
    let get_msg_fn = fn_ctx
        .module
        .get_function("ntsc_exception_get_message")
        .ok_or_else(|| {
            crate::CodegenError::LLVMError("ntsc_exception_get_message not declared".into())
        })?;
    let clone_fn = fn_ctx
        .module
        .get_function("ntsc_string_clone")
        .ok_or_else(|| crate::CodegenError::LLVMError("ntsc_string_clone not declared".into()))?;

    let count_val = emit_expression(fn_ctx, count)?;
    let remaining_ptr = fn_ctx.alloca("retry_remaining", &Ty::Int)?;
    fn_ctx.builder.build_store(remaining_ptr, count_val.value)?;

    let check_bb = fn_ctx.context.append_basic_block(current_fn, "retry.check");
    let body_bb = fn_ctx.context.append_basic_block(current_fn, "retry.body");
    let failure_bb = fn_ctx
        .context
        .append_basic_block(current_fn, "retry.failure");
    let clear_bb = fn_ctx.context.append_basic_block(current_fn, "retry.clear");
    let except_bb = fn_ctx
        .context
        .append_basic_block(current_fn, "retry.except");
    let rethrow_bb = (!has_catch).then(|| {
        fn_ctx
            .context
            .append_basic_block(current_fn, "retry.rethrow")
    });
    let merge_bb = fn_ctx.context.append_basic_block(current_fn, "retry.merge");

    fn_ctx.push_loop_targets(merge_bb, check_bb);
    fn_ctx.builder.build_unconditional_branch(check_bb)?;

    // Loop head: attempt while a retry remains, otherwise handle the last
    // exception (catch or rethrow).
    fn_ctx.builder.position_at_end(check_bb);
    let remaining = fn_ctx
        .builder
        .build_load(fn_ctx.context.i64_type(), remaining_ptr, "retry_left")?
        .into_int_value();
    let still = fn_ctx.builder.build_int_compare(
        IntPredicate::SGT,
        remaining,
        fn_ctx.context.i64_type().const_zero(),
        "retry_still",
    )?;
    fn_ctx
        .builder
        .build_conditional_branch(still, body_bb, except_bb)?;

    fn_ctx.builder.position_at_end(body_bb);
    // The loop's `break` exits the retry; `continue` starts the next
    // attempt. The attempt body runs with the failure handler registered.
    fn_ctx.exception_targets.push(failure_bb);
    emit_statement_in_function(fn_ctx, body)?;
    fn_ctx.exception_targets.pop();
    if fn_ctx
        .builder
        .get_insert_block()
        .unwrap()
        .get_terminator()
        .is_none()
    {
        fn_ctx.builder.build_unconditional_branch(merge_bb)?;
    }

    // Failed attempt: count it, then retry if any remain, otherwise handle
    // the last exception.
    fn_ctx.builder.position_at_end(failure_bb);
    let remaining = fn_ctx
        .builder
        .build_load(fn_ctx.context.i64_type(), remaining_ptr, "retry_left")?
        .into_int_value();
    let next_remaining = fn_ctx.builder.build_int_sub(
        remaining,
        fn_ctx.context.i64_type().const_int(1, false),
        "retry_decrement",
    )?;
    fn_ctx.builder.build_store(remaining_ptr, next_remaining)?;
    let again = fn_ctx.builder.build_int_compare(
        IntPredicate::SGT,
        next_remaining,
        fn_ctx.context.i64_type().const_zero(),
        "retry_again",
    )?;
    fn_ctx
        .builder
        .build_conditional_branch(again, clear_bb, except_bb)?;

    fn_ctx.builder.position_at_end(clear_bb);
    // Retry: drop the failed attempt's exception and start the next
    // attempt.
    fn_ctx.builder.build_call(clear_fn, &[], "retry_clear")?;
    fn_ctx.builder.build_unconditional_branch(check_bb)?;

    // Exhausted: bind an owned copy of the last message and run the catch
    // handler; the runtime's copy is freed first so the catch body runs
    // with a clean pending slot.
    fn_ctx.builder.position_at_end(except_bb);
    if let (Some(var), Some(catch)) = (catch_var, catch_block) {
        let msg = fn_ctx
            .builder
            .build_call(get_msg_fn, &[], "retry_msg")?
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let owned = fn_ctx
            .builder
            .build_call(
                clone_fn,
                &[inkwell::values::BasicMetadataValueEnum::IntValue(msg)],
                "retry_msg_owned",
            )?
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        fn_ctx.builder.build_call(clear_fn, &[], "retry_clear")?;
        let ptr = fn_ctx.alloca(var.lexeme(), &Ty::String)?;

        // As in `try`/`catch`: one slot backs the binding on every pass,
        // so a `retry` inside a loop releases the previous message before
        // rebinding.
        emit_drop_slot_value(fn_ctx, ptr, &Ty::String)?;
        fn_ctx.builder.build_store(ptr, owned)?;
        fn_ctx.define_var(var.lexeme(), ptr, Ty::String);
        fn_ctx.mark_owned_if_heap(var.lexeme(), &Ty::String);
        emit_statement_in_function(fn_ctx, catch)?;
        if fn_ctx
            .builder
            .get_insert_block()
            .unwrap()
            .get_terminator()
            .is_none()
        {
            fn_ctx.builder.build_unconditional_branch(merge_bb)?;
        }
    } else {
        fn_ctx
            .builder
            .build_unconditional_branch(rethrow_bb.expect("rethrow block present"))?;
    }

    if let Some(rethrow_bb) = rethrow_bb {
        fn_ctx.builder.position_at_end(rethrow_bb);
        let take_msg_fn = fn_ctx
            .module
            .get_function("ntsc_exception_take_message")
            .ok_or_else(|| {
                crate::CodegenError::LLVMError("ntsc_exception_take_message not declared".into())
            })?;
        let msg = fn_ctx
            .builder
            .build_call(take_msg_fn, &[], "rethrow_msg")?
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let rethrow_fn = fn_ctx
            .module
            .get_function("ntsc_rethrow")
            .ok_or_else(|| crate::CodegenError::LLVMError("ntsc_rethrow not declared".into()))?;
        fn_ctx.builder.build_call(
            rethrow_fn,
            &[inkwell::values::BasicMetadataValueEnum::IntValue(msg)],
            "rethrow",
        )?;
        let outer_handler = fn_ctx.current_exception_handler();
        fn_ctx.builder.build_unconditional_branch(outer_handler)?;
    }

    fn_ctx.builder.position_at_end(merge_bb);
    fn_ctx.pop_loop_targets();
    Ok(())
}

// ── Match ───────────────────────────────────────────────────────────────

pub(crate) fn emit_match<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    expression: &Expr,
    cases: &[ntsc_ast::stmt::MatchCase],
    default_case: &Option<Box<Stmt>>,
) -> Result<(), crate::CodegenError> {
    let expr_val = emit_expression(fn_ctx, expression)?;
    let current_fn = fn_ctx.function;

    // Variant patterns (`Ok(v)` / `Err(e)`) dispatch on the result cell's
    // tag and bind the payload for the arm body.
    let scrutinee = match &expr_val.ntsc_type {
        Ty::Result { ok, err } => Some(((**ok).clone(), (**err).clone())),
        _ => None,
    };
    if cases.iter().any(|case| case.pattern.is_some()) {
        if fn_ctx.future_base.is_some() {
            return Err(crate::CodegenError::LLVMError(
                "match variant patterns are not supported inside async functions yet".into(),
            ));
        }
        let Some((ok_ty, err_ty)) = scrutinee else {
            return Err(crate::CodegenError::LLVMError(
                "match variant patterns require a `result[.., ..]` scrutinee".into(),
            ));
        };
        return emit_match_with_patterns(
            fn_ctx,
            expression,
            cases,
            default_case,
            expr_val,
            ok_ty,
            err_ty,
        );
    }

    let merge_bb = fn_ctx.context.append_basic_block(current_fn, "match.merge");

    for (i, case) in cases.iter().enumerate() {
        let case_bb = fn_ctx
            .context
            .append_basic_block(current_fn, &format!("match.case{i}"));
        let next_bb = fn_ctx
            .context
            .append_basic_block(current_fn, &format!("match.next{i}"));

        // A wildcard `case _` matches unconditionally. For a real pattern,
        // compare against the scrutinee: scalars by value, strings by
        // content; a value of a different scalar type never matches and
        // falls through.
        let is_wildcard = matches!(
            &case.value,
            Expr::Variable { name } if name.lexeme() == "_"
        );
        if !is_wildcard {
            let case_val = emit_expression(fn_ctx, &case.value)?;
            let matched = match (&expr_val.ntsc_type, &case_val.ntsc_type) {
                (Ty::Int, Ty::Int) | (Ty::Bool, Ty::Bool) => fn_ctx.builder.build_int_compare(
                    IntPredicate::EQ,
                    expr_val.value.into_int_value(),
                    case_val.value.into_int_value(),
                    "matchcmp",
                )?,
                (Ty::String, Ty::String) => {
                    let eq_fn = fn_ctx
                        .module
                        .get_function("ntsc_string_equals")
                        .ok_or_else(|| {
                            crate::CodegenError::LLVMError("ntsc_string_equals not declared".into())
                        })?;
                    let result = fn_ctx.builder.build_call(
                        eq_fn,
                        &[
                            BasicMetadataValueEnum::IntValue(expr_val.value.into_int_value()),
                            BasicMetadataValueEnum::IntValue(case_val.value.into_int_value()),
                        ],
                        "matcheq",
                    )?;
                    let i8_val = call_result_to_value(fn_ctx, &result).into_int_value();
                    fn_ctx.builder.build_int_truncate(
                        i8_val,
                        fn_ctx.context.bool_type(),
                        "matcheq_i1",
                    )?
                }

                _ if case_val.value.is_int_value() && expr_val.value.is_int_value() => {
                    fn_ctx.builder.build_int_compare(
                        IntPredicate::EQ,
                        expr_val.value.into_int_value(),
                        case_val.value.into_int_value(),
                        "matchcmp",
                    )?
                }
                _ => fn_ctx.context.bool_type().const_zero(),
            };

            let matched = if let Some(guard) = &case.guard {
                // Apply an optional guard: `case value if cond => body`.
                let guard_val = emit_expression(fn_ctx, guard)?;
                let guard_cond = match guard_val.value {
                    BasicValueEnum::IntValue(i) if guard_val.ntsc_type == Ty::Bool => i,
                    _ => {
                        return Err(crate::CodegenError::LLVMError(format!(
                            "match guard must be `bool`, got `{}`",
                            guard_val.ntsc_type
                        )));
                    }
                };
                fn_ctx
                    .builder
                    .build_and(matched, guard_cond, "matchguard")?
            } else {
                matched
            };

            fn_ctx
                .builder
                .build_conditional_branch(matched, case_bb, next_bb)?;
        } else {
            fn_ctx
                .builder
                .build_unconditional_branch(case_bb)
                .map_err(|e| crate::CodegenError::LLVMError(format!("match: {e}")))?;
        }

        fn_ctx.builder.position_at_end(case_bb);
        emit_statement_in_function(fn_ctx, &case.body)?;
        if fn_ctx
            .builder
            .get_insert_block()
            .unwrap()
            .get_terminator()
            .is_none()
        {
            fn_ctx.builder.build_unconditional_branch(merge_bb)?;
        }

        fn_ctx.builder.position_at_end(next_bb);
    }

    // Default case: runs when no pattern matched.
    if let Some(default) = default_case {
        emit_statement_in_function(fn_ctx, default)?;
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

    fn_ctx.builder.position_at_end(merge_bb);
    Ok(())
}

/// Lower a match whose scrutinee is a `result[.., ..]` and which has at
/// least one variant-pattern arm. Pattern arms test the cell's tag, bind
/// the active payload to the arm's binder (an owned copy for heap payloads,
/// so the scrutinee cell keeps its own), run an optional guard, then execute
/// the body. Plain value arms keep the sequential-compare behavior. A fresh
/// (temporary) scrutinee cell is dropped on the fall-through path; an owned
/// scrutinee stays alive for its variable.
fn emit_match_with_patterns<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    expression: &Expr,
    cases: &[ntsc_ast::stmt::MatchCase],
    default_case: &Option<Box<Stmt>>,
    expr_val: TypedValue<'ctx>,
    ok_ty: Ty,
    err_ty: Ty,
) -> Result<(), crate::CodegenError> {
    let current_fn = fn_ctx.function;
    let cell = option_cell_pointer(fn_ctx, expr_val.value)?;
    let fresh_scrutinee = expr_is_fresh(fn_ctx, expression, &expr_val);

    let merge_bb = fn_ctx.context.append_basic_block(current_fn, "match.merge");

    for (i, case) in cases.iter().enumerate() {
        let case_bb = fn_ctx
            .context
            .append_basic_block(current_fn, &format!("match.case{i}"));
        let next_bb = fn_ctx
            .context
            .append_basic_block(current_fn, &format!("match.next{i}"));

        match &case.pattern {
            Some(pattern) => {
                // Guards are evaluated inside the arm, after the binder is
                // in scope, so `Ok(v) if v > 3` can read the payload.
                let want_ok = pattern.variant.lexeme() == "Ok";
                let tag = result_tag(fn_ctx, cell)?;
                let expected = if want_ok {
                    fn_ctx.context.i64_type().const_zero()
                } else {
                    fn_ctx.context.i64_type().const_int(1, false)
                };
                let matched = fn_ctx.builder.build_int_compare(
                    IntPredicate::EQ,
                    tag,
                    expected,
                    "matchtag",
                )?;
                fn_ctx
                    .builder
                    .build_conditional_branch(matched, case_bb, next_bb)?;
            }
            None => {
                let is_wildcard = matches!(
                    &case.value,
                    Expr::Variable { name } if name.lexeme() == "_"
                );
                if !is_wildcard {
                    let case_val = emit_expression(fn_ctx, &case.value)?;
                    let matched = match (&expr_val.ntsc_type, &case_val.ntsc_type) {
                        (Ty::Int, Ty::Int) | (Ty::Bool, Ty::Bool) => {
                            fn_ctx.builder.build_int_compare(
                                IntPredicate::EQ,
                                expr_val.value.into_int_value(),
                                case_val.value.into_int_value(),
                                "matchcmp",
                            )?
                        }
                        _ if case_val.value.is_int_value() && expr_val.value.is_int_value() => {
                            fn_ctx.builder.build_int_compare(
                                IntPredicate::EQ,
                                expr_val.value.into_int_value(),
                                case_val.value.into_int_value(),
                                "matchcmp",
                            )?
                        }
                        _ => fn_ctx.context.bool_type().const_zero(),
                    };

                    let matched = if let Some(guard) = &case.guard {
                        let guard_val = emit_expression(fn_ctx, guard)?;
                        match guard_val.value {
                            BasicValueEnum::IntValue(i) if guard_val.ntsc_type == Ty::Bool => {
                                fn_ctx.builder.build_and(matched, i, "matchguard")?
                            }
                            _ => {
                                return Err(crate::CodegenError::LLVMError(format!(
                                    "match guard must be `bool`, got `{}`",
                                    guard_val.ntsc_type
                                )));
                            }
                        }
                    } else {
                        matched
                    };
                    fn_ctx
                        .builder
                        .build_conditional_branch(matched, case_bb, next_bb)?;
                } else {
                    fn_ctx
                        .builder
                        .build_unconditional_branch(case_bb)
                        .map_err(|e| crate::CodegenError::LLVMError(format!("match: {e}")))?;
                }
            }
        }

        fn_ctx.builder.position_at_end(case_bb);
        let outer_scope = fn_ctx.begin_block_scope();
        if let Some(pattern) = &case.pattern {
            bind_pattern_payload(fn_ctx, cell, pattern, &ok_ty, &err_ty)?;
        }

        // A pattern arm's guard runs after its binder is in scope; a false
        // guard falls through to the next arm.
        if let Some(guard) = &case.guard
            && case.pattern.is_some()
        {
            let guard_val = emit_expression(fn_ctx, guard)?;
            let guard_cond = match guard_val.value {
                BasicValueEnum::IntValue(i) if guard_val.ntsc_type == Ty::Bool => i,
                _ => {
                    return Err(crate::CodegenError::LLVMError(format!(
                        "match guard must be `bool`, got `{}`",
                        guard_val.ntsc_type
                    )));
                }
            };
            let body_bb = fn_ctx
                .context
                .append_basic_block(current_fn, &format!("match.armbody{i}"));
            fn_ctx
                .builder
                .build_conditional_branch(guard_cond, body_bb, next_bb)?;
            fn_ctx.builder.position_at_end(body_bb);
        }

        emit_statement_in_function(fn_ctx, &case.body)?;
        fn_ctx.end_block_scope(outer_scope)?;
        branch_to_merge_releasing_scrutinee(
            fn_ctx,
            fresh_scrutinee,
            &expr_val,
            (&ok_ty, &err_ty),
            merge_bb,
        )?;

        fn_ctx.builder.position_at_end(next_bb);
    }

    // Default case: runs when no pattern matched.
    if let Some(default) = default_case {
        emit_statement_in_function(fn_ctx, default)?;
    }

    branch_to_merge_releasing_scrutinee(
        fn_ctx,
        fresh_scrutinee,
        &expr_val,
        (&ok_ty, &err_ty),
        merge_bb,
    )?;

    fn_ctx.builder.position_at_end(merge_bb);
    Ok(())
}

/// Branch to the match's merge block, first releasing a fresh temporary
/// scrutinee cell. Every path into the merge block funnels through here so
/// a matched arm cannot leave the cell behind; paths ending in `return` or
/// a jump already have a terminator and are skipped.
fn branch_to_merge_releasing_scrutinee<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    fresh_scrutinee: bool,
    expr_val: &TypedValue<'ctx>,
    (ok_ty, err_ty): (&Ty, &Ty),
    merge_bb: inkwell::basic_block::BasicBlock<'ctx>,
) -> Result<(), crate::CodegenError> {
    let terminated = fn_ctx
        .builder
        .get_insert_block()
        .is_none_or(|block| block.get_terminator().is_some());
    if fresh_scrutinee && !terminated {
        emit_drop_result_value(fn_ctx, ok_ty, err_ty, expr_val)?;
    }
    if !terminated {
        fn_ctx
            .builder
            .build_unconditional_branch(merge_bb)
            .map_err(|e| crate::CodegenError::LLVMError(format!("match: {e}")))?;
    }
    Ok(())
}

/// Bind a variant pattern's payload to the arm's binder: heap payloads are
/// deep-copied so the binder owns its value independently of the cell;
/// scalars move by value. `_` (no binding token) skips binding entirely.
fn bind_pattern_payload<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    cell: PointerValue<'ctx>,
    pattern: &ntsc_ast::stmt::MatchPattern,
    ok_ty: &Ty,
    err_ty: &Ty,
) -> Result<(), crate::CodegenError> {
    let Some(binding) = &pattern.binding else {
        return Ok(());
    };
    let want_ok = pattern.variant.lexeme() == "Ok";
    let payload_ty = if want_ok {
        ok_ty.clone()
    } else {
        err_ty.clone()
    };

    let loaded = load_result_payload(fn_ctx, cell, want_ok, &payload_ty)?;
    let owned = if payload_is_heap(&payload_ty) {
        emit_copy_value(fn_ctx, TypedValue::new(loaded, payload_ty.clone()))?.value
    } else {
        loaded
    };
    let ptr = fn_ctx.alloca(binding.lexeme(), &payload_ty)?;
    // The entry slot backs this binding on every pass of an enclosing loop;
    // release the previous copy before rebinding.
    if fn_ctx.future_base.is_none() {
        emit_drop_slot_value(fn_ctx, ptr, &payload_ty)?;
    }
    fn_ctx.builder.build_store(ptr, owned)?;
    fn_ctx.define_var(binding.lexeme(), ptr, payload_ty.clone());
    fn_ctx.mark_owned_if_heap(binding.lexeme(), &payload_ty);
    Ok(())
}
