//! RC-backed array operations and routed stdlib module operations.

use super::*;

/// Emit a call to an `arrays.*` operation backed by the RC dynamic-array
/// functions. Every operation routes here so heap arrays work uniformly;
/// the legacy `ntsc_arrays_*` functions use a newline-delimited string ABI.
pub(crate) fn emit_rc_array_op<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    prop_name: &str,
    arg_values: &[TypedValue<'ctx>],
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    let ctx = fn_ctx.context;
    let i64_ty = ctx.i64_type();

    match prop_name {
        "new" => {
            let new_fn = fn_ctx
                .module
                .get_function("ntsc_array_new_typed")
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError("ntsc_array_new_typed not declared".into())
                })?;
            let elem_size = i64_ty.const_int(8, false);
            let result = fn_ctx.builder.build_call(
                new_fn,
                &[
                    inkwell::values::BasicMetadataValueEnum::IntValue(elem_size),
                    inkwell::values::BasicMetadataValueEnum::IntValue(i64_ty.const_zero()),
                    inkwell::values::BasicMetadataValueEnum::IntValue(
                        ctx.i8_type().const_int(1, false),
                    ),
                ],
                "arr_new",
            )?;
            let val = call_result_to_value(fn_ctx, &result);
            return Ok(TypedValue::new(val, Ty::Array(Box::new(Ty::Any))));
        }
        "range" => {
            let start = arg_values.first().ok_or_else(|| {
                crate::CodegenError::LLVMError("arrays.range requires start and end".into())
            })?;
            let end = arg_values.get(1).ok_or_else(|| {
                crate::CodegenError::LLVMError("arrays.range requires start and end".into())
            })?;
            let range_fn = fn_ctx
                .module
                .get_function("ntsc_array_range")
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError("ntsc_array_range not declared".into())
                })?;
            let result = fn_ctx.builder.build_call(
                range_fn,
                &[start.value.into(), end.value.into()],
                "arr_range",
            )?;
            let val = call_result_to_value(fn_ctx, &result);
            return Ok(TypedValue::new(val, Ty::Array(Box::new(Ty::Int))));
        }
        "fill" => {
            let val = arg_values.first().ok_or_else(|| {
                crate::CodegenError::LLVMError("arrays.fill requires a value and count".into())
            })?;
            let count = arg_values.get(1).ok_or_else(|| {
                crate::CodegenError::LLVMError("arrays.fill requires a value and count".into())
            })?;
            let fill_fn = fn_ctx
                .module
                .get_function("ntsc_array_fill")
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError("ntsc_array_fill not declared".into())
                })?;
            let elem_size = i64_ty.const_int(array_elem_size(&val.ntsc_type) as u64, false);
            let string_elems = matches!(val.ntsc_type, Ty::String);
            let string_elems_val = ctx.i8_type().const_int(u64::from(string_elems), false);
            let val_bits = encode_array_scalar(fn_ctx, val)?;
            let result = fn_ctx.builder.build_call(
                fill_fn,
                &[
                    val_bits.into(),
                    count.value.into(),
                    inkwell::values::BasicMetadataValueEnum::IntValue(elem_size),
                    inkwell::values::BasicMetadataValueEnum::IntValue(string_elems_val),
                ],
                "arr_fill",
            )?;
            let arr_val = call_result_to_value(fn_ctx, &result);
            emit_copy_in_owned_elements(fn_ctx, arr_val.into_int_value(), &val.ntsc_type)?;
            return Ok(TypedValue::new(
                arr_val,
                Ty::Array(Box::new(val.ntsc_type.clone())),
            ));
        }
        _ => {}
    }

    let array_arg = arg_values.first().ok_or_else(|| {
        crate::CodegenError::LLVMError("arrays operation requires an array argument".into())
    })?;
    let handle = array_arg.value.into_int_value();
    let elem_ty = array_elem_ty(&array_arg.ntsc_type);
    let array_result_ty = Ty::Array(Box::new(elem_ty.clone()));

    let len_fn = fn_ctx
        .module
        .get_function("ntsc_array_len")
        .ok_or_else(|| crate::CodegenError::LLVMError("ntsc_array_len not declared".into()))?;

    match prop_name {
        "length" => {
            let result = fn_ctx
                .builder
                .build_call(len_fn, &[array_arg.value.into()], "arr_len")?;
            let ret_val = call_result_to_value(fn_ctx, &result);
            Ok(TypedValue::new(ret_val, Ty::Int))
        }
        "isEmpty" | "is_empty" => {
            let result = fn_ctx
                .builder
                .build_call(len_fn, &[array_arg.value.into()], "arr_len")?;
            let ret_val = call_result_to_value(fn_ctx, &result).into_int_value();
            let zero = i64_ty.const_zero();
            let cmp =
                fn_ctx
                    .builder
                    .build_int_compare(IntPredicate::EQ, ret_val, zero, "arr_empty")?;
            Ok(TypedValue::new(cmp.into(), Ty::Bool))
        }
        "push" => {
            let value = arg_values.get(1).ok_or_else(|| {
                crate::CodegenError::LLVMError("arrays.push requires a value argument".into())
            })?;

            let is_untyped = matches!(&array_arg.ntsc_type, Ty::Array(inner) if **inner == Ty::Any);
            let is_string_like = matches!(value.ntsc_type, Ty::String | Ty::Any);
            let push_value = if is_untyped && !is_string_like {
                convert_to_string(fn_ctx, value)?
            } else {
                value.clone()
            };

            let push_fn = fn_ctx
                .module
                .get_function("ntsc_array_push")
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError("ntsc_array_push not declared".into())
                })?;

            let result = fn_ctx.builder.build_call(
                push_fn,
                &[
                    inkwell::values::BasicMetadataValueEnum::IntValue(handle),
                    encode_array_scalar(fn_ctx, &push_value)?.into(),
                ],
                "arr_push",
            )?;

            if is_untyped && !is_string_like {
                emit_drop_value(fn_ctx, &push_value)?;
            }
            let ret_val = call_result_to_value(fn_ctx, &result);
            Ok(TypedValue::new(ret_val, Ty::Void))
        }
        "at" | "get" => {
            let index = arg_values.get(1).ok_or_else(|| {
                crate::CodegenError::LLVMError("arrays.at requires an index argument".into())
            })?;
            let tv = emit_array_element(fn_ctx, handle, index.value.into_int_value(), &elem_ty)?;
            Ok(tv)
        }
        "pop" => {
            let pop_fn = fn_ctx
                .module
                .get_function("ntsc_array_pop")
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError("ntsc_array_pop not declared".into())
                })?;

            let result = fn_ctx
                .builder
                .build_call(pop_fn, &[array_arg.value.into()], "arr_pop")?;
            let val = call_result_to_value(fn_ctx, &result);
            let pop_ty = if elem_ty == Ty::Any {
                Ty::String
            } else {
                elem_ty.clone()
            };
            Ok(TypedValue::new(val, pop_ty))
        }
        "contains" => {
            let search = arg_values.get(1).ok_or_else(|| {
                crate::CodegenError::LLVMError("arrays.contains requires a value argument".into())
            })?;
            let len_val = fn_ctx
                .builder
                .build_call(len_fn, &[array_arg.value.into()], "arr_len")?
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            let idx = emit_array_find_index(fn_ctx, handle, len_val, &elem_ty, search)?;
            let found = fn_ctx.builder.build_int_compare(
                IntPredicate::SGE,
                idx,
                i64_ty.const_zero(),
                "contains_found",
            )?;
            Ok(TypedValue::new(found.into(), Ty::Bool))
        }
        "index_of" => {
            let search = arg_values.get(1).ok_or_else(|| {
                crate::CodegenError::LLVMError("arrays.index_of requires a value argument".into())
            })?;
            let len_val = fn_ctx
                .builder
                .build_call(len_fn, &[array_arg.value.into()], "arr_len")?
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            let idx = emit_array_find_index(fn_ctx, handle, len_val, &elem_ty, search)?;
            Ok(TypedValue::new(idx.into(), Ty::Int))
        }
        "remove" => {
            let search = arg_values.get(1).ok_or_else(|| {
                crate::CodegenError::LLVMError("arrays.remove requires a value argument".into())
            })?;
            let len_val = fn_ctx
                .builder
                .build_call(len_fn, &[array_arg.value.into()], "arr_len")?
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            let idx = emit_array_find_index(fn_ctx, handle, len_val, &elem_ty, search)?;
            let remove_fn = fn_ctx
                .module
                .get_function("ntsc_array_remove_at")
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError("ntsc_array_remove_at not declared".into())
                })?;
            let current_fn = fn_ctx.function;
            let do_bb = ctx.append_basic_block(current_fn, "remove.do");
            let skip_bb = ctx.append_basic_block(current_fn, "remove.skip");
            let merge_bb = ctx.append_basic_block(current_fn, "remove.merge");
            let is_found = fn_ctx.builder.build_int_compare(
                IntPredicate::SGE,
                idx,
                i64_ty.const_zero(),
                "remove_found",
            )?;
            fn_ctx
                .builder
                .build_conditional_branch(is_found, do_bb, skip_bb)?;
            fn_ctx.builder.position_at_end(do_bb);
            let result = fn_ctx.builder.build_call(
                remove_fn,
                &[array_arg.value.into(), idx.into()],
                "arr_remove",
            )?;
            let new_val = call_result_to_value(fn_ctx, &result);
            fn_ctx.builder.build_unconditional_branch(merge_bb)?;
            fn_ctx.builder.position_at_end(skip_bb);
            fn_ctx.builder.build_unconditional_branch(merge_bb)?;
            fn_ctx.builder.position_at_end(merge_bb);
            let phi = fn_ctx.builder.build_phi(i64_ty, "remove_handle")?;
            phi.add_incoming(&[
                (&new_val, do_bb),
                (&array_arg.value.into_int_value(), skip_bb),
            ]);
            Ok(TypedValue::new(phi.as_basic_value(), array_result_ty))
        }
        "remove_at" => {
            let index = arg_values.get(1).ok_or_else(|| {
                crate::CodegenError::LLVMError("arrays.remove_at requires an index argument".into())
            })?;
            let remove_fn = fn_ctx
                .module
                .get_function("ntsc_array_remove_at")
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError("ntsc_array_remove_at not declared".into())
                })?;
            let result = fn_ctx.builder.build_call(
                remove_fn,
                &[array_arg.value.into(), index.value.into()],
                "arr_remove_at",
            )?;
            let val = call_result_to_value(fn_ctx, &result);
            emit_copy_in_owned_elements(fn_ctx, val.into_int_value(), &elem_ty)?;
            Ok(TypedValue::new(val, array_result_ty))
        }
        "reverse" => emit_unary_array_op_with_fixup(
            fn_ctx,
            "ntsc_array_reverse",
            handle,
            array_result_ty,
            &elem_ty,
        ),
        "clone" | "flat" => emit_unary_array_op_with_fixup(
            fn_ctx,
            "ntsc_array_clone",
            handle,
            array_result_ty,
            &elem_ty,
        ),
        // `clear` empties the array in place and returns its handle;
        // typing the result as a fresh array (like the functional ops
        // below) would make statement-level cleanup free the caller's own
        // array out from under it. Like `push`, this is `Void`.
        "clear" => {
            let clear_fn = fn_ctx
                .module
                .get_function("ntsc_array_clear")
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError("ntsc_array_clear not declared".into())
                })?;
            fn_ctx
                .builder
                .build_call(clear_fn, &[array_arg.value.into()], "arr_clear")?;
            Ok(TypedValue::new(
                fn_ctx.context.i64_type().const_zero().into(),
                Ty::Void,
            ))
        }
        "shuffle" => emit_unary_array_op_with_fixup(
            fn_ctx,
            "ntsc_array_shuffle",
            handle,
            array_result_ty,
            &elem_ty,
        ),
        "sort" => {
            let sort_fn = fn_ctx
                .module
                .get_function("ntsc_array_sort")
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError("ntsc_array_sort not declared".into())
                })?;
            let mode = match &elem_ty {
                Ty::Float => 1,
                Ty::String | Ty::Any => 2,
                _ => 0,
            };
            let result = fn_ctx.builder.build_call(
                sort_fn,
                &[
                    array_arg.value.into(),
                    inkwell::values::BasicMetadataValueEnum::IntValue(
                        ctx.i8_type().const_int(mode, false),
                    ),
                ],
                "arr_sort",
            )?;
            let val = call_result_to_value(fn_ctx, &result);
            emit_copy_in_owned_elements(fn_ctx, val.into_int_value(), &elem_ty)?;
            Ok(TypedValue::new(val, array_result_ty))
        }
        "slice" => {
            let start = arg_values.get(1).ok_or_else(|| {
                crate::CodegenError::LLVMError("arrays.slice requires start and end".into())
            })?;
            let end = arg_values.get(2).ok_or_else(|| {
                crate::CodegenError::LLVMError("arrays.slice requires start and end".into())
            })?;
            let slice_fn = fn_ctx
                .module
                .get_function("ntsc_array_slice")
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError("ntsc_array_slice not declared".into())
                })?;
            let result = fn_ctx.builder.build_call(
                slice_fn,
                &[array_arg.value.into(), start.value.into(), end.value.into()],
                "arr_slice",
            )?;
            let val = call_result_to_value(fn_ctx, &result);
            emit_copy_in_owned_elements(fn_ctx, val.into_int_value(), &elem_ty)?;
            Ok(TypedValue::new(val, array_result_ty))
        }
        "join" => {
            let delim = arg_values.get(1).ok_or_else(|| {
                crate::CodegenError::LLVMError("arrays.join requires a delimiter argument".into())
            })?;
            let delim_val = convert_to_string(fn_ctx, delim)?.value.into_int_value();
            let len_val = fn_ctx
                .builder
                .build_call(len_fn, &[array_arg.value.into()], "arr_len")?
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            let current_fn = fn_ctx.function;

            let empty_str = emit_string_const(fn_ctx, "")?;
            let result_ptr = fn_ctx.alloca("join_result", &Ty::String)?;
            fn_ctx.builder.build_store(result_ptr, empty_str)?;
            let idx_ptr = fn_ctx.alloca("join_idx", &Ty::Int)?;
            fn_ctx.builder.build_store(idx_ptr, i64_ty.const_zero())?;

            let cond_bb = ctx.append_basic_block(current_fn, "join.cond");
            let body_bb = ctx.append_basic_block(current_fn, "join.body");
            let first_bb = ctx.append_basic_block(current_fn, "join.first");
            let later_bb = ctx.append_basic_block(current_fn, "join.later");
            let incr_bb = ctx.append_basic_block(current_fn, "join.incr");
            let merge_bb = ctx.append_basic_block(current_fn, "join.merge");

            fn_ctx.builder.build_unconditional_branch(cond_bb)?;
            fn_ctx.builder.position_at_end(cond_bb);
            let idx = fn_ctx
                .builder
                .build_load(i64_ty, idx_ptr, "join_idx")?
                .into_int_value();
            let cond =
                fn_ctx
                    .builder
                    .build_int_compare(IntPredicate::SLT, idx, len_val, "join_cond")?;
            fn_ctx
                .builder
                .build_conditional_branch(cond, body_bb, merge_bb)?;

            fn_ctx.builder.position_at_end(body_bb);
            let elem = emit_array_element(fn_ctx, handle, idx, &elem_ty)?;
            let elem_str = convert_to_string(fn_ctx, &elem)?;
            // A scalar element renders into a fresh handle this loop owns
            // and must drop once consumed; string elements borrow the
            // container's handle and are never dropped here.
            let elem_str_scratch = !matches!(elem.ntsc_type, Ty::String);
            let cur = fn_ctx
                .builder
                .build_load(i64_ty, result_ptr, "join_cur")?
                .into_int_value();
            let is_first = fn_ctx.builder.build_int_compare(
                IntPredicate::EQ,
                idx,
                i64_ty.const_zero(),
                "join_is_first",
            )?;
            fn_ctx
                .builder
                .build_conditional_branch(is_first, first_bb, later_bb)?;

            fn_ctx.builder.position_at_end(first_bb);
            // Concatenate even the first element in, so the slot never
            // aliases a container element: every handle reachable from
            // this slot is owned by it.
            let first = emit_concat(fn_ctx, empty_str, elem_str.value)?;
            fn_ctx
                .builder
                .build_store(result_ptr, first.into_int_value())?;
            if elem_str_scratch {
                emit_drop_value(fn_ctx, &TypedValue::new(elem_str.value, Ty::String))?;
            }
            fn_ctx.builder.build_unconditional_branch(incr_bb)?;

            fn_ctx.builder.position_at_end(later_bb);
            let with_delim = emit_concat(fn_ctx, cur.into(), delim_val.into())?;
            let joined = emit_concat(fn_ctx, with_delim, elem_str.value)?;
            fn_ctx.builder.build_store(result_ptr, joined)?;
            // The overwritten slot content (`cur`) and the delimiter-
            // prefixed scratch are both owned here; leaving either live
            // leaked one handle per iteration.
            emit_drop_value(
                fn_ctx,
                &TypedValue::new(BasicValueEnum::IntValue(cur), Ty::String),
            )?;
            emit_drop_value(fn_ctx, &TypedValue::new(with_delim, Ty::String))?;
            if elem_str_scratch {
                emit_drop_value(fn_ctx, &TypedValue::new(elem_str.value, Ty::String))?;
            }
            fn_ctx.builder.build_unconditional_branch(incr_bb)?;

            fn_ctx.builder.position_at_end(incr_bb);
            let next =
                fn_ctx
                    .builder
                    .build_int_add(idx, i64_ty.const_int(1, false), "join_next")?;
            fn_ctx.builder.build_store(idx_ptr, next)?;
            fn_ctx.builder.build_unconditional_branch(cond_bb)?;

            fn_ctx.builder.position_at_end(merge_bb);
            let final_str = fn_ctx
                .builder
                .build_load(i64_ty, result_ptr, "join_result")?
                .into_int_value();
            Ok(TypedValue::new(final_str.into(), Ty::String))
        }
        "every" | "some" => {
            let pred = arg_values.get(1).ok_or_else(|| {
                crate::CodegenError::LLVMError(format!(
                    "arrays.{prop_name} requires a predicate function"
                ))
            })?;
            let Ty::Function { params, .. } = &pred.ntsc_type else {
                return Err(crate::CodegenError::LLVMError(format!(
                    "arrays.{prop_name} predicate must be a function, got `{}`",
                    pred.ntsc_type
                )));
            };
            if params.len() != 1 {
                return Err(crate::CodegenError::LLVMError(format!(
                    "arrays.{prop_name} predicate must take exactly one parameter"
                )));
            }
            let pred_param_ty = &params[0];
            let fn_ptr = pred.value.into_pointer_value();
            let is_every = prop_name == "every";
            let len_val = fn_ctx
                .builder
                .build_call(len_fn, &[array_arg.value.into()], "arr_len")?
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            let current_fn = fn_ctx.function;

            let acc_ptr = fn_ctx.alloca("pred_acc", &Ty::Bool)?;
            let init = ctx
                .bool_type()
                .const_int(if is_every { 1 } else { 0 }, false);
            fn_ctx.builder.build_store(acc_ptr, init)?;
            let idx_ptr = fn_ctx.alloca("pred_idx", &Ty::Int)?;
            fn_ctx.builder.build_store(idx_ptr, i64_ty.const_zero())?;

            let cond_bb = ctx.append_basic_block(current_fn, "pred.cond");
            let body_bb = ctx.append_basic_block(current_fn, "pred.body");
            let check_bb = ctx.append_basic_block(current_fn, "pred.check");
            let done_bb = ctx.append_basic_block(current_fn, "pred.done");
            let incr_bb = ctx.append_basic_block(current_fn, "pred.incr");
            let merge_bb = ctx.append_basic_block(current_fn, "pred.merge");

            fn_ctx.builder.build_unconditional_branch(cond_bb)?;
            fn_ctx.builder.position_at_end(cond_bb);
            let idx = fn_ctx
                .builder
                .build_load(i64_ty, idx_ptr, "pred_idx")?
                .into_int_value();
            let cond =
                fn_ctx
                    .builder
                    .build_int_compare(IntPredicate::SLT, idx, len_val, "pred_cond")?;
            fn_ctx
                .builder
                .build_conditional_branch(cond, body_bb, merge_bb)?;

            fn_ctx.builder.position_at_end(body_bb);
            let elem = emit_array_element(fn_ctx, handle, idx, &elem_ty)?;
            let arg = coerce_value(fn_ctx, elem, pred_param_ty)?;
            if arg.ntsc_type != *pred_param_ty {
                return Err(crate::CodegenError::LLVMError(format!(
                    "arrays.{prop_name} predicate parameter type `{pred_param_ty}` is incompatible with array element type `{elem_ty}`"
                )));
            }
            let res = emit_function_pointer_call(fn_ctx, fn_ptr, &[arg], &pred.ntsc_type)?;
            let res_i1 = match res.ntsc_type {
                Ty::Bool => res.value.into_int_value(),
                Ty::Int => fn_ctx.builder.build_int_compare(
                    IntPredicate::NE,
                    res.value.into_int_value(),
                    i64_ty.const_zero(),
                    "pred_truthy",
                )?,
                _ => {
                    let as_int = fn_ctx.builder.build_ptr_to_int(
                        res.value.into_pointer_value(),
                        i64_ty,
                        "pred_ptr",
                    )?;
                    fn_ctx.builder.build_int_compare(
                        IntPredicate::NE,
                        as_int,
                        i64_ty.const_zero(),
                        "pred_truthy",
                    )?
                }
            };
            fn_ctx.builder.build_unconditional_branch(check_bb)?;

            fn_ctx.builder.position_at_end(check_bb);
            let stop = if is_every {
                fn_ctx
                    .builder
                    .build_xor(res_i1, ctx.bool_type().const_all_ones(), "every_stop")?
            } else {
                res_i1
            };
            fn_ctx
                .builder
                .build_conditional_branch(stop, done_bb, incr_bb)?;

            fn_ctx.builder.position_at_end(done_bb);
            let done_val = ctx
                .bool_type()
                .const_int(if is_every { 0 } else { 1 }, false);
            fn_ctx.builder.build_store(acc_ptr, done_val)?;
            fn_ctx.builder.build_unconditional_branch(merge_bb)?;

            fn_ctx.builder.position_at_end(incr_bb);
            let next =
                fn_ctx
                    .builder
                    .build_int_add(idx, i64_ty.const_int(1, false), "pred_next")?;
            fn_ctx.builder.build_store(idx_ptr, next)?;
            fn_ctx.builder.build_unconditional_branch(cond_bb)?;

            fn_ctx.builder.position_at_end(merge_bb);
            let result = fn_ctx
                .builder
                .build_load(ctx.bool_type(), acc_ptr, "pred_result")?
                .into_int_value();
            Ok(TypedValue::new(result.into(), Ty::Bool))
        }
        _ => Err(crate::CodegenError::LLVMError(format!(
            "unsupported arrays operation `{prop_name}`"
        ))),
    }
}

pub(crate) fn array_elem_ty(ty: &Ty) -> Ty {
    match ty {
        Ty::Array(inner) => *inner.clone(),
        _ => Ty::Any,
    }
}

// ── Routed stdlib module ops ─────────────────────────────────────────────

/// Dispatch element-type-aware stdlib operations that cannot use the plain
/// `ntsc_<module>_<fn>` ABI because the runtime cannot infer the element
/// type of heap arrays at runtime.
pub(crate) fn emit_routed_module_op<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    module_name: &str,
    prop_name: &str,
    arg_values: &[TypedValue<'ctx>],
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    match module_name {
        "sort" => emit_sort_op(fn_ctx, prop_name, arg_values),
        "random" => emit_random_op(fn_ctx, prop_name, arg_values),
        "testing" => emit_testing_op(fn_ctx, prop_name, arg_values),
        "process" => emit_process_op(fn_ctx, prop_name, arg_values),
        _ => Err(crate::CodegenError::LLVMError(format!(
            "no routed codegen for module `{module_name}`"
        ))),
    }
}

/// `process.spawn_thread` — spawn a thread running `fn(arg)`. The worker
/// must be a no-capture function taking exactly one `int` parameter
/// (typically a channel handle) and returning void; the generated function
/// pointer is passed to the runtime with its C ABI type.
pub(crate) fn emit_process_op<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    prop_name: &str,
    arg_values: &[TypedValue<'ctx>],
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    match prop_name {
        "spawn_thread" => {
            let worker = arg_values.first().ok_or_else(|| {
                crate::CodegenError::LLVMError(
                    "process.spawn_thread requires a worker function".into(),
                )
            })?;
            let Ty::Function {
                params,
                return_type,
            } = &worker.ntsc_type
            else {
                return Err(crate::CodegenError::LLVMError(format!(
                    "process.spawn_thread expects a function, got `{}`",
                    worker.ntsc_type
                )));
            };
            if params.len() != 1 || params[0] != Ty::Int {
                return Err(crate::CodegenError::LLVMError(
                    "process.spawn_thread worker must take exactly one `int` parameter".into(),
                ));
            }
            if **return_type != Ty::Void {
                return Err(crate::CodegenError::LLVMError(
                    "process.spawn_thread worker must return void".into(),
                ));
            }
            let arg = arg_values.get(1).ok_or_else(|| {
                crate::CodegenError::LLVMError(
                    "process.spawn_thread requires a second `int` argument".into(),
                )
            })?;
            let arg = coerce_value(fn_ctx, arg.clone(), &Ty::Int)?;

            let fn_ptr = worker.value.into_pointer_value();
            let fn_val = fn_ctx
                .module
                .get_function("ntsc_process_spawn_thread")
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError("ntsc_process_spawn_thread not declared".into())
                })?;
            let result = fn_ctx.builder.build_call(
                fn_val,
                &[fn_ptr.into(), arg.value.into()],
                "thread_spawn",
            )?;
            let val = call_result_to_value(fn_ctx, &result);
            Ok(TypedValue::new(val, Ty::Int))
        }
        _ => Err(crate::CodegenError::LLVMError(format!(
            "no routed codegen for process.{prop_name}"
        ))),
    }
}

/// Map an array element type to the runtime comparison mode used by the
/// `sort` module (0 = `i64`, 1 = `f64`, 2 = strings). Untyped (`[]`) arrays
/// store elements as string pointers, so they sort as strings.
pub(crate) fn array_sort_mode(elem_ty: &Ty) -> Option<i8> {
    match elem_ty {
        Ty::Int => Some(0),
        Ty::Float => Some(1),
        Ty::String | Ty::Any => Some(2),
        _ => None,
    }
}

pub(crate) fn emit_sort_op<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    prop_name: &str,
    arg_values: &[TypedValue<'ctx>],
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    let ctx = fn_ctx.context;
    let array_arg = arg_values.first().ok_or_else(|| {
        crate::CodegenError::LLVMError(format!("sort.{prop_name} requires an array argument"))
    })?;
    let elem_ty = array_elem_ty(&array_arg.ntsc_type);
    let mode = array_sort_mode(&elem_ty).ok_or_else(|| {
        crate::CodegenError::LLVMError(format!(
            "sort.{prop_name} requires an array of int, float, or string elements, got `{elem_ty}`"
        ))
    })?;
    let array_ty = Ty::Array(Box::new(elem_ty.clone()));
    let handle = array_arg.value.into_int_value();
    let mode_val = ctx.i8_type().const_int(mode as u64, false);

    match prop_name {
        "stable_sort" => {
            let fn_val = fn_ctx
                .module
                .get_function("ntsc_sort_stable_sort")
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError("ntsc_sort_stable_sort not declared".into())
                })?;
            let result = fn_ctx.builder.build_call(
                fn_val,
                &[BasicMetadataValueEnum::IntValue(handle), mode_val.into()],
                "arr_sort",
            )?;
            let val = call_result_to_value(fn_ctx, &result);
            Ok(TypedValue::new(val, array_ty))
        }
        "sort_by" => {
            let cmp = arg_values.get(1).ok_or_else(|| {
                crate::CodegenError::LLVMError("sort.sort_by requires a comparator function".into())
            })?;
            let Ty::Function {
                params,
                return_type,
            } = &cmp.ntsc_type
            else {
                return Err(crate::CodegenError::LLVMError(format!(
                    "sort.sort_by comparator must be a function, got `{}`",
                    cmp.ntsc_type
                )));
            };
            if params.len() != 2 {
                return Err(crate::CodegenError::LLVMError(
                    "sort.sort_by comparator must take exactly two parameters".into(),
                ));
            }
            if **return_type != Ty::Bool {
                return Err(crate::CodegenError::LLVMError(
                    "sort.sort_by comparator must return bool".into(),
                ));
            }

            let expected = match mode {
                0 => Ty::Int,
                1 => Ty::Float,
                _ => Ty::String,
            };
            if params[0] != expected || params[1] != expected {
                return Err(crate::CodegenError::LLVMError(format!(
                    "sort.sort_by comparator parameters must be `{expected}` for an array of `{elem_ty}`"
                )));
            }
            let fn_val = fn_ctx
                .module
                .get_function("ntsc_sort_sort_by")
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError("ntsc_sort_sort_by not declared".into())
                })?;
            let result = fn_ctx.builder.build_call(
                fn_val,
                &[
                    BasicMetadataValueEnum::IntValue(handle),
                    BasicMetadataValueEnum::PointerValue(cmp.value.into_pointer_value()),
                    mode_val.into(),
                ],
                "arr_sortby",
            )?;
            let val = call_result_to_value(fn_ctx, &result);
            Ok(TypedValue::new(val, array_ty))
        }
        "binary_search" => {
            let value = arg_values.get(1).ok_or_else(|| {
                crate::CodegenError::LLVMError("sort.binary_search requires a search value".into())
            })?;

            let value = if elem_ty == Ty::Any {
                if value.ntsc_type != Ty::String {
                    return Err(crate::CodegenError::LLVMError(
                        "sort.binary_search on an untyped array requires a string value".into(),
                    ));
                }
                value.clone()
            } else {
                coerce_value(fn_ctx, value.clone(), &elem_ty)?
            };

            let value_bits = match value.ntsc_type {
                Ty::Float => fn_ctx.builder.build_bit_cast(
                    value.value.into_float_value(),
                    ctx.i64_type(),
                    "bs_bits",
                )?,
                _ => value.value.into_int_value().into(),
            };
            let fn_val = fn_ctx
                .module
                .get_function("ntsc_sort_binary_search")
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError("ntsc_sort_binary_search not declared".into())
                })?;
            let result = fn_ctx.builder.build_call(
                fn_val,
                &[
                    BasicMetadataValueEnum::IntValue(handle),
                    BasicMetadataValueEnum::IntValue(value_bits.into_int_value()),
                    mode_val.into(),
                ],
                "arr_bsearch",
            )?;
            let ret = call_result_to_value(fn_ctx, &result).into_int_value();
            Ok(TypedValue::new(ret.into(), Ty::Int))
        }
        _ => Err(crate::CodegenError::LLVMError(format!(
            "unsupported sort operation `{prop_name}`"
        ))),
    }
}

pub(crate) fn emit_random_op<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    prop_name: &str,
    arg_values: &[TypedValue<'ctx>],
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    let ctx = fn_ctx.context;
    let array_arg = arg_values.first().ok_or_else(|| {
        crate::CodegenError::LLVMError(format!("random.{prop_name} requires an array argument"))
    })?;
    let elem_ty = array_elem_ty(&array_arg.ntsc_type);
    let handle = array_arg.value.into_int_value();

    match prop_name {
        "shuffle" => {
            let array_ty = Ty::Array(Box::new(elem_ty));
            let fn_val = fn_ctx
                .module
                .get_function("ntsc_random_shuffle")
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError("ntsc_random_shuffle not declared".into())
                })?;
            let result = fn_ctx.builder.build_call(
                fn_val,
                &[BasicMetadataValueEnum::IntValue(handle)],
                "rand_shuffle",
            )?;
            let val = call_result_to_value(fn_ctx, &result);
            Ok(TypedValue::new(val, array_ty))
        }
        "weighted" => {
            let mode = match &elem_ty {
                Ty::Int => Some(0),
                Ty::Float => Some(1),
                _ => None,
            }
            .ok_or_else(|| {
                crate::CodegenError::LLVMError(format!(
                    "random.weighted requires an array[int] or array[float] of weights, got `{elem_ty}`"
                ))
            })?;
            let fn_val = fn_ctx
                .module
                .get_function("ntsc_random_weighted")
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError("ntsc_random_weighted not declared".into())
                })?;
            let result = fn_ctx.builder.build_call(
                fn_val,
                &[
                    BasicMetadataValueEnum::IntValue(handle),
                    ctx.i8_type().const_int(mode as u64, false).into(),
                ],
                "rand_weighted",
            )?;
            let ret = call_result_to_value(fn_ctx, &result).into_int_value();
            Ok(TypedValue::new(ret.into(), Ty::Int))
        }
        _ => Err(crate::CodegenError::LLVMError(format!(
            "unsupported random operation `{prop_name}`"
        ))),
    }
}

pub(crate) fn emit_testing_op<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    prop_name: &str,
    arg_values: &[TypedValue<'ctx>],
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    let ctx = fn_ctx.context;
    match prop_name {
        "bench" => {
            let fn_arg = arg_values.first().ok_or_else(|| {
                crate::CodegenError::LLVMError("testing.bench requires a function argument".into())
            })?;
            let Ty::Function { .. } = &fn_arg.ntsc_type else {
                return Err(crate::CodegenError::LLVMError(format!(
                    "testing.bench first argument must be a function, got `{}`",
                    fn_arg.ntsc_type
                )));
            };
            let iters = arg_values.get(1).ok_or_else(|| {
                crate::CodegenError::LLVMError(
                    "testing.bench requires an iterations argument".into(),
                )
            })?;
            let iters = coerce_value(fn_ctx, iters.clone(), &Ty::Int)?;
            let warmup = arg_values.get(2).cloned().unwrap_or_else(|| {
                TypedValue::new(ctx.i64_type().const_int(1, false).into(), Ty::Int)
            });
            let warmup = coerce_value(fn_ctx, warmup, &Ty::Int)?;
            let fn_ptr = fn_arg.value.into_pointer_value();
            let bench_fn = fn_ctx
                .module
                .get_function("ntsc_testing_bench")
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError("ntsc_testing_bench not declared".into())
                })?;
            let result = fn_ctx.builder.build_call(
                bench_fn,
                &[fn_ptr.into(), iters.value.into(), warmup.value.into()],
                "bench_result",
            )?;
            let val = call_result_to_value(fn_ctx, &result);
            Ok(TypedValue::new(val, Ty::Float))
        }
        _ => {
            let runtime_fn = match prop_name {
                "assert_true" | "assert_false" => format!("ntsc_testing_{prop_name}"),
                "assert_eq" | "assert_ne" => {
                    let a = arg_values.first().ok_or_else(|| {
                        crate::CodegenError::LLVMError(format!(
                            "testing.{prop_name} requires two arguments"
                        ))
                    })?;
                    let suffix = match a.ntsc_type {
                        Ty::Int => "int",
                        Ty::Float => "float",
                        Ty::Bool => "bool",
                        Ty::String => "string",
                        _ => {
                            return Err(crate::CodegenError::LLVMError(format!(
                                "testing.{prop_name} requires int, float, bool, or string arguments, got `{}`",
                                a.ntsc_type
                            )));
                        }
                    };
                    format!("ntsc_testing_{prop_name}_{suffix}")
                }
                _ => {
                    return Err(crate::CodegenError::LLVMError(format!(
                        "unsupported testing operation `{prop_name}`"
                    )));
                }
            };
            let fn_val = fn_ctx.module.get_function(&runtime_fn).ok_or_else(|| {
                crate::CodegenError::LLVMError(format!("{runtime_fn} not declared"))
            })?;

            let param_tys = fn_val.get_type().get_param_types();
            let mut llvm_args: Vec<inkwell::values::BasicMetadataValueEnum<'ctx>> = Vec::new();
            if prop_name == "assert_eq" || prop_name == "assert_ne" {
                let a = arg_values.first().ok_or_else(|| {
                    crate::CodegenError::LLVMError(format!(
                        "testing.{prop_name} requires two arguments"
                    ))
                })?;
                let b = arg_values.get(1).ok_or_else(|| {
                    crate::CodegenError::LLVMError(format!(
                        "testing.{prop_name} requires two arguments"
                    ))
                })?;
                let a = coerce_value_to_llvm(fn_ctx, a.clone(), &param_tys[0])?;
                let b = coerce_value_to_llvm(fn_ctx, b.clone(), &param_tys[1])?;
                llvm_args.push(a);
                llvm_args.push(b);
            } else if prop_name == "assert_true" || prop_name == "assert_false" {
                let cond = arg_values.first().ok_or_else(|| {
                    crate::CodegenError::LLVMError(format!(
                        "testing.{prop_name} requires an argument"
                    ))
                })?;
                let cond = coerce_value_to_llvm(fn_ctx, cond.clone(), &param_tys[0])?;
                llvm_args.push(cond);
            }

            fn_ctx
                .builder
                .build_call(fn_val, &llvm_args, "assert_call")?;
            Ok(TypedValue::new(
                ctx.bool_type().const_all_ones().into(),
                Ty::Bool,
            ))
        }
    }
}

pub(crate) fn array_elem_size(ty: &Ty) -> i64 {
    if *ty == Ty::Bool { 1 } else { 8 }
}

pub(crate) fn emit_unary_array_op<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    runtime_fn: &str,
    handle: inkwell::values::IntValue<'ctx>,
    result_ty: Ty,
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    let fn_val = fn_ctx
        .module
        .get_function(runtime_fn)
        .ok_or_else(|| crate::CodegenError::LLVMError(format!("{runtime_fn} not declared")))?;
    let result = fn_ctx.builder.build_call(
        fn_val,
        &[inkwell::values::BasicMetadataValueEnum::IntValue(handle)],
        "arr_op",
    )?;
    let val = call_result_to_value(fn_ctx, &result);
    Ok(TypedValue::new(val, result_ty))
}

fn emit_unary_array_op_with_fixup<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    runtime_fn: &str,
    handle: inkwell::values::IntValue<'ctx>,
    result_ty: Ty,
    elem_ty: &Ty,
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    let result = emit_unary_array_op(fn_ctx, runtime_fn, handle, result_ty.clone())?;
    emit_copy_in_owned_elements(fn_ctx, result.value.into_int_value(), elem_ty)?;
    Ok(result)
}

/// Give a freshly created array ownership of its option and shared
/// elements. The runtime copies non-string elements by value, so a new
/// array built from another (`clone`, `slice`, `remove_at`, `sort`,
/// `reverse`, `shuffle`, `fill`) would otherwise share option cells and
/// shared boxes with the source array. Each option cell is deep-copied
/// into a fresh cell owned by the new array (the old cell is reclaimed),
/// and each shared box is retained once for the new array's reference.
pub(crate) fn emit_copy_in_owned_elements<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    arr_handle: inkwell::values::IntValue<'ctx>,
    elem_ty: &Ty,
) -> Result<(), crate::CodegenError> {
    let (len_fn, get_fn) = (
        fn_ctx
            .module
            .get_function("ntsc_array_len")
            .ok_or_else(|| crate::CodegenError::LLVMError("ntsc_array_len not declared".into()))?,
        fn_ctx
            .module
            .get_function("ntsc_array_get")
            .ok_or_else(|| crate::CodegenError::LLVMError("ntsc_array_get not declared".into()))?,
    );
    match elem_ty {
        Ty::Option(inner) => {
            let inner = (**inner).clone();
            let set_fn = fn_ctx
                .module
                .get_function("ntsc_array_set")
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError("ntsc_array_set not declared".into())
                })?;
            let current_fn = fn_ctx.function;
            let i_ptr = fn_ctx.alloca("copyin_i", &Ty::Int)?;
            let cond_bb = fn_ctx.context.append_basic_block(current_fn, "copyin.cond");
            let body_bb = fn_ctx.context.append_basic_block(current_fn, "copyin.body");
            let incr_bb = fn_ctx.context.append_basic_block(current_fn, "copyin.incr");
            let done_bb = fn_ctx.context.append_basic_block(current_fn, "copyin.done");

            let len = fn_ctx
                .builder
                .build_call(len_fn, &[arr_handle.into()], "copyin_len")?
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
                .build_load(fn_ctx.context.i64_type(), i_ptr, "copyin_i")?
                .into_int_value();
            let cond = fn_ctx.builder.build_int_compare(
                inkwell::IntPredicate::SLT,
                i,
                len,
                "copyin_cond",
            )?;
            fn_ctx
                .builder
                .build_conditional_branch(cond, body_bb, done_bb)?;

            fn_ctx.builder.position_at_end(body_bb);
            let elem = fn_ctx
                .builder
                .build_call(get_fn, &[arr_handle.into(), i.into()], "copyin_elem")?
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            let is_null = fn_ctx.builder.build_int_compare(
                inkwell::IntPredicate::EQ,
                elem,
                fn_ctx.context.i64_type().const_zero(),
                "copyin_null",
            )?;
            let clone_bb = fn_ctx
                .context
                .append_basic_block(current_fn, "copyin.clone");
            let next_bb = fn_ctx.context.append_basic_block(current_fn, "copyin.next");
            fn_ctx
                .builder
                .build_conditional_branch(is_null, next_bb, clone_bb)?;

            fn_ctx.builder.position_at_end(clone_bb);
            let cell = clone_option_value(
                fn_ctx,
                &inner,
                &TypedValue::new(elem.into(), Ty::Option(Box::new(inner.clone()))),
            )?;
            fn_ctx.builder.build_call(
                set_fn,
                &[
                    arr_handle.into(),
                    i.into(),
                    fn_ctx
                        .builder
                        .build_ptr_to_int(cell, fn_ctx.context.i64_type(), "copyin_cell_bits")?
                        .into(),
                ],
                "copyin_set",
            )?;
            emit_drop_value(
                fn_ctx,
                &TypedValue::new(elem.into(), Ty::Option(Box::new(inner))),
            )?;
            fn_ctx.builder.build_unconditional_branch(next_bb)?;

            fn_ctx.builder.position_at_end(next_bb);
            fn_ctx.builder.build_unconditional_branch(incr_bb)?;

            fn_ctx.builder.position_at_end(incr_bb);
            let next = fn_ctx.builder.build_int_add(
                i,
                fn_ctx.context.i64_type().const_int(1, false),
                "copyin_next",
            )?;
            fn_ctx.builder.build_store(i_ptr, next)?;
            fn_ctx.builder.build_unconditional_branch(cond_bb)?;

            fn_ctx.builder.position_at_end(done_bb);
            Ok(())
        }
        Ty::Shared(_) => {
            let retain_fn = fn_ctx
                .module
                .get_function("ntsc_shared_retain")
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError("ntsc_shared_retain not declared".into())
                })?;
            let current_fn = fn_ctx.function;
            let i_ptr = fn_ctx.alloca("copyin_shared_i", &Ty::Int)?;
            let cond_bb = fn_ctx
                .context
                .append_basic_block(current_fn, "copyin_shared.cond");
            let body_bb = fn_ctx
                .context
                .append_basic_block(current_fn, "copyin_shared.body");
            let incr_bb = fn_ctx
                .context
                .append_basic_block(current_fn, "copyin_shared.incr");
            let done_bb = fn_ctx
                .context
                .append_basic_block(current_fn, "copyin_shared.done");

            let len = fn_ctx
                .builder
                .build_call(len_fn, &[arr_handle.into()], "copyin_len")?
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
                .build_load(fn_ctx.context.i64_type(), i_ptr, "copyin_i")?
                .into_int_value();
            let cond = fn_ctx.builder.build_int_compare(
                inkwell::IntPredicate::SLT,
                i,
                len,
                "copyin_cond",
            )?;
            fn_ctx
                .builder
                .build_conditional_branch(cond, body_bb, done_bb)?;

            fn_ctx.builder.position_at_end(body_bb);
            let elem = fn_ctx
                .builder
                .build_call(get_fn, &[arr_handle.into(), i.into()], "copyin_elem")?
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            fn_ctx.builder.build_call(
                retain_fn,
                &[BasicMetadataValueEnum::IntValue(elem)],
                "copyin_retain",
            )?;
            fn_ctx.builder.build_unconditional_branch(incr_bb)?;

            fn_ctx.builder.position_at_end(incr_bb);
            let next = fn_ctx.builder.build_int_add(
                i,
                fn_ctx.context.i64_type().const_int(1, false),
                "copyin_next",
            )?;
            fn_ctx.builder.build_store(i_ptr, next)?;
            fn_ctx.builder.build_unconditional_branch(cond_bb)?;

            fn_ctx.builder.position_at_end(done_bb);
            Ok(())
        }
        _ => Ok(()),
    }
}

pub(crate) fn encode_array_scalar<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    val: &TypedValue<'ctx>,
) -> Result<BasicValueEnum<'ctx>, crate::CodegenError> {
    match &val.ntsc_type {
        Ty::Float => {
            let bits = fn_ctx.builder.build_bit_cast(
                val.value.into_float_value(),
                fn_ctx.context.i64_type(),
                "arr_f64_bits",
            )?;
            Ok(bits)
        }
        Ty::Bool => {
            let i1_val = val.value.into_int_value();
            let wide = fn_ctx.builder.build_int_z_extend(
                i1_val,
                fn_ctx.context.i64_type(),
                "arr_bool_ext",
            )?;
            Ok(wide.into())
        }
        // Class instances and function references travel as raw pointers;
        // the runtime stores element bits in an i64 slot, so the pointer is
        // truncated to its bit pattern on the way in.
        _ if val.value.is_pointer_value() => Ok(fn_ctx
            .builder
            .build_ptr_to_int(
                val.value.into_pointer_value(),
                fn_ctx.context.i64_type(),
                "arr_ptr_bits",
            )?
            .into()),
        _ => Ok(val.value),
    }
}

pub(crate) fn decode_array_scalar<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    raw: BasicValueEnum<'ctx>,
    elem_ty: &Ty,
) -> Result<BasicValueEnum<'ctx>, crate::CodegenError> {
    if *elem_ty == Ty::Float {
        let f = fn_ctx.builder.build_bit_cast(
            raw.into_int_value(),
            fn_ctx.context.f64_type(),
            "elem_f64",
        )?;
        return Ok(f);
    }
    if *elem_ty == Ty::Bool {
        let b = fn_ctx.builder.build_int_truncate(
            raw.into_int_value(),
            fn_ctx.context.bool_type(),
            "elem_bool",
        )?;
        return Ok(b.into());
    }
    // Pointer-represented elements (class instances, function references)
    // were stored as raw bits; restore the pointer so member access and
    // calls see the object, not an integer.
    if ty_is_llvm_pointer(elem_ty) {
        let p = fn_ctx.builder.build_int_to_ptr(
            raw.into_int_value(),
            fn_ctx.context.ptr_type(AddressSpace::default()),
            "elem_ptr",
        )?;
        return Ok(p.into());
    }
    Ok(raw)
}

pub(crate) fn emit_array_element<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    handle: inkwell::values::IntValue<'ctx>,
    index: inkwell::values::IntValue<'ctx>,
    elem_ty: &Ty,
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    if *elem_ty == Ty::Any {
        return emit_untyped_array_element(fn_ctx, handle, index);
    }
    let get_fn = fn_ctx
        .module
        .get_function("ntsc_array_get")
        .ok_or_else(|| crate::CodegenError::LLVMError("ntsc_array_get not declared".into()))?;
    let result = fn_ctx.builder.build_call(
        get_fn,
        &[
            inkwell::values::BasicMetadataValueEnum::IntValue(handle),
            inkwell::values::BasicMetadataValueEnum::IntValue(index),
        ],
        "elem_get",
    )?;
    let val = call_result_to_value(fn_ctx, &result);

    let decoded = decode_array_scalar(fn_ctx, val, elem_ty)?;
    Ok(TypedValue::new(decoded, elem_ty.clone()))
}

pub(crate) fn emit_array_elem_eq<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    elem: &TypedValue<'ctx>,
    search: &TypedValue<'ctx>,
    elem_ty: &Ty,
) -> Result<inkwell::values::IntValue<'ctx>, crate::CodegenError> {
    let builder = fn_ctx.builder;
    match elem_ty {
        Ty::Int => {
            let lhs = elem.value.into_int_value();
            let rhs = search.value.into_int_value();
            Ok(builder.build_int_compare(IntPredicate::EQ, lhs, rhs, "arr_eq_int")?)
        }
        Ty::Float => {
            let lhs = elem.value.into_float_value();
            let rhs = search.value.into_float_value();
            Ok(builder.build_float_compare(
                inkwell::FloatPredicate::UEQ,
                lhs,
                rhs,
                "arr_eq_float",
            )?)
        }
        Ty::Bool => {
            let lhs = elem.value.into_int_value();
            let rhs = search.value.into_int_value();
            Ok(builder.build_int_compare(IntPredicate::EQ, lhs, rhs, "arr_eq_bool")?)
        }
        _ => {
            let search_str = convert_to_string(fn_ctx, search)?;
            let eq_fn = fn_ctx
                .module
                .get_function("ntsc_string_equals")
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError("ntsc_string_equals not declared".into())
                })?;
            let result = builder.build_call(
                eq_fn,
                &[
                    inkwell::values::BasicMetadataValueEnum::IntValue(elem.value.into_int_value()),
                    inkwell::values::BasicMetadataValueEnum::IntValue(
                        search_str.value.into_int_value(),
                    ),
                ],
                "arr_eq_str",
            )?;
            let i8_val = call_result_to_value(fn_ctx, &result).into_int_value();
            Ok(builder.build_int_truncate(i8_val, fn_ctx.context.bool_type(), "arr_eq_i1")?)
        }
    }
}

pub(crate) fn emit_array_find_index<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    handle: inkwell::values::IntValue<'ctx>,
    len: inkwell::values::IntValue<'ctx>,
    elem_ty: &Ty,
    search: &TypedValue<'ctx>,
) -> Result<inkwell::values::IntValue<'ctx>, crate::CodegenError> {
    let ctx = fn_ctx.context;
    let i64_ty = ctx.i64_type();
    let current_fn = fn_ctx.function;

    let result_ptr = fn_ctx.alloca("find_result", &Ty::Int)?;
    fn_ctx
        .builder
        .build_store(result_ptr, i64_ty.const_int(u64::MAX, true))?;
    let idx_ptr = fn_ctx.alloca("find_idx", &Ty::Int)?;
    fn_ctx.builder.build_store(idx_ptr, i64_ty.const_zero())?;

    let cond_bb = ctx.append_basic_block(current_fn, "find.cond");
    let body_bb = ctx.append_basic_block(current_fn, "find.body");
    let match_bb = ctx.append_basic_block(current_fn, "find.match");
    let incr_bb = ctx.append_basic_block(current_fn, "find.incr");
    let merge_bb = ctx.append_basic_block(current_fn, "find.merge");

    fn_ctx.builder.build_unconditional_branch(cond_bb)?;
    fn_ctx.builder.position_at_end(cond_bb);
    let idx = fn_ctx
        .builder
        .build_load(i64_ty, idx_ptr, "find_idx")?
        .into_int_value();
    let cond = fn_ctx
        .builder
        .build_int_compare(IntPredicate::SLT, idx, len, "find_cond")?;
    fn_ctx
        .builder
        .build_conditional_branch(cond, body_bb, merge_bb)?;

    fn_ctx.builder.position_at_end(body_bb);
    let elem = emit_array_element(fn_ctx, handle, idx, elem_ty)?;
    let is_match = emit_array_elem_eq(fn_ctx, &elem, search, elem_ty)?;
    fn_ctx
        .builder
        .build_conditional_branch(is_match, match_bb, incr_bb)?;

    fn_ctx.builder.position_at_end(match_bb);
    fn_ctx.builder.build_store(result_ptr, idx)?;
    fn_ctx.builder.build_unconditional_branch(merge_bb)?;

    fn_ctx.builder.position_at_end(incr_bb);
    let next = fn_ctx
        .builder
        .build_int_add(idx, i64_ty.const_int(1, false), "find_next")?;
    fn_ctx.builder.build_store(idx_ptr, next)?;
    fn_ctx.builder.build_unconditional_branch(cond_bb)?;

    fn_ctx.builder.position_at_end(merge_bb);
    let result = fn_ctx
        .builder
        .build_load(i64_ty, result_ptr, "find_result")?
        .into_int_value();
    Ok(result)
}
