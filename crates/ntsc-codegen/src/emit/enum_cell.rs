//! Enum cells: the heap layout behind a payload-carrying enum.
//!
//! A plain enum is an `int` tag and never allocates. An enum with at least
//! one payload variant travels as a pointer to a cell of `1 + slots` i64
//! words: `[0]` is the variant tag, and the remaining words are the payload
//! slots, one per declared field. `slots` is the widest variant's arity, so
//! every variant shares one layout; a shorter variant simply leaves the tail
//! uninitialized, which is never read because the tag gates every access.
//!
//! Payload words hold bits, not typed values: ints and handles are already
//! i64, floats are bitcast, and class pointers are converted to and from an
//! integer. Storing bits keeps one layout for every payload type.

use super::*;
use inkwell::types::{BasicTypeEnum, StructType};
use inkwell::values::{BasicValueEnum, IntValue};

/// The variants of a payload-carrying enum, in declaration order.
pub(crate) fn enum_payloads(enum_name: &str) -> Option<EnumPayloads> {
    ENUM_PAYLOADS.with(|map| map.borrow().get(enum_name).cloned())
}

/// Whether `enum_name` is an enum that carries payloads (and therefore
/// travels as a cell rather than an int).
pub(crate) fn is_payload_enum(enum_name: &str) -> bool {
    ENUM_PAYLOADS.with(|map| map.borrow().contains_key(enum_name))
}

/// The payload types declared for `variant` of `enum_name`.
pub(crate) fn variant_payloads(enum_name: &str, variant: &str) -> Option<Vec<Ty>> {
    enum_payloads(enum_name)?
        .into_iter()
        .find(|(name, _)| name == variant)
        .map(|(_, payload)| payload)
}

/// Whether `name` is a variant of some payload-carrying enum.
pub(crate) fn is_variant_name(name: &str) -> bool {
    ENUM_VARIANT_OWNERS.with(|map| map.borrow().contains_key(name))
}

/// The declaration index of `variant`, which doubles as its tag.
pub(crate) fn variant_tag(enum_name: &str, variant: &str) -> Option<i64> {
    enum_payloads(enum_name)?
        .iter()
        .position(|(name, _)| name == variant)
        .map(|index| index as i64)
}

/// The payload width shared by every variant of the enum (at least one word
/// so the cell always has a slot to point at).
fn slot_count(enum_name: &str) -> u32 {
    enum_payloads(enum_name)
        .map(|variants| {
            variants
                .iter()
                .map(|(_, payload)| payload.len() as u32)
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0)
        .max(1)
}

fn cell_view_type<'ctx>(context: &'ctx inkwell::context::Context, slots: u32) -> StructType<'ctx> {
    let i64_ty = context.i64_type();
    let fields: Vec<BasicTypeEnum<'ctx>> = (0..=slots).map(|_| i64_ty.into()).collect();
    context.struct_type(&fields, false)
}

fn cell_size(enum_name: &str) -> i64 {
    8 * (i64::from(slot_count(enum_name)) + 1)
}

fn field_ptr<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    enum_name: &str,
    cell: PointerValue<'ctx>,
    slot: u32,
) -> Result<PointerValue<'ctx>, crate::CodegenError> {
    let view_ty = cell_view_type(fn_ctx.context, slot_count(enum_name));
    Ok(fn_ctx
        .builder
        .build_struct_gep(view_ty, cell, slot, "enum_field_ptr")?)
}

fn allocate_cell<'ctx>(
    fn_ctx: &FunctionContext<'ctx, '_>,
    enum_name: &str,
) -> Result<PointerValue<'ctx>, crate::CodegenError> {
    let alloc_fn = fn_ctx
        .module
        .get_function("malloc")
        .ok_or_else(|| crate::CodegenError::LLVMError("malloc not declared".into()))?;
    Ok(fn_ctx
        .builder
        .build_call(
            alloc_fn,
            &[BasicMetadataValueEnum::IntValue(
                fn_ctx
                    .context
                    .i64_type()
                    .const_int(cell_size(enum_name) as u64, false),
            )],
            "enum_box",
        )?
        .try_as_basic_value()
        .unwrap_basic()
        .into_pointer_value())
}

/// Reduce a typed value to the bits stored in a payload word.
fn to_slot_bits<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    value: BasicValueEnum<'ctx>,
) -> Result<IntValue<'ctx>, crate::CodegenError> {
    let i64_ty = fn_ctx.context.i64_type();
    Ok(match value {
        BasicValueEnum::IntValue(int) => {
            let width = int.get_type().get_bit_width();
            if width == 64 {
                int
            } else if width == 1 {
                fn_ctx
                    .builder
                    .build_int_z_extend(int, i64_ty, "enum_bool_bits")?
            } else {
                fn_ctx
                    .builder
                    .build_int_s_extend_or_bit_cast(int, i64_ty, "enum_int_bits")?
            }
        }
        BasicValueEnum::FloatValue(float) => fn_ctx
            .builder
            .build_bit_cast(float, i64_ty, "enum_float_bits")?
            .into_int_value(),
        BasicValueEnum::PointerValue(ptr) => {
            fn_ctx
                .builder
                .build_ptr_to_int(ptr, i64_ty, "enum_ptr_bits")?
        }
        other => {
            return Err(crate::CodegenError::LLVMError(format!(
                "cannot store `{}` in an enum payload slot",
                other.get_type()
            )));
        }
    })
}

/// Recover a typed value from the bits stored in a payload word.
fn from_slot_bits<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    bits: IntValue<'ctx>,
    ty: &Ty,
) -> Result<BasicValueEnum<'ctx>, crate::CodegenError> {
    let llvm_ty = ty_to_llvm(ty, fn_ctx.context);
    Ok(match llvm_ty {
        BasicTypeEnum::IntType(int) => {
            let width = int.get_bit_width();
            if width == 64 {
                bits.into()
            } else if width == 1 {
                fn_ctx
                    .builder
                    .build_int_truncate(bits, int, "enum_bool")?
                    .into()
            } else {
                fn_ctx
                    .builder
                    .build_int_truncate_or_bit_cast(bits, int, "enum_int")?
                    .into()
            }
        }
        BasicTypeEnum::FloatType(float) => {
            fn_ctx.builder.build_bit_cast(bits, float, "enum_float")?
        }
        BasicTypeEnum::PointerType(ptr) => fn_ctx
            .builder
            .build_int_to_ptr(bits, ptr, "enum_ptr")?
            .into(),
        other => {
            return Err(crate::CodegenError::LLVMError(format!(
                "cannot load `{other}` from an enum payload slot"
            )));
        }
    })
}

fn store_slot<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    enum_name: &str,
    cell: PointerValue<'ctx>,
    index: usize,
    value: BasicValueEnum<'ctx>,
) -> Result<(), crate::CodegenError> {
    let ptr = field_ptr(fn_ctx, enum_name, cell, index as u32 + 1)?;
    let bits = to_slot_bits(fn_ctx, value)?;
    fn_ctx.builder.build_store(ptr, bits)?;
    Ok(())
}

fn load_slot<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    enum_name: &str,
    cell: PointerValue<'ctx>,
    index: usize,
) -> Result<IntValue<'ctx>, crate::CodegenError> {
    let ptr = field_ptr(fn_ctx, enum_name, cell, index as u32 + 1)?;
    Ok(fn_ctx
        .builder
        .build_load(fn_ctx.context.i64_type(), ptr, "enum_slot")?
        .into_int_value())
}

/// Load payload word `index` with its declared representation.
pub(crate) fn load_payload<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    enum_name: &str,
    cell: PointerValue<'ctx>,
    index: usize,
    ty: &Ty,
) -> Result<BasicValueEnum<'ctx>, crate::CodegenError> {
    let bits = load_slot(fn_ctx, enum_name, cell, index)?;
    from_slot_bits(fn_ctx, bits, ty)
}

/// The tag stored in an enum cell.
pub(crate) fn enum_tag<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    enum_name: &str,
    cell: PointerValue<'ctx>,
) -> Result<IntValue<'ctx>, crate::CodegenError> {
    let ptr = field_ptr(fn_ctx, enum_name, cell, 0)?;
    Ok(fn_ctx
        .builder
        .build_load(fn_ctx.context.i64_type(), ptr, "enum_tag")?
        .into_int_value())
}

pub(crate) fn cell_pointer<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    val: BasicValueEnum<'ctx>,
) -> Result<PointerValue<'ctx>, crate::CodegenError> {
    if val.is_pointer_value() {
        return Ok(val.into_pointer_value());
    }
    Ok(fn_ctx.builder.build_int_to_ptr(
        val.into_int_value(),
        fn_ctx.context.ptr_type(AddressSpace::default()),
        "enum_cell",
    )?)
}

/// Build a fresh cell for `variant` holding the already-prepared arguments.
pub(crate) fn box_enum_value<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    enum_name: &str,
    variant: &str,
    arguments: &[Expr],
    values: &[TypedValue<'ctx>],
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    let payload = variant_payloads(enum_name, variant).ok_or_else(|| {
        crate::CodegenError::LLVMError(format!("enum `{enum_name}` has no variant `{variant}`"))
    })?;
    let tag = variant_tag(enum_name, variant).ok_or_else(|| {
        crate::CodegenError::LLVMError(format!("enum `{enum_name}` has no variant `{variant}`"))
    })?;
    if payload.len() != values.len() {
        return Err(crate::CodegenError::LLVMError(format!(
            "enum variant `{enum_name}.{variant}` expects {} argument(s), got {}",
            payload.len(),
            values.len()
        )));
    }

    let cell = allocate_cell(fn_ctx, enum_name)?;
    let tag_ptr = field_ptr(fn_ctx, enum_name, cell, 0)?;
    fn_ctx.builder.build_store(
        tag_ptr,
        fn_ctx.context.i64_type().const_int(tag as u64, false),
    )?;

    for (index, (value, slot_ty)) in values.iter().zip(payload.iter()).enumerate() {
        let coerced = coerce_value(fn_ctx, value.clone(), slot_ty)?;
        let stored = if matches!(slot_ty, Ty::String)
            && arguments.get(index).is_some_and(expr_is_string_literal)
        {
            clone_string_value(fn_ctx, &coerced)?
        } else {
            coerced.value
        };
        store_slot(fn_ctx, enum_name, cell, index, stored)?;
    }

    Ok(TypedValue::new(
        cell.into(),
        Ty::Class(enum_name.to_string()),
    ))
}

/// Branch into one block per variant the caller cares about, running its
/// per-variant work there. Shared by drop and copy, which differ only in
/// what they do with each payload word.
fn per_variant_blocks<'ctx, W, F>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    enum_name: &str,
    cell: PointerValue<'ctx>,
    done_bb: inkwell::basic_block::BasicBlock<'ctx>,
    mut wants: W,
    mut handle: F,
) -> Result<(), crate::CodegenError>
where
    W: FnMut(&[Ty]) -> bool,
    F: FnMut(
        &mut FunctionContext<'ctx, '_>,
        &str,
        PointerValue<'ctx>,
        &[Ty],
    ) -> Result<(), crate::CodegenError>,
{
    let current_fn = fn_ctx.function;
    let variants = enum_payloads(enum_name).unwrap_or_default();
    let tag = enum_tag(fn_ctx, enum_name, cell)?;

    for (index, (_, payload)) in variants.iter().enumerate() {
        if !wants(payload) {
            continue;
        }
        let arm_bb = fn_ctx
            .context
            .append_basic_block(current_fn, &format!("enum_variant{index}"));
        let next_bb = fn_ctx
            .context
            .append_basic_block(current_fn, &format!("enum_variant_next{index}"));
        let matched = fn_ctx.builder.build_int_compare(
            inkwell::IntPredicate::EQ,
            tag,
            fn_ctx.context.i64_type().const_int(index as u64, false),
            "enum_tag_match",
        )?;
        fn_ctx
            .builder
            .build_conditional_branch(matched, arm_bb, next_bb)?;

        fn_ctx.builder.position_at_end(arm_bb);
        handle(fn_ctx, enum_name, cell, payload)?;
        fn_ctx.builder.build_unconditional_branch(done_bb)?;
        fn_ctx.builder.position_at_end(next_bb);
    }

    fn_ctx.builder.build_unconditional_branch(done_bb)?;
    Ok(())
}

/// Drop an enum value: reclaim every heap payload of the active variant,
/// then free the cell. A null handle is a no-op, like an unset option.
pub(crate) fn emit_drop_enum_value<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    enum_name: &str,
    val: &TypedValue<'ctx>,
) -> Result<(), crate::CodegenError> {
    let cell = cell_pointer(fn_ctx, val.value)?;
    let current_fn = fn_ctx.function;
    let check_bb = fn_ctx
        .context
        .append_basic_block(current_fn, "enum_drop.check");
    let done_bb = fn_ctx
        .context
        .append_basic_block(current_fn, "enum_drop.done");
    let is_null = fn_ctx.builder.build_is_null(cell, "enum_drop_null")?;
    fn_ctx
        .builder
        .build_conditional_branch(is_null, done_bb, check_bb)?;
    fn_ctx.builder.position_at_end(check_bb);

    per_variant_blocks(
        fn_ctx,
        enum_name,
        cell,
        done_bb,
        |payload| payload.iter().any(payload_is_heap),
        |fn_ctx, name, cell, payload| {
            for (slot, ty) in payload.iter().enumerate() {
                if payload_is_heap(ty) {
                    let loaded = load_payload(fn_ctx, name, cell, slot, ty)?;
                    emit_drop_value(fn_ctx, &TypedValue::new(loaded, ty.clone()))?;
                }
            }
            Ok(())
        },
    )?;

    fn_ctx.builder.position_at_end(done_bb);
    let is_null = fn_ctx.builder.build_is_null(cell, "enum_free_null")?;
    let free_bb = fn_ctx
        .context
        .append_basic_block(current_fn, "enum_drop.free");
    let exit_bb = fn_ctx
        .context
        .append_basic_block(current_fn, "enum_drop.exit");
    fn_ctx
        .builder
        .build_conditional_branch(is_null, exit_bb, free_bb)?;
    fn_ctx.builder.position_at_end(free_bb);
    let free_fn = fn_ctx
        .module
        .get_function("free")
        .ok_or_else(|| crate::CodegenError::LLVMError("free not declared".into()))?;
    fn_ctx.builder.build_call(
        free_fn,
        &[BasicMetadataValueEnum::PointerValue(cell)],
        "enum_free",
    )?;
    fn_ctx.builder.build_unconditional_branch(exit_bb)?;
    fn_ctx.builder.position_at_end(exit_bb);
    Ok(())
}

/// Deep-copy an enum value into an independent cell, copying only the
/// active variant's payload words.
pub(crate) fn emit_copy_enum_value<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    enum_name: &str,
    val: &TypedValue<'ctx>,
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    let source = cell_pointer(fn_ctx, val.value)?;
    let current_fn = fn_ctx.function;
    let null_bb = fn_ctx
        .builder
        .get_insert_block()
        .ok_or_else(|| crate::CodegenError::LLVMError("enum copy has no insert block".into()))?;
    let body_bb = fn_ctx
        .context
        .append_basic_block(current_fn, "enum_copy.body");
    let done_bb = fn_ctx
        .context
        .append_basic_block(current_fn, "enum_copy.done");
    let is_null = fn_ctx.builder.build_is_null(source, "enum_copy_null")?;
    fn_ctx
        .builder
        .build_conditional_branch(is_null, done_bb, body_bb)?;

    fn_ctx.builder.position_at_end(body_bb);
    let target = allocate_cell(fn_ctx, enum_name)?;
    let tag = enum_tag(fn_ctx, enum_name, source)?;
    let tag_ptr = field_ptr(fn_ctx, enum_name, target, 0)?;
    fn_ctx.builder.build_store(tag_ptr, tag)?;

    // Every variant arm and the tag chain converge on one join block so the
    // merge needs a single non-null predecessor whatever the payload copies
    // did to the block structure.
    let join_bb = fn_ctx
        .context
        .append_basic_block(current_fn, "enum_copy.join");
    per_variant_blocks(
        fn_ctx,
        enum_name,
        source,
        join_bb,
        |payload| !payload.is_empty(),
        |fn_ctx, name, cell, payload| {
            for (slot, ty) in payload.iter().enumerate() {
                let loaded = load_payload(fn_ctx, name, cell, slot, ty)?;
                let copied = if payload_is_heap(ty) {
                    emit_copy_value(fn_ctx, TypedValue::new(loaded, ty.clone()))?.value
                } else {
                    loaded
                };
                store_slot(fn_ctx, name, target, slot, copied)?;
            }
            Ok(())
        },
    )?;
    fn_ctx.builder.position_at_end(join_bb);
    fn_ctx.builder.build_unconditional_branch(done_bb)?;

    fn_ctx.builder.position_at_end(done_bb);
    let ptr_ty = fn_ctx.context.ptr_type(AddressSpace::default());
    let phi = fn_ctx.builder.build_phi(ptr_ty, "enum_copy")?;
    let null = ptr_ty.const_null();
    phi.add_incoming(&[(&target, join_bb), (&null, null_bb)]);
    Ok(TypedValue::new(
        phi.as_basic_value(),
        Ty::Class(enum_name.to_string()),
    ))
}
