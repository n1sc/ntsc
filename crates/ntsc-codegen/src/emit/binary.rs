//! Binary operations, including checked-arithmetic guards.

use super::*;

pub(crate) fn emit_binary<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    left: &Expr,
    op: &ntsc_ast::token::Token,
    right: &Expr,
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    // `&&` and `||` decide on the left operand alone, so the right one is
    // emitted in a separate block instead of unconditionally.
    if let Some(result) = emit_short_circuit(fn_ctx, op, left, right)? {
        return Ok(result);
    }

    let lhs = {
        let lhs_val = emit_expression(fn_ctx, left)?;
        peel_view(normalize_self(fn_ctx, lhs_val)?)
    };
    let rhs = {
        let rhs_val = emit_expression(fn_ctx, right)?;
        peel_view(normalize_self(fn_ctx, rhs_val)?)
    };

    // Operator overloading: when at least one operand is a class type,
    // attempt to dispatch to an operator method on that class.
    if let Some(result) = try_emit_operator_method(fn_ctx, op, &lhs, &rhs)? {
        return Ok(result);
    }

    let op_ty = match (&lhs.ntsc_type, &rhs.ntsc_type) {
        (Ty::Int, Ty::Int) => Ty::Int,
        (Ty::Float, Ty::Float) | (Ty::Float, Ty::Int) | (Ty::Int, Ty::Float) => Ty::Float,
        (Ty::String, Ty::String) => Ty::String,

        // String concatenation with a scalar: the scalar is coerced to a
        // string at emit time. A dynamically-typed operand (e.g. an element
        // of an untyped `[]` array) is passed through as a runtime string
        // pointer, mirroring `say(any_value)`.
        (Ty::String, Ty::Int | Ty::Float | Ty::Bool) => Ty::String,
        (Ty::Int | Ty::Float | Ty::Bool, Ty::String) => Ty::String,

        (Ty::String, Ty::Any) => Ty::String,
        (Ty::Any, Ty::String) => Ty::String,
        (Ty::Bool, Ty::Bool) => Ty::Bool,

        // `nil` and `option[T]` compare for nullness / identity: the
        // operands are pointers, so the comparison is an integer equality
        // check on their addresses. `Ty::Nil` is the internal dispatch tag
        // for this pointer-equality operation.
        (Ty::Nil | Ty::Option(_), Ty::Nil | Ty::Option(_)) => Ty::Nil,
        // Fall back to preventing a crash on unsupported operand pairs;
        // the type checker rejects them before codegen runs.
        _ => {
            return Ok(TypedValue::new(
                default_llvm_value(&Ty::Any, fn_ctx.context),
                Ty::Any,
            ));
        }
    };

    let builder = fn_ctx.builder;
    let _context = fn_ctx.context;

    // Mixed arithmetic widens the int operand to float first: the type
    // checker permits it, so the int is promoted before the float op is
    // built.
    let (lhs, rhs) = if op_ty == Ty::Float {
        let l = match lhs.ntsc_type {
            Ty::Int => coerce_int_to_float(fn_ctx, lhs)?,
            _ => lhs,
        };
        let r = match rhs.ntsc_type {
            Ty::Int => coerce_int_to_float(fn_ctx, rhs)?,
            _ => rhs,
        };
        (l, r)
    } else {
        (lhs, rhs)
    };

    match (&op.kind, &op_ty) {
        // ── Arithmetic: int ──────────────────────────────────────────────
        (TokenKind::Plus, Ty::Int) => {
            let result = emit_checked_int_arith(
                fn_ctx,
                IntArith::Add,
                lhs.value.into_int_value(),
                rhs.value.into_int_value(),
            )?;
            Ok(TypedValue::new(result.into(), Ty::Int))
        }
        (TokenKind::Minus, Ty::Int) => {
            let result = emit_checked_int_arith(
                fn_ctx,
                IntArith::Sub,
                lhs.value.into_int_value(),
                rhs.value.into_int_value(),
            )?;
            Ok(TypedValue::new(result.into(), Ty::Int))
        }
        (TokenKind::Star, Ty::Int) => {
            let result = emit_checked_int_arith(
                fn_ctx,
                IntArith::Mul,
                lhs.value.into_int_value(),
                rhs.value.into_int_value(),
            )?;
            Ok(TypedValue::new(result.into(), Ty::Int))
        }
        (TokenKind::Slash, Ty::Int) => {
            emit_checked_divisor_guard(
                fn_ctx,
                lhs.value.into_int_value(),
                rhs.value.into_int_value(),
            )?;
            let result = builder.build_int_signed_div(
                lhs.value.into_int_value(),
                rhs.value.into_int_value(),
                "divtmp",
            )?;
            Ok(TypedValue::new(result.into(), Ty::Int))
        }
        (TokenKind::Percent, Ty::Int) => {
            emit_checked_divisor_guard(
                fn_ctx,
                lhs.value.into_int_value(),
                rhs.value.into_int_value(),
            )?;
            let result = builder.build_int_signed_rem(
                lhs.value.into_int_value(),
                rhs.value.into_int_value(),
                "remtmp",
            )?;
            Ok(TypedValue::new(result.into(), Ty::Int))
        }

        // ── Arithmetic: float ────────────────────────────────────────────
        (TokenKind::Plus, Ty::Float) => {
            let result = builder.build_float_add(
                lhs.value.into_float_value(),
                rhs.value.into_float_value(),
                "faddtmp",
            )?;
            Ok(TypedValue::new(result.into(), Ty::Float))
        }
        (TokenKind::Minus, Ty::Float) => {
            let result = builder.build_float_sub(
                lhs.value.into_float_value(),
                rhs.value.into_float_value(),
                "fsubtmp",
            )?;
            Ok(TypedValue::new(result.into(), Ty::Float))
        }
        (TokenKind::Star, Ty::Float) => {
            let result = builder.build_float_mul(
                lhs.value.into_float_value(),
                rhs.value.into_float_value(),
                "fmultmp",
            )?;
            Ok(TypedValue::new(result.into(), Ty::Float))
        }
        (TokenKind::Slash, Ty::Float) => {
            let result = builder.build_float_div(
                lhs.value.into_float_value(),
                rhs.value.into_float_value(),
                "fdivtmp",
            )?;
            Ok(TypedValue::new(result.into(), Ty::Float))
        }

        // ── String concatenation ─────────────────────────────────────────
        (TokenKind::Plus, Ty::String) => {
            let concat_fn = fn_ctx
                .module
                .get_function("ntsc_string_concat")
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError("ntsc_string_concat not declared".into())
                })?;

            // A numeric operand is converted here, producing a fresh owned
            // string that is released after the concat has read it.
            let mut lhs_temp: Option<TypedValue<'ctx>> = None;
            let mut rhs_temp: Option<TypedValue<'ctx>> = None;
            let lhs_str = if lhs.ntsc_type == Ty::String {
                lhs.value.into_int_value()
            } else {
                let converted = convert_to_string(fn_ctx, &lhs)?;
                lhs_temp = Some(converted.clone());
                converted.value.into_int_value()
            };
            let rhs_str = if rhs.ntsc_type == Ty::String {
                rhs.value.into_int_value()
            } else {
                let converted = convert_to_string(fn_ctx, &rhs)?;
                rhs_temp = Some(converted.clone());
                converted.value.into_int_value()
            };

            let result = builder.build_call(
                concat_fn,
                &[
                    BasicMetadataValueEnum::IntValue(lhs_str),
                    BasicMetadataValueEnum::IntValue(rhs_str),
                ],
                "concat",
            )?;

            // `ntsc_string_concat` borrows its operands: every fresh owned
            // string it was handed (converted scalars and fresh string
            // temps) is released once the result has been produced.
            if let Some(temp) = lhs_temp {
                emit_drop_value(fn_ctx, &temp)?;
            }
            if let Some(temp) = rhs_temp {
                emit_drop_value(fn_ctx, &temp)?;
            }
            if lhs.ntsc_type == Ty::String && expr_is_fresh(fn_ctx, left, &lhs) {
                emit_drop_value(fn_ctx, &lhs)?;
            }
            if rhs.ntsc_type == Ty::String && expr_is_fresh(fn_ctx, right, &rhs) {
                emit_drop_value(fn_ctx, &rhs)?;
            }

            let val = call_result_to_value(fn_ctx, &result);
            Ok(TypedValue::new(val, Ty::String))
        }

        // ── Comparison: int ──────────────────────────────────────────────
        (TokenKind::EqualEqual, Ty::Int) => {
            let cmp = builder.build_int_compare(
                IntPredicate::EQ,
                lhs.value.into_int_value(),
                rhs.value.into_int_value(),
                "cmptmp",
            )?;
            Ok(TypedValue::new(cmp.into(), Ty::Bool))
        }
        (TokenKind::BangEqual, Ty::Int) => {
            let cmp = builder.build_int_compare(
                IntPredicate::NE,
                lhs.value.into_int_value(),
                rhs.value.into_int_value(),
                "cmptmp",
            )?;
            Ok(TypedValue::new(cmp.into(), Ty::Bool))
        }
        (TokenKind::Less, Ty::Int) => {
            let cmp = builder.build_int_compare(
                IntPredicate::SLT,
                lhs.value.into_int_value(),
                rhs.value.into_int_value(),
                "cmptmp",
            )?;
            Ok(TypedValue::new(cmp.into(), Ty::Bool))
        }
        (TokenKind::LessEqual, Ty::Int) => {
            let cmp = builder.build_int_compare(
                IntPredicate::SLE,
                lhs.value.into_int_value(),
                rhs.value.into_int_value(),
                "cmptmp",
            )?;
            Ok(TypedValue::new(cmp.into(), Ty::Bool))
        }
        (TokenKind::Greater, Ty::Int) => {
            let cmp = builder.build_int_compare(
                IntPredicate::SGT,
                lhs.value.into_int_value(),
                rhs.value.into_int_value(),
                "cmptmp",
            )?;
            Ok(TypedValue::new(cmp.into(), Ty::Bool))
        }
        (TokenKind::GreaterEqual, Ty::Int) => {
            let cmp = builder.build_int_compare(
                IntPredicate::SGE,
                lhs.value.into_int_value(),
                rhs.value.into_int_value(),
                "cmptmp",
            )?;
            Ok(TypedValue::new(cmp.into(), Ty::Bool))
        }

        // ── Comparison: float ────────────────────────────────────────────
        (TokenKind::EqualEqual, Ty::Float) => {
            let cmp = builder.build_float_compare(
                inkwell::FloatPredicate::OEQ,
                lhs.value.into_float_value(),
                rhs.value.into_float_value(),
                "fcmptmp",
            )?;
            Ok(TypedValue::new(cmp.into(), Ty::Bool))
        }
        (TokenKind::BangEqual, Ty::Float) => {
            let cmp = builder.build_float_compare(
                inkwell::FloatPredicate::ONE,
                lhs.value.into_float_value(),
                rhs.value.into_float_value(),
                "fcmptmp",
            )?;
            Ok(TypedValue::new(cmp.into(), Ty::Bool))
        }
        (TokenKind::Less, Ty::Float) => {
            let cmp = builder.build_float_compare(
                inkwell::FloatPredicate::OLT,
                lhs.value.into_float_value(),
                rhs.value.into_float_value(),
                "fcmptmp",
            )?;
            Ok(TypedValue::new(cmp.into(), Ty::Bool))
        }
        (TokenKind::LessEqual, Ty::Float) => {
            let cmp = builder.build_float_compare(
                inkwell::FloatPredicate::OLE,
                lhs.value.into_float_value(),
                rhs.value.into_float_value(),
                "fcmptmp",
            )?;
            Ok(TypedValue::new(cmp.into(), Ty::Bool))
        }
        (TokenKind::Greater, Ty::Float) => {
            let cmp = builder.build_float_compare(
                inkwell::FloatPredicate::OGT,
                lhs.value.into_float_value(),
                rhs.value.into_float_value(),
                "fcmptmp",
            )?;
            Ok(TypedValue::new(cmp.into(), Ty::Bool))
        }
        (TokenKind::GreaterEqual, Ty::Float) => {
            let cmp = builder.build_float_compare(
                inkwell::FloatPredicate::OGE,
                lhs.value.into_float_value(),
                rhs.value.into_float_value(),
                "fcmptmp",
            )?;
            Ok(TypedValue::new(cmp.into(), Ty::Bool))
        }

        // ── Comparison: bool (and string equality) ───────────────────────
        (TokenKind::EqualEqual, Ty::Bool) => {
            let cmp = builder.build_int_compare(
                IntPredicate::EQ,
                lhs.value.into_int_value(),
                rhs.value.into_int_value(),
                "boolcmptmp",
            )?;
            Ok(TypedValue::new(cmp.into(), Ty::Bool))
        }
        (TokenKind::BangEqual, Ty::Bool) => {
            let cmp = builder.build_int_compare(
                IntPredicate::NE,
                lhs.value.into_int_value(),
                rhs.value.into_int_value(),
                "boolcmptmp",
            )?;
            Ok(TypedValue::new(cmp.into(), Ty::Bool))
        }

        // ── Comparison: string ────────────────────────────────────────────
        (TokenKind::EqualEqual | TokenKind::BangEqual, Ty::String) => {
            let eq_fn = fn_ctx
                .module
                .get_function("ntsc_string_equals")
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError("ntsc_string_equals not declared".into())
                })?;
            // A string is a registry handle travelling in an `i64`, so both
            // operands are passed as integers: `ntsc_string_equals` compares
            // the bytes behind the handles, not the handles themselves.
            let result = builder.build_call(
                eq_fn,
                &[
                    inkwell::values::BasicMetadataValueEnum::IntValue(lhs.value.into_int_value()),
                    inkwell::values::BasicMetadataValueEnum::IntValue(rhs.value.into_int_value()),
                ],
                "streq",
            )?;
            let i8_val = call_result_to_value(fn_ctx, &result).into_int_value();

            // The comparison borrows both operands: a fresh temporary (a
            // concatenation, a call result) has no owning slot, so it is
            // reclaimed here now that its bytes have been read.
            if lhs.ntsc_type == Ty::String && expr_is_fresh(fn_ctx, left, &lhs) {
                emit_drop_value(fn_ctx, &lhs)?;
            }
            if rhs.ntsc_type == Ty::String && expr_is_fresh(fn_ctx, right, &rhs) {
                emit_drop_value(fn_ctx, &rhs)?;
            }
            let cmp = builder.build_int_truncate(i8_val, fn_ctx.context.bool_type(), "streq_i1")?;

            // `!=` is the negation of the equality the runtime reported.
            let cmp = if op.kind == TokenKind::BangEqual {
                builder.build_int_compare(
                    IntPredicate::EQ,
                    cmp,
                    fn_ctx.context.bool_type().const_zero(),
                    "strne",
                )?
            } else {
                cmp
            };
            Ok(TypedValue::new(cmp.into(), Ty::Bool))
        }

        // ── Comparison: nil / option (pointer identity) ──────────────────
        (TokenKind::EqualEqual | TokenKind::BangEqual, Ty::Nil) => {
            // `nil` is a null pointer and `option[T]` is a heap cell whose
            // bits travel in an i64 slot, so equality is an address
            // comparison: `option == nil` is a nullness test, and
            // `option == option` is an identity test. Both sides are
            // normalized to their address bits first: pointers are converted
            // to `i64`, i64 slots are used as-is.
            let lhs_int = if lhs.value.is_pointer_value() {
                builder.build_ptr_to_int(
                    lhs.value.into_pointer_value(),
                    fn_ctx.context.i64_type(),
                    "ptrcmpl",
                )?
            } else {
                lhs.value.into_int_value()
            };
            let rhs_int = if rhs.value.is_pointer_value() {
                builder.build_ptr_to_int(
                    rhs.value.into_pointer_value(),
                    fn_ctx.context.i64_type(),
                    "ptrcmpr",
                )?
            } else {
                rhs.value.into_int_value()
            };
            let pred = if op.kind == TokenKind::BangEqual {
                IntPredicate::NE
            } else {
                IntPredicate::EQ
            };
            let cmp = builder.build_int_compare(pred, lhs_int, rhs_int, "cmptmp")?;
            Ok(TypedValue::new(cmp.into(), Ty::Bool))
        }

        // ── Logical AND/OR ───────────────────────────────────────────────
        (TokenKind::AndSym | TokenKind::And, Ty::Bool) => {
            let cmp = builder.build_and(
                lhs.value.into_int_value(),
                rhs.value.into_int_value(),
                "andtmp",
            )?;
            Ok(TypedValue::new(cmp.into(), Ty::Bool))
        }
        (TokenKind::OrSym | TokenKind::Or, Ty::Bool) => {
            let cmp = builder.build_or(
                lhs.value.into_int_value(),
                rhs.value.into_int_value(),
                "ortmp",
            )?;
            Ok(TypedValue::new(cmp.into(), Ty::Bool))
        }

        // ── Bitwise: int ─────────────────────────────────────────────────
        (TokenKind::Ampersand, Ty::Int) => {
            let result = builder.build_and(
                lhs.value.into_int_value(),
                rhs.value.into_int_value(),
                "andtmp",
            )?;
            Ok(TypedValue::new(result.into(), Ty::Int))
        }
        (TokenKind::Pipe, Ty::Int) => {
            let result = builder.build_or(
                lhs.value.into_int_value(),
                rhs.value.into_int_value(),
                "ortmp",
            )?;
            Ok(TypedValue::new(result.into(), Ty::Int))
        }
        (TokenKind::Caret, Ty::Int) => {
            let result = builder.build_xor(
                lhs.value.into_int_value(),
                rhs.value.into_int_value(),
                "xortmp",
            )?;
            Ok(TypedValue::new(result.into(), Ty::Int))
        }
        (TokenKind::LessLess, Ty::Int) => {
            emit_shift_amount_guard(fn_ctx, rhs.value.into_int_value())?;
            let result = fn_ctx.builder.build_left_shift(
                lhs.value.into_int_value(),
                rhs.value.into_int_value(),
                "shltmp",
            )?;
            Ok(TypedValue::new(result.into(), Ty::Int))
        }
        (TokenKind::GreaterGreater, Ty::Int) => {
            emit_shift_amount_guard(fn_ctx, rhs.value.into_int_value())?;
            let result = fn_ctx.builder.build_right_shift(
                lhs.value.into_int_value(),
                rhs.value.into_int_value(),
                true,
                "ashtmp",
            )?;
            Ok(TypedValue::new(result.into(), Ty::Int))
        }

        _ => Err(crate::CodegenError::LLVMError(format!(
            "unsupported binary operation {:?} on type {} in function `{}` (lhs `{}`, rhs `{}`)",
            op.kind,
            op_ty,
            fn_ctx.function.get_name().to_string_lossy(),
            lhs.ntsc_type,
            rhs.ntsc_type
        ))),
    }
}

/// Emit `&&` and `||` with short-circuit evaluation.
///
/// The right operand is emitted in its own basic block, reached only when the
/// left operand does not already decide the result, so a bounds check, a
/// division, or a call guarded by the left operand never runs when it must
/// not. Both operands use the same truthiness rule an `if` condition does.
///
/// Returns `None` for every other operator so the caller falls through to
/// the ordinary two-operand path.
fn emit_short_circuit<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    op: &ntsc_ast::token::Token,
    left: &Expr,
    right: &Expr,
) -> Result<Option<TypedValue<'ctx>>, crate::CodegenError> {
    let is_and = match op.kind {
        TokenKind::AndSym => true,
        TokenKind::OrSym => false,
        _ => return Ok(None),
    };

    let lhs_raw = emit_expression(fn_ctx, left)?;
    let lhs_norm = normalize_self(fn_ctx, lhs_raw)?;
    let lhs = bool_to_i1(fn_ctx, peel_view(lhs_norm));

    let current_fn = fn_ctx.function;
    let ctx = fn_ctx.context;
    let bool_ty = ctx.bool_type();
    let entry_bb = fn_ctx
        .builder
        .get_insert_block()
        .ok_or_else(|| crate::CodegenError::LLVMError("no insert block for && / ||".into()))?;
    let rhs_bb = ctx.append_basic_block(current_fn, if is_and { "and.rhs" } else { "or.rhs" });
    let merge_bb =
        ctx.append_basic_block(current_fn, if is_and { "and.merge" } else { "or.merge" });

    // `&&`: a false left operand decides the result, so the right one runs
    // only on the true edge. `||` is the mirror image.
    let decided_bb = if is_and {
        None
    } else {
        let bb = ctx.append_basic_block(current_fn, "or.decided");
        fn_ctx.builder.build_conditional_branch(lhs, bb, rhs_bb)?;
        fn_ctx.builder.position_at_end(bb);
        fn_ctx.builder.build_unconditional_branch(merge_bb)?;
        Some(bb)
    };
    if is_and {
        fn_ctx
            .builder
            .build_conditional_branch(lhs, rhs_bb, merge_bb)?;
    }

    fn_ctx.builder.position_at_end(rhs_bb);
    let rhs_raw = emit_expression(fn_ctx, right)?;
    let rhs_norm = normalize_self(fn_ctx, rhs_raw)?;
    let rhs = bool_to_i1(fn_ctx, peel_view(rhs_norm));
    // Emitting the operand can itself split blocks (an exception check, a
    // temporary's teardown), so the edge into the merge leaves from wherever
    // emission actually ended rather than from `rhs_bb`.
    let rhs_end = fn_ctx.builder.get_insert_block().ok_or_else(|| {
        crate::CodegenError::LLVMError("empty block after && / || operand".into())
    })?;
    if rhs_end.get_terminator().is_none() {
        fn_ctx.builder.build_unconditional_branch(merge_bb)?;
    }

    fn_ctx.builder.position_at_end(merge_bb);
    let result = fn_ctx
        .builder
        .build_phi(bool_ty, if is_and { "and_result" } else { "or_result" })?;
    match decided_bb {
        Some(decided) => {
            let short_circuit = bool_ty.const_int(1, false);
            result.add_incoming(&[(&short_circuit, decided), (&rhs, rhs_end)]);
        }
        None => {
            let long_circuit = bool_ty.const_int(0, false);
            result.add_incoming(&[(&long_circuit, entry_bb), (&rhs, rhs_end)]);
        }
    }
    Ok(Some(TypedValue::new(result.as_basic_value(), Ty::Bool)))
}

/// Try to emit a binary operator as a method call on a class type.
/// Returns `Some(result)` when at least one operand is a class with the
/// operator method, `None` to fall through to built-in operations.
fn try_emit_operator_method<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    op: &ntsc_ast::token::Token,
    lhs: &TypedValue<'ctx>,
    rhs: &TypedValue<'ctx>,
) -> Result<Option<TypedValue<'ctx>>, crate::CodegenError> {
    let method_name = match &op.kind {
        TokenKind::Plus => "+",
        TokenKind::Minus => "-",
        TokenKind::Star => "*",
        TokenKind::Slash => "/",
        TokenKind::Percent => "%",
        TokenKind::EqualEqual => "==",
        TokenKind::BangEqual => "!=",
        TokenKind::Less => "<",
        TokenKind::LessEqual => "<=",
        TokenKind::Greater => ">",
        TokenKind::GreaterEqual => ">=",
        _ => return Ok(None),
    };

    // Determine the receiver (left operand for most operators).
    let dispatch_label = match &lhs.ntsc_type {
        Ty::View(inner, _) => inner.label(),
        other => other.label(),
    };

    let Some(declaring) = class_method_declaring_class(&dispatch_label, method_name) else {
        return Ok(None);
    };

    let fn_name = format!("{declaring}.{method_name}");
    let Some(fn_val) = fn_ctx.module.get_function(&fn_name) else {
        return Ok(None);
    };

    let method_param_tys = class_method_declared_param_types(&declaring, method_name);

    // Build the LLVM argument list: receiver pointer + right operand.
    // Operator parameters are `view` by convention, so the right operand
    // is passed by pointer without ownership transfer.
    let receiver = if declaring == dispatch_label {
        lhs.value.into_pointer_value()
    } else {
        fn_ctx.builder.build_pointer_cast(
            lhs.value.into_pointer_value(),
            fn_ctx.context.ptr_type(AddressSpace::default()),
            "op_receiver",
        )?
    };

    let mut llvm_args = vec![BasicMetadataValueEnum::PointerValue(receiver)];
    // Pass each parameter (excluding `this`) from the right-hand operand.
    // Most operators take exactly one parameter; multi-operand cases are
    // future-proofed by iterating the declared parameter types.
    for param_ty in &method_param_tys {
        let val = match param_ty {
            Ty::View(..) => {
                TypedValue::new(rhs.value, Ty::View(Box::new(rhs.ntsc_type.clone()), false))
            }
            _ => rhs.clone(),
        };
        llvm_args.push(val.value.into());
    }

    let result = fn_ctx.builder.build_call(fn_val, &llvm_args, "op_method")?;
    let ret_val = call_result_to_value(fn_ctx, &result);
    let ret_ty = class_method_ret_ty(&declaring, method_name).unwrap_or(Ty::Any);

    Ok(Some(TypedValue::new(ret_val, ret_ty)))
}

#[derive(Copy, Clone)]
pub(crate) enum IntArith {
    Add,
    Sub,
    Mul,
}

impl IntArith {
    /// The LLVM overflow-checking intrinsic for this operation.
    fn intrinsic(self) -> &'static str {
        match self {
            IntArith::Add => "llvm.sadd.with.overflow",
            IntArith::Sub => "llvm.ssub.with.overflow",
            IntArith::Mul => "llvm.smul.with.overflow",
        }
    }

    /// The exception message an overflow of this operation throws.
    fn message(self) -> &'static str {
        match self {
            IntArith::Add => "integer addition overflow",
            IntArith::Sub => "integer subtraction overflow",
            IntArith::Mul => "integer multiplication overflow",
        }
    }

    /// A short label for the blocks and values this operation emits.
    fn label(self) -> &'static str {
        match self {
            IntArith::Add => "add",
            IntArith::Sub => "sub",
            IntArith::Mul => "mul",
        }
    }
}

/// Throw a catchable language exception when `condition` holds, and leave
/// the builder positioned on the path where it does not. `ntsc_throw` never
/// returns, so the throw path branches straight to the enclosing handler.
pub(crate) fn emit_guard_throw<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    condition: inkwell::values::IntValue<'ctx>,
    message: &str,
    label: &str,
) -> Result<(), crate::CodegenError> {
    let ctx = fn_ctx.context;
    let throw_bb = ctx.append_basic_block(fn_ctx.function, &format!("{label}.throw"));
    let continue_bb = ctx.append_basic_block(fn_ctx.function, &format!("{label}.continue"));
    fn_ctx
        .builder
        .build_conditional_branch(condition, throw_bb, continue_bb)?;

    let throw_fn = fn_ctx
        .module
        .get_function("ntsc_throw")
        .ok_or_else(|| crate::CodegenError::LLVMError("ntsc_throw not declared".into()))?;
    let handler = fn_ctx.current_exception_handler();
    fn_ctx.builder.position_at_end(throw_bb);
    let message = build_string_words_permanent(fn_ctx, message)?;
    fn_ctx
        .builder
        .build_call(throw_fn, &[message.into()], "throw_guard")?;
    fn_ctx.builder.build_unconditional_branch(handler)?;

    fn_ctx.builder.position_at_end(continue_bb);
    Ok(())
}

/// Emit `lhs op rhs` on 64-bit signed integers with an overflow check.
///
/// NTSC defines integer overflow as a catchable exception in *every* build
/// mode. Plain `add`/`sub`/`mul` wrap and the `nsw` variants are poison on
/// overflow, so an optimized build could compute a different value than a
/// debug build from the same source; the check removes that difference.
/// Leaves the builder on the non-overflowing path.
pub(crate) fn emit_checked_int_arith<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    op: IntArith,
    lhs: inkwell::values::IntValue<'ctx>,
    rhs: inkwell::values::IntValue<'ctx>,
) -> Result<inkwell::values::IntValue<'ctx>, crate::CodegenError> {
    let i64_ty = fn_ctx.context.i64_type();
    let name = op.intrinsic();
    let decl = inkwell::intrinsics::Intrinsic::find(name)
        .and_then(|intrinsic| intrinsic.get_declaration(fn_ctx.module, &[i64_ty.into()]))
        .ok_or_else(|| crate::CodegenError::LLVMError(format!("{name} is not available")))?;
    let call = fn_ctx
        .builder
        .build_call(decl, &[lhs.into(), rhs.into()], op.label())?;

    // The intrinsic returns `{ i64, i1 }`: the wrapped result and whether
    // the operation overflowed.
    let pair = call
        .try_as_basic_value()
        .basic()
        .ok_or_else(|| crate::CodegenError::LLVMError(format!("{name} returned no value")))?
        .into_struct_value();
    let result = fn_ctx
        .builder
        .build_extract_value(pair, 0, &format!("{}_value", op.label()))?
        .into_int_value();
    let overflowed = fn_ctx
        .builder
        .build_extract_value(pair, 1, &format!("{}_overflow", op.label()))?
        .into_int_value();
    emit_guard_throw(fn_ctx, overflowed, op.message(), op.label())?;
    Ok(result)
}

/// Guard a shift against an out-of-range shift amount.
///
/// `shl` and `ashr` are poison when the amount is negative or at least the
/// operand's bit width — exactly the build-mode-dependent result the
/// language forbids — so an out-of-range amount throws instead. One
/// *unsigned* comparison covers both ends: a negative `i64` reads as a very
/// large unsigned value, so it fails the same bound.
pub(crate) fn emit_shift_amount_guard<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    amount: inkwell::values::IntValue<'ctx>,
) -> Result<(), crate::CodegenError> {
    let width = fn_ctx.context.i64_type().const_int(64, false);
    let out_of_range = fn_ctx.builder.build_int_compare(
        inkwell::IntPredicate::UGE,
        amount,
        width,
        "shift_range_check",
    )?;
    emit_guard_throw(fn_ctx, out_of_range, "shift amount out of range", "shift")
}

/// Guard signed integer division and remainder against LLVM's two invalid
/// cases: a zero divisor and `i64::MIN / -1` overflow. Both throw a
/// catchable language exception instead of producing poison. Leaves the
/// builder at the continuation block.
pub(crate) fn emit_checked_divisor_guard<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    lhs: inkwell::values::IntValue<'ctx>,
    rhs: inkwell::values::IntValue<'ctx>,
) -> Result<(), crate::CodegenError> {
    let builder = fn_ctx.builder;
    let ctx = fn_ctx.context;
    let i64_ty = ctx.i64_type();
    let is_zero = builder.build_int_compare(
        inkwell::IntPredicate::EQ,
        rhs,
        i64_ty.const_zero(),
        "divzero_check",
    )?;
    let is_min = builder.build_int_compare(
        inkwell::IntPredicate::EQ,
        lhs,
        i64_ty.const_int(i64::MIN as u64, false),
        "div_min_check",
    )?;
    let is_neg_one = builder.build_int_compare(
        inkwell::IntPredicate::EQ,
        rhs,
        i64_ty.const_int((-1_i64) as u64, false),
        "div_neg_one_check",
    )?;
    let overflows = builder.build_and(is_min, is_neg_one, "div_overflow_check")?;
    let overflow_check_bb = ctx.append_basic_block(fn_ctx.function, "division.overflow_check");
    let zero_throw_bb = ctx.append_basic_block(fn_ctx.function, "division.zero_throw");
    let overflow_throw_bb = ctx.append_basic_block(fn_ctx.function, "division.overflow_throw");
    let continue_bb = ctx.append_basic_block(fn_ctx.function, "division.continue");
    builder.build_conditional_branch(is_zero, zero_throw_bb, overflow_check_bb)?;

    let throw_fn = fn_ctx
        .module
        .get_function("ntsc_throw")
        .ok_or_else(|| crate::CodegenError::LLVMError("ntsc_throw not declared".into()))?;
    let handler = fn_ctx.current_exception_handler();

    builder.position_at_end(zero_throw_bb);
    let message = build_string_words_permanent(fn_ctx, "division by zero")?;
    builder.build_call(throw_fn, &[message.into()], "throw_zero")?;
    builder.build_unconditional_branch(handler)?;

    builder.position_at_end(overflow_check_bb);
    builder.build_conditional_branch(overflows, overflow_throw_bb, continue_bb)?;

    builder.position_at_end(overflow_throw_bb);
    let message = build_string_words_permanent(fn_ctx, "integer division overflow")?;
    builder.build_call(throw_fn, &[message.into()], "throw_overflow")?;
    builder.build_unconditional_branch(handler)?;

    builder.position_at_end(continue_bb);
    Ok(())
}

/// Widen an `int` operand to `float` for mixed arithmetic.
pub(crate) fn coerce_int_to_float<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    val: TypedValue<'ctx>,
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    let f = fn_ctx.builder.build_signed_int_to_float(
        val.value.into_int_value(),
        fn_ctx.context.f64_type(),
        "int_to_float",
    )?;
    Ok(TypedValue::new(f.into(), Ty::Float))
}
