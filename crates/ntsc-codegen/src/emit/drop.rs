//! Owned-value drops, call-argument ownership transfer, and option boxes.

use super::*;

/// Get (or lazily create) the drop thunk for a class instance: it reclaims
/// the instance's owned heap fields (arrays, strings, shared boxes, nested
/// class instances) without freeing the instance struct itself. The struct
/// may live on the stack (escape analysis) or on the heap (`malloc`), so it
/// is left for its owner; only the fields are reclaimed here. The thunk is
/// null-safe so uninitialized or moved-from slots can be dropped
/// unconditionally.
pub(crate) fn get_or_create_class_drop_thunk<'ctx>(
    module: &Module<'ctx>,
    context: &'ctx Context,
    class_name: &str,
) -> Result<FunctionValue<'ctx>, crate::CodegenError> {
    let name = format!("ntsc_class_drop_{class_name}");
    if let Some(existing) = module.get_function(&name) {
        return Ok(existing);
    }
    let void_ty = context.void_type();
    let i8_ptr = context.ptr_type(AddressSpace::default());
    let function = module.add_function(
        &name,
        void_ty.fn_type(&[i8_ptr.into()], false),
        Some(inkwell::module::Linkage::Internal),
    );
    let entry = context.append_basic_block(function, "entry");
    let builder = context.create_builder();
    builder.position_at_end(entry);
    let entry_builder = context.create_builder();
    entry_builder.position_at_end(entry);
    let mut thunk_ctx = FunctionContext::new(
        function,
        &builder,
        &entry_builder,
        entry,
        module,
        Ty::Void,
        context,
    );
    let instance = function
        .get_nth_param(0)
        .ok_or_else(|| crate::CodegenError::LLVMError("class drop thunk has no param".into()))?
        .into_pointer_value();

    let is_null = builder.build_is_null(instance, "class_drop_null")?;
    let body_bb = context.append_basic_block(function, "class_drop.body");
    let done_bb = context.append_basic_block(function, "class_drop.done");
    builder.build_conditional_branch(is_null, done_bb, body_bb)?;

    builder.position_at_end(body_bb);
    let instance_val = TypedValue::new(instance.into(), Ty::Class(class_name.to_string()));
    let field_names = class_all_fields(class_name);
    let field_tys = class_all_field_types(class_name);
    for (field_name, field_ty) in field_names.iter().zip(field_tys.iter()) {
        if !ty_is_owned_handle(field_ty) {
            continue;
        }
        let prop = ntsc_ast::token::Token::new(
            ntsc_ast::token::TokenKind::Identifier(field_name.clone()),
            ntsc_ast::span::Span::dummy(),
        );
        let Some(gep) = emit_member_gep(&mut thunk_ctx, &instance_val, &prop)? else {
            continue;
        };
        let loaded =
            builder.build_load(ty_to_llvm(field_ty, context), gep.ptr, "class_drop_field")?;
        emit_drop_value(&mut thunk_ctx, &TypedValue::new(loaded, field_ty.clone()))?;
    }
    builder.build_unconditional_branch(done_bb)?;
    builder.position_at_end(done_bb);
    builder.build_return(None)?;
    Ok(function)
}

/// A class instance normally travels as a raw pointer, but unwrapping a
/// `shared Class` box yields the wrapped value's bits as an i64 (a box
/// stores an i64 handle whatever it wraps). Recover the instance pointer
/// either way.
pub(crate) fn class_instance_pointer<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    val: BasicValueEnum<'ctx>,
) -> Result<PointerValue<'ctx>, crate::CodegenError> {
    if val.is_pointer_value() {
        Ok(val.into_pointer_value())
    } else {
        Ok(fn_ctx.builder.build_int_to_ptr(
            val.into_int_value(),
            fn_ctx.context.ptr_type(AddressSpace::default()),
            "class_instance",
        )?)
    }
}

/// Emit a drop call for a value that is owned by the current scope, if the
/// value's static type owns a heap allocation: heap arrays (dropped via
/// `ntsc_array_drop`, recursing into nested array elements first), heap
/// strings (`ntsc_string_drop`), shared boxes (`ntsc_shared_release`), option
/// cells, and class instances (per-class thunk, reclaiming owned fields only).
/// Scalars, views, and non-owned elements are no-ops.
pub(crate) fn emit_drop_value<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    val: &TypedValue<'ctx>,
) -> Result<(), crate::CodegenError> {
    if matches!(val.ntsc_type, Ty::Slice(_)) {
        if let Some(drop_fn) = fn_ctx.module.get_function("ntsc_slices_drop") {
            fn_ctx.builder.build_call(
                drop_fn,
                &[BasicMetadataValueEnum::IntValue(val.value.into_int_value())],
                "slice_drop",
            )?;
        }
        return Ok(());
    }
    if matches!(val.ntsc_type, Ty::Pointer) {
        if let Some(drop_fn) = fn_ctx.module.get_function("ntsc_memory_drop") {
            fn_ctx.builder.build_call(
                drop_fn,
                &[BasicMetadataValueEnum::IntValue(val.value.into_int_value())],
                "pointer_drop",
            )?;
        }
        return Ok(());
    }
    match &val.ntsc_type {
        Ty::Int | Ty::Float | Ty::Bool | Ty::Void => return Ok(()),
        Ty::View(..) => return Ok(()),

        // A reference and a raw pointer borrow their pointee; neither owns
        // the allocation, so neither reclaims it.
        Ty::Ref(..) | Ty::RawPointer(..) => return Ok(()),

        // A fat pointer owns its header and the wrapped instance.
        Ty::Dyn(trait_name) => {
            return super::dyn_obj::emit_drop_dyn_value(
                fn_ctx,
                val.value.into_pointer_value(),
                trait_name,
            );
        }
        _ => {}
    }

    // An owning allocation reclaims the pointee's owned contents and then
    // frees the allocation itself. A null slot (never assigned, or moved
    // from) is a safe no-op.
    if let Ty::Own(inner) = &val.ntsc_type {
        let inner = (**inner).clone();
        let ptr = val.value.into_pointer_value();
        let current_fn = fn_ctx.function;
        let body_bb = fn_ctx
            .context
            .append_basic_block(current_fn, "own_drop.body");
        let done_bb = fn_ctx
            .context
            .append_basic_block(current_fn, "own_drop.done");
        let is_null = fn_ctx.builder.build_is_null(ptr, "own_drop_null")?;
        fn_ctx
            .builder
            .build_conditional_branch(is_null, done_bb, body_bb)?;
        fn_ctx.builder.position_at_end(body_bb);

        match &inner {
            // The allocation *is* the instance struct: reclaim its owned
            // fields with the class thunk, then free the struct.
            Ty::Class(name) => {
                let thunk =
                    get_or_create_class_drop_thunk(fn_ctx.module, fn_ctx.context, name.as_str())?;
                fn_ctx
                    .builder
                    .build_call(thunk, &[ptr.into()], "own_class_drop")?;
            }

            // A boxed owned handle: reclaim the handle the cell holds.
            other if ty_is_owned_handle(other) => {
                let loaded = fn_ctx.builder.build_load(
                    ty_to_llvm(other, fn_ctx.context),
                    ptr,
                    "own_inner",
                )?;
                emit_drop_value(fn_ctx, &TypedValue::new(loaded, other.clone()))?;
            }

            // A boxed scalar owns nothing beyond its cell.
            _ => {}
        }

        let free_fn = fn_ctx
            .module
            .get_function("free")
            .ok_or_else(|| crate::CodegenError::LLVMError("free not declared".into()))?;
        fn_ctx.builder.build_call(
            free_fn,
            &[BasicMetadataValueEnum::PointerValue(ptr)],
            "own_free",
        )?;
        fn_ctx.builder.build_unconditional_branch(done_bb)?;
        fn_ctx.builder.position_at_end(done_bb);
        return Ok(());
    }
    if let Ty::Shared(inner) = &val.ntsc_type {
        // Releasing the last copy returns the wrapped value's handle,
        // which the caller must then drop.
        let release_fn = fn_ctx
            .module
            .get_function("ntsc_shared_release")
            .ok_or_else(|| {
                crate::CodegenError::LLVMError("ntsc_shared_release not declared".into())
            })?;
        let result = fn_ctx.builder.build_call(
            release_fn,
            &[BasicMetadataValueEnum::IntValue(val.value.into_int_value())],
            "shared_release",
        )?;
        let inner_id = call_result_to_value(fn_ctx, &result).into_int_value();
        let current_fn = fn_ctx.function;
        let drop_bb = fn_ctx
            .context
            .append_basic_block(current_fn, "shared_inner.drop");
        let done_bb = fn_ctx
            .context
            .append_basic_block(current_fn, "shared_inner.done");
        let is_zero = fn_ctx.builder.build_int_compare(
            inkwell::IntPredicate::EQ,
            inner_id,
            fn_ctx.context.i64_type().const_zero(),
            "shared_inner_zero",
        )?;
        fn_ctx
            .builder
            .build_conditional_branch(is_zero, done_bb, drop_bb)?;
        fn_ctx.builder.position_at_end(drop_bb);
        emit_drop_value(fn_ctx, &TypedValue::new(inner_id.into(), (**inner).clone()))?;
        fn_ctx.builder.build_unconditional_branch(done_bb)?;
        fn_ctx.builder.position_at_end(done_bb);
        return Ok(());
    }

    // Option cells: drop the heap inner value, then free the cell. A null
    // cell (an unset option) is a no-op.
    if let Ty::Option(inner) = &val.ntsc_type {
        let cell = option_cell_pointer(fn_ctx, val.value)?;
        return emit_drop_option_value(fn_ctx, inner, cell);
    }

    // Result cells: drop the active payload's heap data, then free the cell.
    if let Ty::Result { ok, err } = &val.ntsc_type {
        return super::result_cell::emit_drop_result_value(fn_ctx, ok, err, val);
    }

    // A channel is a virtual-task registry handle.
    if matches!(val.ntsc_type, Ty::Chan(_)) {
        if let Some(drop_fn) = fn_ctx.module.get_function("ntask_chan_drop") {
            fn_ctx.builder.build_call(
                drop_fn,
                &[BasicMetadataValueEnum::IntValue(val.value.into_int_value())],
                "chan_drop",
            )?;
        }
        return Ok(());
    }

    // A string and an `object` are both registry-backed string handles (an
    // object is its JSON text), so both are reclaimed by the same drop.
    if matches!(val.ntsc_type, Ty::String | Ty::Object) {
        if let Some(drop_fn) = fn_ctx.module.get_function("ntsc_string_drop") {
            fn_ctx.builder.build_call(
                drop_fn,
                &[BasicMetadataValueEnum::IntValue(val.value.into_int_value())],
                "string_drop",
            )?;
        }
        return Ok(());
    }

    if let Ty::Class(name) = &val.ntsc_type {
        // The instance struct itself is left in place: stack-allocated
        // objects must never be freed, and a heap object may be aliased
        // through a nested field. The per-class thunk is only invoked for
        // values the escape analysis proved are not aliased.
        let thunk = get_or_create_class_drop_thunk(fn_ctx.module, fn_ctx.context, name)?;

        let instance = class_instance_pointer(fn_ctx, val.value)?;
        fn_ctx.builder.build_call(
            thunk,
            &[BasicMetadataValueEnum::PointerValue(instance)],
            "class_drop",
        )?;
        return Ok(());
    }
    if let Ty::Array(elem_ty) = &val.ntsc_type {
        let handle = val.value.into_int_value();

        // Drop nested arrays, class instances, option/result cells, and
        // shared boxes stored as elements before freeing the container.
        // Every insertion path copies option/result cells and retains
        // shared boxes (the array owns its own copies), so reclaiming them
        // here is safe. String elements are freed by the runtime itself;
        // scalars are non-owning.
        if matches!(
            **elem_ty,
            Ty::Array(_) | Ty::Class(_) | Ty::Option(_) | Ty::Result { .. } | Ty::Shared(_)
        ) && let (Some(len_fn), Some(get_fn)) = (
            fn_ctx.module.get_function("ntsc_array_len"),
            fn_ctx.module.get_function("ntsc_array_get"),
        ) {
            let current_fn = fn_ctx.function;
            let i_ptr = fn_ctx.alloca("arr_drop_i", &Ty::Int)?;
            let cond_bb = fn_ctx
                .context
                .append_basic_block(current_fn, "arr_drop.cond");
            let body_bb = fn_ctx
                .context
                .append_basic_block(current_fn, "arr_drop.body");
            let incr_bb = fn_ctx
                .context
                .append_basic_block(current_fn, "arr_drop.incr");
            let done_bb = fn_ctx
                .context
                .append_basic_block(current_fn, "arr_drop.done");

            let len = fn_ctx
                .builder
                .build_call(len_fn, &[handle.into()], "arr_drop_len")?
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            fn_ctx
                .builder
                .build_store(i_ptr, fn_ctx.context.i64_type().const_zero())?;
            fn_ctx.builder.build_unconditional_branch(cond_bb)?;

            fn_ctx.builder.position_at_end(cond_bb);
            let i = fn_ctx
                .builder
                .build_load(fn_ctx.context.i64_type(), i_ptr, "arr_drop_i")?
                .into_int_value();
            let cond = fn_ctx.builder.build_int_compare(
                inkwell::IntPredicate::SLT,
                i,
                len,
                "arr_drop_cond",
            )?;
            fn_ctx
                .builder
                .build_conditional_branch(cond, body_bb, done_bb)?;

            fn_ctx.builder.position_at_end(body_bb);
            let elem = fn_ctx
                .builder
                .build_call(get_fn, &[handle.into(), i.into()], "arr_drop_elem")?
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            let is_null = fn_ctx.builder.build_int_compare(
                inkwell::IntPredicate::EQ,
                elem,
                fn_ctx.context.i64_type().const_zero(),
                "arr_drop_elem_null",
            )?;
            let drop_bb = fn_ctx
                .context
                .append_basic_block(current_fn, "arr_drop.elem");
            let next_bb = fn_ctx
                .context
                .append_basic_block(current_fn, "arr_drop.next");
            fn_ctx
                .builder
                .build_conditional_branch(is_null, next_bb, drop_bb)?;
            fn_ctx.builder.position_at_end(drop_bb);
            let elem_ty_clone = (**elem_ty).clone();
            emit_drop_value(fn_ctx, &TypedValue::new(elem.into(), elem_ty_clone))?;
            fn_ctx.builder.build_unconditional_branch(next_bb)?;
            fn_ctx.builder.position_at_end(next_bb);
            fn_ctx.builder.build_unconditional_branch(incr_bb)?;

            fn_ctx.builder.position_at_end(incr_bb);
            let next = fn_ctx.builder.build_int_add(
                i,
                fn_ctx.context.i64_type().const_int(1, false),
                "arr_drop_next",
            )?;
            fn_ctx.builder.build_store(i_ptr, next)?;
            fn_ctx.builder.build_unconditional_branch(cond_bb)?;

            fn_ctx.builder.position_at_end(done_bb);
        }

        if let Some(drop_fn) = fn_ctx.module.get_function("ntsc_array_drop") {
            fn_ctx
                .builder
                .build_call(drop_fn, &[handle.into()], "array_drop")?;
        }
    }
    Ok(())
}

/// Drop the value a slot holds before a replacement overwrites it, unless
/// the replacement *is* that value. Self-assignment (`xs = xs`,
/// `b.items = b.items`) hands back the same handle, so freeing it first
/// would leave the slot pointing at a reclaimed value. Registry-backed
/// values are i64 handles, which makes the identity test a plain integer
/// compare. Class slots are excluded: class values have reference semantics
/// and may be aliased, so an overwrite cannot know whether another name
/// still reads the instance.
pub(crate) fn emit_drop_replaced_value<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    ptr: PointerValue<'ctx>,
    ty: &Ty,
    replacement: &TypedValue<'ctx>,
) -> Result<(), crate::CodegenError> {
    if !matches!(
        ty,
        Ty::Array(_)
            | Ty::String
            | Ty::Object
            | Ty::Shared(_)
            | Ty::Option(_)
            | Ty::Result { .. }
            | Ty::Pointer
            | Ty::Slice(_)
            | Ty::Own(_)
            | Ty::Dyn(_)
            | Ty::Class(_)
    ) {
        return Ok(());
    }
    let old = fn_ctx
        .builder
        .build_load(ty_to_llvm(ty, fn_ctx.context), ptr, "old_value")?;
    let (old_handle, new_handle) = match (old, replacement.value) {
        // Two handles of the same width can be compared for identity; a
        // narrower integer is a scalar payload (an `option[bool]` cell is
        // replaced by an `i1`), not a handle.
        (BasicValueEnum::IntValue(old), BasicValueEnum::IntValue(new))
            if old.get_type() == new.get_type() =>
        {
            (old, new)
        }

        // Two owning allocations are compared by address, so a
        // self-assignment does not free the value it stores back.
        (BasicValueEnum::PointerValue(old_ptr), BasicValueEnum::PointerValue(new_ptr)) => {
            let i64_ty = fn_ctx.context.i64_type();
            let old_addr = fn_ctx
                .builder
                .build_ptr_to_int(old_ptr, i64_ty, "old_addr")?;
            let new_addr = fn_ctx
                .builder
                .build_ptr_to_int(new_ptr, i64_ty, "new_addr")?;
            (old_addr, new_addr)
        }

        // Not a handle pair (a coerced or scalar replacement): nothing can
        // alias the old value, so reclaim it unconditionally.
        _ => return emit_drop_value(fn_ctx, &TypedValue::new(old, ty.clone())),
    };
    let same = fn_ctx.builder.build_int_compare(
        inkwell::IntPredicate::EQ,
        old_handle,
        new_handle,
        "replaced_same",
    )?;
    let drop_bb = fn_ctx
        .context
        .append_basic_block(fn_ctx.function, "replace.drop");
    let done_bb = fn_ctx
        .context
        .append_basic_block(fn_ctx.function, "replace.done");
    fn_ctx
        .builder
        .build_conditional_branch(same, done_bb, drop_bb)?;
    fn_ctx.builder.position_at_end(drop_bb);
    emit_drop_value(fn_ctx, &TypedValue::new(old, ty.clone()))?;
    fn_ctx.builder.build_unconditional_branch(done_bb)?;
    fn_ctx.builder.position_at_end(done_bb);
    Ok(())
}

/// Whether a value of `ty` is an owned heap handle: something a drop path
/// reads out of a slot or field and hands to a release function. Every one
/// of these must be null-initialized by `FunctionContext::alloca` before any
/// drop can read it; keeping the single definition here keeps the two in
/// step.
pub(crate) fn ty_is_owned_handle(ty: &Ty) -> bool {
    matches!(
        ty,
        Ty::Array(_)
            | Ty::String
            | Ty::Object
            | Ty::Shared(_)
            | Ty::Class(_)
            | Ty::Chan(_)
            | Ty::Option(_)
            | Ty::Result { .. }
            | Ty::Pointer
            | Ty::Slice(_)
            | Ty::Own(_)
            | Ty::Dyn(_)
    )
}

/// Drop the value currently stored in an owned variable slot, if the slot is
/// statically an owned heap value. Called before overwriting a variable so
/// loop-carried values are reclaimed on every iteration, not just at
/// function exit. Null slots (never initialized, or moved-from) are a safe
/// no-op.
pub(crate) fn emit_drop_slot_value<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    ptr: PointerValue<'ctx>,
    ty: &Ty,
) -> Result<(), crate::CodegenError> {
    if ty_is_owned_handle(ty) {
        let loaded = fn_ctx
            .builder
            .build_load(ty_to_llvm(ty, fn_ctx.context), ptr, "old_value")?;
        emit_drop_value(fn_ctx, &TypedValue::new(loaded, ty.clone()))?;
    }
    Ok(())
}

/// Drop every owned local at a function exit point. Async poll functions
/// are skipped: their locals live in the heap-allocated future struct whose
/// lifetime is managed by the executor, not the poll's stack frame.
pub(crate) fn emit_drop_all_owned<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
) -> Result<(), crate::CodegenError> {
    if fn_ctx.future_base.is_some() {
        return Ok(());
    }
    let mut names: Vec<String> = fn_ctx.owned_slots.iter().cloned().collect();
    names.sort();
    for name in names {
        if let Some((ptr, ty)) = fn_ctx.variables.get(&name).map(|(p, t)| (*p, t.clone())) {
            emit_drop_slot_value(fn_ctx, ptr, &ty)?;
        }
    }

    let shadowed = fn_ctx.shadowed_owned_slots.clone();
    for (ptr, ty) in shadowed {
        emit_drop_slot_value(fn_ctx, ptr, &ty)?;
    }
    // Tracked Rust object handles are owned by their slots exactly like
    // the values above (each share retains, so the last release frees).
    // Async poll functions skip everything here: their locals live in the
    // future struct, whose drop path consults the same table.
    let mut tracked: Vec<String> = fn_ctx
        .variables
        .keys()
        .filter(|name| super::rust_object_var(name).is_some())
        .cloned()
        .collect();
    tracked.sort();
    for name in tracked {
        super::release_tracked_object(fn_ctx, &name)?;
    }
    Ok(())
}

/// Drop the return-escape class locals this return does not hand to the caller.
///
/// These instances (see [`super::analyze_return_escape_class_drops`]) are kept
/// out of `owned_slots`/`class_drops` because they escape through `return` on
/// some path, so [`emit_drop_all_owned`] never touches them. On a return that
/// yields a *different* value they do not escape, so their owned fields would
/// leak. `returned` names the instance(s) this return moves out — already
/// handled by the caller's move logic — so they are never dropped here and
/// cannot be freed twice. Each slot is null-initialized and the drop thunk is
/// null-safe, so a candidate not yet constructed is a no-op.
pub(crate) fn drop_unreturned_escape_class_locals<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    returned: &[String],
) -> Result<(), crate::CodegenError> {
    if fn_ctx.future_base.is_some() {
        return Ok(());
    }
    let mut names: Vec<String> = fn_ctx
        .exception_class_drops
        .iter()
        .filter(|name| !returned.iter().any(|r| r == *name))
        .cloned()
        .collect();
    names.sort();
    for name in names {
        if let Some((ptr, ty)) = fn_ctx.variables.get(&name).map(|(p, t)| (*p, t.clone())) {
            emit_drop_slot_value(fn_ctx, ptr, &ty)?;
        }
    }
    Ok(())
}

/// array literals and `copy(...)` (which deep-clones), string concatenation,
/// `arrays.*`/`sort.*`/`random.shuffle` construction ops, calls to
/// user-defined functions (their return convention hands an owned reference
/// to the caller) and non-array module calls returning strings, and an owned
/// element read out of a fresh container (copied before the container is
/// dropped). Bare variables are *not* fresh: they are slots whose drop
/// happens at their own exit; member loads and element reads are borrowed.
/// The local variables whose owned value `expr` moves out of this scope.
///
/// A returned value is handed to the caller, so every local it moves must
/// stop being dropped here or the caller receives an already-released
/// handle. Only a local in a *value* position moves: a tuple or array
/// element that is the variable itself. Everything else either borrows
/// (an element or field read leaves its container owned here), duplicates
/// (`copy(...)`), or produces a fresh value (a concatenation, a call), and
/// nulling those slots would leak the original.
pub(crate) fn moved_locals_in(expr: &Expr, out: &mut Vec<String>) {
    match expr {
        Expr::Copy { .. } => {}
        Expr::Variable { name } => {
            if !out.iter().any(|existing| existing == name.lexeme()) {
                out.push(name.lexeme().to_string());
            }
        }
        Expr::Grouping { expression, .. } => moved_locals_in(expression, out),
        Expr::TupleLiteral { elements, .. } | Expr::ArrayLiteral { elements, .. } => {
            for element in elements {
                moved_locals_in(element, out);
            }
        }
        // Either branch may be the one that runs, so both are treated as
        // moved: leaking the untaken local beats releasing the taken one.
        Expr::Ternary {
            then_branch,
            else_branch,
            ..
        } => {
            moved_locals_in(then_branch, out);
            moved_locals_in(else_branch, out);
        }
        _ => {}
    }
}

pub(crate) fn expr_is_fresh<'ctx>(
    fn_ctx: &FunctionContext<'ctx, '_>,
    expr: &Expr,
    tv: &TypedValue<'ctx>,
) -> bool {
    if !ty_is_owned_handle(&tv.ntsc_type) {
        return false;
    }
    match expr {
        Expr::ArrayLiteral { .. } => true,

        // An object literal is built by `json.parse`-ing a freshly
        // concatenated source string, so the handle it yields has no
        // other owner.
        Expr::ObjectLiteral { .. } => true,

        // `copy(...)` hands the caller a freshly allocated owned value,
        // regardless of what the source expression was.
        Expr::Copy { .. } => true,
        Expr::Grouping { expression, .. } => expr_is_fresh(fn_ctx, expression, tv),

        // Owned fields of fresh class receivers are preserved before the
        // receiver is dropped, so their consumers adopt the resulting value.
        // A field read through a live slot or a returned view stays borrowed.
        Expr::Member { object, .. } => {
            let Some(receiver_ty) = member_receiver_type(fn_ctx, object) else {
                return false;
            };
            matches!(receiver_ty, Ty::Class(_) | Ty::Own(_))
                && expr_is_fresh(fn_ctx, object, &TypedValue::new(tv.value, receiver_ty))
        }

        // String concatenation allocates a fresh string.
        Expr::Binary { op, .. } if matches!(op.kind, ntsc_ast::token::TokenKind::Plus) => {
            matches!(tv.ntsc_type, Ty::String)
        }

        // String array stores copy the assigned value; the assignment
        // expression still owns a fresh right-hand side until consumed.
        Expr::IndexSet { value, .. } if matches!(tv.ntsc_type, Ty::String) => {
            expr_is_fresh(fn_ctx, value, tv)
        }

        // An owned element read out of a fresh container (`[1,2][0]`,
        // `copy(a)[i]`, `makeArr()[i]`) is copied out before the container
        // is dropped, so the result is a fresh owned value that must be
        // dropped. Option and shared elements are never transferred this
        // way: their cells/boxes are owned by the container, so a read
        // always copies (cloning the cell or retaining the box) and is
        // never fresh.
        Expr::IndexGet { object, .. } => match &tv.ntsc_type {
            Ty::Option(_) | Ty::Shared(_) => false,
            _ => {
                let container_tv =
                    TypedValue::new(tv.value, Ty::Array(Box::new(tv.ntsc_type.clone())));
                expr_is_fresh(fn_ctx, object, &container_tv)
            }
        },
        Expr::Call { callee, .. } => match callee.as_ref() {
            // `alloc(value)` hands the caller a fresh owning allocation.
            Expr::Variable { name } if name.lexeme() == "alloc" => {
                matches!(tv.ntsc_type, Ty::Own(_))
            }

            // `Ok(v)` / `Err(e)` build a brand-new result cell that owns
            // its payload.
            Expr::Variable { name } if matches!(name.lexeme(), "Ok" | "Err") => {
                matches!(tv.ntsc_type, Ty::Result { .. })
            }
            // A call to a user-defined function that yields an owned handle
            // (gated above) transfers ownership to the caller, so the result
            // is fresh and must be dropped. This must not depend on whether
            // the callee's body has been emitted yet: a forward reference
            // (callee defined later in the module) is still a fresh call.
            Expr::Variable { name } => {
                is_user_function(name.lexeme()) || is_class_type(name.lexeme())
            }
            Expr::Member { object, property } => {
                let prop = property.lexeme();
                match object.as_ref() {
                    Expr::Variable { name } if name.lexeme() == "arrays" => {
                        // Most `arrays.*` ops return a new array; `pop`
                        // additionally transfers ownership of the removed
                        // element to the caller (the container no longer
                        // reclaims it). This covers option cells and shared
                        // boxes as well: the popped cell/box reference is
                        // the array's own copy, and the caller takes it
                        // over. `join` materializes a fresh joined string.
                        if prop == "pop" {
                            matches!(
                                tv.ntsc_type,
                                Ty::Array(_) | Ty::String | Ty::Option(_) | Ty::Shared(_)
                            )
                        } else if prop == "join" {
                            matches!(tv.ntsc_type, Ty::String)
                        } else {
                            matches!(tv.ntsc_type, Ty::Array(_))
                        }
                    }

                    // `sort.*` and `random.shuffle` clone their input into
                    // a new array; `random.weighted`/`choice` return a
                    // borrowed element, so they are deliberately not fresh.
                    Expr::Variable { name } if name.lexeme() == "sort" => {
                        matches!(tv.ntsc_type, Ty::Array(_))
                    }
                    Expr::Variable { name } if name.lexeme() == "random" && prop == "shuffle" => {
                        matches!(tv.ntsc_type, Ty::Array(_))
                    }

                    // Other module calls returning owned handles allocate a
                    // fresh value; array-returning module calls are covered above.
                    Expr::Variable { name } => {
                        matches!(tv.ntsc_type, Ty::String)
                            || (name.lexeme() == "memory" && matches!(tv.ntsc_type, Ty::Pointer))

                            // A window is a fresh registry entry, and
                            // `to_array` materializes a fresh array.
                            || (name.lexeme() == "slices"
                                && matches!(tv.ntsc_type, Ty::Slice(_) | Ty::Array(_)))

                            // `sys.args()` materializes a fresh string
                            // array from the process argument vector.
                            || (name.lexeme() == "sys"
                                && prop == "args"
                                && matches!(tv.ntsc_type, Ty::Array(_)))

                            // `chan.new(cap)` hands over the channel's first
                            // reference, so its destination adopts it.
                            || (name.lexeme() == "chan"
                                && prop == "new"
                                && matches!(tv.ntsc_type, Ty::Chan(_)))
                    }

                    // User methods return by move, so any owned-handle
                    // result is fresh — regardless of the receiver (`this`
                    // inside a method, an index expression, a field...).
                    // Leaving these out leaked every owned return of a
                    // method whose receiver was not a bare variable.
                    _ => matches!(tv.ntsc_type, Ty::String | Ty::Array(_)),
                }
            }
            _ => false,
        },
        _ => false,
    }
}

/// Class elements whose ownership is transferred by a tuple expression.
/// Tuples are LLVM aggregates, so the ordinary whole-value freshness check
/// cannot identify ownership of an individual class field.
pub(crate) fn owned_class_elements_in_tuple(expr: &Expr, tuple_ty: &Ty) -> HashSet<usize> {
    let Ty::Tuple(element_tys) = tuple_ty else {
        return HashSet::new();
    };
    match expr {
        Expr::Grouping { expression, .. } => owned_class_elements_in_tuple(expression, tuple_ty),
        Expr::TupleLiteral { elements, .. } => elements
            .iter()
            .enumerate()
            .filter_map(|(index, element)| {
                let class_ty = element_tys.get(index)?;
                if matches!(class_ty, Ty::Class(_)) && tuple_class_expr_is_owned(element, class_ty)
                {
                    Some(index)
                } else {
                    None
                }
            })
            .collect(),
        Expr::Call { callee, .. } => {
            let Expr::Variable { name } = callee.as_ref() else {
                return HashSet::new();
            };
            let Some(Ty::Tuple(elements)) = function_declared_ret_ty(name.lexeme()) else {
                return HashSet::new();
            };
            elements
                .iter()
                .enumerate()
                .filter_map(|(index, element)| matches!(element, Ty::Class(_)).then_some(index))
                .collect()
        }
        _ => HashSet::new(),
    }
}

fn tuple_class_expr_is_owned(expr: &Expr, expected: &Ty) -> bool {
    match expr {
        Expr::Grouping { expression, .. } => tuple_class_expr_is_owned(expression, expected),
        Expr::Variable { .. } => true,
        Expr::Call { callee, .. } => {
            let Expr::Variable { name } = callee.as_ref() else {
                return false;
            };
            is_class_type(name.lexeme())
                || matches!(function_declared_ret_ty(name.lexeme()), Some(ret) if &ret == expected)
        }
        _ => false,
    }
}

/// Whether the value produced by `expr` is owned by the current scope and
/// must not be dropped by anyone else: either a fresh allocation (see
/// `expr_is_fresh`) or a bare variable, which is a move (its slot is nulled
/// by the caller).
pub(crate) fn expr_is_owned<'ctx>(
    fn_ctx: &FunctionContext<'ctx, '_>,
    expr: &Expr,
    tv: &TypedValue<'ctx>,
) -> bool {
    matches!(expr, Expr::Variable { .. }) || expr_is_fresh(fn_ctx, expr, tv)
}

/// Prepare call arguments for ownership transfer **before** the call is
/// emitted:
///
/// * a bare-variable argument to an owned (non-`view`) parameter is moved:
///   the source's owned slot is nulled so it is not dropped at the source's
///   exit,
/// * a *borrowed* variable (a `for-in` loop variable, a borrowed element
///   read) passed to an owned parameter is copied first, so the callee can
///   own its value without freeing the borrow's source,
/// * a fresh value passed to an owned parameter is transferred directly (the
///   callee now owns it).
///
/// View parameters borrow their arguments; fresh temps passed to them are
/// dropped after the call by `emit_drop_borrowed_fresh_args`.
pub(crate) fn prepare_call_args<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    arguments: &[Expr],
    arg_values: &[TypedValue<'ctx>],
    param_types: &[Ty],
) -> Result<Vec<TypedValue<'ctx>>, crate::CodegenError> {
    let mut prepared: Vec<TypedValue<'ctx>> = Vec::with_capacity(arg_values.len());
    for (i, (arg, val)) in arguments.iter().zip(arg_values).enumerate() {
        let param_ty = param_types.get(i);

        // A trait-object parameter adopts a freshly constructed instance:
        // ownership moves into the fat pointer's header.
        let dyn_target = match param_ty {
            Some(Ty::Dyn(_)) => param_ty,
            Some(Ty::Own(inner)) if matches!(inner.as_ref(), Ty::Dyn(_)) => param_ty,
            _ => None,
        };
        if let (true, Some(target)) = (matches!(val.ntsc_type, Ty::Class(_)), dyn_target) {
            if !super::dyn_obj::expr_is_fresh_construction(fn_ctx, arg) {
                return Err(crate::CodegenError::LLVMError(
                    "only a newly constructed instance can become a trait object".into(),
                ));
            }
            prepared.push(coerce_value(fn_ctx, val.clone(), target)?);
            continue;
        }

        if matches!(val.ntsc_type, Ty::Class(_)) && !matches!(param_ty, Some(Ty::View(..))) {
            // A class instance must not be passed by value: the pointer
            // would be copied and both sides would treat it as their own.
            let class_name = if let Ty::Class(name) = &val.ntsc_type {
                name.clone()
            } else {
                String::new()
            };
            return Err(crate::CodegenError::LLVMError(format!(
                "cannot pass a class instance by value (class `{class_name}`); pass a view instead"
            )));
        }

        if matches!(val.ntsc_type, Ty::Shared(_)) {
            match param_ty {
                Some(Ty::Shared(_)) => {
                    if !expr_is_fresh(fn_ctx, arg, val) {
                        let retain_fn = fn_ctx
                            .module
                            .get_function("ntsc_shared_retain")
                            .ok_or_else(|| {
                                crate::CodegenError::LLVMError(
                                    "ntsc_shared_retain not declared".into(),
                                )
                            })?;
                        fn_ctx.builder.build_call(
                            retain_fn,
                            &[BasicMetadataValueEnum::IntValue(val.value.into_int_value())],
                            "shared_arg_retain",
                        )?;
                    }
                    prepared.push(val.clone());
                }
                _ => {
                    let pointee = deref_shared(fn_ctx, val.clone())?;
                    if param_ty.is_none() || !matches!(param_ty, Some(Ty::View(..))) {
                        prepared.push(pointee);
                    } else {
                        prepared.push(TypedValue::new(
                            pointee.value,
                            Ty::View(Box::new(pointee.ntsc_type), false),
                        ));
                    }
                }
            }
            continue;
        }
        // A `chan` parameter holds its own reference, released by the callee's
        // exit drop; the caller keeps its own.
        if matches!(val.ntsc_type, Ty::Chan(_)) {
            retain_chan_value(fn_ctx, arg, val)?;
            prepared.push(val.clone());
            continue;
        }
        let takes_ownership = param_ty
            .map(|t| !matches!(t, Ty::View(..)))
            .unwrap_or(false);

        if matches!(param_ty, Some(Ty::Shared(_))) {
            let boxed = box_or_retain_shared(fn_ctx, param_ty.unwrap(), arg, val)?;
            if let Expr::Variable { name } = arg
                && fn_ctx.owned_slots.contains(name.lexeme())
            {
                fn_ctx.null_var_slot(name.lexeme());
            }
            prepared.push(boxed);
            continue;
        }
        if !takes_ownership {
            prepared.push(val.clone());
            continue;
        }
        if let Expr::Variable { name } = arg {
            if fn_ctx.owned_slots.contains(name.lexeme()) {
                fn_ctx.null_var_slot(name.lexeme());
                prepared.push(val.clone());
            } else {
                prepared.push(copy_owned_value(fn_ctx, val)?);
            }
        } else if expr_is_fresh(fn_ctx, arg, val) {
            prepared.push(val.clone());
        } else {
            prepared.push(copy_owned_value(fn_ctx, val)?);
        }
    }
    Ok(prepared)
}

/// Drop fresh temps that were borrowed (passed to view parameters): the
/// callee never owned them, so the caller reclaims them after the call.
pub(crate) fn emit_drop_borrowed_fresh_args<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    arguments: &[Expr],
    arg_values: &[TypedValue<'ctx>],
    param_types: &[Ty],
) -> Result<(), crate::CodegenError> {
    for (i, (arg, val)) in arguments.iter().zip(arg_values).enumerate() {
        let takes_ownership = param_types
            .get(i)
            .map(|t| !matches!(t, Ty::View(..)))
            .unwrap_or(false);
        if !takes_ownership && expr_is_fresh(fn_ctx, arg, val) {
            emit_drop_value(fn_ctx, val)?;
        }
    }
    Ok(())
}

pub(crate) fn copy_owned_value<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    val: &TypedValue<'ctx>,
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    let value = val.value;
    match &val.ntsc_type {
        Ty::Array(inner) => {
            let levels = array_nesting_depth(inner);
            let clone_fn = fn_ctx
                .module
                .get_function("ntsc_array_deep_clone")
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError("ntsc_array_deep_clone not declared".into())
                })?;
            let levels = fn_ctx.context.i64_type().const_int(levels, false);
            let clone_result = fn_ctx.builder.build_call(
                clone_fn,
                &[
                    BasicMetadataValueEnum::IntValue(value.into_int_value()),
                    BasicMetadataValueEnum::IntValue(levels),
                ],
                "arg_copy",
            )?;
            let new_handle = call_result_to_value(fn_ctx, &clone_result);
            Ok(TypedValue::new(new_handle, Ty::Array(inner.clone())))
        }

        Ty::String | Ty::Object => Ok(TypedValue::new(
            clone_string_value(fn_ctx, val)?,
            val.ntsc_type.clone(),
        )),
        // A view borrows its value: materialize an independent owned copy
        // from the inner type. Aliasing the borrowed handle would let the
        // copy's drop reclaim the original out from under its owner.
        Ty::View(inner, _) => {
            let inner = (**inner).clone();
            copy_owned_value(fn_ctx, &TypedValue::new(value, inner))
        }
        _ => Ok(val.clone()),
    }
}

/// Record another live copy of a channel handle: every owning slot, future
/// field, and parameter holds one reference. `chan.new(...)` hands over the
/// first reference, so a fresh channel is adopted without retaining.
pub(crate) fn retain_chan_value<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    expr: &Expr,
    val: &TypedValue<'ctx>,
) -> Result<(), crate::CodegenError> {
    if !matches!(val.ntsc_type, Ty::Chan(_)) || expr_is_fresh(fn_ctx, expr, val) {
        return Ok(());
    }
    let retain_fn = fn_ctx
        .module
        .get_function("ntask_chan_retain")
        .ok_or_else(|| crate::CodegenError::LLVMError("ntask_chan_retain not declared".into()))?;
    fn_ctx.builder.build_call(
        retain_fn,
        &[BasicMetadataValueEnum::IntValue(val.value.into_int_value())],
        "chan_retain",
    )?;
    Ok(())
}

pub(crate) fn expr_is_string_literal(expr: &Expr) -> bool {
    match expr {
        Expr::Literal {
            value: ntsc_ast::expr::LiteralValue::String(_),
            ..
        } => true,
        Expr::Grouping { expression, .. } => expr_is_string_literal(expression),
        _ => false,
    }
}

pub(crate) fn clone_string_value<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    val: &TypedValue<'ctx>,
) -> Result<BasicValueEnum<'ctx>, crate::CodegenError> {
    let clone_fn = fn_ctx
        .module
        .get_function("ntsc_string_clone")
        .ok_or_else(|| crate::CodegenError::LLVMError("ntsc_string_clone not declared".into()))?;
    let cloned = fn_ctx.builder.build_call(
        clone_fn,
        &[inkwell::values::BasicMetadataValueEnum::IntValue(
            val.value.into_int_value(),
        )],
        "string_store_copy",
    )?;
    Ok(cloned.try_as_basic_value().unwrap_basic())
}

/// Byte size of an option cell for `inner`: a narrow integer payload
/// (`option[bool]` → `i1`) is packed; everything else reserves 8 bytes.
pub(crate) fn option_cell_size(inner_llvm: inkwell::types::BasicTypeEnum<'_>) -> i64 {
    match inner_llvm {
        inkwell::types::BasicTypeEnum::IntType(i) => i64::from(i.get_bit_width().div_ceil(8)),
        inkwell::types::BasicTypeEnum::FloatType(_) => 8,
        _ => 8,
    }
}

pub(crate) fn option_cell_pointer<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    val: BasicValueEnum<'ctx>,
) -> Result<PointerValue<'ctx>, crate::CodegenError> {
    if val.is_pointer_value() {
        Ok(val.into_pointer_value())
    } else {
        Ok(fn_ctx.builder.build_int_to_ptr(
            val.into_int_value(),
            fn_ctx.context.ptr_type(AddressSpace::default()),
            "option_cell",
        )?)
    }
}

pub(crate) fn allocate_option_cell<'ctx>(
    fn_ctx: &FunctionContext<'ctx, '_>,
    inner_llvm: inkwell::types::BasicTypeEnum<'ctx>,
) -> Result<PointerValue<'ctx>, crate::CodegenError> {
    let alloc_fn = fn_ctx
        .module
        .get_function("malloc")
        .ok_or_else(|| crate::CodegenError::LLVMError("malloc not declared".into()))?;
    let cell = fn_ctx
        .builder
        .build_call(
            alloc_fn,
            &[BasicMetadataValueEnum::IntValue(
                fn_ctx
                    .context
                    .i64_type()
                    .const_int(option_cell_size(inner_llvm) as u64, false),
            )],
            "option_box",
        )?
        .try_as_basic_value()
        .unwrap_basic()
        .into_pointer_value();
    Ok(cell)
}

pub(crate) fn box_option_value<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    inner: &Ty,
    expr: &Expr,
    val: &TypedValue<'ctx>,
) -> Result<PointerValue<'ctx>, crate::CodegenError> {
    let inner_llvm = ty_to_llvm(inner, fn_ctx.context);
    let cell = allocate_option_cell(fn_ctx, inner_llvm)?;

    let inner_val = if matches!(*inner, Ty::String) && expr_is_string_literal(expr) {
        clone_string_value(fn_ctx, val)?
    } else {
        val.value
    };
    fn_ctx.builder.build_store(cell, inner_val)?;
    Ok(cell)
}

pub(crate) fn clone_option_value<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    inner: &Ty,
    val: &TypedValue<'ctx>,
) -> Result<PointerValue<'ctx>, crate::CodegenError> {
    let source = TypedValue::new(val.value, Ty::Option(Box::new(inner.clone())));
    let copied = emit_copy_option_value(fn_ctx, inner, source)?;
    Ok(copied.value.into_pointer_value())
}

pub(crate) fn emit_drop_option_value<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    inner: &Ty,
    cell: PointerValue<'ctx>,
) -> Result<(), crate::CodegenError> {
    let inner_llvm = ty_to_llvm(inner, fn_ctx.context);

    let is_null = fn_ctx.builder.build_is_null(cell, "opt_drop_null")?;
    let current_fn = fn_ctx.function;
    let body_bb = fn_ctx
        .context
        .append_basic_block(current_fn, "opt_drop.body");
    let done_bb = fn_ctx
        .context
        .append_basic_block(current_fn, "opt_drop.done");
    fn_ctx
        .builder
        .build_conditional_branch(is_null, done_bb, body_bb)?;
    fn_ctx.builder.position_at_end(body_bb);

    let loaded = fn_ctx
        .builder
        .build_load(inner_llvm, cell, "option_inner")?;
    let inner_ty = (*inner).clone();
    if matches!(
        inner_ty,
        Ty::String | Ty::Array(_) | Ty::Shared(_) | Ty::Class(_) | Ty::Option(_)
    ) {
        emit_drop_value(fn_ctx, &TypedValue::new(loaded, inner_ty))?;
    }
    let free_fn = fn_ctx
        .module
        .get_function("free")
        .ok_or_else(|| crate::CodegenError::LLVMError("free not declared".into()))?;
    fn_ctx.builder.build_call(
        free_fn,
        &[BasicMetadataValueEnum::PointerValue(cell)],
        "option_free",
    )?;
    fn_ctx.builder.build_unconditional_branch(done_bb)?;
    fn_ctx.builder.position_at_end(done_bb);
    Ok(())
}

/// Store a value into an owned slot, boxing (shared), celling (option), or
/// cloning as needed, and returning whether the slot now owns a value that
/// must be dropped later.
/// The i64 value a leak mark should attribute, when one exists.
///
/// Registry-backed values arrive as raw `i64` handles. `memory.alloc`
/// regions are registered as `Handle::Memory` ids, so they are marked too;
/// anything that is not an i64 (a raw malloc'd cell) has no registry entry
/// and therefore nothing for the leak report to resolve.
pub(crate) fn int_like_leak_value<'ctx>(
    fn_ctx: &FunctionContext<'ctx, '_>,
    val: &TypedValue<'ctx>,
) -> Option<inkwell::values::IntValue<'ctx>> {
    if val.value.is_int_value() {
        return Some(val.value.into_int_value());
    }
    if val.value.is_pointer_value() {
        let id = fn_ctx
            .builder
            .build_ptr_to_int(
                val.value.into_pointer_value(),
                fn_ctx.context.i64_type(),
                "leak_id",
            )
            .ok()?;
        return Some(id);
    }
    None
}

pub(crate) fn store_into_owned_slot<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    ptr: PointerValue<'ctx>,
    ty: &Ty,
    expr: &Expr,
    val: &TypedValue<'ctx>,
) -> Result<bool, crate::CodegenError> {
    if matches!(ty, Ty::Shared(_)) {
        let boxed = box_or_retain_shared(fn_ctx, ty, expr, val)?;
        fn_ctx.builder.build_store(ptr, boxed.value)?;
        if let Expr::Variable { name: source } = expr
            && !matches!(val.ntsc_type, Ty::Shared(_))
        {
            fn_ctx.null_var_slot(source.lexeme());
        }
        return Ok(true);
    }

    // A `dyn` slot always owns its fat pointer: a fresh coercion or a move
    // from another dyn variable (whose slot is then nulled).
    if matches!(ty, Ty::Dyn(_)) {
        fn_ctx.builder.build_store(ptr, val.value)?;
        if let Expr::Variable { name: source } = expr {
            fn_ctx.null_var_slot(source.lexeme());
        }
        return Ok(true);
    }

    if let Ty::Option(inner) = ty {
        if matches!(val.ntsc_type, Ty::Nil) {
            fn_ctx.builder.build_store(ptr, val.value)?;
            return Ok(false);
        }
        let cell = if matches!(val.ntsc_type, Ty::Option(_)) {
            if expr_is_fresh(fn_ctx, expr, val) {
                option_cell_pointer(fn_ctx, val.value)?
            } else {
                clone_option_value(fn_ctx, inner, val)?
            }
        } else {
            box_option_value(fn_ctx, inner, expr, val)?
        };
        fn_ctx.builder.build_store(ptr, cell)?;
        if let Expr::Variable { name: source } = expr
            && !matches!(val.ntsc_type, Ty::Option(_) | Ty::Nil)
        {
            fn_ctx.null_var_slot(source.lexeme());
        }
        return Ok(true);
    }

    // A result slot owns its cell: a fresh constructor result is adopted,
    // anything else is deep-copied into a fresh cell of this slot's shape.
    if let Ty::Result { ok, err } = ty {
        let handle = match &val.ntsc_type {
            Ty::Result {
                ok: src_ok,
                err: src_err,
            } if **src_ok == **ok && **src_err == **err => {
                if expr_is_fresh(fn_ctx, expr, val) {
                    val.value
                } else {
                    super::result_cell::emit_copy_result_value(fn_ctx, ok, err, val)?.value
                }
            }
            // A differently-shaped or non-result value cannot be adopted as
            // is; rebuild a cell around the value on the Ok side.
            _ => {
                super::result_cell::box_result_value(
                    fn_ctx,
                    ok,
                    err,
                    expr,
                    &TypedValue::new(val.value, (**ok).clone()),
                    true,
                )?
                .value
            }
        };
        fn_ctx.builder.build_store(ptr, handle)?;
        if let Expr::Variable { name: source } = expr
            && !matches!(val.ntsc_type, Ty::Result { .. })
        {
            fn_ctx.null_var_slot(source.lexeme());
        }
        return Ok(true);
    }
    // A `chan` slot holds one reference, taken before the slot's previous one
    // is released so a redeclaration reading its own slot is safe.
    if matches!(ty, Ty::Chan(_)) {
        retain_chan_value(fn_ctx, expr, val)?;
        emit_drop_replaced_value(fn_ctx, ptr, ty, val)?;
        fn_ctx.builder.build_store(ptr, val.value)?;
        return Ok(true);
    }

    let mut owned = expr_is_owned(fn_ctx, expr, val);
    let is_str_lit = expr_is_string_literal(expr);
    if is_str_lit && matches!(ty, Ty::String) {
        let cloned = clone_string_value(fn_ctx, val)?;
        fn_ctx.builder.build_store(ptr, cloned)?;
        owned = true;
    } else {
        fn_ctx.builder.build_store(ptr, val.value)?;
    }

    if owned && let Expr::Variable { name } = expr {
        fn_ctx.null_var_slot(name.lexeme());
    }
    Ok(owned)
}
