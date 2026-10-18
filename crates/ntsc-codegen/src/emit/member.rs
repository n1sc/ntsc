//! Field stores, constructors, and member access (GEP resolution).

use super::*;

/// Store `val` (emitted from `value`) into the field `gep` points at,
/// applying the field's ownership rules — the same rules as variable slots:
/// a shared field stores a box (retain or adopt), a string field
/// heap-copies a literal, an option field stores a boxed cell, and a bare
/// owned variable moves out of its slot (nulled) so its exit-time drop
/// cannot free the value that now lives in the field. Shared by field
/// assignment and construction-time field initialization.
pub(crate) fn store_into_field<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    gep: &GepResult<'ctx>,
    value: &Expr,
    val: &TypedValue<'ctx>,
) -> Result<(), crate::CodegenError> {
    let field_ty = gep.field_ty.clone();

    if matches!(field_ty, Ty::Shared(_)) {
        let boxed = box_or_retain_shared(fn_ctx, &field_ty, value, val)?;

        // Release the box the field held, or reassignment leaks a
        // reference. `box_or_retain_shared` has already retained the
        // replacement, so assigning the same box back cannot drive its
        // count to zero here.
        emit_drop_slot_value(fn_ctx, gep.ptr, &field_ty)?;
        fn_ctx.builder.build_store(gep.ptr, boxed.value)?;
        if let Expr::Variable { name } = value
            && fn_ctx.owned_slots.contains(name.lexeme())
            && !matches!(val.ntsc_type, Ty::Shared(_))
        {
            fn_ctx.null_var_slot(name.lexeme());
        }
        return Ok(());
    }

    if let Ty::Option(inner) = &field_ty {
        // Reclaim the cell the field currently holds so reassignment does
        // not leak it. A field of a freshly-constructed instance is
        // zero-initialized, so the drop sees a null cell and is a no-op.
        // Storing a bare value here instead of a cell would leave the field
        // pointing at a non-cell, and the class drop thunk would then load
        // through it and free garbage.
        emit_drop_slot_value(fn_ctx, gep.ptr, &field_ty)?;
        let cell = if matches!(val.ntsc_type, Ty::Nil) {
            fn_ctx
                .context
                .ptr_type(AddressSpace::default())
                .const_null()
        } else if matches!(val.ntsc_type, Ty::Option(_)) {
            // A fresh option temp already owns its cell; adopt it rather
            // than allocating a second one and orphaning it.
            if expr_is_fresh(fn_ctx, value, val) {
                val.value.into_pointer_value()
            } else {
                clone_option_value(fn_ctx, inner, val)?
            }
        } else {
            box_option_value(fn_ctx, inner, value, val)?
        };
        fn_ctx.builder.build_store(gep.ptr, cell)?;
        if let Expr::Variable { name: source } = value
            && !matches!(val.ntsc_type, Ty::Option(_) | Ty::Nil)
        {
            fn_ctx.null_var_slot(source.lexeme());
        }
        return Ok(());
    }
    let is_str_lit = expr_is_string_literal(value);
    let owned_new = expr_is_owned(fn_ctx, value, val);
    correct_empty_array_flag(fn_ctx, value, val, &field_ty)?;

    // The field owns its value, so a string literal is cloned in rather
    // than shared with the literal's constant handle.
    let stored = if is_str_lit && matches!(field_ty, Ty::String) {
        TypedValue::new(clone_string_value(fn_ctx, val)?, Ty::String)
    } else {
        val.clone()
    };

    if matches!(field_ty, Ty::String | Ty::Array(_) | Ty::Class(_)) {
        // Reclaim what the field held before the store overwrites it: once
        // the field points elsewhere, nothing can reach the old value
        // again. A freshly-constructed instance zero-initializes its
        // fields, so the first assignment drops a null handle; the
        // identity guard skips self-assignment (`b.items = b.items`).
        // Class instances are single-owner like arrays and strings: a plain
        // read moves them (nulling the source), so a live field holds the
        // only reference and its owned heap fields must be reclaimed here
        // rather than orphaned when the field is repointed.
        emit_drop_replaced_value(fn_ctx, gep.ptr, &field_ty, &stored)?;
    }
    fn_ctx.builder.build_store(gep.ptr, stored.value)?;
    if owned_new && let Expr::Variable { name: source } = value {
        fn_ctx.null_var_slot(source.lexeme());
    }
    Ok(())
}

/// A resolved field location: the GEP'd pointer plus the field's static
/// type, so ownership rules can be applied without re-deriving it.
pub(crate) struct GepResult<'ctx> {
    pub(crate) ptr: PointerValue<'ctx>,
    pub(crate) field_ty: Ty,
}

/// Emit the fallback `exception.cleanup` path that reclaims the partially
/// constructed fields when a constructor throws mid-initialization.
pub(crate) fn emit_partial_construction_cleanup<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    class_name: &str,
    obj_ptr: PointerValue<'ctx>,
) -> Result<(), crate::CodegenError> {
    if !fn_ctx.exception_checks {
        return Ok(());
    }
    let handler = fn_ctx.current_exception_handler();
    let pending_fn = fn_ctx
        .module
        .get_function("ntsc_exception_pending")
        .ok_or_else(|| {
            crate::CodegenError::LLVMError("ntsc_exception_pending not declared".into())
        })?;
    let pending = fn_ctx
        .builder
        .build_call(pending_fn, &[], "ctor_exc_pending")?
        .try_as_basic_value()
        .unwrap_basic()
        .into_int_value();
    let active = fn_ctx.builder.build_int_compare(
        IntPredicate::NE,
        pending,
        fn_ctx.context.i8_type().const_zero(),
        "ctor_exc_active",
    )?;
    let unwind_bb = fn_ctx
        .context
        .append_basic_block(fn_ctx.function, "ctor.unwind");
    let continue_bb = fn_ctx
        .context
        .append_basic_block(fn_ctx.function, "ctor.continue");
    fn_ctx
        .builder
        .build_conditional_branch(active, unwind_bb, continue_bb)?;

    fn_ctx.builder.position_at_end(unwind_bb);
    emit_drop_value(
        fn_ctx,
        &TypedValue::new(obj_ptr.into(), Ty::Class(class_name.to_string())),
    )?;
    fn_ctx.builder.build_unconditional_branch(handler)?;

    fn_ctx.builder.position_at_end(continue_bb);
    Ok(())
}

pub(crate) fn emit_declared_field_initializers<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    struct_ty: inkwell::types::StructType<'ctx>,
    class_name: &str,
    obj_ptr: PointerValue<'ctx>,
) -> Result<HashSet<usize>, crate::CodegenError> {
    let mut initialized = HashSet::new();
    let field_names = class_all_fields(class_name);
    let field_tys = class_all_field_types(class_name);
    for (idx, init) in class_all_field_inits(class_name).into_iter().enumerate() {
        let Some(init) = init else { continue };
        let Some(field_ty) = field_tys.get(idx) else {
            continue;
        };
        let value = emit_expression(fn_ctx, &init)?;
        let field_ptr = fn_ctx.builder.build_struct_gep(
            struct_ty,
            obj_ptr,
            idx as u32,
            &format!(
                "field_{}_init_ptr",
                field_names.get(idx).map_or("x", String::as_str)
            ),
        )?;
        let gep = GepResult {
            ptr: field_ptr,
            field_ty: field_ty.clone(),
        };
        store_into_field(fn_ctx, &gep, &init, &value)?;
        initialized.insert(idx);
    }
    Ok(initialized)
}

pub(crate) fn emit_class_constructor<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    struct_ty: inkwell::types::StructType<'ctx>,
    class_name: &str,
    arguments: &[Expr],
    arg_values: &[TypedValue<'ctx>],
    slot: Option<PointerValue<'ctx>>,
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    let size = struct_ty.size_of().ok_or_else(|| {
        crate::CodegenError::LLVMError(format!("class `{class_name}` is unsized"))
    })?;

    let obj_ptr = match slot {
        Some(slot) => slot,
        None => {
            let alloc_fn = fn_ctx
                .module
                .get_function("malloc")
                .ok_or_else(|| crate::CodegenError::LLVMError("malloc not declared".into()))?;
            let alloc_result =
                fn_ctx
                    .builder
                    .build_call(alloc_fn, &[size.into()], "class_alloc")?;
            let raw = call_result_to_value(fn_ctx, &alloc_result).into_pointer_value();
            fn_ctx.builder.build_pointer_cast(
                raw,
                fn_ctx.context.ptr_type(AddressSpace::default()),
                "class_ptr",
            )?
        }
    };

    let zero = fn_ctx.context.i8_type().const_zero();
    fn_ctx.builder.build_memset(obj_ptr, 1, zero, size)?;

    let initialized = emit_declared_field_initializers(fn_ctx, struct_ty, class_name, obj_ptr)?;

    let init_name = format!("{class_name}.init");
    if let Some(init_fn) = fn_ctx.module.get_function(&init_name) {
        let param_tys = init_fn.get_type().get_param_types();

        let declared = class_method_declared_param_types(class_name, "init");
        let prepared = prepare_call_args(fn_ctx, arguments, arg_values, &declared)?;
        let mut llvm_args = vec![BasicMetadataValueEnum::PointerValue(obj_ptr)];
        for (arg_val, param_ty) in prepared.iter().zip(param_tys.iter().skip(1)) {
            llvm_args.push(coerce_value_to_llvm(fn_ctx, arg_val.clone(), param_ty)?);
        }
        fn_ctx
            .builder
            .build_call(init_fn, &llvm_args, "class_init")?;
        emit_drop_borrowed_fresh_args(fn_ctx, arguments, arg_values, &declared)?;
        emit_partial_construction_cleanup(fn_ctx, class_name, obj_ptr)?;
    } else if !arguments.is_empty() {
        return Err(crate::CodegenError::LLVMError(format!(
            "class `{class_name}` has no `init` method but was constructed with arguments"
        )));
    } else {
        let field_names = class_all_fields(class_name);
        let field_tys = class_all_field_types(class_name);
        for (idx, (field_name, field_ty)) in field_names.iter().zip(field_tys.iter()).enumerate() {
            if initialized.contains(&idx) {
                continue;
            }
            let default = match field_ty {
                Ty::Array(elem) => {
                    let elem_size = if matches!(**elem, Ty::Bool) {
                        fn_ctx.context.i64_type().const_int(1, false)
                    } else {
                        fn_ctx.context.i64_type().const_int(8, false)
                    };
                    let string_elems = matches!(**elem, Ty::String | Ty::Any);
                    let arr_fn = fn_ctx
                        .module
                        .get_function("ntsc_array_new_typed")
                        .ok_or_else(|| {
                            crate::CodegenError::LLVMError(
                                "ntsc_array_new_typed not declared".into(),
                            )
                        })?;
                    let res = fn_ctx.builder.build_call(
                        arr_fn,
                        &[
                            elem_size.into(),
                            fn_ctx.context.i64_type().const_int(8, false).into(),
                            fn_ctx
                                .context
                                .i8_type()
                                .const_int(u64::from(string_elems), false)
                                .into(),
                        ],
                        "field_default_array",
                    )?;
                    call_result_to_value(fn_ctx, &res)
                }
                Ty::String => {
                    let global = emit_string_const(fn_ctx, "")?;
                    clone_string_value(fn_ctx, &TypedValue::new(global, Ty::String))?
                }
                _ => continue,
            };
            let field_ptr = fn_ctx.builder.build_struct_gep(
                struct_ty,
                obj_ptr,
                idx as u32,
                &format!("field_{field_name}_default_ptr"),
            )?;
            fn_ctx.builder.build_store(field_ptr, default)?;
        }
    }

    Ok(TypedValue::new(
        obj_ptr.into(),
        Ty::Class(class_name.to_string()),
    ))
}

pub(crate) fn emit_member_gep<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    obj_val: &TypedValue<'ctx>,
    property: &ntsc_ast::token::Token,
) -> Result<Option<GepResult<'ctx>>, crate::CodegenError> {
    let class_name = match &obj_val.ntsc_type {
        Ty::Class(name) => name.clone(),

        Ty::View(inner, _) if matches!(**inner, Ty::Class(_)) => {
            if let Ty::Class(name) = &**inner {
                name.clone()
            } else {
                return Ok(None);
            }
        }

        // An owning allocation and a reference both hold the instance
        // address, so a field reaches through them unchanged.
        Ty::Own(inner) | Ty::Ref(inner, _) => {
            if let Ty::Class(name) = &**inner {
                name.clone()
            } else {
                return Ok(None);
            }
        }
        _ => return Ok(None),
    };
    let field_name = property.lexeme().to_string();

    let field_idx = class_field_index(&class_name, &field_name);

    let idx = match field_idx {
        Some(i) => i,
        None => return Ok(None),
    };

    let struct_ty = fn_ctx.module.get_struct_type(&class_name).ok_or_else(|| {
        crate::CodegenError::LLVMError(format!("struct type `{class_name}` not found"))
    })?;

    let obj_ptr = if obj_val.value.is_pointer_value() {
        obj_val.value.into_pointer_value()
    } else {
        return Ok(None);
    };

    let gep = fn_ctx
        .builder
        .build_struct_gep(struct_ty, obj_ptr, idx as u32, "field_ptr")?;

    let field_ty = class_all_field_types(&class_name)
        .get(idx)
        .cloned()
        .unwrap_or_else(|| {
            struct_ty
                .get_field_type_at_index(idx as u32)
                .map(|bt| llvm_to_ty(bt))
                .unwrap_or(Ty::Any)
        });

    Ok(Some(GepResult { ptr: gep, field_ty }))
}

pub(crate) fn llvm_to_ty(bt: inkwell::types::BasicTypeEnum<'_>) -> Ty {
    match bt {
        inkwell::types::BasicTypeEnum::IntType(i) if i.get_bit_width() == 1 => Ty::Bool,
        inkwell::types::BasicTypeEnum::IntType(i) if i.get_bit_width() == 64 => Ty::Int,
        inkwell::types::BasicTypeEnum::FloatType(_) => Ty::Float,
        inkwell::types::BasicTypeEnum::PointerType(_) => Ty::String,
        _ => Ty::Any,
    }
}

pub(crate) fn llvm_ret_ty_to_ty(bt: inkwell::types::BasicTypeEnum<'_>) -> Ty {
    match bt {
        inkwell::types::BasicTypeEnum::IntType(i) if i.get_bit_width() == 1 => Ty::Bool,
        inkwell::types::BasicTypeEnum::IntType(i) if i.get_bit_width() == 64 => Ty::Int,
        inkwell::types::BasicTypeEnum::FloatType(_) => Ty::Float,
        inkwell::types::BasicTypeEnum::PointerType(_) => Ty::String,
        _ => Ty::Any,
    }
}

pub(crate) fn emit_member_access<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    obj_val: &TypedValue<'ctx>,
    property: &ntsc_ast::token::Token,
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    if let Ty::Class(name) = &obj_val.ntsc_type {
        let value = ENUM_VALUES.with(|map| {
            map.borrow()
                .get(name)
                .and_then(|members| members.get(property.lexeme()))
                .copied()
        });
        if let Some(value) = value {
            return Ok(TypedValue::new(
                fn_ctx
                    .context
                    .i64_type()
                    .const_int(value as u64, false)
                    .into(),
                Ty::Int,
            ));
        }
    }
    if obj_val.ntsc_type == Ty::Object
        || matches!(&obj_val.ntsc_type, Ty::View(inner, _) if **inner == Ty::Object)
    {
        let get_fn = fn_ctx
            .module
            .get_function("ntsc_json_get")
            .ok_or_else(|| crate::CodegenError::LLVMError("ntsc_json_get not declared".into()))?;
        let key = emit_string_const(fn_ctx, property.lexeme())?;
        let result = fn_ctx.builder.build_call(
            get_fn,
            &[
                BasicMetadataValueEnum::IntValue(obj_val.value.into_int_value()),
                BasicMetadataValueEnum::IntValue(key.into_int_value()),
            ],
            "json_get",
        )?;
        let val = call_result_to_value(fn_ctx, &result);
        return Ok(TypedValue::new(val, Ty::String));
    }
    if let Some(gep) = emit_member_gep(fn_ctx, obj_val, property)? {
        let llvm_ty = ty_to_llvm(&gep.field_ty, fn_ctx.context);
        let val = fn_ctx.builder.build_load(llvm_ty, gep.ptr, "field_load")?;
        let tv = TypedValue::new(val, gep.field_ty);
        Ok(tv)
    } else {
        Ok(TypedValue::new(
            fn_ctx
                .context
                .ptr_type(AddressSpace::default())
                .const_null()
                .into(),
            Ty::Any,
        ))
    }
}
