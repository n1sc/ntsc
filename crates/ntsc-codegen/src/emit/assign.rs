//! Assignment and destructuring emission.

use super::*;

pub(crate) fn emit_assign<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    name: &ntsc_ast::token::Token,
    value: &Expr,
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    let val = emit_expression(fn_ctx, value)?;
    // Adopt Rust object handles into caller-side ownership: transfer fresh
    // bridge results, retain shares, release what the slot held. This runs
    // before the store below so `x = x` retains first and cannot free
    // through itself.
    super::adopt_object_assignment(fn_ctx, name.lexeme(), value)?;

    match fn_ctx
        .lookup_var(name.lexeme())
        .map(|(p, t)| (p, t.clone()))
    {
        Some((ptr, ty)) => {
            // A shared slot stores a box: a shared value is copied by
            // retaining (sharing never moves), an owned one is boxed and
            // adopted. The new reference is taken before the old one is
            // released, so self-assignment is safe.
            if matches!(ty, Ty::Shared(_)) {
                let boxed = box_or_retain_shared(fn_ctx, &ty, value, &val)?;
                if fn_ctx.owned_slots.contains(name.lexeme()) {
                    emit_drop_slot_value(fn_ctx, ptr, &ty)?;
                }
                fn_ctx.builder.build_store(ptr, boxed.value)?;
                fn_ctx.mark_owned_if_heap(name.lexeme(), &ty);
                // The box adopted a moved source; null its slot so its
                // exit-time drop cannot free the boxed value twice.
                if let Expr::Variable { name: source } = value
                    && !matches!(val.ntsc_type, Ty::Shared(_))
                {
                    fn_ctx.null_var_slot(source.lexeme());
                }
                return Ok(val);
            }

            // An option slot stores a boxed cell: `nil` is a null cell, a
            // plain value is auto-wrapped, and another option is
            // deep-copied so each slot owns a distinct cell.
            if let Ty::Option(inner) = &ty {
                // The old cell is reclaimed unconditionally: an option
                // always owns its cell, and consulting `owned_slots` here
                // would be wrong for a loop-carried option, whose body is
                // emitted before the slot has been marked owned — no drop
                // would ever be emitted and every iteration but the last
                // would leak. A null cell drops as a no-op.
                emit_drop_slot_value(fn_ctx, ptr, &ty)?;
                let cell = if matches!(val.ntsc_type, Ty::Nil) {
                    fn_ctx
                        .context
                        .ptr_type(AddressSpace::default())
                        .const_null()
                } else if matches!(val.ntsc_type, Ty::Option(_)) {
                    // Adopt the fresh temp's cell rather than allocating a
                    // second one and orphaning it.
                    if expr_is_fresh(fn_ctx, value, &val) {
                        val.value.into_pointer_value()
                    } else {
                        clone_option_value(fn_ctx, inner, &val)?
                    }
                } else {
                    box_option_value(fn_ctx, inner, value, &val)?
                };
                fn_ctx.builder.build_store(ptr, cell)?;
                fn_ctx.mark_owned_if_heap(name.lexeme(), &ty);
                if let Expr::Variable { name: source } = value
                    && !matches!(val.ntsc_type, Ty::Option(_) | Ty::Nil)
                {
                    fn_ctx.null_var_slot(source.lexeme());
                }
                return Ok(val);
            }

            // A `dyn` slot always owns its fat pointer (`own dyn` boxes the
            // same fat pointer into a cell). The identity-guarded
            // replaced-value drop reclaims the previous header without
            // freeing a value that was just stored back (`d = d`).
            let dyn_target = match &ty {
                Ty::Dyn(_) => Some(ty.clone()),
                Ty::Own(inner) if matches!(**inner, Ty::Dyn(_)) => Some((**inner).clone()),
                _ => None,
            };
            if let Some(dyn_ty) = dyn_target {
                let stored = if val.ntsc_type == dyn_ty {
                    val.clone()
                } else {
                    match &val.ntsc_type {
                        Ty::Class(_) | Ty::Own(_) => {
                            // Coercion moves ownership into the header, so
                            // only freshly constructed instances may enter
                            // the slot.
                            if !super::dyn_obj::expr_is_fresh_construction(fn_ctx, value) {
                                return Err(crate::CodegenError::LLVMError(format!(
                                    "only a newly constructed instance can be assigned to a `dyn` slot, got `{}`",
                                    val.ntsc_type
                                )));
                            }
                            super::helper::coerce_value(fn_ctx, val.clone(), &ty)?
                        }
                        other => {
                            return Err(crate::CodegenError::LLVMError(format!(
                                "cannot assign `{other}` to a `dyn` slot"
                            )));
                        }
                    }
                };
                if fn_ctx.owned_slots.contains(name.lexeme()) {
                    emit_drop_replaced_value(fn_ctx, ptr, &ty, &stored)?;
                }
                fn_ctx.builder.build_store(ptr, stored.value)?;
                fn_ctx.mark_owned_if_heap(name.lexeme(), &ty);
                if let Expr::Variable { name: source } = value
                    && source.lexeme() != name.lexeme()
                {
                    fn_ctx.null_var_slot(source.lexeme());
                }
                return Ok(val);
            }

            // A `chan` slot holds one reference: take the new one before
            // releasing the old, so `ch = ch` cannot destroy the channel it
            // stores back.
            if matches!(ty, Ty::Chan(_)) {
                retain_chan_value(fn_ctx, value, &val)?;
                emit_drop_replaced_value(fn_ctx, ptr, &ty, &val)?;
                fn_ctx.builder.build_store(ptr, val.value)?;
                return Ok(val);
            }

            correct_empty_array_flag(fn_ctx, value, &val, &ty)?;
            // A string literal assigned into a `string` slot is heap-copied
            // (the slot must never free an immutable global); an owned
            // value — including a bare variable, which is a move — transfers.
            let is_str_lit = expr_is_string_literal(value);
            let owned_new = expr_is_owned(fn_ctx, value, &val);
            let stored = if is_str_lit && matches!(ty, Ty::String) {
                TypedValue::new(clone_string_value(fn_ctx, &val)?, Ty::String)
            } else {
                val.clone()
            };

            // Reclaim the value the slot currently owns (a no-op on a null
            // slot). The replacement is computed first, so `xs = xs` hands
            // the same handle back and the identity guard inside the drop
            // leaves it alone. Loop-carried values are reclaimed on every
            // iteration, not just at exit.
            //
            // A return-escaping class local is reassigned before it is moved
            // out: it never enters `owned_slots` (the final value belongs to
            // the caller), but its previous value is still ours to reclaim.
            // The value returned is only ever the final one, so dropping the
            // replaced value here cannot free what the caller receives.
            //
            // A plain class local that scope-exit-drop reclaims (`class_drops`)
            // must likewise drop the instance it currently holds before a new
            // one overwrites it; membership in `class_drops` means the value is
            // neither aliased nor escaping, so reclaiming it here is safe.
            if fn_ctx.owned_slots.contains(name.lexeme())
                || (matches!(ty, Ty::Class(_))
                    && (fn_ctx.exception_class_drops.contains(name.lexeme())
                        || fn_ctx.class_drops.contains(name.lexeme())))
            {
                emit_drop_replaced_value(fn_ctx, ptr, &ty, &stored)?;
            }
            fn_ctx.builder.build_store(ptr, stored.value)?;
            if owned_new && matches!(ty, Ty::Array(_) | Ty::String) {
                fn_ctx.mark_owned_if_heap(name.lexeme(), &ty);
            }

            // Null the moved-from source slot so its exit-time drop cannot
            // free the value twice. Self-assignment is exempt: the slot
            // being nulled would be the one now holding the value, orphaning
            // it. `owned_slots` was not consulted because a slot holding a
            // borrowed value must not free it.
            if owned_new
                && let Expr::Variable { name: source } = value
                && source.lexeme() != name.lexeme()
            {
                fn_ctx.null_var_slot(source.lexeme());
            }
            Ok(val)
        }
        None => Err(crate::CodegenError::LLVMError(format!(
            "cannot assign to undefined variable `{}`",
            name.lexeme()
        ))),
    }
}

/// Bind every name of a destructuring declaration to its own slot:
/// `var [a, b] = xs` reads elements 0 and 1, `var {x, y} = o` reads the
/// matching keys.
///
/// Each bound name gets a value of its own, so an owned element is copied
/// out rather than aliased: the source keeps its own reference, and its
/// scope-exit drop must not free what a bound name now owns. A fresh source
/// (`var [a, b] = [1, 2]`) is owned by nobody once its elements have been
/// read, and is dropped here.
pub(crate) fn emit_destructure<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    is_array: bool,
    names: &[ntsc_ast::token::Token],
    keys: &[String],
    initializer: &Expr,
) -> Result<(), crate::CodegenError> {
    let source = emit_expression(fn_ctx, initializer)?;
    let source = deref_shared(fn_ctx, source)?;

    // Indexing reads through a view exactly as it reads an array: the view
    // value already is the underlying handle.
    let element_ty = match &source.ntsc_type {
        Ty::Array(inner) => Some((**inner).clone()),
        Ty::View(inner, _) => match &**inner {
            Ty::Array(inner) => Some((**inner).clone()),
            _ => None,
        },
        _ => None,
    };
    for (position, name) in names.iter().enumerate() {
        let bound = if is_array {
            let index = fn_ctx.context.i64_type().const_int(position as u64, false);
            match &element_ty {
                // An untyped container yields untyped elements; the read
                // itself reports an out-of-range index.
                None | Some(Ty::Any) => {
                    emit_untyped_array_element(fn_ctx, source.value.into_int_value(), index)?
                }
                Some(element_ty) => {
                    let get_fn = fn_ctx
                        .module
                        .get_function("ntsc_array_get")
                        .ok_or_else(|| {
                            crate::CodegenError::LLVMError("ntsc_array_get not declared".into())
                        })?;
                    let raw = fn_ctx
                        .builder
                        .build_call(
                            get_fn,
                            &[source.value.into(), index.into()],
                            "destructure_get",
                        )?
                        .try_as_basic_value()
                        .unwrap_basic();

                    // A too-short source throws from the runtime; the
                    // pending exception must reach the enclosing handler
                    // rather than binding the failure value.
                    fn_ctx.emit_pending_exception_check()?;
                    let value = decode_array_scalar(fn_ctx, raw, element_ty)?;
                    TypedValue::new(value, element_ty.clone())
                }
            }
        } else {
            // Object destructuring reads the property named by the key at
            // this position; objects are JSON-backed, so every read is a
            // string.
            let key = keys.get(position).map_or(name.lexeme(), String::as_str);
            let get_fn = fn_ctx.module.get_function("ntsc_json_get").ok_or_else(|| {
                crate::CodegenError::LLVMError("ntsc_json_get not declared".into())
            })?;
            let key = emit_string_const(fn_ctx, key)?;
            let result = fn_ctx.builder.build_call(
                get_fn,
                &[
                    BasicMetadataValueEnum::IntValue(source.value.into_int_value()),
                    BasicMetadataValueEnum::IntValue(key.into_int_value()),
                ],
                "destructure_json_get",
            )?;
            let value = call_result_to_value(fn_ctx, &result);
            TypedValue::new(value, Ty::String)
        };

        let owned = matches!(bound.ntsc_type, Ty::Array(_) | Ty::String);
        // The read borrows the container's own value, so an owned element
        // is copied before it gets a second owner.
        let bound = if owned && is_array {
            copy_owned_value(fn_ctx, &bound)?
        } else {
            bound
        };
        let ptr = fn_ctx.alloca(name.lexeme(), &bound.ntsc_type)?;

        // A destructuring pattern in a loop rebinds the same slot on every
        // pass, so an owned binding releases the previous iteration's value
        // before overwriting it. The new value is already in hand, so the
        // drop cannot disturb it.
        if owned && fn_ctx.future_base.is_none() {
            emit_drop_slot_value(fn_ctx, ptr, &bound.ntsc_type)?;
        }
        fn_ctx.builder.build_store(ptr, bound.value)?;
        fn_ctx.define_var(name.lexeme(), ptr, bound.ntsc_type.clone());
        if owned && fn_ctx.future_base.is_none() {
            fn_ctx.mark_owned_if_heap(name.lexeme(), &bound.ntsc_type);
        }
    }
    if expr_is_fresh(fn_ctx, initializer, &source) {
        emit_drop_value(fn_ctx, &source)?;
    }
    Ok(())
}

/// Destructure a tuple value into individual variable bindings.
///
/// Tuples are stack-allocated LLVM structs (value types). Each element is
/// extracted via GEP and stored in a fresh alloca.
pub(crate) fn emit_tuple_destructure<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    names: &[ntsc_ast::token::Token],
    initializer: &Expr,
) -> Result<(), crate::CodegenError> {
    let source = emit_expression(fn_ctx, initializer)?;
    let source = deref_shared(fn_ctx, source)?;
    let stored_tuple_ownership = if let Expr::Variable { name } = initializer {
        fn_ctx
            .tuple_class_elements
            .remove(name.lexeme())
            .unwrap_or_default()
    } else {
        HashSet::new()
    };
    if let Ty::Tuple(element_tys) = &source.ntsc_type {
        let expression_tuple_ownership =
            owned_class_elements_in_tuple(initializer, &source.ntsc_type);
        let tuple_ll_ty = ty_to_llvm(&source.ntsc_type, fn_ctx.context);
        for (position, name) in names.iter().enumerate() {
            if position >= element_tys.len() {
                break;
            }
            let element_ty = &element_tys[position];
            if let inkwell::types::BasicTypeEnum::StructType(_st) = tuple_ll_ty {
                let extracted = fn_ctx.builder.build_extract_value(
                    source.value.into_struct_value(),
                    position as u32,
                    "tuple_elem",
                )?;
                let bound = TypedValue::new(extracted, element_ty.clone());
                let ptr = fn_ctx.alloca(name.lexeme(), &bound.ntsc_type)?;
                fn_ctx.builder.build_store(ptr, bound.value)?;
                fn_ctx.define_var(name.lexeme(), ptr, bound.ntsc_type.clone());
                let class_owned = stored_tuple_ownership.contains(&position)
                    || expression_tuple_ownership.contains(&position);
                if class_owned {
                    fn_ctx.class_drops.insert(name.lexeme().to_string());
                }
                if ty_is_owned_handle(element_ty) && fn_ctx.future_base.is_none() {
                    fn_ctx.mark_owned_if_heap(name.lexeme(), &bound.ntsc_type);
                }
            }
        }
    }
    Ok(())
}

/// Load an element from an untyped (`[]`) array as a runtime string
/// pointer.
///
/// Untyped arrays store every element in its `Ty::Any` representation (a
/// string pointer): `arrays.push` coerces scalars to strings before
/// boxing, so a plain pointer load is sufficient here. An out-of-range
/// index throws from the runtime, so the pending exception is propagated
/// to the enclosing handler.
pub(crate) fn emit_untyped_array_element<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    handle: inkwell::values::IntValue<'ctx>,
    index: inkwell::values::IntValue<'ctx>,
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    let get_fn = fn_ctx
        .module
        .get_function("ntsc_array_get")
        .ok_or_else(|| crate::CodegenError::LLVMError("ntsc_array_get not declared".into()))?;
    let result = fn_ctx.builder.build_call(
        get_fn,
        &[
            BasicMetadataValueEnum::IntValue(handle),
            BasicMetadataValueEnum::IntValue(index),
        ],
        "any_get",
    )?;

    fn_ctx.emit_pending_exception_check()?;
    let val = call_result_to_value(fn_ctx, &result);
    Ok(TypedValue::new(val, Ty::String))
}
