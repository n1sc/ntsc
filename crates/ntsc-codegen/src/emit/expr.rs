//! Expression, copy, and unary-operation emission.

use super::*;

pub(crate) fn emit_expression<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    expr: &Expr,
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    match expr {
        Expr::Literal { value, .. } => emit_literal(fn_ctx, value),
        Expr::Variable { name } => emit_variable(fn_ctx, name),
        Expr::Binary { left, op, right } => emit_binary(fn_ctx, left, op, right),
        Expr::Unary { op, right } => emit_unary(fn_ctx, op, right),
        Expr::Grouping { expression, .. } => emit_expression(fn_ctx, expression),
        Expr::Call {
            callee, arguments, ..
        } => emit_call(fn_ctx, callee, arguments),
        Expr::Assign { name, value } => emit_assign(fn_ctx, name, value),
        Expr::Borrow {
            target, mutable, ..
        } => emit_borrow(fn_ctx, target, *mutable),
        Expr::RawDeref { target, .. } => {
            let (ptr, pointee) = emit_pointer_operand(fn_ctx, target)?;
            let loaded = fn_ctx.builder.build_load(
                ty_to_llvm(&pointee, fn_ctx.context),
                ptr,
                "raw_deref",
            )?;
            Ok(TypedValue::new(loaded, pointee))
        }
        Expr::RawDerefSet { target, value, .. } => {
            let val = emit_expression(fn_ctx, value)?;
            let (ptr, pointee) = emit_pointer_operand(fn_ctx, target)?;
            let coerced = coerce_value(fn_ctx, val, &pointee)?;
            fn_ctx.builder.build_store(ptr, coerced.value)?;
            Ok(coerced)
        }
        Expr::Ternary {
            condition,
            then_branch,
            else_branch,
        } => emit_ternary(fn_ctx, condition, then_branch, else_branch),
        Expr::Propagate { value, .. } => emit_propagate(fn_ctx, value),
        Expr::Member { object, property } => {
            let obj_val = emit_expression(fn_ctx, object)?;
            let fresh_receiver = matches!(obj_val.ntsc_type, Ty::Class(_) | Ty::Own(_))
                && expr_is_fresh(fn_ctx, object, &obj_val);
            let obj_val = deref_shared(fn_ctx, obj_val)?;
            let member = emit_member_access(fn_ctx, &obj_val, property)?;
            if fresh_receiver {
                // Move the selected field out before reclaiming its temporary
                // owner; cloning a container could still alias its class elements.
                if ty_is_owned_handle(&member.ntsc_type)
                    && let Some(gep) = emit_member_gep(fn_ctx, &obj_val, property)?
                {
                    fn_ctx.builder.build_store(
                        gep.ptr,
                        default_llvm_value(&gep.field_ty, fn_ctx.context),
                    )?;
                }
                emit_drop_value(fn_ctx, &obj_val)?;
            }
            Ok(member)
        }
        Expr::MemberSet {
            object,
            property,
            value,
        } => {
            let val = emit_expression(fn_ctx, value)?;
            let obj_val = emit_expression(fn_ctx, object)?;
            let obj_val = deref_shared(fn_ctx, obj_val)?;

            // Get a pointer to the field via GEP, then store.
            if let Some(gep) = emit_member_gep(fn_ctx, &obj_val, property)? {
                store_into_field(fn_ctx, &gep, value, &val)?;
                Ok(val)
            } else {
                // Not a class or field not found — silently ignore store.
                Ok(val)
            }
        }
        Expr::IndexGet { object, index } => {
            let obj_val = emit_expression(fn_ctx, object)?;
            let obj_val = deref_shared(fn_ctx, obj_val)?;
            let idx_val = emit_expression(fn_ctx, index)?;
            // A window is bounds-checked against the window, not the array
            // behind it, so it reads through the slice ABI.
            if let Ty::Slice(element) = &obj_val.ntsc_type {
                let element = (**element).clone();
                let get_fn = fn_ctx
                    .module
                    .get_function("ntsc_slices_get")
                    .ok_or_else(|| {
                        crate::CodegenError::LLVMError("ntsc_slices_get not declared".into())
                    })?;
                let raw = fn_ctx
                    .builder
                    .build_call(
                        get_fn,
                        &[obj_val.value.into(), idx_val.value.into()],
                        "slice_get",
                    )?
                    .try_as_basic_value()
                    .unwrap_basic();
                fn_ctx.emit_pending_exception_check()?;
                let element = if element == Ty::Any { Ty::Int } else { element };
                let val = decode_array_scalar(fn_ctx, raw, &element)?;
                return Ok(TypedValue::new(val, element));
            }

            let mut elem_ty = Ty::Any;
            match &obj_val.ntsc_type {
                Ty::Array(inner) => elem_ty = *inner.clone(),

                // Indexing a view of an array reads through the borrow: the
                // view value is already the underlying array handle.
                Ty::View(inner, _) if matches!(**inner, Ty::Array(_)) => {
                    if let Ty::Array(inner_arr) = &**inner {
                        elem_ty = *inner_arr.clone();
                    }
                }
                _ => {}
            }
            if elem_ty == Ty::Any {
                return emit_untyped_array_element(
                    fn_ctx,
                    obj_val.value.into_int_value(),
                    idx_val.value.into_int_value(),
                );
            }
            let get_fn = fn_ctx
                .module
                .get_function("ntsc_array_get")
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError("ntsc_array_get not declared".into())
                })?;

            // `ntsc_array_get` returns the element value directly: a borrow
            // of the owned string handle for string arrays, the raw scalar
            // bits for scalar arrays, 0 when out of bounds.
            let raw = fn_ctx
                .builder
                .build_call(
                    get_fn,
                    &[obj_val.value.into(), idx_val.value.into()],
                    "array_get",
                )?
                .try_as_basic_value()
                .unwrap_basic();

            // An out-of-bounds read throws from the runtime; the pending
            // exception must reach the enclosing handler instead of
            // silently consuming the failure value.
            fn_ctx.emit_pending_exception_check()?;
            let val = decode_array_scalar(fn_ctx, raw, &elem_ty)?;
            let mut tv = TypedValue::new(val, elem_ty);

            // A fresh container used only to read an element (e.g. `a[i][j]`
            // or `makeArr()[0]`) is not owned anywhere else; its owned value
            // is dropped once the element has been read. The element is a
            // borrow into the container, so when it is itself an owned
            // (array/string) value it must be copied out first or the drop
            // would free it out from under the caller.
            if expr_is_fresh(fn_ctx, object, &obj_val) {
                if matches!(tv.ntsc_type, Ty::Array(_) | Ty::String) {
                    tv = copy_owned_value(fn_ctx, &tv)?;
                }
                emit_drop_value(fn_ctx, &obj_val)?;
            }
            Ok(tv)
        }
        Expr::IndexSet {
            object,
            index,
            value,
        } => {
            let val = emit_expression(fn_ctx, value)?;
            let obj_val = emit_expression(fn_ctx, object)?;
            let obj_val = deref_shared(fn_ctx, obj_val)?;
            let idx_val = emit_expression(fn_ctx, index)?;

            // Nested arrays, option cells, and shared boxes are owned by
            // their container, but `ntsc_array_set` only reclaims the
            // replaced element when it is a string. Read the old element
            // first so it can be reclaimed after the overwrite.
            let old_val = if matches!(val.ntsc_type, Ty::Array(_) | Ty::Option(_) | Ty::Shared(_)) {
                let get_fn = fn_ctx
                    .module
                    .get_function("ntsc_array_get")
                    .ok_or_else(|| {
                        crate::CodegenError::LLVMError("ntsc_array_get not declared".into())
                    })?;
                let old = fn_ctx
                    .builder
                    .build_call(
                        get_fn,
                        &[obj_val.value.into(), idx_val.value.into()],
                        "array_set_old",
                    )?
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();
                Some(TypedValue::new(old.into(), val.ntsc_type.clone()))
            } else {
                None
            };

            // Option cells and shared boxes are stored by value: give the
            // array its own copy (cloned cell / retained box) so the
            // caller's value keeps its own.
            let set_val = if let Ty::Option(inner) = &val.ntsc_type {
                let inner = (**inner).clone();
                let cell = clone_option_value(fn_ctx, &inner, &val)?;
                TypedValue::new(
                    fn_ctx
                        .builder
                        .build_ptr_to_int(cell, fn_ctx.context.i64_type(), "index_set_cell")?
                        .into(),
                    val.ntsc_type.clone(),
                )
            } else if matches!(val.ntsc_type, Ty::Shared(_)) {
                let retain_fn = fn_ctx
                    .module
                    .get_function("ntsc_shared_retain")
                    .ok_or_else(|| {
                        crate::CodegenError::LLVMError("ntsc_shared_retain not declared".into())
                    })?;
                fn_ctx.builder.build_call(
                    retain_fn,
                    &[inkwell::values::BasicMetadataValueEnum::IntValue(
                        val.value.into_int_value(),
                    )],
                    "index_set_retain",
                )?;
                val.clone()
            } else {
                val.clone()
            };

            let set_fn = fn_ctx
                .module
                .get_function("ntsc_array_set")
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError("ntsc_array_set not declared".into())
                })?;

            let elem_bits = encode_array_scalar(fn_ctx, &set_val)?;
            fn_ctx.builder.build_call(
                set_fn,
                &[obj_val.value.into(), idx_val.value.into(), elem_bits.into()],
                "array_set",
            )?;

            fn_ctx.emit_pending_exception_check()?;
            // `ntsc_array_set` deep-copies a string element into a fresh
            // handle owned by the array (reclaiming the old one) and
            // replaces scalar elements by value, so the caller's value is
            // never borrowed. An out-of-bounds write throws from the
            // runtime, which the check above propagated.
            if let Some(old) = old_val {
                emit_drop_value(fn_ctx, &old)?;
            }
            Ok(val)
        }
        Expr::TupleLiteral { elements, .. } => {
            let mut elem_vals = Vec::new();
            let mut elem_tys = Vec::new();
            for elem in elements {
                let val = emit_expression(fn_ctx, elem)?;
                elem_vals.push(val.value);
                elem_tys.push(val.ntsc_type.clone());
            }
            let ty = Ty::Tuple(elem_tys);
            let ll_ty = ty_to_llvm(&ty, fn_ctx.context);
            if let inkwell::types::BasicTypeEnum::StructType(st) = ll_ty {
                let mut agg: inkwell::values::AggregateValueEnum = st.get_undef().into();
                for (i, val) in elem_vals.iter().enumerate() {
                    agg = fn_ctx
                        .builder
                        .build_insert_value(agg, *val, i as u32, "tuple_insert")?;
                }
                let sv = agg.into_struct_value();
                Ok(TypedValue::new(sv.into(), ty))
            } else {
                unreachable!()
            }
        }
        Expr::TupleIndex {
            object,
            index,
            dot_span,
        } => {
            let obj_val = emit_expression(fn_ctx, object)?;
            if let Ty::Tuple(element_tys) = &obj_val.ntsc_type
                && *index < element_tys.len()
            {
                let element_ty = element_tys[*index].clone();
                let ll_ty = ty_to_llvm(&obj_val.ntsc_type, fn_ctx.context);
                if let inkwell::types::BasicTypeEnum::StructType(_st) = ll_ty {
                    let extracted = fn_ctx.builder.build_extract_value(
                        obj_val.value.into_struct_value(),
                        *index as u32,
                        "tuple_idx",
                    )?;
                    return Ok(TypedValue::new(extracted, element_ty));
                }
            }
            Err(crate::CodegenError::LLVMError(format!(
                "cannot index into non-tuple at {:?}",
                dot_span
            )))
        }
        Expr::This { .. } => {
            if let Some((ptr, ty)) = fn_ctx.lookup_var("this") {
                let loaded =
                    fn_ctx
                        .builder
                        .build_load(ty_to_llvm(ty, fn_ctx.context), ptr, "this_load")?;
                Ok(TypedValue::new(loaded, ty.clone()))
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
        Expr::Lambda {
            params,
            return_type,
            body,
            ..
        } => {
            // Generate an anonymous function and return a pointer to it,
            // with no capture: lambdas are plain closures over nothing.
            let name = format!(
                "__lambda_{}",
                fn_ctx.function.get_name().to_str().unwrap_or("anon")
            );
            let (fn_ty, _) = fn_type_from_params(fn_ctx.context, params, return_type);
            let function =
                fn_ctx
                    .module
                    .add_function(&name, fn_ty, Some(inkwell::module::Linkage::Internal));

            if !body.is_empty() {
                let entry = fn_ctx.context.append_basic_block(function, "entry");
                let builder = fn_ctx.context.create_builder();
                builder.position_at_end(entry);
                let entry_builder = fn_ctx.context.create_builder();
                entry_builder.position_at_end(entry);

                let ret_ty = function_return_ty(return_type);
                let mut lambda_ctx = FunctionContext::new(
                    function,
                    &builder,
                    &entry_builder,
                    entry,
                    fn_ctx.module,
                    ret_ty.clone(),
                    fn_ctx.context,
                );

                // Owned lambda params (arrays/strings, not `view`) own the
                // value passed by the caller and must drop it at exit.
                for (i, param) in params.iter().enumerate() {
                    let pty = type_annotation_to_ty(&param.type_annotation);
                    let ptr = lambda_ctx.alloca(param.name.lexeme(), &pty)?;
                    let arg_value = function.get_nth_param(i as u32).ok_or_else(|| {
                        crate::CodegenError::LLVMError(format!(
                            "missing lambda param {}",
                            param.name.lexeme()
                        ))
                    })?;
                    lambda_ctx.builder.build_store(ptr, arg_value)?;
                    lambda_ctx.define_var(param.name.lexeme(), ptr, pty.clone());

                    lambda_ctx.mark_owned_if_heap(param.name.lexeme(), &pty);
                }

                for stmt in body {
                    emit_statement_in_function(&mut lambda_ctx, stmt)?;
                }

                emit_exception_return(&mut lambda_ctx, &ret_ty, fn_ctx.context)?;
                let current_block = lambda_ctx.builder.get_insert_block().unwrap();
                if current_block.get_terminator().is_none() {
                    emit_drop_all_owned(&mut lambda_ctx)?;
                    if ret_ty == Ty::Void {
                        lambda_ctx.builder.build_return(None)?;
                    } else {
                        let default = default_llvm_value(&ret_ty, fn_ctx.context);
                        lambda_ctx.builder.build_return(Some(&default))?;
                    }
                }
            }

            let ptr = function.as_global_value().as_pointer_value();
            let param_tys: Vec<Ty> = params
                .iter()
                .map(|p| type_annotation_to_ty(&p.type_annotation))
                .collect();
            Ok(TypedValue::new(
                ptr.into(),
                Ty::Function {
                    params: param_tys,
                    return_type: Box::new(function_return_ty(return_type)),
                },
            ))
        }
        Expr::ArrayLiteral { elements, span } => {
            // Unroll array-literal spreads into their elements so the count
            // and element types are known statically.
            let flat = flatten_array_elements(elements)?;

            // Emit all elements first so their types select the element
            // size and string flag.
            let elem_values: Vec<TypedValue<'ctx>> = flat
                .iter()
                .map(|e| emit_expression(fn_ctx, e))
                .collect::<Result<_, _>>()?;

            let elem_ty = match flat.first() {
                Some(_) => elem_values[0].ntsc_type.clone(),
                None => Ty::Any,
            };

            // Determine the per-element size in bytes.
            let elem_size: i64 = match &elem_ty {
                Ty::Bool => 1,
                _ => 8,
            };
            let elem_size_val = fn_ctx.context.i64_type().const_int(elem_size as u64, false);
            let count_val = fn_ctx
                .context
                .i64_type()
                .const_int(flat.len() as u64, false);

            let string_elems = matches!(&elem_ty, Ty::String | Ty::Any);
            let string_elems_val = fn_ctx
                .context
                .i8_type()
                .const_int(u64::from(string_elems), false);

            // The runtime deep-copies string elements, but option cells and
            // shared boxes are stored by value: each element must be copied
            // (cloned cell / retained box) so the array owns its own copy
            // and the source keeps its own.
            let elem_values: Vec<TypedValue<'ctx>> = if let Ty::Option(inner) = &elem_ty {
                let inner = (**inner).clone();
                elem_values
                    .into_iter()
                    .map(|e| {
                        let cell = clone_option_value(fn_ctx, &inner, &e)?;
                        Ok(TypedValue::new(
                            fn_ctx
                                .builder
                                .build_ptr_to_int(
                                    cell,
                                    fn_ctx.context.i64_type(),
                                    "literal_opt_cell",
                                )?
                                .into(),
                            Ty::Option(Box::new(inner.clone())),
                        ))
                    })
                    .collect::<Result<_, crate::CodegenError>>()?
            } else if matches!(&elem_ty, Ty::Shared(_)) {
                let retain_fn = fn_ctx
                    .module
                    .get_function("ntsc_shared_retain")
                    .ok_or_else(|| {
                        crate::CodegenError::LLVMError("ntsc_shared_retain not declared".into())
                    })?;
                elem_values
                    .into_iter()
                    .map(|e| {
                        fn_ctx.builder.build_call(
                            retain_fn,
                            &[inkwell::values::BasicMetadataValueEnum::IntValue(
                                e.value.into_int_value(),
                            )],
                            "literal_shared_retain",
                        )?;
                        Ok(e)
                    })
                    .collect::<Result<_, crate::CodegenError>>()?
            } else {
                elem_values
            };

            let arr_new_fn = fn_ctx
                .module
                .get_function("ntsc_array_new_typed")
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError("ntsc_array_new_typed not declared".into())
                })?;

            let arr_result = fn_ctx.builder.build_call(
                arr_new_fn,
                &[
                    inkwell::values::BasicMetadataValueEnum::IntValue(elem_size_val),
                    inkwell::values::BasicMetadataValueEnum::IntValue(count_val),
                    inkwell::values::BasicMetadataValueEnum::IntValue(string_elems_val),
                ],
                "array_new",
            )?;

            let arr_val = call_result_to_value(fn_ctx, &arr_result);
            let arr_handle = arr_val.into_int_value();

            if let Some(mark_fn) = fn_ctx.module.get_function("ntsc_leak_mark") {
                let line = fn_ctx
                    .context
                    .i64_type()
                    .const_int(u64::from(span.line), false);
                let column = fn_ctx
                    .context
                    .i64_type()
                    .const_int(u64::from(span.column), false);
                fn_ctx.builder.build_call(
                    mark_fn,
                    &[arr_handle.into(), line.into(), column.into()],
                    "leak_mark",
                )?;
            }

            if let Some(push_fn_val) = fn_ctx.module.get_function("ntsc_array_push") {
                // Push each element into the array. The runtime stores the
                // element *value* directly: an owned copy of a string
                // handle, the raw value for scalars.
                for elem_val in elem_values.into_iter() {
                    let elem_bits = encode_array_scalar(fn_ctx, &elem_val)?;
                    fn_ctx.builder.build_call(
                        push_fn_val,
                        &[
                            inkwell::values::BasicMetadataValueEnum::IntValue(arr_handle),
                            elem_bits.into(),
                        ],
                        "array_push",
                    )?;
                }
            }

            Ok(TypedValue::new(arr_val, Ty::Array(Box::new(elem_ty))))
        }
        Expr::ObjectLiteral { properties, .. } => {
            let mut json = emit_string_const(fn_ctx, "{")?;
            let mut json_owned = false;
            let fold = |fn_ctx: &mut FunctionContext<'ctx, '_>,
                        json: &mut BasicValueEnum<'ctx>,
                        json_owned: &mut bool,
                        piece: BasicValueEnum<'ctx>,
                        piece_owned: bool|
             -> Result<(), crate::CodegenError> {
                let combined = emit_concat(fn_ctx, *json, piece)?;
                if *json_owned {
                    emit_drop_value(fn_ctx, &TypedValue::new(*json, Ty::String))?;
                }
                if piece_owned {
                    emit_drop_value(fn_ctx, &TypedValue::new(piece, Ty::String))?;
                }
                *json = combined;
                *json_owned = true;
                Ok(())
            };
            for (index, prop) in properties.iter().enumerate() {
                if index > 0 {
                    let comma = emit_string_const(fn_ctx, ",")?;
                    fold(fn_ctx, &mut json, &mut json_owned, comma, false)?;
                }
                let value = emit_expression(fn_ctx, &prop.value)?;
                let (json_value, json_value_owned) = emit_json_value(fn_ctx, &value)?;

                // A fresh property value (a concatenation, a call result) has no
                // owning slot, and the object keeps only its JSON text, so the
                // temporary is reclaimed here.
                if expr_is_fresh(fn_ctx, &prop.value, &value) {
                    emit_drop_value(fn_ctx, &value)?;
                }
                let key = emit_string_const(fn_ctx, &format!("\"{}\":", prop.key))?;
                fold(fn_ctx, &mut json, &mut json_owned, key, false)?;
                fold(
                    fn_ctx,
                    &mut json,
                    &mut json_owned,
                    json_value,
                    json_value_owned,
                )?;
            }
            // The literal is lowered to its JSON text, then parsed. Each
            // `emit_concat` allocates a new string, and each interpolated
            // value may allocate one too, so every intermediate is reclaimed
            // as soon as it has been folded into the next: only the interned
            // literal handles from `emit_string_const` are left alone, since
            // they are permanent and shared by every use of the same text.
            let close = emit_string_const(fn_ctx, "}")?;
            fold(fn_ctx, &mut json, &mut json_owned, close, false)?;

            let parse_fn = fn_ctx
                .module
                .get_function("ntsc_json_parse")
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError("ntsc_json_parse not declared".into())
                })?;
            let result = fn_ctx
                .builder
                .build_call(parse_fn, &[json.into()], "json_parse")?;
            let parsed = call_result_to_value(fn_ctx, &result);
            if json_owned {
                emit_drop_value(fn_ctx, &TypedValue::new(json, Ty::String))?;
            }
            Ok(TypedValue::new(parsed, Ty::Object))
        }
        Expr::OptionalMember { object, property } => {
            let obj_val = emit_expression(fn_ctx, object)?;
            let obj_val = deref_shared(fn_ctx, obj_val)?;

            // Null check: if the pointer is null, return nil (null ptr);
            // otherwise load the member.
            if obj_val.value.is_pointer_value() {
                let ptr = obj_val.value.into_pointer_value();
                let current_fn = fn_ctx.function;
                let null_bb = fn_ctx
                    .context
                    .append_basic_block(current_fn, "opt_member.null");
                let nonnull_bb = fn_ctx
                    .context
                    .append_basic_block(current_fn, "opt_member.nonnull");
                let merge_bb = fn_ctx
                    .context
                    .append_basic_block(current_fn, "opt_member.merge");

                let is_null = fn_ctx.builder.build_is_null(ptr, "is_null")?;
                fn_ctx
                    .builder
                    .build_conditional_branch(is_null, null_bb, nonnull_bb)?;

                fn_ctx.builder.position_at_end(null_bb);
                let null_val: BasicValueEnum<'ctx> = fn_ctx
                    .context
                    .ptr_type(AddressSpace::default())
                    .const_null()
                    .into();
                fn_ctx.builder.build_unconditional_branch(merge_bb)?;

                fn_ctx.builder.position_at_end(nonnull_bb);
                let loaded = emit_member_access(fn_ctx, &obj_val, property)?;
                let loaded_val = loaded.value;
                // Box non-pointer values as pointer-sized so the PHI types
                // match.
                let loaded_ptr_val: BasicValueEnum<'ctx> = if loaded_val.is_pointer_value() {
                    loaded_val
                } else {
                    fn_ctx
                        .context
                        .ptr_type(AddressSpace::default())
                        .const_null()
                        .into()
                };
                fn_ctx.builder.build_unconditional_branch(merge_bb)?;

                // Merge with PHI: null on the null branch, the member value
                // (as a pointer) on the non-null branch.
                fn_ctx.builder.position_at_end(merge_bb);
                let phi = fn_ctx.builder.build_phi(
                    fn_ctx.context.ptr_type(AddressSpace::default()),
                    "opt_member_result",
                )?;
                phi.add_incoming(&[(&null_val, null_bb), (&loaded_ptr_val, nonnull_bb)]);
                Ok(TypedValue::new(phi.as_basic_value(), Ty::Any))
            } else {
                emit_member_access(fn_ctx, &obj_val, property)
            }
        }
        Expr::Spread { value, .. } => emit_expression(fn_ctx, value),
        Expr::View {
            target, mutable, ..
        } => {
            let tv = emit_expression(fn_ctx, target)?;
            // A view is a non-owning borrow of a heap value; it shares the
            // representation of its target (a borrowed pointer), so the
            // target is emitted as-is and only the tracked type becomes the
            // view type.
            let inner = match &tv.ntsc_type {
                Ty::View(inner, _) => (**inner).clone(),
                other => other.clone(),
            };
            Ok(TypedValue::new(
                tv.value,
                Ty::View(Box::new(inner), *mutable),
            ))
        }
        Expr::Copy { expression, .. } => emit_copy(fn_ctx, expression),
        Expr::Await { .. } => Err(crate::CodegenError::LLVMError(
            "internal: await must be lowered by the async state machine, not emitted directly"
                .into(),
        )),
        Expr::AsyncBlock { span, .. } => {
            let anon_name = fn_ctx
                .block_span_to_name
                .as_ref()
                .and_then(|m| m.get(&span.start))
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError(
                        "internal: async block not found in block_span_to_name".into(),
                    )
                })?;

            let struct_name = format!("ntsc_future_{anon_name}");
            let future_ty = fn_ctx.module.get_struct_type(&struct_name).ok_or_else(|| {
                crate::CodegenError::LLVMError(format!(
                    "internal: async block future struct {struct_name} not declared"
                ))
            })?;

            let future_size = future_ty.size_of().ok_or_else(|| {
                crate::CodegenError::LLVMError(format!("internal: {struct_name} has no size"))
            })?;
            let future_ptr = fn_ctx.builder.build_alloca(future_ty, "anon_future")?;
            let zero = fn_ctx.context.i8_type().const_zero();
            fn_ctx
                .builder
                .build_memset(future_ptr, 1, zero, future_size)?;

            let state_ptr =
                fn_ctx
                    .builder
                    .build_struct_gep(future_ty, future_ptr, 0, "state_ptr")?;
            fn_ctx
                .builder
                .build_store(state_ptr, fn_ctx.context.i32_type().const_int(0, false))?;

            let handle = fn_ctx.builder.build_ptr_to_int(
                future_ptr,
                fn_ctx.context.i64_type(),
                "anon_handle",
            )?;

            Ok(TypedValue::new(handle.into(), Ty::Int))
        }
        Expr::PostfixUnary { op, left } => {
            // i++ / i-- — increment/decrement a variable in place and return
            // the OLD value.
            if let Expr::Variable { name } = left.as_ref() {
                if let Some((ptr, ty)) = fn_ctx.lookup_var(name.lexeme()) {
                    let ty = ty.clone();
                    let llvm_ty = ty_to_llvm(&ty, fn_ctx.context);
                    let old_val = fn_ctx.builder.build_load(llvm_ty, ptr, "postfix_old")?;
                    match (&op.kind, &ty) {
                        (TokenKind::PlusPlus, Ty::Int) => {
                            let one = fn_ctx.context.i64_type().const_int(1, false);
                            let new_val = emit_checked_int_arith(
                                fn_ctx,
                                IntArith::Add,
                                old_val.into_int_value(),
                                one,
                            )?;
                            fn_ctx.builder.build_store(ptr, new_val)?;
                        }
                        (TokenKind::MinusMinus, Ty::Int) => {
                            let one = fn_ctx.context.i64_type().const_int(1, false);
                            let new_val = emit_checked_int_arith(
                                fn_ctx,
                                IntArith::Sub,
                                old_val.into_int_value(),
                                one,
                            )?;
                            fn_ctx.builder.build_store(ptr, new_val)?;
                        }
                        (TokenKind::PlusPlus, Ty::Float) => {
                            let one = fn_ctx.context.f64_type().const_float(1.0);
                            let new_val = fn_ctx.builder.build_float_add(
                                old_val.into_float_value(),
                                one,
                                "postinc_f",
                            )?;
                            fn_ctx.builder.build_store(ptr, new_val)?;
                        }
                        (TokenKind::MinusMinus, Ty::Float) => {
                            let one = fn_ctx.context.f64_type().const_float(1.0);
                            let new_val = fn_ctx.builder.build_float_sub(
                                old_val.into_float_value(),
                                one,
                                "postdec_f",
                            )?;
                            fn_ctx.builder.build_store(ptr, new_val)?;
                        }
                        _ => {}
                    }
                    Ok(TypedValue::new(old_val, ty))
                } else {
                    Err(crate::CodegenError::LLVMError(format!(
                        "postfix unary on undeclared variable `{}`",
                        name.lexeme()
                    )))
                }
            } else {
                emit_expression(fn_ctx, left)
            }
        }
        Expr::StructLiteral {
            class_name,
            fields,
            update,
            ..
        } => {
            let class_name_str = class_name.lexeme();

            // Look up the struct type.
            let struct_ty = fn_ctx
                .module
                .get_struct_type(class_name_str)
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError(format!("`{class_name_str}` is not a class"))
                })?;

            let update_val = update
                .as_ref()
                .map(|u| emit_expression(fn_ctx, u))
                .transpose()?;

            if fn_ctx
                .module
                .get_function(&format!("{class_name_str}.init"))
                .is_some()
            {
                // Normal path: `ClassName { x: 1, y: 2 }` ≡
                // `ClassName.init(1, 2)`, with the values ordered by the
                // class's declared field order. Fields missing from the
                // literal are read from the `..base` update; the argument is
                // presented as a synthetic `Member` read so ownership
                // handling deep-copies strings/arrays instead of moving the
                // base.
                let declared_fields = class_all_fields(class_name_str);
                let mut args: Vec<Expr> = Vec::new();
                let mut arg_values: Vec<TypedValue> = Vec::new();
                for field_name in &declared_fields {
                    if let Some(prop) = fields.iter().find(|p| &p.key == field_name) {
                        let val = emit_expression(fn_ctx, &prop.value)?;
                        args.push(prop.value.clone());
                        arg_values.push(val);
                    } else if let (Some(update_expr), Some(update_val)) =
                        (update.as_ref(), update_val.as_ref())
                    {
                        let field_token = ntsc_ast::token::Token::new(
                            ntsc_ast::token::TokenKind::Identifier(field_name.clone()),
                            ntsc_ast::span::Span::dummy(),
                        );
                        let val = emit_member_access(fn_ctx, update_val, &field_token)?;
                        args.push(Expr::Member {
                            object: update_expr.clone(),
                            property: field_token,
                        });
                        arg_values.push(val);
                    }
                }
                emit_class_constructor(fn_ctx, struct_ty, class_name_str, &args, &arg_values, None)
            } else {
                // No `init`: allocate a zeroed instance and assign the named
                // fields directly. Fields missing from the literal are
                // copied out of the `..base` update (deep-copied for owned
                // handles so both instances stay independent).
                let obj_val =
                    emit_class_constructor(fn_ctx, struct_ty, class_name_str, &[], &[], None)?;
                for prop in fields {
                    let val = emit_expression(fn_ctx, &prop.value)?;
                    let field_token = ntsc_ast::token::Token::new(
                        ntsc_ast::token::TokenKind::Identifier(prop.key.clone()),
                        ntsc_ast::span::Span::dummy(),
                    );
                    if let Some(gep) = emit_member_gep(fn_ctx, &obj_val, &field_token)? {
                        store_into_field(fn_ctx, &gep, &prop.value, &val)?;
                    }
                }
                if let (Some(update_expr), Some(update_val)) =
                    (update.as_ref(), update_val.as_ref())
                {
                    let declared_fields = class_all_fields(class_name_str);
                    for field_name in &declared_fields {
                        if fields.iter().any(|p| &p.key == field_name) {
                            continue;
                        }
                        let field_token = ntsc_ast::token::Token::new(
                            ntsc_ast::token::TokenKind::Identifier(field_name.clone()),
                            ntsc_ast::span::Span::dummy(),
                        );
                        let Some(gep) = emit_member_gep(fn_ctx, &obj_val, &field_token)? else {
                            continue;
                        };
                        let read = emit_member_access(fn_ctx, update_val, &field_token)?;
                        let copied = copy_owned_value(fn_ctx, &read)?;
                        store_into_field(
                            fn_ctx,
                            &gep,
                            &Expr::Member {
                                object: update_expr.clone(),
                                property: field_token,
                            },
                            &copied,
                        )?;
                    }
                }
                Ok(obj_val)
            }
        }
        Expr::ChanSend { .. } | Expr::ChanRecv { .. } => {
            Err(crate::CodegenError::LLVMError(
                "a channel send/receive must be a top-level statement of an async function; it is not yet supported here".into(),
            ))
        }
        Expr::Close { channel, .. } => {
            let chan = emit_expression(fn_ctx, channel)?;
            let close_fn = fn_ctx
                .module
                .get_function("ntask_chan_close")
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError("ntask_chan_close not declared".into())
                })?;
            fn_ctx.builder.build_call(
                close_fn,
                &[chan.value.into_int_value().into()],
                "chan_close",
            )?;
            Ok(TypedValue::new(
                default_llvm_value(&Ty::Void, fn_ctx.context),
                Ty::Void,
            ))
        }
    }
}

/// Recover the receiver's static type for member freshness without emitting it
/// again. In particular, a function returning a view is not an owned receiver.
pub(crate) fn member_receiver_type(fn_ctx: &FunctionContext<'_, '_>, expr: &Expr) -> Option<Ty> {
    match expr {
        Expr::Variable { name } => fn_ctx.lookup_var(name.lexeme()).map(|(_, ty)| ty.clone()),
        Expr::Grouping { expression, .. } => member_receiver_type(fn_ctx, expression),
        Expr::Copy { expression, .. } => {
            let ty = member_receiver_type(fn_ctx, expression)?;
            Some(match ty {
                Ty::View(inner, _) | Ty::Shared(inner) => *inner,
                Ty::Slice(inner) => Ty::Array(inner),
                other => other,
            })
        }
        Expr::Call {
            callee, arguments, ..
        } => match callee.as_ref() {
            Expr::Variable { name } if is_class_type(name.lexeme()) => {
                Some(Ty::Class(name.lexeme().to_string()))
            }
            Expr::Variable { name } if name.lexeme() == "alloc" => {
                member_receiver_type(fn_ctx, arguments.first()?).map(|ty| Ty::Own(Box::new(ty)))
            }
            Expr::Variable { name } => function_declared_ret_ty(name.lexeme()),
            Expr::Member { object, property } => {
                let ty = member_receiver_type(fn_ctx, object)?;
                class_method_ret_ty(member_receiver_class(&ty)?, property.lexeme())
            }
            _ => None,
        },
        Expr::Member { object, property } => {
            let ty = member_receiver_type(fn_ctx, object)?;
            let class = member_receiver_class(&ty)?;
            let index = class_field_index(class, property.lexeme())?;
            class_all_field_types(class).get(index).cloned()
        }
        Expr::StructLiteral { class_name, .. } => Some(Ty::Class(class_name.lexeme().to_string())),
        _ => None,
    }
}

fn member_receiver_class(ty: &Ty) -> Option<&str> {
    match ty {
        Ty::Class(name) => Some(name),
        Ty::Own(inner) | Ty::Shared(inner) | Ty::View(inner, _) | Ty::Ref(inner, _) => {
            member_receiver_class(inner)
        }
        _ => None,
    }
}

// ── Pointers and references ─────────────────────────────────────────────

/// Lower `operand?`: branch on the result cell's tag. The Err path converts
/// the payload to the enclosing function's error type, boxes it into a fresh
/// cell of the function's return shape, drops every owned local, and returns
/// from the enclosing function; the Ok path yields the Ok payload. A fresh
/// cell is adopted (freed after its active payload is read out); a shared
/// one has its payload deep-copied.
fn emit_propagate<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    operand: &Expr,
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    let val = emit_expression(fn_ctx, operand)?;
    let (operand_ok, operand_err) = match &val.ntsc_type {
        Ty::Result { ok, err } => ((**ok).clone(), (**err).clone()),
        _ => {
            return Err(crate::CodegenError::LLVMError(
                "`?` requires a `result[.., ..]` operand".into(),
            ));
        }
    };
    let (fn_ok, fn_err) = match &fn_ctx.return_type {
        Ty::Result { ok, err } => ((**ok).clone(), (**err).clone()),
        _ => {
            return Err(crate::CodegenError::LLVMError(
                "`?` requires an enclosing function returning `result[.., ..]`".into(),
            ));
        }
    };
    // An async function's early return must go through its poll state
    // machine instead of a plain `return`; propagation there is not
    // supported yet.
    if fn_ctx.future_base.is_some() {
        return Err(crate::CodegenError::LLVMError(
            "`?` is not supported inside async functions yet".into(),
        ));
    }

    let cell = option_cell_pointer(fn_ctx, val.value)?;
    let fresh = expr_is_fresh(fn_ctx, operand, &val);

    let current_fn = fn_ctx.function;
    let ok_bb = fn_ctx
        .context
        .append_basic_block(current_fn, "propagate.ok");
    let err_bb = fn_ctx
        .context
        .append_basic_block(current_fn, "propagate.err");
    let merge_bb = fn_ctx
        .context
        .append_basic_block(current_fn, "propagate.merge");

    let tag = result_tag(fn_ctx, cell)?;
    let is_ok = fn_ctx.builder.build_int_compare(
        IntPredicate::EQ,
        tag,
        fn_ctx.context.i64_type().const_zero(),
        "propagate_is_ok",
    )?;
    fn_ctx
        .builder
        .build_conditional_branch(is_ok, ok_bb, err_bb)?;

    // Ok path: extract the payload for the caller of this expression.
    fn_ctx.builder.position_at_end(ok_bb);
    let loaded = load_result_payload(fn_ctx, cell, true, &operand_ok)?;
    let payload = TypedValue::new(loaded, operand_ok.clone());
    let payload = if fresh {
        free_result_cell(fn_ctx, cell)?;
        payload
    } else {
        emit_copy_value(fn_ctx, payload)?
    };
    let ok_payload = payload;
    fn_ctx.builder.build_unconditional_branch(merge_bb)?;

    // Err path: re-box the error as this function's own result and return it.
    fn_ctx.builder.position_at_end(err_bb);
    let loaded = load_result_payload(fn_ctx, cell, false, &operand_err)?;
    let payload = TypedValue::new(loaded, operand_err);
    let converted = if fn_err == Ty::String && payload.ntsc_type != Ty::String {
        // The function reports errors as strings: stringify any error value.
        let stringified = convert_to_string(fn_ctx, &payload)?;
        if fresh {
            free_result_cell(fn_ctx, cell)?;
        }
        stringified
    } else if fresh {
        free_result_cell(fn_ctx, cell)?;
        payload
    } else {
        emit_copy_value(fn_ctx, payload)?
    };
    let converted = coerce_value(fn_ctx, converted, &fn_err)?;
    let boxed = box_result_value(fn_ctx, &fn_ok, &fn_err, operand, &converted, false)?;
    emit_drop_all_owned(fn_ctx)?;
    fn_ctx.builder.build_return(Some(&boxed.value))?;

    fn_ctx.builder.position_at_end(merge_bb);
    Ok(TypedValue::new(ok_payload.value, operand_ok))
}

/// Evaluate a pointer-typed operand and yield its address plus the pointee
/// type. References, raw pointers, and owning allocations all carry the
/// address of the pointee's storage.
pub(crate) fn emit_pointer_operand<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    target: &Expr,
) -> Result<(PointerValue<'ctx>, Ty), crate::CodegenError> {
    let tv = emit_expression(fn_ctx, target)?;
    match &tv.ntsc_type {
        Ty::RawPointer(inner, _) | Ty::Ref(inner, _) | Ty::Own(inner) => {
            Ok((tv.value.into_pointer_value(), (**inner).clone()))
        }
        other => Err(crate::CodegenError::LLVMError(format!(
            "cannot dereference `{other}` as a pointer"
        ))),
    }
}

/// Emit `&place` / `&mut place`. Only a place expression can be borrowed; a
/// temporary has no storage to point at.
pub(crate) fn emit_borrow<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    target: &Expr,
    mutable: bool,
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    match target {
        Expr::Variable { name } => {
            let (slot, ty) = fn_ctx
                .lookup_var(name.lexeme())
                .map(|(ptr, ty)| (ptr, ty.clone()))
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError(format!(
                        "cannot take a reference to undeclared variable `{}`",
                        name.lexeme()
                    ))
                })?;
            reference_to_place(fn_ctx, slot, &ty, mutable)
        }
        Expr::Member { object, property } => {
            let obj_val = emit_expression(fn_ctx, object)?;
            let obj_val = deref_shared(fn_ctx, obj_val)?;
            let gep = emit_member_gep(fn_ctx, &obj_val, property)?.ok_or_else(|| {
                crate::CodegenError::LLVMError(format!(
                    "cannot take a reference to field `{}`",
                    property.lexeme()
                ))
            })?;
            let field_ty = gep.field_ty.clone();
            reference_to_place(fn_ctx, gep.ptr, &field_ty, mutable)
        }
        Expr::Grouping { expression, .. } => emit_borrow(fn_ctx, expression, mutable),
        _ => Err(crate::CodegenError::LLVMError(
            "cannot take a reference to a temporary value".into(),
        )),
    }
}

/// A reference is the address of the referent's storage. When the place
/// already holds an address (a class instance, an owning allocation, or
/// another reference) that stored address *is* the reference; otherwise the
/// place's own slot is.
fn reference_to_place<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    slot: PointerValue<'ctx>,
    ty: &Ty,
    mutable: bool,
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    let pointee = match ty {
        Ty::Own(inner) | Ty::Ref(inner, _) => (**inner).clone(),
        other => other.clone(),
    };
    if ty_is_llvm_pointer(ty) {
        let loaded = fn_ctx.builder.build_load(
            fn_ctx.context.ptr_type(AddressSpace::default()),
            slot,
            "ref_load",
        )?;
        return Ok(TypedValue::new(loaded, Ty::Ref(Box::new(pointee), mutable)));
    }
    Ok(TypedValue::new(
        slot.into(),
        Ty::Ref(Box::new(pointee), mutable),
    ))
}

/// Emit `alloc(value)`: move `value` into an owning heap allocation. A class
/// instance is already heap-allocated, so it is adopted rather than copied;
/// everything else is boxed into a fresh cell.
pub(crate) fn emit_box_value<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    val: TypedValue<'ctx>,
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    if matches!(val.ntsc_type, Ty::Own(_)) {
        return Ok(val);
    }
    if matches!(val.ntsc_type, Ty::Class(_)) {
        return Ok(TypedValue::new(val.value, Ty::Own(Box::new(val.ntsc_type))));
    }
    let malloc = fn_ctx
        .module
        .get_function("malloc")
        .ok_or_else(|| crate::CodegenError::LLVMError("malloc not declared".into()))?;

    // Every boxed representation is a scalar or an i64 handle, so one
    // pointer-sized cell holds any of them.
    let size = fn_ctx
        .context
        .i64_type()
        .const_int(std::mem::size_of::<i64>() as u64, false);
    let cell = fn_ctx
        .builder
        .build_call(malloc, &[size.into()], "own_alloc")?
        .try_as_basic_value()
        .unwrap_basic()
        .into_pointer_value();
    fn_ctx.builder.build_store(cell, val.value)?;
    Ok(TypedValue::new(
        cell.into(),
        Ty::Own(Box::new(val.ntsc_type)),
    ))
}

// ── Copy emission ───────────────────────────────────────────────────────

/// Emit `copy(expr)` — an owned deep copy of a heap value. Strings and JSON
/// objects duplicate their buffer; arrays clone the container. Scalars are
/// already plain values and are returned unchanged; `copy` of a view
/// dereferences the borrowed value back to the owned type.
pub(crate) fn emit_copy<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    expression: &Expr,
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    let tv = emit_expression(fn_ctx, expression)?;
    // `copy` clones into an independent owned value, so when the source is a
    // fresh temporary (e.g. `copy(strip_v(v))`) nothing else references the
    // original after the clone: drop it here or it leaks. A borrowed view or
    // a value owned by a live slot is not fresh, so `expr_is_fresh` leaves it
    // untouched and its real owner still reclaims it.
    let fresh = expr_is_fresh(fn_ctx, expression, &tv);
    let copied = emit_copy_value(fn_ctx, tv.clone())?;
    if fresh {
        emit_drop_value(fn_ctx, &tv)?;
    }
    Ok(copied)
}

/// Deep-copy an already-computed value to an owned copy.
pub(crate) fn emit_copy_value<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    tv: TypedValue<'ctx>,
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    let (clone_fn, out_ty, extra_args) = match &tv.ntsc_type {
        Ty::String | Ty::Object => ("ntsc_string_clone", tv.ntsc_type.clone(), Vec::new()),

        // Deep-clone nested arrays so the copy is independent of the
        // source: a shallow clone would share child arrays, and two owned
        // drops of the same child would double-free it. `levels` is the
        // array nesting depth of the element type.
        Ty::Array(inner) => (
            "ntsc_array_deep_clone",
            Ty::Array(inner.clone()),
            vec![array_nesting_depth(inner)],
        ),
        // A view borrows its value: materialize an independent owned copy
        // from the inner type. Simply retyping the borrowed handle as owned
        // aliases the source, so the copy's drop would reclaim the original
        // out from under its owner (use-after-free on later reads).
        Ty::View(inner, _) => {
            return emit_copy_value(fn_ctx, TypedValue::new(tv.value, (**inner).clone()));
        }
        Ty::Pointer => ("ntsc_memory_clone", Ty::Pointer, Vec::new()),

        // `copy` materializes an owned value, so copying a window produces an
        // independent array of its elements rather than another borrow.
        Ty::Slice(inner) => ("ntsc_slices_to_array", Ty::Array(inner.clone()), Vec::new()),

        Ty::Own(inner) => {
            let inner = (**inner).clone();
            if let Ty::Class(name) = &inner {
                let instance = TypedValue::new(tv.value, Ty::Class(name.clone()));
                let name = name.clone();
                let copied = emit_copy_class_value(fn_ctx, &name, instance)?;
                return Ok(TypedValue::new(copied.value, Ty::Own(Box::new(inner))));
            }
            let loaded = fn_ctx.builder.build_load(
                ty_to_llvm(&inner, fn_ctx.context),
                tv.value.into_pointer_value(),
                "own_copy_load",
            )?;
            let inner_copy = emit_copy_value(fn_ctx, TypedValue::new(loaded, inner))?;
            return emit_box_value(fn_ctx, inner_copy);
        }

        // A reference and a raw pointer are non-owning addresses: copying
        // one copies the address, not the pointee.
        Ty::Ref(..) | Ty::RawPointer(..) => return Ok(tv),

        // Copying a shared box borrows the wrapped value (a registry
        // handle) and deep-copies it to a fresh owned value; the box itself
        // is left untouched (no retain).
        Ty::Shared(inner) => {
            let inner_fn = fn_ctx
                .module
                .get_function("ntsc_shared_inner")
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError("ntsc_shared_inner not declared".into())
                })?;
            let pointee = fn_ctx
                .builder
                .build_call(
                    inner_fn,
                    &[inkwell::values::BasicMetadataValueEnum::IntValue(
                        tv.value.into_int_value(),
                    )],
                    "shared_inner",
                )?
                .try_as_basic_value()
                .unwrap_basic();
            return emit_copy_value(fn_ctx, TypedValue::new(pointee, (**inner).clone()));
        }

        // Scalars, functions, `nil`, and `any` have no heap payload to
        // duplicate; the value is already owned by the caller.
        Ty::Int | Ty::Float | Ty::Bool | Ty::Void | Ty::Nil | Ty::Any | Ty::Function { .. } => {
            return Ok(tv);
        }
        // Tuples are stack-allocated value types — no deep copy needed.
        Ty::Tuple(_) => return Ok(tv),
        Ty::Class(name) => {
            let name = name.clone();
            return emit_copy_class_value(fn_ctx, &name, tv);
        }

        // The concrete class behind a trait object is unknown here, so no
        // independent copy can be built.
        Ty::Dyn(trait_name) => {
            return Err(crate::CodegenError::LLVMError(format!(
                "cannot copy a dyn {trait_name} value"
            )));
        }
        Ty::Option(inner) => {
            let inner = (**inner).clone();
            return emit_copy_option_value(fn_ctx, &inner, tv);
        }
        Ty::Result { ok, err } => {
            return super::result_cell::emit_copy_result_value(fn_ctx, ok, err, &tv);
        }
        Ty::Chan(_) => {
            return Err(crate::CodegenError::LLVMError(
                "cannot copy a channel; channel values are not yet supported by code generation"
                    .into(),
            ));
        }
    };
    let callee = fn_ctx
        .module
        .get_function(clone_fn)
        .ok_or_else(|| crate::CodegenError::LLVMError(format!("{clone_fn} not declared")))?;
    let mut args: Vec<BasicMetadataValueEnum<'ctx>> = vec![tv.value.into()];
    args.extend(extra_args.into_iter().map(|levels| {
        BasicMetadataValueEnum::IntValue(fn_ctx.context.i64_type().const_int(levels, false))
    }));
    let ptr = fn_ctx
        .builder
        .build_call(callee, &args, "copy_result")?
        .try_as_basic_value()
        .unwrap_basic();
    Ok(TypedValue::new(ptr, out_ty))
}

/// Deep-copy an `option[T]` value into an independent owned box. A `nil`
/// option copies to `nil`; a set option copies its inner value into a fresh
/// cell.
pub(crate) fn emit_copy_option_value<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    inner: &Ty,
    tv: TypedValue<'ctx>,
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    let cell = option_cell_pointer(fn_ctx, tv.value)?;
    let inner_llvm = ty_to_llvm(inner, fn_ctx.context);
    let is_null = fn_ctx.builder.build_is_null(cell, "opt_copy_null")?;
    let null_bb = fn_ctx
        .builder
        .get_insert_block()
        .ok_or_else(|| crate::CodegenError::LLVMError("option copy has no insert block".into()))?;
    let current_fn = fn_ctx.function;
    let some_bb = fn_ctx
        .context
        .append_basic_block(current_fn, "opt_copy.some");
    let done_bb = fn_ctx
        .context
        .append_basic_block(current_fn, "opt_copy.done");
    fn_ctx
        .builder
        .build_conditional_branch(is_null, done_bb, some_bb)?;

    fn_ctx.builder.position_at_end(some_bb);
    let loaded = fn_ctx
        .builder
        .build_load(inner_llvm, cell, "opt_copy_inner")?;
    let copied = emit_copy_value(fn_ctx, TypedValue::new(loaded, (*inner).clone()))?;
    let new_cell = allocate_option_cell(fn_ctx, inner_llvm)?;
    fn_ctx.builder.build_store(new_cell, copied.value)?;

    let some_end_bb = fn_ctx
        .builder
        .get_insert_block()
        .ok_or_else(|| crate::CodegenError::LLVMError("option copy has no insert block".into()))?;
    fn_ctx.builder.build_unconditional_branch(done_bb)?;

    fn_ctx.builder.position_at_end(done_bb);
    let phi = fn_ctx.builder.build_phi(
        fn_ctx.context.ptr_type(AddressSpace::default()),
        "opt_copy_result",
    )?;
    phi.add_incoming(&[
        (
            &fn_ctx
                .context
                .ptr_type(AddressSpace::default())
                .const_null(),
            null_bb,
        ),
        (&new_cell, some_end_bb),
    ]);
    Ok(TypedValue::new(phi.as_basic_value(), tv.ntsc_type))
}

/// Deep-copy a class instance into a fresh heap allocation, recursively
/// copying every owned field so the copy is independent of the source.
///
/// A class that transitively contains a field of its own type cannot be
/// copied this way — the emission would recurse without bound — so such a
/// copy is rejected with a codegen error rather than overflowing the
/// compiler's stack.
pub(crate) fn emit_copy_class_value<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    class_name: &str,
    tv: TypedValue<'ctx>,
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    // The in-progress stack detects cycles through nested field copies.
    let recursive = CLASS_COPY_IN_PROGRESS.with(|stack| {
        let mut stack = stack.borrow_mut();
        if stack.iter().any(|c| c == class_name) {
            true
        } else {
            stack.push(class_name.to_string());
            false
        }
    });
    if recursive {
        return Err(crate::CodegenError::LLVMError(format!(
            "cannot copy class `{class_name}`: it contains a field of its own type, \
             so a deep copy would be unbounded; use a view or a shared handle instead"
        )));
    }
    let result = emit_copy_class_fields(fn_ctx, class_name, tv);
    CLASS_COPY_IN_PROGRESS.with(|stack| {
        stack.borrow_mut().pop();
    });
    result
}

pub(crate) fn emit_copy_class_fields<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    class_name: &str,
    tv: TypedValue<'ctx>,
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    let source = tv.value.into_pointer_value();
    let struct_ty = fn_ctx.module.get_struct_type(class_name).ok_or_else(|| {
        crate::CodegenError::LLVMError(format!("struct type `{class_name}` not found"))
    })?;
    let size = struct_ty.size_of().ok_or_else(|| {
        crate::CodegenError::LLVMError(format!("struct type `{class_name}` has no size"))
    })?;
    let alloc_fn = fn_ctx
        .module
        .get_function("malloc")
        .ok_or_else(|| crate::CodegenError::LLVMError("malloc not declared".into()))?;
    let copy = fn_ctx
        .builder
        .build_call(
            alloc_fn,
            &[BasicMetadataValueEnum::IntValue(size)],
            "class_copy",
        )?
        .try_as_basic_value()
        .unwrap_basic()
        .into_pointer_value();

    // `malloc` hands back uninitialized memory. Zero it first so a field
    // that is skipped below (one whose name does not resolve) holds a null
    // pointer rather than garbage the drop path would later follow.
    let zero = fn_ctx.context.i8_type().const_zero();
    fn_ctx.builder.build_memset(copy, 1, zero, size)?;

    let source_val = TypedValue::new(source.into(), tv.ntsc_type.clone());
    let field_names = class_all_fields(class_name);
    let field_tys = class_all_field_types(class_name);
    for (index, (field_name, field_ty)) in field_names.iter().zip(field_tys.iter()).enumerate() {
        let prop = ntsc_ast::token::Token::new(
            ntsc_ast::token::TokenKind::Identifier(field_name.clone()),
            ntsc_ast::span::Span::dummy(),
        );
        let Some(gep) = emit_member_gep(fn_ctx, &source_val, &prop)? else {
            continue;
        };
        let field_llvm = ty_to_llvm(field_ty, fn_ctx.context);
        let loaded = fn_ctx
            .builder
            .build_load(field_llvm, gep.ptr, "class_copy_field")?;
        let copied = emit_copy_value(fn_ctx, TypedValue::new(loaded, field_ty.clone()))?;
        let dst_ptr =
            fn_ctx
                .builder
                .build_struct_gep(struct_ty, copy, index as u32, "class_copy_dst")?;
        fn_ctx.builder.build_store(dst_ptr, copied.value)?;
    }

    Ok(TypedValue::new(copy.into(), tv.ntsc_type))
}

/// The number of consecutive `Array` wrappers around `ty` (0 for a
/// non-array).
pub(crate) fn array_nesting_depth(ty: &Ty) -> u64 {
    match ty {
        Ty::Array(inner) => 1 + array_nesting_depth(inner),
        _ => 0,
    }
}

pub(crate) fn emit_unary<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    op: &ntsc_ast::token::Token,
    right: &Expr,
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    let operand = emit_expression(fn_ctx, right)?;
    let builder = fn_ctx.builder;

    match &op.kind {
        TokenKind::Minus => match &operand.ntsc_type {
            Ty::Int => {
                // `-i64::MIN` is not representable, so negation is checked
                // like any other subtraction rather than wrapping back to
                // `i64::MIN`.
                let zero = fn_ctx.context.i64_type().const_zero();
                let operand = operand.value.into_int_value();
                let result = emit_checked_int_arith(fn_ctx, IntArith::Sub, zero, operand)?;
                Ok(TypedValue::new(result.into(), Ty::Int))
            }
            Ty::Float => {
                let result =
                    builder.build_float_neg(operand.value.into_float_value(), "fnegtmp")?;
                Ok(TypedValue::new(result.into(), Ty::Float))
            }
            // Unary operator overloading for class types.
            ty @ Ty::Class(_) => emit_unary_operator_method(fn_ctx, "-", &operand, ty),
            _ => Ok(TypedValue::new(
                default_llvm_value(&Ty::Any, fn_ctx.context),
                Ty::Any,
            )),
        },
        TokenKind::Bang => {
            // Unary operator overloading for class types.
            if let Ty::Class(_) = &operand.ntsc_type
                && let Some(result) = try_emit_unary_operator_method(fn_ctx, "!", &operand)?
            {
                return Ok(result);
            }
            let zero = fn_ctx.context.bool_type().const_zero();
            let cmp = builder.build_int_compare(
                IntPredicate::EQ,
                operand.value.into_int_value(),
                zero,
                "nottmp",
            )?;
            Ok(TypedValue::new(cmp.into(), Ty::Bool))
        }
        TokenKind::Tilde => {
            let result = builder.build_not(operand.value.into_int_value(), "invtmp")?;
            Ok(TypedValue::new(result.into(), Ty::Int))
        }

        _ => Ok(TypedValue::new(
            default_llvm_value(&Ty::Any, fn_ctx.context),
            Ty::Any,
        )),
    }
}

/// Emit a unary operator method call on a class type.
fn emit_unary_operator_method<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    method_name: &str,
    operand: &TypedValue<'ctx>,
    ty: &Ty,
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    let dispatch_label = ty.label();
    let Some(declaring) = class_method_declaring_class(&dispatch_label, method_name) else {
        return Ok(TypedValue::new(
            default_llvm_value(&Ty::Any, fn_ctx.context),
            Ty::Any,
        ));
    };
    let fn_name = format!("{declaring}.{method_name}");
    let Some(fn_val) = fn_ctx.module.get_function(&fn_name) else {
        return Ok(TypedValue::new(
            default_llvm_value(&Ty::Any, fn_ctx.context),
            Ty::Any,
        ));
    };
    let receiver = operand.value.into_pointer_value();
    let llvm_args = vec![BasicMetadataValueEnum::PointerValue(receiver)];
    let result = fn_ctx.builder.build_call(fn_val, &llvm_args, "unary_op")?;
    let ret_val = call_result_to_value(fn_ctx, &result);
    let ret_ty = class_method_ret_ty(&declaring, method_name).unwrap_or(Ty::Any);
    Ok(TypedValue::new(ret_val, ret_ty))
}

/// Try to emit a unary `!` operator as a method call. Returns `Some(result)`
/// when the class defines `!`, `None` to fall through to bool negation.
fn try_emit_unary_operator_method<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    method_name: &str,
    operand: &TypedValue<'ctx>,
) -> Result<Option<TypedValue<'ctx>>, crate::CodegenError> {
    let dispatch_label = operand.ntsc_type.label();
    let Some(declaring) = class_method_declaring_class(&dispatch_label, method_name) else {
        return Ok(None);
    };
    let fn_name = format!("{declaring}.{method_name}");
    let Some(fn_val) = fn_ctx.module.get_function(&fn_name) else {
        return Ok(None);
    };
    let receiver = operand.value.into_pointer_value();
    let llvm_args = vec![BasicMetadataValueEnum::PointerValue(receiver)];
    let result = fn_ctx.builder.build_call(fn_val, &llvm_args, "unary_op")?;
    let ret_val = call_result_to_value(fn_ctx, &result);
    let ret_ty = class_method_ret_ty(&declaring, method_name).unwrap_or(Ty::Any);
    Ok(Some(TypedValue::new(ret_val, ret_ty)))
}
