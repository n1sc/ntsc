//! Result cells: the heap layout behind `result[Ok, Err]`.
//!
//! A result value is an i64 handle to a cell of at least 24 bytes:
//! `[0] i64 tag (0 = Ok, 1 = Err)`, `[1] Ok payload`, `[2] Err payload`.
//! Exactly one payload slot is valid at a time — which one is gated by the
//! tag — so uninitialized garbage in the other slot is never read. All
//! accesses go through a fixed `{ i64, i64, i64 }` view; typed payloads are
//! stored/loaded through bitcast field pointers. Aggregate payloads extend
//! the allocation beyond their slot; only the active slot is accessed.

use super::*;
use inkwell::types::{BasicTypeEnum, StructType};
use inkwell::values::IntValue;

/// Byte size of a result cell: tag plus two 8-byte payload slots.
pub(crate) const RESULT_CELL_SIZE: i64 = 24;

/// Field index of the Ok payload inside the fixed view.
const OK_SLOT: u32 = 1;

/// Field index of the Err payload inside the fixed view.
const ERR_SLOT: u32 = 2;

/// The `{ i64, i64, i64 }` view every cell access goes through.
fn result_view_type<'ctx>(context: &'ctx inkwell::context::Context) -> StructType<'ctx> {
    let i64 = context.i64_type();
    context.struct_type(&[i64.into(), i64.into(), i64.into()], false)
}

pub(crate) fn allocate_result_cell<'ctx>(
    fn_ctx: &FunctionContext<'ctx, '_>,
    ok: &Ty,
    err: &Ty,
) -> Result<PointerValue<'ctx>, crate::CodegenError> {
    let size = [(OK_SLOT, ok), (ERR_SLOT, err)].into_iter().try_fold(
        RESULT_CELL_SIZE as u64,
        |size, (slot, payload)| {
            let payload_size =
                llvm_allocation_size(fn_ctx.module, ty_to_llvm(payload, fn_ctx.context))?;
            let end = payload_size
                .checked_add(u64::from(slot) * 8)
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError(
                        "result payload size overflows its allocation".into(),
                    )
                })?;
            Ok::<_, crate::CodegenError>(size.max(end))
        },
    )?;
    let alloc_fn = fn_ctx
        .module
        .get_function("malloc")
        .ok_or_else(|| crate::CodegenError::LLVMError("malloc not declared".into()))?;
    let cell = fn_ctx
        .builder
        .build_call(
            alloc_fn,
            &[BasicMetadataValueEnum::IntValue(
                fn_ctx.context.i64_type().const_int(size, false),
            )],
            "result_box",
        )?
        .try_as_basic_value()
        .unwrap_basic()
        .into_pointer_value();
    Ok(cell)
}

/// Cast a raw cell pointer to the fixed struct view and select a field.
fn field_ptr<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    cell: PointerValue<'ctx>,
    slot: u32,
) -> Result<PointerValue<'ctx>, crate::CodegenError> {
    let view_ty = result_view_type(fn_ctx.context);
    let typed = fn_ctx.builder.build_pointer_cast(
        cell,
        fn_ctx.context.ptr_type(AddressSpace::default()),
        "result_view",
    )?;
    Ok(fn_ctx
        .builder
        .build_struct_gep(view_ty, typed, slot, "result_field_ptr")?)
}

/// Load the tag of a result cell: 0 = Ok, nonzero = Err.
pub(crate) fn result_tag<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    cell: PointerValue<'ctx>,
) -> Result<IntValue<'ctx>, crate::CodegenError> {
    Ok(fn_ctx
        .builder
        .build_load(
            fn_ctx.context.i64_type(),
            field_ptr(fn_ctx, cell, 0)?,
            "result_tag",
        )?
        .into_int_value())
}

fn store_result_tag<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    cell: PointerValue<'ctx>,
    want_ok: bool,
) -> Result<(), crate::CodegenError> {
    let tag = fn_ctx
        .context
        .i64_type()
        .const_int(u64::from(!want_ok), false);
    let ptr = field_ptr(fn_ctx, cell, 0)?;
    fn_ctx.builder.build_store(ptr, tag)?;
    Ok(())
}

/// Typed pointer to one payload slot, bitcast from the fixed i64 view.
fn payload_slot_ptr<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    cell: PointerValue<'ctx>,
    want_ok: bool,
    payload_ty: &Ty,
) -> Result<PointerValue<'ctx>, crate::CodegenError> {
    let slot = if want_ok { OK_SLOT } else { ERR_SLOT };
    let i64_ptr = field_ptr(fn_ctx, cell, slot)?;
    let payload_llvm = ty_to_llvm(payload_ty, fn_ctx.context);
    if matches!(payload_llvm, BasicTypeEnum::IntType(t) if t == fn_ctx.context.i64_type()) {
        return Ok(i64_ptr);
    }
    Ok(fn_ctx.builder.build_pointer_cast(
        i64_ptr,
        fn_ctx.context.ptr_type(AddressSpace::default()),
        "result_payload_typed",
    )?)
}

/// Load one payload slot with its declared representation.
pub(crate) fn load_result_payload<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    cell: PointerValue<'ctx>,
    want_ok: bool,
    payload_ty: &Ty,
) -> Result<BasicValueEnum<'ctx>, crate::CodegenError> {
    let ptr = payload_slot_ptr(fn_ctx, cell, want_ok, payload_ty)?;
    Ok(fn_ctx.builder.build_load(
        ty_to_llvm(payload_ty, fn_ctx.context),
        ptr,
        "result_payload",
    )?)
}

/// Store one payload slot, coercing a mismatched scalar to its declared
/// representation and giving string literals an owned copy first.
fn store_result_payload<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    cell: PointerValue<'ctx>,
    want_ok: bool,
    payload_ty: &Ty,
    expr: &Expr,
    payload: &TypedValue<'ctx>,
) -> Result<(), crate::CodegenError> {
    let coerced = coerce_value(fn_ctx, payload.clone(), payload_ty)?;
    let value = if matches!(payload_ty, Ty::String) && expr_is_string_literal(expr) {
        clone_string_value(fn_ctx, &coerced)?
    } else {
        coerced.value
    };
    let ptr = payload_slot_ptr(fn_ctx, cell, want_ok, payload_ty)?;
    fn_ctx.builder.build_store(ptr, value)?;
    Ok(())
}

/// Convert a raw cell pointer to the i64 handle results travel as.
fn cell_handle<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    cell: PointerValue<'ctx>,
) -> Result<BasicValueEnum<'ctx>, crate::CodegenError> {
    let handle =
        fn_ctx
            .builder
            .build_ptr_to_int(cell, fn_ctx.context.i64_type(), "result_handle")?;
    Ok(handle.into())
}

/// Build a fresh result cell holding `payload` on the given side (`want_ok`
/// selects the tag and slot).
pub(crate) fn box_result_value<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    ok: &Ty,
    err: &Ty,
    expr: &Expr,
    payload: &TypedValue<'ctx>,
    want_ok: bool,
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    let slot_payload = if want_ok { ok } else { err };
    let cell = allocate_result_cell(fn_ctx, ok, err)?;
    store_result_tag(fn_ctx, cell, want_ok)?;
    store_result_payload(fn_ctx, cell, want_ok, slot_payload, expr, payload)?;
    Ok(TypedValue::new(
        cell_handle(fn_ctx, cell)?,
        Ty::Result {
            ok: Box::new(ok.clone()),
            err: Box::new(err.clone()),
        },
    ))
}

/// Whether a payload type owns heap data that must be dropped or deep-copied
/// when its owning result cell is reclaimed or copied.
pub(crate) fn payload_is_heap(ty: &Ty) -> bool {
    ty_is_owned_handle(ty)
}

/// Free a result cell block without touching either payload slot. Only safe
/// once the active payload has been moved out and the inactive slot was
/// never initialized with heap data by this cell's writers.
pub(crate) fn free_result_cell<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    cell: PointerValue<'ctx>,
) -> Result<(), crate::CodegenError> {
    let free_fn = fn_ctx
        .module
        .get_function("free")
        .ok_or_else(|| crate::CodegenError::LLVMError("free not declared".into()))?;
    fn_ctx.builder.build_call(
        free_fn,
        &[BasicMetadataValueEnum::PointerValue(cell)],
        "result_free",
    )?;
    Ok(())
}

/// Drop a result value: reclaim the active payload's heap data (if any),
/// then free the cell block. A null handle (an uninitialized result slot)
/// drops as no-op, like an unset option.
pub(crate) fn emit_drop_result_value<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    ok: &Ty,
    err: &Ty,
    val: &TypedValue<'ctx>,
) -> Result<(), crate::CodegenError> {
    let cell = option_cell_pointer(fn_ctx, val.value)?;
    let current_fn = fn_ctx.function;
    let insert_bb = fn_ctx
        .builder
        .get_insert_block()
        .ok_or_else(|| crate::CodegenError::LLVMError("result drop has no insert block".into()))?;
    let check_bb = fn_ctx
        .context
        .append_basic_block(current_fn, "result_drop.check");
    let ok_bb = fn_ctx
        .context
        .append_basic_block(current_fn, "result_drop.ok");
    let err_bb = fn_ctx
        .context
        .append_basic_block(current_fn, "result_drop.err");
    let done_bb = fn_ctx
        .context
        .append_basic_block(current_fn, "result_drop.done");
    let is_null = fn_ctx.builder.build_is_null(cell, "result_drop_null")?;
    fn_ctx
        .builder
        .build_conditional_branch(is_null, done_bb, check_bb)?;

    fn_ctx.builder.position_at_end(check_bb);
    let tag = result_tag(fn_ctx, cell)?;
    let is_ok = fn_ctx.builder.build_int_compare(
        IntPredicate::EQ,
        tag,
        fn_ctx.context.i64_type().const_zero(),
        "result_is_ok",
    )?;
    fn_ctx
        .builder
        .build_conditional_branch(is_ok, ok_bb, err_bb)?;

    for (bb, want_ok, payload_ty) in [(ok_bb, true, ok), (err_bb, false, err)] {
        fn_ctx.builder.position_at_end(bb);
        if payload_is_heap(payload_ty) {
            let loaded = load_result_payload(fn_ctx, cell, want_ok, payload_ty)?;
            emit_drop_value(fn_ctx, &TypedValue::new(loaded, payload_ty.clone()))?;
        }
        free_result_cell(fn_ctx, cell)?;
        fn_ctx.builder.build_unconditional_branch(done_bb)?;
    }

    let _ = insert_bb;
    fn_ctx.builder.position_at_end(done_bb);
    Ok(())
}

/// Deep-copy a result into an independent owned cell. Only the active
/// payload slot is copied — copying both would walk uninitialized memory.
pub(crate) fn emit_copy_result_value<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    ok: &Ty,
    err: &Ty,
    val: &TypedValue<'ctx>,
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    let source = option_cell_pointer(fn_ctx, val.value)?;
    let target = allocate_result_cell(fn_ctx, ok, err)?;

    let current_fn = fn_ctx.function;
    let ok_bb = fn_ctx
        .context
        .append_basic_block(current_fn, "result_copy.ok");
    let err_bb = fn_ctx
        .context
        .append_basic_block(current_fn, "result_copy.err");
    let done_bb = fn_ctx
        .context
        .append_basic_block(current_fn, "result_copy.done");

    // Copy the tag first on the entry path so both branches share it.
    let tag = result_tag(fn_ctx, source)?;
    let target_entry = field_ptr(fn_ctx, target, 0)?;
    fn_ctx.builder.build_store(target_entry, tag)?;
    let is_ok = fn_ctx.builder.build_int_compare(
        IntPredicate::EQ,
        tag,
        fn_ctx.context.i64_type().const_zero(),
        "result_copy_is_ok",
    )?;
    fn_ctx
        .builder
        .build_conditional_branch(is_ok, ok_bb, err_bb)?;

    for (bb, want_ok, payload_ty) in [(ok_bb, true, ok), (err_bb, false, err)] {
        fn_ctx.builder.position_at_end(bb);
        let loaded = load_result_payload(fn_ctx, source, want_ok, payload_ty)?;
        let payload = TypedValue::new(loaded, payload_ty.clone());
        let copied = if payload_is_heap(payload_ty) {
            emit_copy_value(fn_ctx, payload)?.value
        } else {
            loaded
        };
        let ptr = payload_slot_ptr(fn_ctx, target, want_ok, payload_ty)?;
        fn_ctx.builder.build_store(ptr, copied)?;
        fn_ctx.builder.build_unconditional_branch(done_bb)?;
    }

    fn_ctx.builder.position_at_end(done_bb);
    Ok(TypedValue::new(
        cell_handle(fn_ctx, target)?,
        Ty::Result {
            ok: Box::new(ok.clone()),
            err: Box::new(err.clone()),
        },
    ))
}

/// Extract the active payload of a result cell for reuse outside it. A fresh
/// cell is adopted (freed after the read); a shared one has its payload
/// deep-copied and stays intact.
pub(crate) fn take_result_payload<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    cell: PointerValue<'ctx>,
    want_ok: bool,
    payload_ty: &Ty,
    fresh: bool,
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    let loaded = load_result_payload(fn_ctx, cell, want_ok, payload_ty)?;
    let payload = TypedValue::new(loaded, payload_ty.clone());
    if fresh {
        free_result_cell(fn_ctx, cell)?;
        Ok(payload)
    } else {
        emit_copy_value(fn_ctx, payload)
    }
}

/// Build an owned result cell around an already-prepared payload value
/// without re-evaluating any expression.
pub(crate) fn rebox_payload<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    ok: &Ty,
    err: &Ty,
    want_ok: bool,
    payload: TypedValue<'ctx>,
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    let slot_ty = if want_ok { ok } else { err };
    let coerced = coerce_value(fn_ctx, payload, slot_ty)?;
    let cell = allocate_result_cell(fn_ctx, ok, err)?;
    store_result_tag(fn_ctx, cell, want_ok)?;
    let ptr = payload_slot_ptr(fn_ctx, cell, want_ok, slot_ty)?;
    fn_ctx.builder.build_store(ptr, coerced.value)?;
    Ok(TypedValue::new(
        cell_handle(fn_ctx, cell)?,
        Ty::Result {
            ok: Box::new(ok.clone()),
            err: Box::new(err.clone()),
        },
    ))
}

/// The return type of a lambda-valued argument (`fun(..) -> R`).
fn lambda_return_ty(arg: &TypedValue<'_>) -> Result<Ty, crate::CodegenError> {
    match &arg.ntsc_type {
        Ty::Function { return_type, .. } => Ok((**return_type).clone()),
        other => Err(crate::CodegenError::LLVMError(format!(
            "combinator expects a function argument, got `{other}`"
        ))),
    }
}

/// Call a `fun`-typed value with already-prepared arguments.
fn call_function_value<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    f: &TypedValue<'ctx>,
    args: &[TypedValue<'ctx>],
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    let f = f.clone();
    if !f.value.is_pointer_value() {
        return Err(crate::CodegenError::LLVMError(
            "combinator argument is not a callable".into(),
        ));
    }
    let called = super::call::emit_function_pointer_call(
        fn_ctx,
        f.value.into_pointer_value(),
        args,
        &f.ntsc_type,
    )?;
    fn_ctx.emit_pending_exception_check()?;
    Ok(called)
}

/// Emit a builtin combinator on a `result[.., ..]` or `option[..]` receiver.
/// Returns `None` when the receiver type or name does not name one, letting
/// the normal method path proceed (a class may still declare methods with
/// these names; typeck only routes here for result/option receivers).
pub(crate) fn emit_result_combinator<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    object: &Expr,
    receiver: &TypedValue<'ctx>,
    prop_name: &str,
    arguments: &[Expr],
    arg_values: &[TypedValue<'ctx>],
) -> Result<Option<TypedValue<'ctx>>, crate::CodegenError> {
    let fresh_source = expr_is_fresh(fn_ctx, object, receiver);

    let result_shape = |ok: &Ty, err: &Ty| Ty::Result {
        ok: Box::new(ok.clone()),
        err: Box::new(err.clone()),
    };

    // ── result receivers ────────────────────────────────────────────────
    if let Ty::Result { ok, err } = &receiver.ntsc_type {
        if !matches!(prop_name, "unwrap_or" | "map" | "and_then" | "or_else") {
            return Ok(None);
        }
        if arguments.len() != 1 {
            return Err(crate::CodegenError::LLVMError(format!(
                "{prop_name} expects 1 argument"
            )));
        }
        let cell = option_cell_pointer(fn_ctx, receiver.value)?;

        // Argument preparation emits copies/moves into the current block, so
        // it must happen before the tag branch terminates it.
        let default = if prop_name == "unwrap_or" {
            let param_tys = [ok.as_ref().clone()];
            // The default becomes an owned value (moved or copied per its
            // source) that exactly one branch consumes; the Ok branch drops
            // it unused.
            let prepared = prepare_call_args(fn_ctx, arguments, arg_values, &param_tys)?;
            Some(coerce_value(fn_ctx, prepared[0].clone(), ok)?)
        } else {
            None
        };

        let current_fn = fn_ctx.function;
        let ok_bb = fn_ctx.context.append_basic_block(current_fn, "comb.ok");
        let err_bb = fn_ctx.context.append_basic_block(current_fn, "comb.err");
        let done_bb = fn_ctx.context.append_basic_block(current_fn, "comb.done");
        let tag = result_tag(fn_ctx, cell)?;
        let is_ok = fn_ctx.builder.build_int_compare(
            IntPredicate::EQ,
            tag,
            fn_ctx.context.i64_type().const_zero(),
            "comb_is_ok",
        )?;
        fn_ctx
            .builder
            .build_conditional_branch(is_ok, ok_bb, err_bb)?;

        match prop_name {
            "unwrap_or" => {
                let Some(default) = default else {
                    return Err(crate::CodegenError::LLVMError(
                        "internal: unwrap_or without a prepared default".into(),
                    ));
                };
                fn_ctx.builder.position_at_end(ok_bb);
                let payload = take_result_payload(fn_ctx, cell, true, ok, fresh_source)?;
                emit_drop_value(fn_ctx, &default)?;
                // The payload work may split this branch into several
                // blocks; record the actual predecessor for the PHI.
                let ok_pred = fn_ctx.builder.get_insert_block().unwrap();
                fn_ctx.builder.build_unconditional_branch(done_bb)?;

                fn_ctx.builder.position_at_end(err_bb);
                if fresh_source {
                    emit_drop_result_value(fn_ctx, ok, err, receiver)?;
                }
                let err_pred = fn_ctx.builder.get_insert_block().unwrap();
                fn_ctx.builder.build_unconditional_branch(done_bb)?;

                fn_ctx.builder.position_at_end(done_bb);
                let phi = fn_ctx
                    .builder
                    .build_phi(ty_to_llvm(ok, fn_ctx.context), "unwrap_or")?;
                phi.add_incoming(&[(&payload.value, ok_pred), (&default.value, err_pred)]);
                return Ok(Some(TypedValue::new(phi.as_basic_value(), (**ok).clone())));
            }

            "map" => {
                let f = arg_values[0].clone();
                let u = lambda_return_ty(&f)?;
                fn_ctx.builder.position_at_end(ok_bb);
                let payload = take_result_payload(fn_ctx, cell, true, ok, fresh_source)?;
                let mapped = call_function_value(fn_ctx, &f, &[payload])?;
                let boxed = rebox_payload(fn_ctx, &u, err, true, mapped)?;
                let ok_cell = boxed.value;
                let ok_pred = fn_ctx.builder.get_insert_block().unwrap();
                fn_ctx.builder.build_unconditional_branch(done_bb)?;

                fn_ctx.builder.position_at_end(err_bb);
                let payload = take_result_payload(fn_ctx, cell, false, err, fresh_source)?;
                let boxed = rebox_payload(fn_ctx, &u, err, false, payload)?;
                let err_cell = boxed.value;
                let err_pred = fn_ctx.builder.get_insert_block().unwrap();
                fn_ctx.builder.build_unconditional_branch(done_bb)?;

                fn_ctx.builder.position_at_end(done_bb);
                let phi = fn_ctx.builder.build_phi(fn_ctx.context.i64_type(), "map")?;
                phi.add_incoming(&[(&ok_cell, ok_pred), (&err_cell, err_pred)]);
                return Ok(Some(TypedValue::new(
                    phi.as_basic_value(),
                    result_shape(&u, err),
                )));
            }

            "and_then" => {
                let f = arg_values[0].clone();
                let chained = lambda_return_ty(&f)?;
                fn_ctx.builder.position_at_end(ok_bb);
                let payload = take_result_payload(fn_ctx, cell, true, ok, fresh_source)?;
                let called = call_function_value(fn_ctx, &f, &[payload])?;
                let ok_cell = called.value;
                let ok_pred = fn_ctx.builder.get_insert_block().unwrap();
                fn_ctx.builder.build_unconditional_branch(done_bb)?;

                fn_ctx.builder.position_at_end(err_bb);
                let payload = take_result_payload(fn_ctx, cell, false, err, fresh_source)?;
                let boxed = match &chained {
                    Ty::Result {
                        ok: chain_ok,
                        err: chain_err,
                    } => rebox_payload(fn_ctx, chain_ok, chain_err, false, payload)?,
                    _ => {
                        return Err(crate::CodegenError::LLVMError(
                            "and_then expects a `result`-returning function".into(),
                        ));
                    }
                };
                let err_cell = boxed.value;
                let err_pred = fn_ctx.builder.get_insert_block().unwrap();
                fn_ctx.builder.build_unconditional_branch(done_bb)?;

                fn_ctx.builder.position_at_end(done_bb);
                let phi = fn_ctx
                    .builder
                    .build_phi(fn_ctx.context.i64_type(), "and_then")?;
                phi.add_incoming(&[(&ok_cell, ok_pred), (&err_cell, err_pred)]);
                return Ok(Some(TypedValue::new(phi.as_basic_value(), chained)));
            }

            "or_else" => {
                let f = arg_values[0].clone();
                let chained = lambda_return_ty(&f)?;
                fn_ctx.builder.position_at_end(ok_bb);
                let payload = take_result_payload(fn_ctx, cell, true, ok, fresh_source)?;
                let boxed = match &chained {
                    Ty::Result {
                        ok: chain_ok,
                        err: chain_err,
                    } => rebox_payload(fn_ctx, chain_ok, chain_err, true, payload)?,
                    _ => {
                        return Err(crate::CodegenError::LLVMError(
                            "or_else expects a `result`-returning function".into(),
                        ));
                    }
                };
                let ok_cell = boxed.value;
                let ok_pred = fn_ctx.builder.get_insert_block().unwrap();
                fn_ctx.builder.build_unconditional_branch(done_bb)?;

                fn_ctx.builder.position_at_end(err_bb);
                let payload = take_result_payload(fn_ctx, cell, false, err, fresh_source)?;
                let called = call_function_value(fn_ctx, &f, &[payload])?;
                let err_cell = called.value;
                let err_pred = fn_ctx.builder.get_insert_block().unwrap();
                fn_ctx.builder.build_unconditional_branch(done_bb)?;

                fn_ctx.builder.position_at_end(done_bb);
                let phi = fn_ctx
                    .builder
                    .build_phi(fn_ctx.context.i64_type(), "or_else")?;
                phi.add_incoming(&[(&ok_cell, ok_pred), (&err_cell, err_pred)]);
                return Ok(Some(TypedValue::new(phi.as_basic_value(), chained)));
            }

            _ => {
                return Err(crate::CodegenError::LLVMError(format!(
                    "internal: unhandled result combinator `{prop_name}`"
                )));
            }
        }
    }

    // ── option receivers ────────────────────────────────────────────────
    if let Ty::Option(inner) = &receiver.ntsc_type {
        if !matches!(prop_name, "ok_or" | "ok_or_else") {
            return Ok(None);
        }
        if arguments.len() != 1 {
            return Err(crate::CodegenError::LLVMError(format!(
                "{prop_name} expects 1 argument"
            )));
        }
        // Argument preparation emits copies/moves into the current block, so
        // it must happen before the null branch terminates it.
        let inner = (**inner).clone();
        let (err_ty, e_value) = if prop_name == "ok_or" {
            let param_tys = [arg_values[0].ntsc_type.clone()];
            let prepared = prepare_call_args(fn_ctx, arguments, arg_values, &param_tys)?;
            let mut prepared = prepared;
            let e = prepared.pop().ok_or_else(|| {
                crate::CodegenError::LLVMError("internal: ok_or without a default".into())
            })?;
            (e.ntsc_type.clone(), Some(e))
        } else {
            let f = arg_values[0].clone();
            (lambda_return_ty(&f)?, None)
        };

        let cell = option_cell_pointer(fn_ctx, receiver.value)?;
        let current_fn = fn_ctx.function;
        let some_bb = fn_ctx.context.append_basic_block(current_fn, "comb.some");
        let none_bb = fn_ctx.context.append_basic_block(current_fn, "comb.none");
        let done_bb = fn_ctx.context.append_basic_block(current_fn, "comb.done");
        let is_null = fn_ctx.builder.build_is_null(cell, "opt_comb_null")?;
        fn_ctx
            .builder
            .build_conditional_branch(is_null, none_bb, some_bb)?;

        // Some: box the payload as the Ok side of a fresh cell.
        fn_ctx.builder.position_at_end(some_bb);
        let loaded =
            fn_ctx
                .builder
                .build_load(ty_to_llvm(&inner, fn_ctx.context), cell, "option_inner")?;
        let payload = TypedValue::new(loaded, inner.clone());
        let payload = if fresh_source {
            free_result_cell(fn_ctx, cell)?;
            payload
        } else {
            emit_copy_value(fn_ctx, payload)?
        };
        if let Some(unused_error) = &e_value {
            emit_drop_value(fn_ctx, unused_error)?;
        }
        let boxed = rebox_payload(fn_ctx, &inner, &err_ty, true, payload)?;
        let some_cell = boxed.value;
        let some_pred = fn_ctx.builder.get_insert_block().unwrap();
        fn_ctx.builder.build_unconditional_branch(done_bb)?;

        // None: produce the Err side from the argument (or the thunk).
        fn_ctx.builder.position_at_end(none_bb);
        let e_payload = match e_value {
            Some(v) => v,
            None => call_function_value(fn_ctx, &arg_values[0], &[])?,
        };
        let boxed = rebox_payload(fn_ctx, &inner, &err_ty, false, e_payload)?;
        let none_cell = boxed.value;
        let none_pred = fn_ctx.builder.get_insert_block().unwrap();
        fn_ctx.builder.build_unconditional_branch(done_bb)?;

        fn_ctx.builder.position_at_end(done_bb);
        let phi = fn_ctx
            .builder
            .build_phi(fn_ctx.context.i64_type(), "ok_or")?;
        phi.add_incoming(&[(&some_cell, some_pred), (&none_cell, none_pred)]);
        return Ok(Some(TypedValue::new(
            phi.as_basic_value(),
            result_shape(&inner, &err_ty),
        )));
    }

    Ok(None)
}
