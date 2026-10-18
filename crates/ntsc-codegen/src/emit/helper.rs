//! Shared helpers: to-string conversion, coercion, and `say`.

use super::*;

pub(crate) fn emit_say_call<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    arg: &Expr,
) -> Result<(), crate::CodegenError> {
    let arg_val = emit_expression(fn_ctx, arg)?;
    let string_val = convert_to_string(fn_ctx, &arg_val)?;

    let say_fn = fn_ctx
        .module
        .get_function("ntsc_say")
        .ok_or_else(|| crate::CodegenError::LLVMError("ntsc_say not declared".into()))?;

    let handle = string_val.value.into_int_value();
    fn_ctx.builder.build_call(
        say_fn,
        &[BasicMetadataValueEnum::IntValue(handle)],
        "say_call",
    )?;

    // `say` borrows its argument, so a fresh array literal passed directly
    // has no other owner once the call completes.
    if expr_is_fresh(fn_ctx, arg, &arg_val) {
        emit_drop_value(fn_ctx, &arg_val)?;
    }
    // A converted scalar (`say(some_bool)`) produced a fresh string that
    // nothing else owns.
    if arg_val.ntsc_type != Ty::String {
        emit_drop_value(fn_ctx, &string_val)?;
    }

    Ok(())
}

// ── Helper: convert value to string ─────────────────────────────────────

/// Emit a reference to a cached string handle (borrowed constant text).
///
/// Interning happens at compile time in the runtime library, so the handle
/// references immutable global storage and must never be dropped.
pub(crate) fn emit_string_const<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    text: &str,
) -> Result<BasicValueEnum<'ctx>, crate::CodegenError> {
    emit_string_handle(fn_ctx, text)
}

/// Concatenate two string handles via the runtime.
pub(crate) fn emit_concat<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    left: BasicValueEnum<'ctx>,
    right: BasicValueEnum<'ctx>,
) -> Result<BasicValueEnum<'ctx>, crate::CodegenError> {
    let concat_fn = fn_ctx
        .module
        .get_function("ntsc_string_concat")
        .ok_or_else(|| crate::CodegenError::LLVMError("ntsc_string_concat not declared".into()))?;
    let result =
        fn_ctx
            .builder
            .build_call(concat_fn, &[left.into(), right.into()], "json_concat")?;
    Ok(call_result_to_value(fn_ctx, &result))
}

/// Convert a runtime value into its JSON string representation:
/// quoted/escaped for strings, plain text for scalars, `null` otherwise.
/// The second element of the pair reports whether the returned handle was
/// freshly allocated and so has to be dropped once consumed — the `null`
/// fallback is an interned literal handle that must never be dropped.
pub(crate) fn emit_json_value<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    val: &TypedValue<'ctx>,
) -> Result<(BasicValueEnum<'ctx>, bool), crate::CodegenError> {
    match &val.ntsc_type {
        Ty::String => {
            let escape_fn = fn_ctx
                .module
                .get_function("ntsc_json_escape_string")
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError("ntsc_json_escape_string not declared".into())
                })?;
            let result =
                fn_ctx
                    .builder
                    .build_call(escape_fn, &[val.value.into()], "json_escape")?;
            Ok((call_result_to_value(fn_ctx, &result), true))
        }
        Ty::Int | Ty::Float | Ty::Bool => {
            let s = convert_to_string(fn_ctx, val)?;
            Ok((s.value, true))
        }
        _ => Ok((emit_string_const(fn_ctx, "null")?, false)),
    }
}

/// Convert a value into a fresh-owned string handle: strings pass through,
/// scalars are rendered by their runtime conversion function. This is the
/// path behind `say` and implicit string formatting.
pub(crate) fn convert_to_string<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    val: &TypedValue<'ctx>,
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    match &val.ntsc_type {
        Ty::String => Ok(TypedValue::new(val.value, Ty::String)),
        Ty::Int => {
            let convert_fn = fn_ctx
                .module
                .get_function("ntsc_i64_to_string")
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError("ntsc_i64_to_string not declared".into())
                })?;
            let result = fn_ctx.builder.build_call(
                convert_fn,
                &[BasicMetadataValueEnum::IntValue(val.value.into_int_value())],
                "int_to_str",
            )?;
            let val = call_result_to_value(fn_ctx, &result);
            Ok(TypedValue::new(val, Ty::String))
        }
        Ty::Float => {
            let convert_fn = fn_ctx
                .module
                .get_function("ntsc_f64_to_string")
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError("ntsc_f64_to_string not declared".into())
                })?;
            let result = fn_ctx.builder.build_call(
                convert_fn,
                &[BasicMetadataValueEnum::FloatValue(
                    val.value.into_float_value(),
                )],
                "float_to_str",
            )?;
            let val = call_result_to_value(fn_ctx, &result);
            Ok(TypedValue::new(val, Ty::String))
        }
        Ty::Bool => {
            let convert_fn = fn_ctx
                .module
                .get_function("ntsc_bool_to_string")
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError("ntsc_bool_to_string not declared".into())
                })?;
            let i1_val = val.value.into_int_value();
            let i8_val = if i1_val.get_type().get_bit_width() == 8 {
                i1_val
            } else {
                fn_ctx
                    .builder
                    .build_int_z_extend(i1_val, fn_ctx.context.i8_type(), "bool_ext")?
            };
            let result = fn_ctx.builder.build_call(
                convert_fn,
                &[BasicMetadataValueEnum::IntValue(i8_val)],
                "bool_to_str",
            )?;
            let val = call_result_to_value(fn_ctx, &result);
            Ok(TypedValue::new(val, Ty::String))
        }
        _ => Ok(TypedValue::new(val.value, Ty::String)),
    }
}

// ── Helper: coerce value to expected type ───────────────────────────────

pub(crate) fn normalize_self<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    val: TypedValue<'ctx>,
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    if val.ntsc_type == Ty::Bool && val.value.is_int_value() {
        let int = val.value.into_int_value();
        if int.get_type().get_bit_width() != 1 {
            let truncated =
                fn_ctx
                    .builder
                    .build_int_truncate(int, fn_ctx.context.bool_type(), "bool_norm")?;
            return Ok(TypedValue::new(truncated.into(), Ty::Bool));
        }
    }
    Ok(val)
}

pub(crate) fn peel_view<'ctx>(val: TypedValue<'ctx>) -> TypedValue<'ctx> {
    match &val.ntsc_type {
        Ty::View(inner, _) => TypedValue::new(val.value, (**inner).clone()),
        _ => val,
    }
}

pub(crate) fn correct_empty_array_flag<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    value: &Expr,
    val: &TypedValue<'ctx>,
    dest_ty: &Ty,
) -> Result<(), crate::CodegenError> {
    if !matches!(value, Expr::ArrayLiteral { elements, .. } if elements.is_empty()) {
        return Ok(());
    }

    let inner = match dest_ty {
        Ty::Array(inner) => Some(inner),
        Ty::Shared(inner) => match &**inner {
            Ty::Array(inner_arr) => Some(inner_arr),
            _ => None,
        },
        _ => None,
    };
    if let Some(inner) = inner {
        let set_flag_fn = fn_ctx
            .module
            .get_function("ntsc_array_set_string_elements")
            .ok_or_else(|| {
                crate::CodegenError::LLVMError("ntsc_array_set_string_elements not declared".into())
            })?;
        let string_elems = matches!(**inner, Ty::String | Ty::Any);
        let flag = fn_ctx
            .context
            .i8_type()
            .const_int(u64::from(string_elems), false);
        fn_ctx.builder.build_call(
            set_flag_fn,
            &[
                BasicMetadataValueEnum::IntValue(val.value.into_int_value()),
                BasicMetadataValueEnum::IntValue(flag),
            ],
            "set_array_string_elements",
        )?;
    }
    Ok(())
}

pub(crate) fn coerce_value<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    val: TypedValue<'ctx>,
    target: &Ty,
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    let val = normalize_self(fn_ctx, val)?;
    if val.ntsc_type == *target {
        return Ok(val);
    }
    let builder = fn_ctx.builder;
    let ctx = fn_ctx.context;
    match (&val.ntsc_type, target) {
        (Ty::Bool, Ty::Int) => {
            let widened = builder.build_int_z_extend(
                val.value.into_int_value(),
                ctx.i64_type(),
                "bool_to_int",
            )?;
            Ok(TypedValue::new(widened.into(), Ty::Int))
        }
        (Ty::Int, Ty::Bool) => {
            let is_nonzero = builder.build_int_compare(
                IntPredicate::NE,
                val.value.into_int_value(),
                ctx.i64_type().const_zero(),
                "int_to_bool",
            )?;
            Ok(TypedValue::new(is_nonzero.into(), Ty::Bool))
        }
        (Ty::Bool | Ty::Int, Ty::Float) => {
            let int_val = if val.ntsc_type == Ty::Bool {
                builder.build_int_z_extend(
                    val.value.into_int_value(),
                    ctx.i64_type(),
                    "bool_to_int",
                )?
            } else {
                val.value.into_int_value()
            };
            let f = builder.build_signed_int_to_float(int_val, ctx.f64_type(), "int_to_float")?;
            Ok(TypedValue::new(f.into(), Ty::Float))
        }
        (Ty::Float, Ty::Int) => {
            let f = val.value.into_float_value();
            let i64_ty = ctx.i64_type();
            let two_63 = ctx.f64_type().const_float(9223372036854775808.0);
            let neg_two_63 = ctx.f64_type().const_float(-9223372036854775808.0);
            let is_nan =
                builder.build_float_compare(inkwell::FloatPredicate::UNO, f, f, "float_is_nan")?;
            let too_big = builder.build_float_compare(
                inkwell::FloatPredicate::UGE,
                f,
                two_63,
                "float_too_big",
            )?;
            let too_small = builder.build_float_compare(
                inkwell::FloatPredicate::OLT,
                f,
                neg_two_63,
                "float_too_small",
            )?;
            let raw = builder.build_float_to_signed_int(f, i64_ty, "float_to_int")?;
            let saturated = builder.build_select(
                too_small,
                i64_ty.const_int(i64::MIN as u64, false),
                raw,
                "sat_min",
            )?;
            let saturated = saturated.into_int_value();
            let saturated = builder.build_select(
                too_big,
                i64_ty.const_int(i64::MAX as u64, false),
                saturated,
                "sat_max",
            )?;
            let saturated = saturated.into_int_value();
            let saturated =
                builder.build_select(is_nan, i64_ty.const_zero(), saturated, "sat_nan")?;
            Ok(TypedValue::new(saturated, Ty::Int))
        }

        // A class instance becomes an owning fat pointer. An `own dyn`
        // target additionally boxes the header into a cell, matching how
        // every other `own` handle is stored.
        (Ty::Class(_), Ty::Dyn(trait_name)) => {
            super::dyn_obj::emit_dyn_coercion(fn_ctx, val, trait_name)
        }
        (Ty::Own(inner), Ty::Dyn(trait_name)) if matches!(inner.as_ref(), Ty::Class(_)) => {
            let coerced = super::dyn_obj::emit_dyn_coercion(fn_ctx, val, trait_name)?;
            emit_box_value(fn_ctx, coerced)
        }
        (Ty::Class(_) | Ty::Own(_) | Ty::Dyn(_), Ty::Own(inner))
            if matches!(inner.as_ref(), Ty::Dyn(_)) =>
        {
            let trait_name = match inner.as_ref() {
                Ty::Dyn(trait_name) => trait_name.clone(),
                _ => return Ok(val),
            };
            let coerced = match &val.ntsc_type {
                Ty::Dyn(_) => val,
                _ => super::dyn_obj::emit_dyn_coercion(fn_ctx, val, &trait_name)?,
            };
            emit_box_value(fn_ctx, coerced)
        }
        _ => Ok(val),
    }
}

/// Loads the value an owning cell holds, e.g. the fat pointer inside an
/// `own dyn` handle. The cell itself is left intact.
pub(crate) fn load_own_cell<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    tv: &TypedValue<'ctx>,
    inner: &Ty,
) -> Result<inkwell::values::BasicValueEnum<'ctx>, crate::CodegenError> {
    let cell = tv.value.into_pointer_value();
    Ok(fn_ctx.builder.build_load(
        super::typing::ty_to_llvm(inner, fn_ctx.context),
        cell,
        "own_load",
    )?)
}

pub(crate) fn deref_shared<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    tv: TypedValue<'ctx>,
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    if let Ty::Shared(inner) = &tv.ntsc_type {
        let inner_fn = fn_ctx
            .module
            .get_function("ntsc_shared_inner")
            .ok_or_else(|| {
                crate::CodegenError::LLVMError("ntsc_shared_inner not declared".into())
            })?;

        let result = fn_ctx
            .builder
            .build_call(inner_fn, &[tv.value.into()], "shared_inner")?;
        let inner_val = call_result_to_value(fn_ctx, &result);
        Ok(TypedValue::new(inner_val, (**inner).clone()))
    } else {
        Ok(tv)
    }
}

pub(crate) fn box_or_retain_shared<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    target: &Ty,
    expr: &Expr,
    val: &TypedValue<'ctx>,
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    let Ty::Shared(inner) = target else {
        return Ok(val.clone());
    };
    if matches!(val.ntsc_type, Ty::Shared(_)) {
        if !expr_is_fresh(fn_ctx, expr, val) {
            let retain_fn = fn_ctx
                .module
                .get_function("ntsc_shared_retain")
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError("ntsc_shared_retain not declared".into())
                })?;
            fn_ctx.builder.build_call(
                retain_fn,
                &[BasicMetadataValueEnum::IntValue(val.value.into_int_value())],
                "shared_retain",
            )?;
        }
        return Ok(val.clone());
    }
    let inner_handle = if expr_is_string_literal(expr) && matches!(**inner, Ty::String) {
        clone_string_value(fn_ctx, val)?.into_int_value()
    } else {
        val.value.into_int_value()
    };
    let new_fn = fn_ctx
        .module
        .get_function("ntsc_shared_new")
        .ok_or_else(|| crate::CodegenError::LLVMError("ntsc_shared_new not declared".into()))?;
    let boxed = fn_ctx.builder.build_call(
        new_fn,
        &[BasicMetadataValueEnum::IntValue(inner_handle)],
        "shared_new",
    )?;
    let box_handle = call_result_to_value(fn_ctx, &boxed);
    Ok(TypedValue::new(box_handle, target.clone()))
}

pub(crate) fn coerce_value_to_llvm<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    val: TypedValue<'ctx>,
    target: &BasicMetadataTypeEnum<'ctx>,
) -> Result<BasicMetadataValueEnum<'ctx>, crate::CodegenError> {
    let val = normalize_self(fn_ctx, val)?;
    let builder = fn_ctx.builder;
    let ctx = fn_ctx.context;
    match target {
        BasicMetadataTypeEnum::IntType(t) if val.value.is_int_value() => {
            let int = val.value.into_int_value();
            let src_width = int.get_type().get_bit_width();
            let dst_width = t.get_bit_width();
            if src_width == dst_width {
                Ok(int.into())
            } else if dst_width == 1 {
                let is_nonzero = builder.build_int_compare(
                    IntPredicate::NE,
                    int,
                    int.get_type().const_zero(),
                    "int_to_bool",
                )?;
                Ok(is_nonzero.into())
            } else if dst_width < src_width {
                Ok(builder.build_int_truncate(int, *t, "trunc")?.into())
            } else {
                let widened = builder.build_int_z_extend(int, *t, "zext")?;
                Ok(widened.into())
            }
        }
        BasicMetadataTypeEnum::FloatType(_) if val.value.is_int_value() => {
            let int_val = if val.ntsc_type == Ty::Bool {
                builder.build_int_z_extend(
                    val.value.into_int_value(),
                    ctx.i64_type(),
                    "bool_to_int",
                )?
            } else {
                val.value.into_int_value()
            };
            let f = builder.build_signed_int_to_float(int_val, ctx.f64_type(), "int_to_float")?;
            Ok(f.into())
        }
        // A `float` reaching an integer slot is converted rather than
        // emitted as-is: the type checker types a stdlib call like
        // `time.now()` loosely enough that a float can reach an `int`
        // parameter, and passing the raw double would produce a module that
        // fails to verify. The value saturates and NaN becomes 0, matching
        // the documented conversion rule.
        BasicMetadataTypeEnum::IntType(t) if val.value.is_float_value() => {
            let float = val.value.into_float_value();
            let converted = builder.build_float_to_signed_int(float, *t, "float_to_int")?;
            let unordered = builder.build_float_compare(
                inkwell::FloatPredicate::UNO,
                float,
                float,
                "float_is_nan",
            )?;
            let settled =
                builder.build_select(unordered, t.const_zero(), converted, "float_to_int_nan")?;
            Ok(settled.into())
        }
        _ => Ok(val.value.into()),
    }
}

// ── Helper: convert to expected return type ─────────────────────────────

pub(crate) fn convert_to_expected<'ctx>(
    _fn_ctx: &mut FunctionContext<'ctx, '_>,
    val: TypedValue<'ctx>,
    _expected: &Ty,
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    Ok(val)
}

// ── Helper: bool to i1 ──────────────────────────────────────────────────

pub(crate) fn bool_to_i1<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    val: TypedValue<'ctx>,
) -> inkwell::values::IntValue<'ctx> {
    match &val.ntsc_type {
        Ty::Bool => val.value.into_int_value(),
        _ => {
            // The zero must match the *value's* width, not the Neutron type.
            // A Rust bridge returns `option[T]` as a cell handle, which is an
            // i64 like every other handle: comparing one against a `bool`
            // zero emitted an icmp with mismatched operand types and the
            // module failed to verify.
            if val.value.is_int_value() {
                let int = val.value.into_int_value();
                let zero = int.get_type().const_zero();
                fn_ctx
                    .builder
                    .build_int_compare(IntPredicate::NE, int, zero, "bool_conv")
                    .unwrap_or(zero)
            } else if val.value.is_pointer_value() {
                let ptr = val.value.into_pointer_value();
                let false_val = fn_ctx.context.bool_type().const_zero();
                let is_null = fn_ctx
                    .builder
                    .build_is_null(ptr, "bool_conv")
                    .unwrap_or(false_val);
                fn_ctx
                    .builder
                    .build_not(is_null, "bool_not_null")
                    .unwrap_or(false_val)
            } else if val.value.is_float_value() {
                fn_ctx
                    .builder
                    .build_float_compare(
                        inkwell::FloatPredicate::ONE,
                        val.value.into_float_value(),
                        fn_ctx.context.f64_type().const_zero(),
                        "bool_conv",
                    )
                    .unwrap_or(fn_ctx.context.bool_type().const_zero())
            } else {
                fn_ctx.context.bool_type().const_zero()
            }
        }
    }
}
