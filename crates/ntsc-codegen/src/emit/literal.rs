//! Literal, string-constant, variable, function-reference, and enum emission.

use super::*;

pub(crate) fn emit_literal<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    value: &LiteralValue,
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    match value {
        LiteralValue::String(s) => Ok(TypedValue::new(emit_string_handle(fn_ctx, s)?, Ty::String)),
        LiteralValue::Number(n) => {
            if n.contains('.') {
                let val: f64 = n.parse().map_err(|e| {
                    crate::CodegenError::LLVMError(format!("invalid float literal: {e}"))
                })?;
                Ok(TypedValue::new(
                    fn_ctx.context.f64_type().const_float(val).into(),
                    Ty::Float,
                ))
            } else {
                let val = ntsc_ast::expr::parse_int_literal(n).ok_or_else(|| {
                    crate::CodegenError::LLVMError(format!("invalid int literal: {n}"))
                })?;
                Ok(TypedValue::new(
                    fn_ctx.context.i64_type().const_int(val as u64, true).into(),
                    Ty::Int,
                ))
            }
        }
        LiteralValue::Bool(b) => Ok(TypedValue::new(
            fn_ctx
                .context
                .bool_type()
                .const_int(*b as u64, false)
                .into(),
            Ty::Bool,
        )),
        LiteralValue::Nil => Ok(TypedValue::new(
            fn_ctx
                .context
                .ptr_type(AddressSpace::default())
                .const_null()
                .into(),
            Ty::Nil,
        )),
    }
}

/// Deterministic hash keying the per-literal string cache. The standard
/// `DefaultHasher` is fine here: the key only has to be stable within a
/// single compile, and it is, so the cache stays coherent.
pub(crate) fn literal_cache_key(text: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// Build a fresh permanent string handle for `text` via
/// `ntsc_string_from_words_permanent` (a string literal: never dropped).
pub(crate) fn build_string_words_permanent<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    text: &str,
) -> Result<BasicValueEnum<'ctx>, crate::CodegenError> {
    build_string_words_via(fn_ctx, text, "ntsc_string_from_words_permanent")
}

/// Shared implementation of [`build_string_words_permanent`], dispatching
/// on the runtime entry point name. The text is shipped to the runtime as
/// an array of 8-byte words plus a byte count, so the runtime never needs
/// a direct reference to the string's memory.
pub(crate) fn build_string_words_via<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    text: &str,
    runtime_fn: &str,
) -> Result<BasicValueEnum<'ctx>, crate::CodegenError> {
    let i64_ty = fn_ctx.context.i64_type();
    let byte_count = i64_ty.const_int(text.len() as u64, false);
    let from_words = fn_ctx
        .module
        .get_function(runtime_fn)
        .ok_or_else(|| crate::CodegenError::LLVMError(format!("{runtime_fn} not declared")))?;
    let words: Vec<u64> = text
        .as_bytes()
        .chunks(8)
        .map(|chunk| {
            let mut word = [0u8; 8];
            word[..chunk.len()].copy_from_slice(chunk);
            u64::from_ne_bytes(word)
        })
        .collect();
    let arr_handle = if words.is_empty() {
        i64_ty.const_zero()
    } else {
        let new_arr = fn_ctx
            .module
            .get_function("ntsc_array_new_typed")
            .ok_or_else(|| {
                crate::CodegenError::LLVMError("ntsc_array_new_typed not declared".into())
            })?;
        let elem_size = i64_ty.const_int(8, false);
        let capacity = i64_ty.const_int(words.len() as u64, false);
        let scalar_flags = fn_ctx.context.i8_type().const_zero();
        let arr = fn_ctx
            .builder
            .build_call(
                new_arr,
                &[
                    elem_size.into(),
                    capacity.into(),
                    inkwell::values::BasicMetadataValueEnum::IntValue(scalar_flags),
                ],
                "lit_words",
            )?
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let push_fn = fn_ctx
            .module
            .get_function("ntsc_array_push")
            .ok_or_else(|| crate::CodegenError::LLVMError("ntsc_array_push not declared".into()))?;
        for word in words {
            let word_val = i64_ty.const_int(word, false);
            fn_ctx.builder.build_call(
                push_fn,
                &[
                    inkwell::values::BasicMetadataValueEnum::IntValue(arr),
                    inkwell::values::BasicMetadataValueEnum::IntValue(word_val),
                ],
                "lit_word",
            )?;
        }
        arr
    };
    let result = fn_ctx.builder.build_call(
        from_words,
        &[
            inkwell::values::BasicMetadataValueEnum::IntValue(arr_handle),
            inkwell::values::BasicMetadataValueEnum::IntValue(byte_count),
        ],
        "str_literal",
    )?;
    Ok(call_result_to_value(fn_ctx, &result))
}

/// Produce the borrowed string handle for constant text (string literals,
/// empty strings, property keys), building it once on first use and caching
/// it in a module-level global. Cached strings are borrowed and never
/// dropped; every use shares one handle, so an owned slot always receives a
/// clone rather than a move.
pub(crate) fn emit_string_handle<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    text: &str,
) -> Result<BasicValueEnum<'ctx>, crate::CodegenError> {
    let i64_ty = fn_ctx.context.i64_type();
    let global_name = format!("str_lit_{:016x}", literal_cache_key(text));
    let slot = if let Some(global) = fn_ctx.module.get_global(&global_name) {
        global
    } else {
        let global = fn_ctx
            .module
            .add_global(i64_ty, Some(AddressSpace::default()), &global_name);
        global.set_initializer(&i64_ty.const_zero());
        global
    };
    let slot_ptr = slot.as_pointer_value();
    let loaded = fn_ctx
        .builder
        .build_load(i64_ty, slot_ptr, "str_lit_load")?;
    let loaded_int = loaded.into_int_value();
    let is_ready = fn_ctx.builder.build_int_compare(
        IntPredicate::NE,
        loaded_int,
        i64_ty.const_zero(),
        "str_lit_ready",
    )?;
    let current_fn = fn_ctx.function;
    let ready_bb = fn_ctx.builder.get_insert_block().ok_or_else(|| {
        crate::CodegenError::LLVMError("internal: no insert block for string literal".into())
    })?;
    let build_bb = fn_ctx
        .context
        .append_basic_block(current_fn, "str_lit.build");
    let done_bb = fn_ctx
        .context
        .append_basic_block(current_fn, "str_lit.done");
    fn_ctx
        .builder
        .build_conditional_branch(is_ready, done_bb, build_bb)?;
    fn_ctx.builder.position_at_end(build_bb);
    let built = build_string_words_permanent(fn_ctx, text)?;
    let built_int = built.into_int_value();
    fn_ctx.builder.build_store(slot_ptr, built)?;
    fn_ctx.builder.build_unconditional_branch(done_bb)?;
    fn_ctx.builder.position_at_end(done_bb);
    let phi = fn_ctx.builder.build_phi(i64_ty, "str_lit")?;
    phi.add_incoming(&[(&loaded_int, ready_bb), (&built_int, build_bb)]);
    Ok(phi.as_basic_value())
}

// ── Variable emission ───────────────────────────────────────────────────

pub(crate) fn emit_variable<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    name: &ntsc_ast::token::Token,
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    let name_str = name.lexeme();

    if name_str == "say" || name_str == "ntsc_say" {
        return Ok(TypedValue::new(
            fn_ctx
                .context
                .ptr_type(AddressSpace::default())
                .const_null()
                .into(),
            Ty::Function {
                params: vec![Ty::String],
                return_type: Box::new(Ty::Void),
            },
        ));
    }

    let stripped = name_str.trim_start_matches('@');
    match fn_ctx.lookup_var(stripped) {
        Some((ptr, ty)) => {
            let llvm_ty = ty_to_llvm(ty, fn_ctx.context);
            let loaded = fn_ctx.builder.build_load(llvm_ty, ptr, stripped)?;
            Ok(TypedValue::new(loaded, ty.clone()))
        }
        None => {
            // Standard library modules are represented as opaque objects.
            // Only when no local variable shadows the module name.
            match stripped {
                "math" | "fmt" | "time" | "sys" | "strings" | "json" | "http" | "crypto"
                | "collections" | "regex" | "arrays" | "process" | "csv" | "yaml" | "term" => {
                    return Ok(TypedValue::new(
                        fn_ctx
                            .context
                            .ptr_type(AddressSpace::default())
                            .const_null()
                            .into(),
                        Ty::Object,
                    ));
                }
                _ => {}
            }
            if super::RUST_CRATE_NAMESPACES.with(|m| m.borrow().contains_key(stripped)) {
                return Ok(TypedValue::new(
                    fn_ctx
                        .context
                        .ptr_type(AddressSpace::default())
                        .const_null()
                        .into(),
                    Ty::Object,
                ));
            }

            match emit_static_const_variable(fn_ctx, name_str)? {
                Some(value) => Ok(value),
                None => match enum_value_expression(name_str, fn_ctx) {
                    Ok(value) => Ok(value),
                    Err(err) => function_reference(fn_ctx, name_str).ok_or(err),
                },
            }
        }
    }
}

/// Resolve a `static const` variable: a module-level global emitted by
/// `emit_static_const`. String constants are built lazily on first use,
/// exactly like string literals, and the handle is then cached in the
/// global; scalar constants carry a constant initializer and are loaded
/// directly. Returns `None` when `name` is not a static const.
pub(crate) fn emit_static_const_variable<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    name: &str,
) -> Result<Option<TypedValue<'ctx>>, crate::CodegenError> {
    let ty = match STATIC_CONST_TYPES.with(|map| map.borrow().get(name).cloned()) {
        Some(ty) => ty,
        None => return Ok(None),
    };
    let global_name = format!("ntsc_const_{name}");
    let global = fn_ctx
        .module
        .get_global(&global_name)
        .ok_or_else(|| crate::CodegenError::LLVMError(format!("undefined constant `{name}`")))?;
    let slot = global.as_pointer_value();

    if let Some(Expr::Literal {
        value: LiteralValue::String(text),
        ..
    }) = STATIC_CONST_INITS.with(|map| map.borrow().get(name).and_then(|init| init.clone()))
    {
        let i64_ty = fn_ctx.context.i64_type();
        let loaded = fn_ctx.builder.build_load(i64_ty, slot, "const_load")?;
        let loaded_int = loaded.into_int_value();
        let is_ready = fn_ctx.builder.build_int_compare(
            IntPredicate::NE,
            loaded_int,
            i64_ty.const_zero(),
            "const_ready",
        )?;
        let current_fn = fn_ctx.function;
        let ready_bb = fn_ctx.builder.get_insert_block().ok_or_else(|| {
            crate::CodegenError::LLVMError("internal: no insert block for const".into())
        })?;
        let build_bb = fn_ctx.context.append_basic_block(current_fn, "const.build");
        let done_bb = fn_ctx.context.append_basic_block(current_fn, "const.done");
        fn_ctx
            .builder
            .build_conditional_branch(is_ready, done_bb, build_bb)?;
        fn_ctx.builder.position_at_end(build_bb);
        let built = build_string_words_permanent(fn_ctx, &text)?;
        let built_int = built.into_int_value();
        fn_ctx.builder.build_store(slot, built)?;
        fn_ctx.builder.build_unconditional_branch(done_bb)?;
        fn_ctx.builder.position_at_end(done_bb);
        let phi = fn_ctx.builder.build_phi(i64_ty, "const")?;
        phi.add_incoming(&[(&loaded_int, ready_bb), (&built_int, build_bb)]);
        return Ok(Some(TypedValue::new(phi.as_basic_value(), Ty::String)));
    }

    let llvm_ty = ty_to_llvm(&ty, fn_ctx.context);
    let loaded = fn_ctx.builder.build_load(llvm_ty, slot, name)?;
    Ok(Some(TypedValue::new(loaded, ty)))
}

/// A bare identifier naming a top-level function evaluates to that
/// function's pointer, typed with the function's declared signature — the
/// same shape a lambda expression produces, so both forms work wherever a
/// function value is expected. `main` is excluded: it is emitted under an
/// internal name and is the entry point, not a value.
pub(crate) fn function_reference<'ctx>(
    fn_ctx: &FunctionContext<'ctx, '_>,
    name: &str,
) -> Option<TypedValue<'ctx>> {
    if name == "main" || !is_user_function(name) {
        return None;
    }
    let function = fn_ctx.module.get_function(name)?;
    Some(TypedValue::new(
        function.as_global_value().as_pointer_value().into(),
        Ty::Function {
            params: function_declared_param_types(name),
            return_type: Box::new(function_declared_ret_ty(name).unwrap_or(Ty::Void)),
        },
    ))
}

pub(crate) fn is_user_function(name: &str) -> bool {
    FUNCTION_PARAM_TYPES.with(|map| map.borrow().contains_key(name))
}

/// Resolve a bare identifier that is not a local/global variable: either an
/// enum member constant (`case North`) or an enum type name used as a
/// member-access prefix (`Color` in `Color.RED`).
pub(crate) fn enum_value_expression<'ctx>(
    name_str: &str,
    fn_ctx: &FunctionContext<'ctx, '_>,
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    let int_const = |value: i32| {
        fn_ctx
            .context
            .i64_type()
            .const_int(value as u64, false)
            .into()
    };
    let member = ENUM_MEMBER_VALUES.with(|map| map.borrow().get(name_str).copied());
    if let Some(value) = member {
        return Ok(TypedValue::new(int_const(value), Ty::Int));
    }
    let is_enum_type = ENUM_VALUES.with(|map| map.borrow().contains_key(name_str));
    if is_enum_type {
        return Ok(TypedValue::new(
            int_const(0),
            Ty::Class(name_str.to_string()),
        ));
    }
    Err(crate::CodegenError::LLVMError(format!(
        "undefined variable `{name_str}`"
    )))
}
