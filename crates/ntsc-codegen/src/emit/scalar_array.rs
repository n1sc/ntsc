//! Direct, bounds-checked access to nonescaping local scalar-array buffers.

use super::*;

#[derive(Clone)]
pub(crate) struct ScalarArrayBuffer<'ctx> {
    data: PointerValue<'ctx>,
    length: inkwell::values::IntValue<'ctx>,
    element: Ty,
}

fn is_variable(expression: &Expr, name: &str) -> bool {
    matches!(expression, Expr::Variable { name: variable } if variable.lexeme() == name)
}

fn element_uses_only(expression: &Expr, name: &str) -> bool {
    if !expr_mentions_var(expression, name) {
        return true;
    }
    match expression {
        Expr::IndexGet { object, index } => {
            (is_variable(object, name) || element_uses_only(object, name))
                && element_uses_only(index, name)
        }
        Expr::IndexSet {
            object,
            index,
            value,
        } => {
            (is_variable(object, name) || element_uses_only(object, name))
                && element_uses_only(index, name)
                && element_uses_only(value, name)
        }
        Expr::Binary { left, right, .. } => {
            element_uses_only(left, name) && element_uses_only(right, name)
        }
        Expr::Unary { right, .. } => element_uses_only(right, name),
        Expr::Grouping { expression, .. } => element_uses_only(expression, name),
        Expr::Assign {
            name: assigned,
            value,
        } if assigned.lexeme() != name => element_uses_only(value, name),
        Expr::Call {
            callee, arguments, ..
        } => {
            element_uses_only(callee, name)
                && arguments
                    .iter()
                    .all(|argument| element_uses_only(argument, name))
        }
        Expr::Ternary {
            condition,
            then_branch,
            else_branch,
        } => {
            element_uses_only(condition, name)
                && element_uses_only(then_branch, name)
                && element_uses_only(else_branch, name)
        }
        _ => false,
    }
}

fn statement_uses_only_elements(statement: &Stmt, name: &str) -> bool {
    match statement {
        Stmt::Expression { expression } | Stmt::Say { expression, .. } => {
            element_uses_only(expression, name)
        }
        Stmt::Var {
            initializer, view, ..
        } => initializer.as_ref().is_none_or(|expression| {
            (view.is_none() || !expr_mentions_var(expression, name))
                && element_uses_only(expression, name)
        }),
        Stmt::Return { value } => value
            .as_ref()
            .is_none_or(|value| element_uses_only(value, name)),
        Stmt::Throw { value } => element_uses_only(value, name),
        Stmt::Block { statements, .. } => statements
            .iter()
            .all(|statement| statement_uses_only_elements(statement, name)),
        Stmt::If {
            condition,
            then_branch,
            elif_branches,
            else_branch,
        } => {
            element_uses_only(condition, name)
                && statement_uses_only_elements(then_branch, name)
                && elif_branches.iter().all(|branch| {
                    element_uses_only(&branch.condition, name)
                        && statement_uses_only_elements(&branch.body, name)
                })
                && else_branch
                    .as_deref()
                    .is_none_or(|branch| statement_uses_only_elements(branch, name))
        }
        Stmt::While { condition, body } | Stmt::DoWhile { condition, body } => {
            element_uses_only(condition, name) && statement_uses_only_elements(body, name)
        }
        Stmt::For {
            init,
            condition,
            update,
            body,
        } => {
            init.as_deref()
                .is_none_or(|init| statement_uses_only_elements(init, name))
                && condition
                    .as_ref()
                    .is_none_or(|condition| element_uses_only(condition, name))
                && update
                    .as_ref()
                    .is_none_or(|update| element_uses_only(update, name))
                && statement_uses_only_elements(body, name)
        }
        Stmt::Try {
            try_block,
            catch_block,
            finally_block,
            ..
        } => {
            statement_uses_only_elements(try_block, name)
                && catch_block
                    .as_deref()
                    .is_none_or(|block| statement_uses_only_elements(block, name))
                && finally_block
                    .as_deref()
                    .is_none_or(|block| statement_uses_only_elements(block, name))
        }
        Stmt::Retry {
            count,
            body,
            catch_block,
            ..
        } => {
            element_uses_only(count, name)
                && statement_uses_only_elements(body, name)
                && catch_block
                    .as_deref()
                    .is_none_or(|block| statement_uses_only_elements(block, name))
        }
        Stmt::Unsafe { body } | Stmt::Quiet { body, .. } => {
            statement_uses_only_elements(body, name)
        }
        // Deferred bindings are outside walk_stmts' binding inventory.
        Stmt::Defer { .. }
        | Stmt::Private { .. }
        | Stmt::Function { .. }
        | Stmt::AsyncFunction { .. }
        | Stmt::Class { .. }
        | Stmt::Impl { .. }
        | Stmt::ExternFunction { .. }
        | Stmt::ExternBlock { .. } => false,
        _ => !stmt_mentions_var(statement, name),
    }
}

fn fresh_array(expression: &Expr) -> bool {
    match expression {
        Expr::ArrayLiteral { .. } => true,
        Expr::Call { callee, .. } => {
            matches!(callee.as_ref(), Expr::Member { object, property }
                if is_variable(object, "arrays") && property.lexeme() == "fill")
        }
        _ => false,
    }
}

/// The element count of a fresh array when it is written as a literal.
///
/// A known length lets LLVM fold bounds checks on indices whose value range
/// is already bounded (for example `(x >> shift) & 255` against a 256-slot
/// histogram), which the runtime's opaque `ntsc_array_len` prevents.
fn literal_length(expression: &Expr) -> Option<u64> {
    match expression {
        Expr::ArrayLiteral { elements, .. } => u64::try_from(elements.len()).ok(),
        Expr::Call {
            callee, arguments, ..
        } => {
            let Expr::Member { object, property } = callee.as_ref() else {
                return None;
            };
            if !is_variable(object, "arrays") || property.lexeme() != "fill" {
                return None;
            }
            let [_, count] = arguments.as_slice() else {
                return None;
            };
            int_literal_value(count)
        }
        _ => None,
    }
}

fn int_literal_value(expression: &Expr) -> Option<u64> {
    match expression {
        Expr::Literal {
            value: ntsc_ast::expr::LiteralValue::Number(text),
            ..
        } => text.parse().ok(),
        Expr::Grouping { expression, .. } => int_literal_value(expression),
        _ => None,
    }
}

pub(crate) fn nonescaping_scalar_arrays(
    body: &[Stmt],
    params: &[ntsc_ast::expr::FunctionParam],
) -> HashMap<String, Option<u64>> {
    let mut bindings: HashMap<String, usize> = HashMap::new();
    let mut record = |token: &ntsc_ast::token::Token| {
        *bindings.entry(token.lexeme().to_string()).or_default() += 1;
    };
    params.iter().for_each(|param| record(&param.name));
    super::call::walk_stmts(body, &mut |statement| match statement {
        Stmt::Var { name, .. } => record(name),
        Stmt::ForIn { variable, .. }
        | Stmt::ForAwait { variable, .. }
        | Stmt::ChanRecvFor { variable, .. } => record(variable),
        Stmt::Destructure { names, .. } => names.iter().for_each(&mut record),
        Stmt::Try {
            catch_var: Some(name),
            ..
        }
        | Stmt::Retry {
            catch_var: Some(name),
            ..
        } => record(name),
        Stmt::Match { cases, .. } => {
            for case in cases {
                if let Some(pattern) = &case.pattern {
                    pattern.bindings.iter().for_each(&mut record);
                }
            }
        }
        _ => {}
    });
    body.iter()
        .filter_map(|statement| {
            let Stmt::Var {
                name,
                initializer: Some(initializer),
                view: None,
                is_static: false,
                ..
            } = statement
            else {
                return None;
            };
            let name = name.lexeme();
            (bindings.get(name) == Some(&1)
                && fresh_array(initializer)
                && body
                    .iter()
                    .all(|statement| statement_uses_only_elements(statement, name)))
            .then(|| (name.to_string(), literal_length(initializer)))
        })
        .collect()
}

pub(crate) fn bind_scalar_array_buffer<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    name: &str,
) -> Result<(), crate::CodegenError> {
    let Some(known_length) = fn_ctx.scalar_array_candidates.get(name).copied() else {
        return Ok(());
    };
    let Some((slot, Ty::Array(element))) = fn_ctx.variables.get(name) else {
        return Ok(());
    };
    if !matches!(element.as_ref(), Ty::Int | Ty::Float | Ty::Bool) {
        return Ok(());
    }
    let element = (**element).clone();
    let handle =
        fn_ctx
            .builder
            .build_load(fn_ctx.context.i64_type(), *slot, "scalar_array_handle")?;
    let data_fn = fn_ctx
        .module
        .get_function("ntsc_array_scalar_data")
        .ok_or_else(|| {
            crate::CodegenError::LLVMError("ntsc_array_scalar_data not declared".into())
        })?;
    let data = call_result_to_value(
        fn_ctx,
        &fn_ctx
            .builder
            .build_call(data_fn, &[handle.into()], "scalar_array_data")?,
    )
    .into_pointer_value();
    let length = match known_length {
        Some(length) => fn_ctx.context.i64_type().const_int(length, false),
        None => {
            let length_fn = fn_ctx
                .module
                .get_function("ntsc_array_len")
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError("ntsc_array_len not declared".into())
                })?;
            call_result_to_value(
                fn_ctx,
                &fn_ctx
                    .builder
                    .build_call(length_fn, &[handle.into()], "scalar_array_length")?,
            )
            .into_int_value()
        }
    };
    fn_ctx.scalar_array_buffers.insert(
        name.into(),
        ScalarArrayBuffer {
            data,
            length,
            element,
        },
    );
    Ok(())
}

fn buffer_for<'ctx>(
    fn_ctx: &FunctionContext<'ctx, '_>,
    object: &Expr,
) -> Option<ScalarArrayBuffer<'ctx>> {
    let Expr::Variable { name } = object else {
        return None;
    };
    fn_ctx.scalar_array_buffers.get(name.lexeme()).cloned()
}

/// The name a candidate-array access reads or writes. Callers only reach
/// this after `buffer_for` matched, so the object is always a variable.
fn buffer_name(object: &Expr) -> &str {
    match object {
        Expr::Variable { name } => name.lexeme(),
        _ => "",
    }
}

fn checked_element_pointer<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    name: &str,
    buffer: &ScalarArrayBuffer<'ctx>,
    index: inkwell::values::IntValue<'ctx>,
    index_expression: &Expr,
) -> Result<PointerValue<'ctx>, crate::CodegenError> {
    let trusted = index_is_trusted(fn_ctx.trusted_loop.as_ref(), index_expression, name);
    if !trusted {
        let invalid = fn_ctx.builder.build_int_compare(
            IntPredicate::UGE,
            index,
            buffer.length,
            "scalar_array_out_of_bounds",
        )?;
        emit_guard_throw(
            fn_ctx,
            invalid,
            "array index out of bounds",
            "scalar_array_bounds",
        )?;
    }
    // The unsigned check rejects negative indices too. Escape analysis keeps
    // the owning array alive, unaliased and unresized while this buffer is
    // used; a versioned loop's preheader additionally proved every index in
    // its range is in bounds.
    Ok(unsafe {
        fn_ctx.builder.build_in_bounds_gep(
            fn_ctx.context.i64_type(),
            buffer.data,
            &[index],
            "scalar_array_element",
        )?
    })
}

/// Emit an index expression, lowering its integer arithmetic unchecked when
/// a versioned loop's preheader already proved the range.
pub(crate) fn emit_index_expression<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    object: &Expr,
    index: &Expr,
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    let trusted = buffer_for(fn_ctx, object).is_some()
        && matches!(object, Expr::Variable { name } if index_is_trusted(fn_ctx.trusted_loop.as_ref(), index, name.lexeme()));
    if !trusted {
        return emit_expression(fn_ctx, index);
    }
    let previous = fn_ctx.int_arith_mode;
    fn_ctx.int_arith_mode = IntArithMode::Unchecked;
    let value = emit_expression(fn_ctx, index);
    fn_ctx.int_arith_mode = previous;
    value
}

pub(crate) fn read_scalar_array<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    object: &Expr,
    index: inkwell::values::IntValue<'ctx>,
    index_expression: &Expr,
) -> Result<Option<TypedValue<'ctx>>, crate::CodegenError> {
    let Some(buffer) = buffer_for(fn_ctx, object) else {
        return Ok(None);
    };
    let pointer = checked_element_pointer(
        fn_ctx,
        buffer_name(object),
        &buffer,
        index,
        index_expression,
    )?;
    let raw = fn_ctx
        .builder
        .build_load(fn_ctx.context.i64_type(), pointer, "scalar_array_load")?;
    let value = decode_array_scalar(fn_ctx, raw, &buffer.element)?;
    Ok(Some(TypedValue::new(value, buffer.element)))
}

pub(crate) fn write_scalar_array<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    object: &Expr,
    index: inkwell::values::IntValue<'ctx>,
    index_expression: &Expr,
    value: &TypedValue<'ctx>,
) -> Result<bool, crate::CodegenError> {
    let Some(buffer) = buffer_for(fn_ctx, object) else {
        return Ok(false);
    };
    let pointer = checked_element_pointer(
        fn_ctx,
        buffer_name(object),
        &buffer,
        index,
        index_expression,
    )?;
    let raw = encode_array_scalar(fn_ctx, value)?;
    fn_ctx.builder.build_store(pointer, raw)?;
    Ok(true)
}

// ── Loop versioning ─────────────────────────────────────────────────────

/// One addend of an index expression written as `iv + c1 + c2 + ...`.
#[derive(Clone, Copy)]
enum IndexAddend<'a> {
    /// The induction variable, replaced by a loop bound in the preheader.
    Induction,
    /// A loop-invariant integer expression.
    Invariant(&'a Expr),
}

/// One candidate-array access inside a versioned loop.
struct VersionedAccess<'a, 'ctx> {
    array: &'a str,
    index: &'a Expr,
    addends: Vec<IndexAddend<'a>>,
    length: inkwell::values::IntValue<'ctx>,
}

/// The loop facts the emitter needs to skip a bounds check at an access
/// site. Produced by the same analysis that emitted the preheader proof, so
/// the two always agree on which indices are known to be in range.
pub(crate) struct TrustedLoop {
    pub(crate) iv: String,
    indices: HashMap<String, Vec<Expr>>,
    /// Set when the loop was proved by its guard rather than by an exact
    /// range: see [`BoundedIndex`].
    pub(crate) bounded: Option<BoundedIndex>,
}

/// A loop whose condition `E < bound` bounds every access index.
///
/// The preheader proves `iv >= 0` and `bound + slack <= len(array)` for every
/// accessed array. Inside the body the guard gives `E < bound`, and an index
/// `scale * iv + offset` with `scale <= E.scale` is at most
/// `E + (offset - E.offset)`, so it stays below `bound + slack` and in range.
pub(crate) struct BoundedIndex {
    /// Coefficients of the guard's left-hand side `E`.
    guard: AffineForm,
    /// Affine form of each local the proof tracks.
    forms: HashMap<String, AffineForm>,
    /// Per-array overshoot the preheader checked against the array length.
    slack: HashMap<String, u64>,
}

/// Whether the enclosing versioned loop proved `index` stays in range for an
/// access to `array`.
fn index_is_trusted(trusted: Option<&TrustedLoop>, index: &Expr, array: &str) -> bool {
    let Some(trusted) = trusted else {
        return false;
    };
    if let Some(bounded) = &trusted.bounded {
        let Some(form) = affine_form(index, &trusted.iv, &bounded.forms) else {
            return false;
        };
        let Some(slack) = bounded.slack.get(array) else {
            return false;
        };
        return form.scale <= bounded.guard.scale
            && form.offset <= bounded.guard.offset.saturating_add(*slack);
    }
    trusted
        .indices
        .get(array)
        .is_some_and(|indices| indices.contains(index))
}

/// A loop the analysis proved safe to run unchecked, together with the proof
/// its preheader emits.
enum VersionedLoop<'a, 'ctx> {
    /// `iv < limit` with `iv` stepped by one. The preheader computes the
    /// exact range the induction variable walks and checks it against every
    /// accessed array.
    Induction {
        iv: String,
        iv_ptr: PointerValue<'ctx>,
        limit: &'a Expr,
        accesses: Vec<VersionedAccess<'a, 'ctx>>,
    },
    /// `E < bound` with `E` affine in `iv`. The guard itself bounds every
    /// index, so the preheader only proves `iv >= 0` and that `bound` plus
    /// the largest overshoot fits inside each accessed array.
    Bounded {
        iv: String,
        iv_ptr: PointerValue<'ctx>,
        bound: &'a Expr,
        guard: AffineForm,
        update: AffineForm,
        forms: HashMap<String, AffineForm>,
        accesses: Vec<BoundedAccess<'ctx>>,
    },
}

/// One accessed array of a guard-bounded loop, with the largest amount an
/// index may exceed the guard's left-hand side.
struct BoundedAccess<'ctx> {
    name: String,
    slack: u64,
    length: inkwell::values::IntValue<'ctx>,
}

/// An index written as `scale * iv + offset` with non-negative coefficients.
///
/// Every form the analysis derives is an upper bound for the expression
/// while the induction variable stays non-negative, which the guard-bounded
/// proof establishes separately.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct AffineForm {
    scale: u64,
    offset: u64,
}

impl AffineForm {
    const INDUCTION: Self = Self {
        scale: 1,
        offset: 0,
    };

    fn add(self, other: Self) -> Option<Self> {
        Some(Self {
            scale: self.scale.checked_add(other.scale)?,
            offset: self.offset.checked_add(other.offset)?,
        })
    }

    fn scale_by(self, factor: u64) -> Option<Self> {
        if factor == 0 {
            return None;
        }
        Some(Self {
            scale: self.scale.checked_mul(factor)?,
            offset: self.offset.checked_mul(factor)?,
        })
    }

    fn shift(self, step: u64) -> Option<Self> {
        Some(Self {
            scale: self.scale,
            offset: self.offset.checked_add(step)?,
        })
    }
}

/// The affine form of `expression` in `iv`, or `None` when it is not a
/// non-negative combination of the induction variable and literals.
fn affine_form(
    expression: &Expr,
    iv: &str,
    forms: &HashMap<String, AffineForm>,
) -> Option<AffineForm> {
    match expression {
        Expr::Grouping { expression, .. } => affine_form(expression, iv, forms),
        Expr::Literal { .. } => Some(AffineForm {
            scale: 0,
            offset: non_negative_literal(expression)?,
        }),
        Expr::Variable { name } if name.lexeme() == iv => Some(AffineForm::INDUCTION),
        Expr::Variable { name } => forms.get(name.lexeme()).copied(),
        Expr::Binary { left, op, right } => match op.kind {
            TokenKind::Plus => affine_form(left, iv, forms)?.add(affine_form(right, iv, forms)?),
            TokenKind::Star => match (non_negative_literal(left), non_negative_literal(right)) {
                (Some(factor), _) => affine_form(right, iv, forms)?.scale_by(factor),
                (_, Some(factor)) => affine_form(left, iv, forms)?.scale_by(factor),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

/// The value of an integer literal, when it is non-negative.
fn non_negative_literal(expression: &Expr) -> Option<u64> {
    let Expr::Literal {
        value: LiteralValue::Number(text),
        ..
    } = expression
    else {
        return None;
    };
    u64::try_from(ntsc_ast::expr::parse_int_literal(text)?).ok()
}

/// Resolve `index` into `iv` plus loop-invariant addends, keeping the source
/// order so the preheader evaluates exactly the same additions the loop body
/// would. `None` means the index does not move one-for-one with the
/// induction variable, so no range can be proven for it.
fn resolve_index_addends<'a>(
    fn_ctx: &FunctionContext<'_, '_>,
    index: &'a Expr,
    iv: &str,
    locals: &HashMap<String, Vec<IndexAddend<'a>>>,
    forbidden: &HashSet<String>,
) -> Option<Vec<IndexAddend<'a>>> {
    match index {
        Expr::Grouping { expression, .. } => {
            resolve_index_addends(fn_ctx, expression, iv, locals, forbidden)
        }
        Expr::Variable { name } if name.lexeme() == iv => Some(vec![IndexAddend::Induction]),
        Expr::Variable { name } => match locals.get(name.lexeme()) {
            Some(addends) => Some(addends.clone()),
            None => is_invariant_int_expression(fn_ctx, index, forbidden)
                .then_some(vec![IndexAddend::Invariant(index)]),
        },
        Expr::Binary { left, op, right } if op.kind == TokenKind::Plus => {
            let mut addends = resolve_index_addends(fn_ctx, left, iv, locals, forbidden)?;
            addends.extend(resolve_index_addends(fn_ctx, right, iv, locals, forbidden)?);
            Some(addends)
        }
        _ => is_invariant_int_expression(fn_ctx, index, forbidden)
            .then_some(vec![IndexAddend::Invariant(index)]),
    }
}

/// Locals bound once to an index that moves with the loop variable, in
/// declaration order so a later local can build on an earlier one.
fn collect_affine_locals<'a>(
    fn_ctx: &FunctionContext<'_, '_>,
    statements: &'a [Stmt],
    iv: &str,
    assigned: &HashMap<String, usize>,
    declared: &HashMap<String, usize>,
    forbidden: &HashSet<String>,
) -> HashMap<String, Vec<IndexAddend<'a>>> {
    let mut locals = HashMap::new();
    for statement in statements {
        let Stmt::Var {
            name,
            initializer: Some(initializer),
            ..
        } = statement
        else {
            continue;
        };
        // A name declared again in a nested scope could refer to a different
        // value at an access site the proof does not cover.
        if declared.get(name.lexeme()) != Some(&1) || assigned.contains_key(name.lexeme()) {
            continue;
        }
        let Some(addends) = resolve_index_addends(fn_ctx, initializer, iv, &locals, forbidden)
        else {
            continue;
        };
        if addends
            .iter()
            .any(|addend| matches!(addend, IndexAddend::Induction))
        {
            locals.insert(name.lexeme().to_string(), addends);
        }
    }
    locals
}

/// Emit a loop as two versions: an unchecked body guarded by a preheader
/// range proof, and the ordinary checked body as the fallback.
///
/// Returns `false` (emitting nothing) when the loop does not match the
/// supported shape: a `while (iv < limit)` whose body is straight-line code
/// or conditionals, declares only scalar locals, updates `iv` exactly once
/// by one, and reads or writes at least one candidate array through an index
/// that is `iv` plus loop-invariant terms. Accesses the proof does not cover
/// keep their bounds checks in the fast body. The checked fallback preserves
/// exception timing and partial-write behavior exactly, so this is purely an
/// optimization.
pub(crate) fn emit_versioned_while<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    condition: &Expr,
    body: &Stmt,
) -> Result<bool, crate::CodegenError> {
    if fn_ctx.trusted_loop.is_some() {
        return Ok(false);
    }
    let Some(plan) = analyze_versioned_loop(fn_ctx, condition, body) else {
        return Ok(false);
    };

    let previous_mode = fn_ctx.int_arith_mode;
    fn_ctx.int_arith_mode = IntArithMode::Track;
    fn_ctx.tracked_overflow = None;

    let (proven, trusted) = match &plan {
        VersionedLoop::Induction {
            iv,
            iv_ptr,
            limit,
            accesses,
        } => {
            let proven = emit_induction_proof(fn_ctx, *iv_ptr, limit, accesses)?;
            let mut indices: HashMap<String, Vec<Expr>> = HashMap::new();
            for access in accesses {
                indices
                    .entry(access.array.to_string())
                    .or_default()
                    .push(access.index.clone());
            }
            let trusted = TrustedLoop {
                iv: iv.clone(),
                indices,
                bounded: None,
            };
            (proven, trusted)
        }
        VersionedLoop::Bounded {
            iv,
            iv_ptr,
            bound,
            guard,
            update,
            forms,
            accesses,
        } => {
            let proven = emit_bounded_proof(fn_ctx, *iv_ptr, bound, *guard, *update, accesses)?;
            let trusted = TrustedLoop {
                iv: iv.clone(),
                indices: HashMap::new(),
                bounded: Some(BoundedIndex {
                    guard: *guard,
                    forms: forms.clone(),
                    slack: accesses
                        .iter()
                        .map(|access| (access.name.clone(), access.slack))
                        .collect(),
                }),
            };
            (proven, trusted)
        }
    };
    let overflow = fn_ctx.tracked_overflow;
    fn_ctx.int_arith_mode = previous_mode;
    fn_ctx.tracked_overflow = None;

    let no_overflow = match overflow {
        Some(overflow) => fn_ctx
            .builder
            .build_not(overflow, "versioned_no_overflow")?,
        None => fn_ctx.context.bool_type().const_int(1, false),
    };
    let safe = fn_ctx
        .builder
        .build_and(no_overflow, proven, "versioned_safe")?;

    let fast_bb = fn_ctx
        .context
        .append_basic_block(fn_ctx.function, "while.versioned.fast");
    let slow_bb = fn_ctx
        .context
        .append_basic_block(fn_ctx.function, "while.versioned.slow");
    let merge_bb = fn_ctx
        .context
        .append_basic_block(fn_ctx.function, "while.versioned.merge");
    fn_ctx
        .builder
        .build_conditional_branch(safe, fast_bb, slow_bb)?;

    fn_ctx.builder.position_at_end(fast_bb);
    fn_ctx.trusted_loop = Some(trusted);
    emit_while(fn_ctx, condition, body)?;
    fn_ctx.trusted_loop = None;
    fn_ctx.builder.build_unconditional_branch(merge_bb)?;

    fn_ctx.builder.position_at_end(slow_bb);
    emit_while(fn_ctx, condition, body)?;
    fn_ctx.builder.build_unconditional_branch(merge_bb)?;

    fn_ctx.builder.position_at_end(merge_bb);
    Ok(true)
}

/// Emit the preheader proof for `iv < limit` with a one-step update: every
/// access index is `iv` plus loop-invariant addends, so its range is
/// `[start, limit - 1]` plus those addends.
fn emit_induction_proof<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    iv_ptr: PointerValue<'ctx>,
    limit: &Expr,
    accesses: &[VersionedAccess<'_, 'ctx>],
) -> Result<inkwell::values::IntValue<'ctx>, crate::CodegenError> {
    let i64_ty = fn_ctx.context.i64_type();
    let zero = i64_ty.const_zero();
    let start = fn_ctx
        .builder
        .build_load(i64_ty, iv_ptr, "versioned_start")?
        .into_int_value();
    let limit = emit_expression(fn_ctx, limit)?.value.into_int_value();
    let one = i64_ty.const_int(1, false);
    let last_offset = emit_checked_int_arith(fn_ctx, IntArith::Sub, limit, one)?;
    let mut ranges = Vec::with_capacity(accesses.len());
    for access in accesses {
        let mut first = zero;
        let mut last = zero;
        for addend in &access.addends {
            let (first_addend, last_addend) = match addend {
                IndexAddend::Induction => (start, last_offset),
                IndexAddend::Invariant(expression) => {
                    let value = emit_expression(fn_ctx, expression)?.value.into_int_value();
                    (value, value)
                }
            };
            first = emit_checked_int_arith(fn_ctx, IntArith::Add, first, first_addend)?;
            last = emit_checked_int_arith(fn_ctx, IntArith::Add, last, last_addend)?;
        }
        ranges.push((first, last, access.length));
    }

    // An empty loop accesses nothing, so either version is correct.
    let empty =
        fn_ctx
            .builder
            .build_int_compare(IntPredicate::SGE, start, limit, "versioned_empty")?;
    let mut range_ok = None;
    for (first, last, length) in ranges {
        let first_ok = fn_ctx.builder.build_int_compare(
            IntPredicate::SGE,
            first,
            zero,
            "versioned_first_ok",
        )?;
        let last_ok = fn_ctx.builder.build_int_compare(
            IntPredicate::ULT,
            last,
            length,
            "versioned_last_ok",
        )?;
        let in_range = fn_ctx
            .builder
            .build_and(first_ok, last_ok, "versioned_in_range")?;
        range_ok = Some(match range_ok {
            Some(previous) => fn_ctx
                .builder
                .build_and(previous, in_range, "versioned_range")?,
            None => in_range,
        });
    }
    let range_ok = range_ok
        .ok_or_else(|| crate::CodegenError::LLVMError("versioned loop has no accesses".into()))?;
    Ok(fn_ctx
        .builder
        .build_or(empty, range_ok, "versioned_in_bounds")?)
}

/// Emit the preheader proof for `E < bound`: the induction variable starts
/// non-negative, and `bound` plus the largest overshoot still fits inside
/// every accessed array.
fn emit_bounded_proof<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    iv_ptr: PointerValue<'ctx>,
    bound: &Expr,
    guard: AffineForm,
    update: AffineForm,
    accesses: &[BoundedAccess<'ctx>],
) -> Result<inkwell::values::IntValue<'ctx>, crate::CodegenError> {
    let i64_ty = fn_ctx.context.i64_type();
    let zero = i64_ty.const_zero();
    let start = fn_ctx
        .builder
        .build_load(i64_ty, iv_ptr, "versioned_start")?
        .into_int_value();
    let bound = emit_expression(fn_ctx, bound)?.value.into_int_value();
    let starts_above_bound = fn_ctx.builder.build_int_compare(
        IntPredicate::SGT,
        start,
        bound,
        "versioned_starts_above_bound",
    )?;
    let upper = fn_ctx
        .builder
        .build_select(starts_above_bound, start, bound, "versioned_upper")?
        .into_int_value();
    let updated_upper = emit_affine_upper_bound(fn_ctx, update, upper)?;
    let update_grows = fn_ctx.builder.build_int_compare(
        IntPredicate::SGT,
        updated_upper,
        upper,
        "versioned_update_grows",
    )?;
    let next_upper = fn_ctx
        .builder
        .build_select(update_grows, updated_upper, upper, "versioned_next_upper")?
        .into_int_value();
    // Prove the initial guard and the guard after the last update cannot
    // wrap and admit a negative index into the unchecked body.
    emit_affine_upper_bound(fn_ctx, guard, next_upper)?;
    let mut proven =
        fn_ctx
            .builder
            .build_int_compare(IntPredicate::SGE, start, zero, "versioned_start_ok")?;
    for access in accesses {
        let slack = i64_ty.const_int(access.slack, false);
        let padded = emit_checked_int_arith(fn_ctx, IntArith::Add, bound, slack)?;
        let fits = fn_ctx.builder.build_int_compare(
            IntPredicate::ULE,
            padded,
            access.length,
            "versioned_fits",
        )?;
        proven = fn_ctx
            .builder
            .build_and(proven, fits, "versioned_in_bounds")?;
    }
    Ok(proven)
}

fn emit_affine_upper_bound<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    form: AffineForm,
    upper: inkwell::values::IntValue<'ctx>,
) -> Result<inkwell::values::IntValue<'ctx>, crate::CodegenError> {
    let i64_ty = fn_ctx.context.i64_type();
    let scaled = emit_checked_int_arith(
        fn_ctx,
        IntArith::Mul,
        upper,
        i64_ty.const_int(form.scale, false),
    )?;
    emit_checked_int_arith(
        fn_ctx,
        IntArith::Add,
        scaled,
        i64_ty.const_int(form.offset, false),
    )
}

/// Collect declarations, assignments and borrows over the whole body,
/// including nested blocks and conditionals. Returns `false` for statement
/// shapes the analysis cannot reason about.
///
/// A loop reached through a conditional runs rarely, so duplicating it into
/// the fast body is cheap. One in the straight-line body runs every
/// iteration, so it would bloat the hot path instead.
fn scan_body_names(
    statement: &Stmt,
    iv: &str,
    in_conditional: bool,
    assigned: &mut HashMap<String, usize>,
    borrowed: &mut HashSet<String>,
    declared: &mut HashMap<String, usize>,
) -> bool {
    match statement {
        Stmt::Expression { expression } => {
            collect_assigned_names(expression, assigned)
                && collect_borrowed_names(expression, borrowed)
        }
        Stmt::Var {
            name,
            type_annotation,
            initializer,
            view: None,
            is_static: false,
            ..
        } => {
            // Only scalars: a body-declared owned value would need its own
            // drop path in both versions of the body.
            if name.lexeme() == iv
                || !matches!(
                    type_annotation_to_ty(type_annotation),
                    Ty::Int | Ty::Float | Ty::Bool
                )
            {
                return false;
            }
            *declared.entry(name.lexeme().to_string()).or_default() += 1;
            initializer.as_ref().is_none_or(|expression| {
                collect_assigned_names(expression, assigned)
                    && collect_borrowed_names(expression, borrowed)
            })
        }
        Stmt::If {
            condition,
            then_branch,
            elif_branches,
            else_branch,
        } => {
            collect_assigned_names(condition, assigned)
                && collect_borrowed_names(condition, borrowed)
                && scan_body_names(then_branch, iv, true, assigned, borrowed, declared)
                && elif_branches.iter().all(|branch| {
                    collect_assigned_names(&branch.condition, assigned)
                        && collect_borrowed_names(&branch.condition, borrowed)
                        && scan_body_names(&branch.body, iv, true, assigned, borrowed, declared)
                })
                && else_branch.as_ref().is_none_or(|body| {
                    scan_body_names(body, iv, true, assigned, borrowed, declared)
                })
        }
        Stmt::Block { statements, .. } => statements.iter().all(|statement| {
            scan_body_names(statement, iv, in_conditional, assigned, borrowed, declared)
        }),
        Stmt::While { condition, body } => {
            in_conditional
                && collect_assigned_names(condition, assigned)
                && collect_borrowed_names(condition, borrowed)
                && scan_body_names(body, iv, in_conditional, assigned, borrowed, declared)
        }
        Stmt::Break { .. } | Stmt::Continue { .. } => true,
        Stmt::Return { value } => value.as_ref().is_none_or(|expression| {
            collect_assigned_names(expression, assigned)
                && collect_borrowed_names(expression, borrowed)
        }),
        _ => false,
    }
}

/// Collect provable array accesses over the whole body, including nested
/// blocks and conditionals.
fn scan_body_accesses<'a, 'ctx>(
    fn_ctx: &FunctionContext<'ctx, '_>,
    statement: &'a Stmt,
    iv: &str,
    in_conditional: bool,
    locals: &HashMap<String, Vec<IndexAddend<'a>>>,
    forbidden: &HashSet<String>,
    accesses: &mut Vec<VersionedAccess<'a, 'ctx>>,
) -> bool {
    match statement {
        Stmt::Expression { expression } => {
            collect_versioned_accesses(fn_ctx, expression, iv, locals, forbidden, accesses)
        }
        Stmt::Var {
            initializer: Some(initializer),
            ..
        } => collect_versioned_accesses(fn_ctx, initializer, iv, locals, forbidden, accesses),
        Stmt::Var { .. } => true,
        Stmt::If {
            condition,
            then_branch,
            elif_branches,
            else_branch,
        } => {
            collect_versioned_accesses(fn_ctx, condition, iv, locals, forbidden, accesses)
                && scan_body_accesses(fn_ctx, then_branch, iv, true, locals, forbidden, accesses)
                && elif_branches.iter().all(|branch| {
                    collect_versioned_accesses(
                        fn_ctx,
                        &branch.condition,
                        iv,
                        locals,
                        forbidden,
                        accesses,
                    ) && scan_body_accesses(
                        fn_ctx,
                        &branch.body,
                        iv,
                        true,
                        locals,
                        forbidden,
                        accesses,
                    )
                })
                && else_branch.as_ref().is_none_or(|body| {
                    scan_body_accesses(fn_ctx, body, iv, true, locals, forbidden, accesses)
                })
        }
        Stmt::Block { statements, .. } => statements.iter().all(|statement| {
            scan_body_accesses(
                fn_ctx,
                statement,
                iv,
                in_conditional,
                locals,
                forbidden,
                accesses,
            )
        }),
        Stmt::While { condition, body } => {
            in_conditional
                && collect_versioned_accesses(fn_ctx, condition, iv, locals, forbidden, accesses)
                && scan_body_accesses(
                    fn_ctx,
                    body,
                    iv,
                    in_conditional,
                    locals,
                    forbidden,
                    accesses,
                )
        }
        Stmt::Break { .. } | Stmt::Continue { .. } => true,
        Stmt::Return { value } => value.as_ref().is_none_or(|expression| {
            collect_versioned_accesses(fn_ctx, expression, iv, locals, forbidden, accesses)
        }),
        _ => false,
    }
}

fn analyze_versioned_loop<'a, 'ctx>(
    fn_ctx: &FunctionContext<'ctx, '_>,
    condition: &'a Expr,
    body: &'a Stmt,
) -> Option<VersionedLoop<'a, 'ctx>> {
    let Expr::Binary { left, op, right } = condition else {
        return None;
    };
    if op.kind != TokenKind::Less {
        return None;
    }
    let statements: &[Stmt] = match body {
        Stmt::Expression { .. } => std::slice::from_ref(body),
        Stmt::Block { statements, .. } => statements,
        _ => return None,
    };

    // Prefer the exact-range shape, which proves a tighter bound.
    if let Expr::Variable { name: iv_token } = left.as_ref()
        && let Some(plan) =
            analyze_induction_loop(fn_ctx, iv_token.lexeme(), right, statements, body)
    {
        return Some(plan);
    }
    analyze_bounded_loop(fn_ctx, left, right, statements, body)
}

fn analyze_induction_loop<'a, 'ctx>(
    fn_ctx: &FunctionContext<'ctx, '_>,
    iv: &str,
    right: &'a Expr,
    statements: &'a [Stmt],
    body: &'a Stmt,
) -> Option<VersionedLoop<'a, 'ctx>> {
    let (iv_ptr, iv_ty) = fn_ctx.lookup_var(iv)?;
    if !matches!(iv_ty, Ty::Int) {
        return None;
    }

    let mut assigned: HashMap<String, usize> = HashMap::new();
    let mut borrowed = HashSet::new();
    let mut declared: HashMap<String, usize> = HashMap::new();
    if !scan_body_names(body, iv, false, &mut assigned, &mut borrowed, &mut declared) {
        return None;
    }
    if assigned.get(iv).copied().unwrap_or(0) != 1 || borrowed.contains(iv) {
        return None;
    }

    let mut forbidden: HashSet<String> = assigned.keys().cloned().collect();
    forbidden.extend(borrowed);
    forbidden.extend(declared.keys().cloned());
    forbidden.insert(iv.to_string());
    if !is_invariant_int_expression(fn_ctx, right, &forbidden) || expr_mentions_var(right, iv) {
        return None;
    }

    // Only top-level declarations get an affine form: a local from a nested
    // scope is not visible at the access sites this proof covers.
    let locals = collect_affine_locals(fn_ctx, statements, iv, &assigned, &declared, &forbidden);

    let mut updates = 0;
    for statement in statements {
        if let Stmt::Expression {
            expression: Expr::Assign { name, value },
        } = statement
            && name.lexeme() == iv
        {
            updates += 1;
            if !is_iv_increment(value, iv) {
                return None;
            }
        }
    }
    if updates != 1 {
        return None;
    }
    if !matches!(statements.last(),
        Some(Stmt::Expression { expression: Expr::Assign { name, .. } })
            if name.lexeme() == iv)
    {
        return None;
    }

    let mut accesses = Vec::new();
    if !scan_body_accesses(fn_ctx, body, iv, false, &locals, &forbidden, &mut accesses)
        || accesses.is_empty()
    {
        return None;
    }

    Some(VersionedLoop::Induction {
        iv: iv.to_string(),
        iv_ptr,
        limit: right,
        accesses,
    })
}

/// Recognize a loop whose guard bounds every index: `E < bound`, where `E` is
/// an affine function of an induction variable the body assigns exactly once
/// as its last statement.
///
/// The proof needs no exact range. The guard keeps `E` below `bound`, so an
/// index `scale * iv + offset` with `scale <= E.scale` stays below
/// `bound + (offset - E.offset)`. The preheader checks `iv >= 0` and that this
/// padded bound fits inside each accessed array; the update keeps `iv`
/// non-negative because its own form has non-negative coefficients.
fn analyze_bounded_loop<'a, 'ctx>(
    fn_ctx: &FunctionContext<'ctx, '_>,
    left: &'a Expr,
    right: &'a Expr,
    statements: &'a [Stmt],
    body: &'a Stmt,
) -> Option<VersionedLoop<'a, 'ctx>> {
    let mut assigned: HashMap<String, usize> = HashMap::new();
    let mut borrowed = HashSet::new();
    let mut declared: HashMap<String, usize> = HashMap::new();
    if !scan_body_names(body, "", false, &mut assigned, &mut borrowed, &mut declared) {
        return None;
    }

    for iv in variables_in(left) {
        if declared.contains_key(&iv) || borrowed.contains(&iv) {
            continue;
        }
        if assigned.get(&iv).copied().unwrap_or(0) != 1 {
            continue;
        }
        let Some((iv_ptr, Ty::Int)) = fn_ctx.lookup_var(&iv) else {
            continue;
        };
        // Every access must read the value the guard constrained, so the one
        // update has to come last.
        let Some(Stmt::Expression {
            expression: Expr::Assign { name, value },
        }) = statements.last()
        else {
            continue;
        };
        if name.lexeme() != iv {
            continue;
        }

        let mut forbidden: HashSet<String> = assigned.keys().cloned().collect();
        forbidden.extend(borrowed.iter().cloned());
        forbidden.extend(declared.keys().cloned());
        forbidden.insert(iv.clone());
        if !is_invariant_int_expression(fn_ctx, right, &forbidden) || expr_mentions_var(right, &iv)
        {
            continue;
        }

        let forms = collect_affine_forms(statements, body, &iv, &assigned, &declared);
        let Some(guard) = affine_form(left, &iv, &forms) else {
            continue;
        };
        if guard.scale == 0 {
            continue;
        }
        // The update has to keep the induction variable non-negative.
        let Some(update) = affine_form(value, &iv, &forms) else {
            continue;
        };
        if [guard.scale, guard.offset, update.scale, update.offset]
            .into_iter()
            .any(|coefficient| i64::try_from(coefficient).is_err())
        {
            continue;
        }

        let mut accesses: HashMap<String, BoundedAccess<'ctx>> = HashMap::new();
        if !scan_bounded_accesses(fn_ctx, body, &iv, &forms, &guard, &mut accesses)
            || accesses.is_empty()
        {
            continue;
        }
        let mut accesses: Vec<BoundedAccess<'ctx>> = accesses.into_values().collect();
        accesses.sort_by(|left, right| left.name.cmp(&right.name));

        return Some(VersionedLoop::Bounded {
            iv,
            iv_ptr,
            bound: right,
            guard,
            update,
            forms,
            accesses,
        });
    }
    None
}

/// The distinct variables `expression` mentions, in source order.
fn variables_in(expression: &Expr) -> Vec<String> {
    let mut names = Vec::new();
    walk_supported_expression(expression, &mut |node| {
        if let Expr::Variable { name } = node {
            let name = name.lexeme().to_string();
            if !names.contains(&name) {
                names.push(name);
            }
        }
    });
    names
}

/// Affine forms of the locals declared at the top level of a loop body.
///
/// A local may be declared once and then stepped by a constant
/// (`child = child + 1`); its form is the declaration's plus that step, which
/// bounds every value it takes.
fn collect_affine_forms(
    statements: &[Stmt],
    body: &Stmt,
    iv: &str,
    assigned: &HashMap<String, usize>,
    declared: &HashMap<String, usize>,
) -> HashMap<String, AffineForm> {
    let mut forms = HashMap::new();
    for statement in statements {
        let Stmt::Var {
            name,
            initializer: Some(initializer),
            ..
        } = statement
        else {
            continue;
        };
        let name = name.lexeme();
        if declared.get(name) != Some(&1) {
            continue;
        }
        let Some(declared_form) = affine_form(initializer, iv, &forms) else {
            continue;
        };
        let form = match assigned.get(name).copied().unwrap_or(0) {
            0 => Some(declared_form),
            1 => self_increment_step(body, name).and_then(|step| declared_form.shift(step)),
            _ => None,
        };
        if let Some(form) = form {
            forms.insert(name.to_string(), form);
        }
    }
    forms
}

/// The constant step of `name = name + step`, when that is the only
/// assignment to `name` in the body.
fn self_increment_step(body: &Stmt, name: &str) -> Option<u64> {
    let mut values = Vec::new();
    if !collect_assignment_values(body, name, &mut values) || values.len() != 1 {
        return None;
    }
    let Expr::Binary { left, op, right } = values[0] else {
        return None;
    };
    if op.kind != TokenKind::Plus {
        return None;
    }
    if is_variable(left, name) {
        non_negative_literal(right)
    } else if is_variable(right, name) {
        non_negative_literal(left)
    } else {
        None
    }
}

fn collect_assignment_values<'a>(
    statement: &'a Stmt,
    name: &str,
    values: &mut Vec<&'a Expr>,
) -> bool {
    match statement {
        Stmt::Expression { expression } => collect_assigned_values(expression, name, values),
        Stmt::Var {
            initializer: Some(initializer),
            ..
        } => collect_assigned_values(initializer, name, values),
        Stmt::Var { .. } => true,
        Stmt::If {
            condition,
            then_branch,
            elif_branches,
            else_branch,
        } => {
            collect_assigned_values(condition, name, values)
                && collect_assignment_values(then_branch, name, values)
                && elif_branches
                    .iter()
                    .all(|branch| collect_assignment_values(&branch.body, name, values))
                && else_branch
                    .as_ref()
                    .is_none_or(|body| collect_assignment_values(body, name, values))
        }
        Stmt::Block { statements, .. } => statements
            .iter()
            .all(|statement| collect_assignment_values(statement, name, values)),
        Stmt::While { condition, body } => {
            collect_assigned_values(condition, name, values)
                && collect_assignment_values(body, name, values)
        }
        Stmt::Break { .. } | Stmt::Continue { .. } => true,
        Stmt::Return { value } => value
            .as_ref()
            .is_none_or(|value| collect_assigned_values(value, name, values)),
        _ => false,
    }
}

fn collect_assigned_values<'a>(
    expression: &'a Expr,
    name: &str,
    values: &mut Vec<&'a Expr>,
) -> bool {
    walk_supported_expression(expression, &mut |node| {
        if let Expr::Assign {
            name: target,
            value,
        } = node
            && target.lexeme() == name
        {
            values.push(value);
        }
    })
}

/// Collect the arrays a guard-bounded loop accesses, with the largest amount
/// any index may exceed the guard's left-hand side.
fn scan_bounded_accesses<'ctx>(
    fn_ctx: &FunctionContext<'ctx, '_>,
    statement: &Stmt,
    iv: &str,
    forms: &HashMap<String, AffineForm>,
    guard: &AffineForm,
    accesses: &mut HashMap<String, BoundedAccess<'ctx>>,
) -> bool {
    match statement {
        Stmt::Expression { expression } => {
            collect_bounded_accesses(fn_ctx, expression, iv, forms, guard, accesses)
        }
        Stmt::Var {
            initializer: Some(initializer),
            ..
        } => collect_bounded_accesses(fn_ctx, initializer, iv, forms, guard, accesses),
        Stmt::Var { .. } => true,
        Stmt::If {
            condition,
            then_branch,
            elif_branches,
            else_branch,
        } => {
            collect_bounded_accesses(fn_ctx, condition, iv, forms, guard, accesses)
                && scan_bounded_accesses(fn_ctx, then_branch, iv, forms, guard, accesses)
                && elif_branches.iter().all(|branch| {
                    collect_bounded_accesses(fn_ctx, &branch.condition, iv, forms, guard, accesses)
                        && scan_bounded_accesses(fn_ctx, &branch.body, iv, forms, guard, accesses)
                })
                && else_branch.as_ref().is_none_or(|body| {
                    scan_bounded_accesses(fn_ctx, body, iv, forms, guard, accesses)
                })
        }
        Stmt::Block { statements, .. } => statements
            .iter()
            .all(|statement| scan_bounded_accesses(fn_ctx, statement, iv, forms, guard, accesses)),
        Stmt::Break { .. } | Stmt::Continue { .. } => true,
        Stmt::Return { value } => value.as_ref().is_none_or(|expression| {
            collect_bounded_accesses(fn_ctx, expression, iv, forms, guard, accesses)
        }),
        _ => false,
    }
}

fn collect_bounded_accesses<'ctx>(
    fn_ctx: &FunctionContext<'ctx, '_>,
    expression: &Expr,
    iv: &str,
    forms: &HashMap<String, AffineForm>,
    guard: &AffineForm,
    accesses: &mut HashMap<String, BoundedAccess<'ctx>>,
) -> bool {
    walk_supported_expression(expression, &mut |node| {
        let (object, index) = match node {
            Expr::IndexGet { object, index } => (object, index),
            Expr::IndexSet { object, index, .. } => (object, index),
            _ => return,
        };
        let Expr::Variable { name } = object.as_ref() else {
            return;
        };
        let Some(buffer) = fn_ctx.scalar_array_buffers.get(name.lexeme()) else {
            return;
        };
        let Some(form) = affine_form(index, iv, forms) else {
            return;
        };
        if form.scale > guard.scale {
            return;
        }
        let slack = form.offset.saturating_sub(guard.offset);
        let entry = accesses
            .entry(name.lexeme().to_string())
            .or_insert(BoundedAccess {
                name: name.lexeme().to_string(),
                slack,
                length: buffer.length,
            });
        entry.slack = entry.slack.max(slack);
    })
}

fn is_iv_increment(value: &Expr, iv: &str) -> bool {
    let Expr::Binary { left, op, right } = value else {
        return false;
    };
    if op.kind != TokenKind::Plus {
        return false;
    }
    (is_variable(left, iv) && is_int_literal_one(right))
        || (is_int_literal_one(left) && is_variable(right, iv))
}

fn is_int_literal_one(expression: &Expr) -> bool {
    matches!(
        expression,
        Expr::Literal { value: LiteralValue::Number(text), .. }
            if ntsc_ast::expr::parse_int_literal(text) == Some(1)
    )
}

fn is_invariant_int_expression(
    fn_ctx: &FunctionContext<'_, '_>,
    expression: &Expr,
    forbidden: &HashSet<String>,
) -> bool {
    match expression {
        Expr::Literal {
            value: LiteralValue::Number(text),
            ..
        } => ntsc_ast::expr::parse_int_literal(text).is_some(),
        Expr::Variable { name } => {
            let name = name.lexeme();
            !forbidden.contains(name) && matches!(fn_ctx.lookup_var(name), Some((_, Ty::Int)))
        }
        Expr::Grouping { expression, .. } => {
            is_invariant_int_expression(fn_ctx, expression, forbidden)
        }
        Expr::Unary { op, right } if op.kind == TokenKind::Minus => {
            is_invariant_int_expression(fn_ctx, right, forbidden)
        }
        Expr::Binary { left, op, right }
            if matches!(
                op.kind,
                TokenKind::Plus | TokenKind::Minus | TokenKind::Star
            ) =>
        {
            is_invariant_int_expression(fn_ctx, left, forbidden)
                && is_invariant_int_expression(fn_ctx, right, forbidden)
        }
        _ => false,
    }
}

/// Collect assignments to named variables. Returns `false` for expression
/// shapes the versioning analysis does not understand.
fn collect_assigned_names(expression: &Expr, assigned: &mut HashMap<String, usize>) -> bool {
    let mut ok = true;
    walk_supported_expression(expression, &mut |node| match node {
        Expr::Assign { name, .. } => {
            *assigned.entry(name.lexeme().to_string()).or_default() += 1;
        }
        Expr::PostfixUnary { op, left } | Expr::Unary { op, right: left }
            if matches!(op.kind, TokenKind::PlusPlus | TokenKind::MinusMinus) =>
        {
            if let Expr::Variable { name } = left.as_ref() {
                *assigned.entry(name.lexeme().to_string()).or_default() += 1;
            } else {
                ok = false;
            }
        }
        _ => {}
    }) && ok
}

fn collect_borrowed_names(expression: &Expr, borrowed: &mut HashSet<String>) -> bool {
    walk_supported_expression(expression, &mut |node| {
        if let Expr::Borrow { target, .. } | Expr::View { target, .. } = node
            && let Expr::Variable { name } = target.as_ref()
        {
            borrowed.insert(name.lexeme().to_string());
        }
    })
}

fn collect_versioned_accesses<'a, 'ctx>(
    fn_ctx: &FunctionContext<'ctx, '_>,
    expression: &'a Expr,
    iv: &str,
    locals: &HashMap<String, Vec<IndexAddend<'a>>>,
    forbidden: &HashSet<String>,
    accesses: &mut Vec<VersionedAccess<'a, 'ctx>>,
) -> bool {
    walk_supported_expression(expression, &mut |node| {
        let (object, index) = match node {
            Expr::IndexGet { object, index } => (object, index),
            Expr::IndexSet { object, index, .. } => (object, index),
            _ => return,
        };
        let Expr::Variable { name } = object.as_ref() else {
            return;
        };
        let Some(buffer) = fn_ctx.scalar_array_buffers.get(name.lexeme()) else {
            return;
        };
        let Some(addends) = resolve_index_addends(fn_ctx, index, iv, locals, forbidden) else {
            return;
        };
        if addends
            .iter()
            .filter(|addend| matches!(addend, IndexAddend::Induction))
            .count()
            == 1
        {
            accesses.push(VersionedAccess {
                array: name.lexeme(),
                index,
                addends,
                length: buffer.length,
            });
        }
    })
}

/// Visit `expression` and its children, rejecting shapes whose side effects or
/// scoping the loop-versioning analysis cannot reason about.
fn walk_supported_expression<'a>(expression: &'a Expr, visit: &mut dyn FnMut(&'a Expr)) -> bool {
    visit(expression);
    match expression {
        Expr::Literal { .. } | Expr::Variable { .. } | Expr::This { .. } | Expr::Super { .. } => {
            true
        }
        Expr::Binary { left, right, .. } => {
            walk_supported_expression(left, visit) && walk_supported_expression(right, visit)
        }
        Expr::Unary { right, .. } => walk_supported_expression(right, visit),
        Expr::PostfixUnary { left, .. } => walk_supported_expression(left, visit),
        Expr::Grouping { expression, .. } => walk_supported_expression(expression, visit),
        Expr::Member { object, .. } | Expr::OptionalMember { object, .. } => {
            walk_supported_expression(object, visit)
        }
        Expr::Call {
            callee, arguments, ..
        } => {
            walk_supported_expression(callee, visit)
                && arguments
                    .iter()
                    .all(|argument| walk_supported_expression(argument, visit))
        }
        Expr::Assign { value, .. } => walk_supported_expression(value, visit),
        Expr::IndexGet { object, index } => {
            walk_supported_expression(object, visit) && walk_supported_expression(index, visit)
        }
        Expr::IndexSet {
            object,
            index,
            value,
        } => {
            walk_supported_expression(object, visit)
                && walk_supported_expression(index, visit)
                && walk_supported_expression(value, visit)
        }
        Expr::Ternary {
            condition,
            then_branch,
            else_branch,
        } => {
            walk_supported_expression(condition, visit)
                && walk_supported_expression(then_branch, visit)
                && walk_supported_expression(else_branch, visit)
        }
        Expr::Propagate { value, .. } => walk_supported_expression(value, visit),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use inkwell::values::AnyValue;

    fn candidates(source: &str) -> HashSet<String> {
        nonescaping_scalar_arrays_with_lengths(source)
            .into_keys()
            .collect()
    }

    fn nonescaping_scalar_arrays_with_lengths(source: &str) -> HashMap<String, Option<u64>> {
        let program = ntsc_parser::parse(&ntsc_lexer::tokenize(source)).unwrap();
        let Stmt::Function { body, params, .. } = &program.statements[0] else {
            panic!("expected a function");
        };
        nonescaping_scalar_arrays(body, params)
    }

    fn emit_ir(source: &str) -> String {
        let program = ntsc_parser::parse(&ntsc_lexer::tokenize(source)).unwrap();
        let Stmt::Function {
            name,
            params,
            return_type,
            body,
            ..
        } = &program.statements[0]
        else {
            panic!("expected a function");
        };
        let context = Context::create();
        let module = context.create_module("versioned_loop_ir");
        declare_runtime_functions(&module, crate::host_triple());
        let function = emit_function(&context, &module, name, params, return_type, body).unwrap();
        module.verify().unwrap();
        function.print_to_string().to_string()
    }

    /// Count emitted fast-body blocks (a label line, not a branch target).
    fn versioned_fast_blocks(ir: &str) -> usize {
        ir.lines()
            .filter(|line| line.starts_with("while.versioned.fast"))
            .count()
    }

    /// The text of the first unchecked body, up to the checked fallback.
    fn fast_body(ir: &str) -> &str {
        let start = ir
            .find("while.versioned.fast")
            .expect("versioned fast block");
        let end = ir[start..]
            .find("while.versioned.slow")
            .map_or(ir.len(), |offset| start + offset);
        &ir[start..end]
    }

    #[test]
    fn literal_initialisers_record_their_length() {
        let found = nonescaping_scalar_arrays_with_lengths(
            r#"fun compute() -> int {
                var array[int] literal = [1, 2, 3];
                var array[int] filled = arrays.fill(0, 256);
                var array[int] sized = arrays.fill(0, (2 * 3));
                return literal[0] + filled[0] + sized[0];
            }"#,
        );
        assert_eq!(found.get("literal"), Some(&Some(3)));
        assert_eq!(found.get("filled"), Some(&Some(256)));
        assert_eq!(found.get("sized"), Some(&None));
    }

    #[test]
    fn scalar_element_transfers_do_not_escape_their_arrays() {
        let found = candidates(
            r#"fun compute(int n) -> int {
                var array[int] a = arrays.fill(0, n);
                var array[int] b = arrays.fill(0, n);
                var int i = 0;
                while (i < n) {
                    a[i] = b[i] + 1;
                    b[i] = a[i];
                    i = i + 1;
                }
                return a[0] + b[0];
            }"#,
        );
        assert_eq!(found, HashSet::from(["a".into(), "b".into()]));
    }

    #[test]
    fn resizing_aliasing_and_borrowing_retain_the_runtime_path() {
        for operation in [
            "arrays.push(a, 9);",
            "a = arrays.fill(1, n);",
            "var array[int] alias = a;",
            "consume(a);",
            "var window = slices.of(a, 0, n);",
            "view var value = a[0];",
            "go consume(a)",
            "defer say(\"done\");",
        ] {
            let source = format!(
                "fun compute(int n) -> int {{
                    var array[int] a = arrays.fill(0, n);
                    {operation}
                    return a[0];
                }}"
            );
            assert!(candidates(&source).is_empty(), "{operation}");
        }
    }

    #[test]
    fn shadowed_bindings_cannot_reuse_a_buffer() {
        let found = candidates(
            r#"fun compute(int n) -> int {
                var array[int] a = arrays.fill(0, n);
                if (n > 1) {
                    var array[int] a = arrays.fill(9, n);
                    a[0] = 2;
                }
                return a[0];
            }"#,
        );
        assert!(found.is_empty());
    }

    #[test]
    fn emitted_scalar_indexing_has_inline_checks_and_direct_memory_access() {
        let program = ntsc_parser::parse(&ntsc_lexer::tokenize(
            r#"fun compute(int n) -> int {
                var array[int] a = arrays.fill(0, n);
                a[0] = 7;
                return a[0];
            }"#,
        ))
        .unwrap();
        let Stmt::Function {
            name,
            params,
            return_type,
            body,
            ..
        } = &program.statements[0]
        else {
            panic!("expected a function");
        };
        let context = Context::create();
        let module = context.create_module("scalar_array_ir");
        declare_runtime_functions(&module, crate::host_triple());
        let function = emit_function(&context, &module, name, params, return_type, body).unwrap();
        module.verify().unwrap();
        let ir = function.print_to_string().to_string();
        assert!(ir.contains("scalar_array_out_of_bounds"));
        assert!(ir.contains("scalar_array_element"));
        assert!(ir.contains("scalar_array_load"));
        assert!(!ir.contains("call i64 @ntsc_array_get"));
        assert!(!ir.contains("call i64 @ntsc_array_set"));
    }

    #[test]
    fn affine_loop_gets_an_unchecked_fast_body_and_a_checked_fallback() {
        let program = ntsc_parser::parse(&ntsc_lexer::tokenize(
            r#"fun compute(int n) -> int {
                var array[int] a = arrays.fill(0, n);
                var int i = 0;
                while (i < n) {
                    a[i] = i;
                    i = i + 1;
                }
                return a[0];
            }"#,
        ))
        .unwrap();
        let Stmt::Function {
            name,
            params,
            return_type,
            body,
            ..
        } = &program.statements[0]
        else {
            panic!("expected a function");
        };
        let context = Context::create();
        let module = context.create_module("versioned_loop_ir");
        declare_runtime_functions(&module, crate::host_triple());
        let function = emit_function(&context, &module, name, params, return_type, body).unwrap();
        module.verify().unwrap();
        let ir = function.print_to_string().to_string();
        assert!(ir.contains("while.versioned.fast"));
        assert!(ir.contains("while.versioned.slow"));
        assert!(ir.contains("versioned_safe"));
        assert!(ir.contains("int_add"));
        // One guard in the checked fallback plus one for the post-loop read;
        // the fast body has none. Each guard mentions its compare twice
        // (definition and branch condition).
        assert_eq!(ir.matches("scalar_array_out_of_bounds").count(), 4);
    }

    #[test]
    fn affine_locals_inside_the_body_extend_the_range_proof() {
        let ir = emit_ir(
            r#"fun compute(int n) -> int {
                var array[int] a = arrays.fill(0, n);
                var int i = 0;
                while (i < n) {
                    var int offset = i + 1;
                    var int target = offset + 1;
                    a[target] = i;
                    i = i + 1;
                }
                return a[0];
            }"#,
        );
        assert!(ir.contains("while.versioned.fast"));
        // The fast body has no guard; the checked fallback and the
        // post-loop read keep one each.
        assert_eq!(ir.matches("scalar_array_out_of_bounds").count(), 4);
    }

    /// A sift-down heap loop guards `position * 2 + 1 < size`, so the guard
    /// itself bounds every index. The preheader proves the bound fits the
    /// array and the fast body runs without a single bounds check.
    #[test]
    fn guard_bounded_sift_loop_runs_without_bounds_checks() {
        let ir = emit_ir(
            r#"fun compute(int n) -> int {
                var array[int] heap = arrays.fill(0, n);
                var int size = n;
                var int position = 0;
                var int last = 1000;
                while (position * 2 + 1 < size) {
                    var int child = position * 2 + 1;
                    if (child + 1 < size && heap[child + 1] < heap[child]) {
                        child = child + 1;
                    }
                    if (last <= heap[child]) { break; }
                    heap[position] = heap[child];
                    position = child;
                }
                heap[position] = last;
                return heap[0];
            }"#,
        );
        assert!(ir.contains("versioned_start_ok"), "{ir}");
        let fast = fast_body(&ir);
        assert!(
            !fast.contains("scalar_array_out_of_bounds"),
            "fast body kept a bounds check:\n{fast}"
        );
    }

    #[test]
    fn versioned_loops_keep_checks_for_unproven_indices() {
        let ir = emit_ir(
            r#"fun compute(int n) -> int {
                var array[int] a = arrays.fill(0, n);
                var array[int] b = arrays.fill(0, n);
                var int i = 0;
                while (i < n) {
                    var int slot = a[i] % 2;
                    b[slot] = i;
                    i = i + 1;
                }
                return a[0];
            }"#,
        );
        assert!(ir.contains("while.versioned.fast"));
        // `a[i]` is proven and loses its guard in the fast body; `b[slot]`
        // is data-dependent and keeps it. Fast 1 + slow 2 + post-loop 1.
        assert_eq!(ir.matches("scalar_array_out_of_bounds").count(), 8);
    }

    #[test]
    fn loops_nested_in_conditionals_are_versioned_with_the_outer_loop() {
        let ir = emit_ir(
            r#"fun compute(int n) -> int {
                var array[int] a = arrays.fill(0, n);
                var int i = 0;
                while (i < n) {
                    if (a[i] > 0) {
                        var int j = 0;
                        while (j < n) {
                            a[j] = j;
                            j = j + 1;
                        }
                    }
                    i = i + 1;
                }
                return a[0];
            }"#,
        );
        // The outer loop's two versions each contain a versioned inner loop.
        assert_eq!(versioned_fast_blocks(&ir), 2);
    }

    #[test]
    fn straight_line_nested_loops_are_not_duplicated_into_the_fast_body() {
        let ir = emit_ir(
            r#"fun compute(int n) -> int {
                var array[int] a = arrays.fill(0, n);
                var int i = 0;
                while (i < n) {
                    var int j = 0;
                    while (j < n) {
                        a[j] = j;
                        j = j + 1;
                    }
                    i = i + 1;
                }
                return a[0];
            }"#,
        );
        // Only the inner loop is versioned; duplicating it into an outer
        // fast body would bloat the hot path without removing any check.
        assert_eq!(versioned_fast_blocks(&ir), 1);
    }

    #[test]
    fn loops_that_do_not_prove_their_range_stay_checked() {
        for source in [
            // `push` could resize the array.
            r#"fun compute(int n) -> int {
                var array[int] a = arrays.fill(0, n);
                var int i = 0;
                while (i < n) {
                    arrays.push(a, i);
                    i = i + 1;
                }
                return a[0];
            }"#,
            // The index is not affine in the induction variable.
            r#"fun compute(int n) -> int {
                var array[int] a = arrays.fill(0, n);
                var int i = 0;
                while (i < n) {
                    a[i * i] = i;
                    i = i + 1;
                }
                return a[0];
            }"#,
            // The limit is reassigned in the body.
            r#"fun compute(int n) -> int {
                var array[int] a = arrays.fill(0, n);
                var int i = 0;
                while (i < n) {
                    a[i] = i;
                    i = i + 1;
                    n = n - 1;
                }
                return a[0];
            }"#,
        ] {
            let program = ntsc_parser::parse(&ntsc_lexer::tokenize(source)).unwrap();
            let Stmt::Function {
                name,
                params,
                return_type,
                body,
                ..
            } = &program.statements[0]
            else {
                panic!("expected a function");
            };
            let context = Context::create();
            let module = context.create_module("checked_loop_ir");
            declare_runtime_functions(&module, crate::host_triple());
            let function =
                emit_function(&context, &module, name, params, return_type, body).unwrap();
            module.verify().unwrap();
            let ir = function.print_to_string().to_string();
            assert!(!ir.contains("while.versioned.fast"), "{source}");
        }
    }
}
