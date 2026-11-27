//! Trait objects (`dyn Trait`): fat-pointer construction, vtables,
//! dynamic dispatch, and drops.
//!
//! A `dyn T` value owns a 16-byte header `{ object*, vtable* }`. The
//! header and the wrapped instance are both heap allocations owned by the
//! fat pointer, so dropping it runs the class drop thunk through vtable
//! slot 0 and then frees both allocations. Slot 0 is reserved for that
//! drop wrapper; slots 1.. hold the trait's methods in declaration order.
//! Slot contents are installed after every function body exists, because
//! slot values are function pointers defined later in the module.

use super::*;
use std::cell::RefCell;

thread_local! {
    static TRAIT_TABLES: RefCell<HashMap<String, ntsc_typeck::TraitObjectInfo>> =
        RefCell::new(HashMap::new());
    static PENDING_VTABLES: RefCell<Vec<(String, String)>> = const { RefCell::new(Vec::new()) };
}

/// Installs the trait method tables recorded by the type checker and
/// resets per-module state. Called once at the start of `emit_module`.
/// The compiler is single-threaded per compilation, so thread-local state
/// mirrors how class metadata is already handed to the emitter.
pub(crate) fn load_trait_tables(tables: HashMap<String, ntsc_typeck::TraitObjectInfo>) {
    PENDING_VTABLES.with(|pending| pending.borrow_mut().clear());
    TRAIT_TABLES.with(|slot| *slot.borrow_mut() = tables);
}

fn trait_methods(trait_name: &str) -> Vec<ntsc_typeck::TraitMethodInfo> {
    TRAIT_TABLES.with(|tables| {
        tables
            .borrow()
            .get(trait_name)
            .map(|info| info.methods.clone())
            .unwrap_or_default()
    })
}

/// The lazily created, initially zero-filled vtable global for one
/// (trait, class) pair. Its slots are filled in `finalize_trait_vtables`
/// once every referenced function exists.
fn get_or_create_vtable<'ctx>(
    module: &Module<'ctx>,
    context: &'ctx Context,
    trait_name: &str,
    class_name: &str,
) -> Result<inkwell::values::GlobalValue<'ctx>, crate::CodegenError> {
    let name = format!("__ntsc_vtable_{trait_name}_{class_name}");
    if let Some(existing) = module.get_global(&name) {
        return Ok(existing);
    }
    let vtable_ty = vtable_struct_ty(context, trait_methods(trait_name).len() + 1);
    let global = module.add_global(vtable_ty, Some(AddressSpace::default()), &name);
    global.set_initializer(&vtable_ty.const_zero());
    global.set_linkage(inkwell::module::Linkage::Internal);
    PENDING_VTABLES.with(|pending| {
        let mut pending = pending.borrow_mut();
        if !pending.contains(&(trait_name.to_string(), class_name.to_string())) {
            pending.push((trait_name.to_string(), class_name.to_string()));
        }
    });
    Ok(global)
}

/// One pointer slot per method plus the reserved drop wrapper in slot 0.
fn vtable_struct_ty<'ctx>(
    context: &'ctx Context,
    slots: usize,
) -> inkwell::types::StructType<'ctx> {
    let ptr_ty = context.ptr_type(AddressSpace::default());
    context.struct_type(&vec![ptr_ty.into(); slots], false)
}

/// The vtable slot 0 callee: reclaims the instance's owned fields with the
/// class thunk and frees the instance struct. Both callees are null-safe,
/// so an uninitialized object pointer drops harmlessly.
fn get_or_create_dyn_drop_wrapper<'ctx>(
    module: &Module<'ctx>,
    context: &'ctx Context,
    trait_name: &str,
    class_name: &str,
) -> Result<FunctionValue<'ctx>, crate::CodegenError> {
    let name = format!("__ntsc_dyn_drop_{trait_name}_{class_name}");
    if let Some(existing) = module.get_function(&name) {
        return Ok(existing);
    }
    let void_ty = context.void_type();
    let ptr_ty = context.ptr_type(AddressSpace::default());
    let function = module.add_function(
        &name,
        void_ty.fn_type(&[ptr_ty.into()], false),
        Some(inkwell::module::Linkage::Internal),
    );
    let entry = context.append_basic_block(function, "entry");
    let builder = context.create_builder();
    builder.position_at_end(entry);

    let obj = function
        .get_nth_param(0)
        .ok_or_else(|| crate::CodegenError::LLVMError("drop wrapper has no param".into()))?;
    let release = module
        .get_function("ntsc_class_release")
        .expect("runtime declaration");
    builder.build_call(release, &[obj.into()], "")?;
    builder.build_return(None)?;
    Ok(function)
}

fn header_struct_ty<'ctx>(context: &'ctx Context) -> inkwell::types::StructType<'ctx> {
    let ptr_ty = context.ptr_type(AddressSpace::default());
    context.struct_type(&[ptr_ty.into(), ptr_ty.into()], false)
}

/// Wraps a freshly constructed class instance in an owning fat pointer.
/// The instance's ownership moves into the header; callers must ensure the
/// source is a construction, or the same instance ends up owned twice.
pub(crate) fn emit_dyn_coercion<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    val: TypedValue<'ctx>,
    trait_name: &str,
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    // An owning wrapper around a class holds the instance pointer itself
    // (no cell), so `Class` and `own Class` values are already the
    // instance address.
    let obj = super::drop::class_instance_pointer(fn_ctx, val.value)?;
    let class_name = match &val.ntsc_type {
        Ty::Class(name) => name.clone(),
        Ty::Own(inner) => match inner.as_ref() {
            Ty::Class(name) => name.clone(),
            other => {
                return Err(crate::CodegenError::LLVMError(format!(
                    "cannot coerce `{other}` into `dyn {trait_name}`"
                )));
            }
        },
        other => {
            return Err(crate::CodegenError::LLVMError(format!(
                "cannot coerce `{other}` into `dyn {trait_name}`"
            )));
        }
    };

    let malloc = fn_ctx
        .module
        .get_function("malloc")
        .ok_or_else(|| crate::CodegenError::LLVMError("malloc not declared".into()))?;
    let size = fn_ctx
        .context
        .i64_type()
        .const_int(std::mem::size_of::<*const ()>() as u64 * 2, false);
    let header = fn_ctx
        .builder
        .build_call(malloc, &[size.into()], "dyn_header_alloc")?
        .try_as_basic_value()
        .unwrap_basic()
        .into_pointer_value();

    let header_ty = header_struct_ty(fn_ctx.context);
    let obj_slot = fn_ctx
        .builder
        .build_struct_gep(header_ty, header, 0, "dyn_obj_slot")?;
    let vt = get_or_create_vtable(fn_ctx.module, fn_ctx.context, trait_name, &class_name)?;
    let vt_slot = fn_ctx
        .builder
        .build_struct_gep(header_ty, header, 1, "dyn_vt_slot")?;
    fn_ctx.builder.build_store(obj_slot, obj)?;
    fn_ctx.builder.build_store(vt_slot, vt.as_pointer_value())?;
    Ok(TypedValue::new(
        header.into(),
        Ty::Dyn(trait_name.to_string()),
    ))
}

/// Loads the object and vtable pointers out of a fat pointer header.
fn load_header<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    header: PointerValue<'ctx>,
) -> Result<(PointerValue<'ctx>, PointerValue<'ctx>), crate::CodegenError> {
    let header_ty = header_struct_ty(fn_ctx.context);
    let obj_slot = fn_ctx
        .builder
        .build_struct_gep(header_ty, header, 0, "dyn_obj_slot")?;
    let vt_slot = fn_ctx
        .builder
        .build_struct_gep(header_ty, header, 1, "dyn_vt_slot")?;
    let ptr_ty = fn_ctx.context.ptr_type(AddressSpace::default());
    let obj = fn_ctx
        .builder
        .build_load(ptr_ty, obj_slot, "dyn_obj")?
        .into_pointer_value();
    let vt = fn_ctx
        .builder
        .build_load(ptr_ty, vt_slot, "dyn_vt")?
        .into_pointer_value();
    Ok((obj, vt))
}

/// Whether the expression constructs a fresh instance whose ownership can
/// move into a trait-object header without aliasing.
pub(crate) fn expr_is_fresh_construction<'ctx>(
    fn_ctx: &FunctionContext<'ctx, '_>,
    expr: &Expr,
) -> bool {
    match expr {
        Expr::Grouping { expression, .. } => expr_is_fresh_construction(fn_ctx, expression),
        Expr::Call { callee, .. } => match callee.as_ref() {
            Expr::Variable { name } => {
                name.lexeme() == "alloc" || fn_ctx.module.get_struct_type(name.lexeme()).is_some()
            }
            _ => false,
        },
        Expr::StructLiteral { class_name, .. } => {
            fn_ctx.module.get_struct_type(class_name.lexeme()).is_some()
        }
        _ => false,
    }
}

/// Dynamic dispatch: `receiver.method(args)` where the receiver is a
/// `dyn Trait`. The callee is loaded from the receiver's own vtable, so
/// one call site serves every implementing class.
pub(crate) fn emit_dyn_method_call<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    receiver: TypedValue<'ctx>,
    property: &ntsc_ast::token::Token,
    arguments: &[Expr],
    arg_values: &[TypedValue<'ctx>],
) -> Result<TypedValue<'ctx>, crate::CodegenError> {
    let Ty::Dyn(trait_name) = receiver.ntsc_type else {
        return Err(crate::CodegenError::LLVMError(
            "dynamic dispatch requires a `dyn` receiver".into(),
        ));
    };
    let methods = trait_methods(&trait_name);
    let slot_index = methods
        .iter()
        .position(|method| method.name == property.lexeme())
        .map(|index| index + 1)
        .ok_or_else(|| {
            crate::CodegenError::LLVMError(format!(
                "trait `{trait_name}` has no method `{}`",
                property.lexeme()
            ))
        })?;
    let info = &methods[slot_index - 1];

    let BasicValueEnum::PointerValue(header) = receiver.value else {
        return Err(crate::CodegenError::LLVMError(
            "`dyn` value must be a fat pointer".into(),
        ));
    };
    let (obj, vt) = load_header(fn_ctx, header)?;

    // The slot address depends only on the trait's method count, which is
    // known statically.
    let vtable_ty = vtable_struct_ty(fn_ctx.context, methods.len() + 1);
    let slot = fn_ctx
        .builder
        .build_struct_gep(vtable_ty, vt, slot_index as u32, "dyn_vt_slot")?;
    let ptr_ty = fn_ctx.context.ptr_type(AddressSpace::default());
    let callee = fn_ctx
        .builder
        .build_load(ptr_ty, slot, "dyn_callee")?
        .into_pointer_value();

    let prepared = super::drop::prepare_call_args(fn_ctx, arguments, arg_values, &info.param_tys)?;

    // The signature mirrors a class method call: the instance travels as a
    // leading untyped pointer, followed by the declared parameters.
    let mut param_tys: Vec<inkwell::types::BasicMetadataTypeEnum<'ctx>> = vec![ptr_ty.into()];
    for param_ty in &info.param_tys {
        param_tys.push(ty_to_llvm(param_ty, fn_ctx.context).into());
    }
    let llvm_fn_ty = match &info.return_ty {
        Some(return_ty) => ty_to_llvm(return_ty, fn_ctx.context).fn_type(&param_tys, false),
        None => fn_ctx.context.void_type().fn_type(&param_tys, false),
    };

    let mut llvm_args: Vec<BasicMetadataValueEnum<'ctx>> = vec![obj.into()];
    for arg_val in &prepared {
        llvm_args.push(arg_val.value.into());
    }
    let result =
        fn_ctx
            .builder
            .build_indirect_call(llvm_fn_ty, callee, &llvm_args, "dyn_dispatch")?;
    let ret_val = call_result_to_value(fn_ctx, &result);
    let ret_ty = info.return_ty.clone().unwrap_or(Ty::Void);
    fn_ctx.emit_pending_exception_check()?;
    Ok(TypedValue::new(ret_val, ret_ty))
}

/// Drops an owning fat pointer: run the vtable's drop wrapper on the
/// object, then free the header. A null header (moved-from slot) is a
/// no-op.
pub(crate) fn emit_drop_dyn_value<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    header: PointerValue<'ctx>,
    trait_name: &str,
) -> Result<(), crate::CodegenError> {
    let current_fn = fn_ctx.function;
    let body_bb = fn_ctx
        .context
        .append_basic_block(current_fn, "dyn_drop.body");
    let done_bb = fn_ctx
        .context
        .append_basic_block(current_fn, "dyn_drop.done");
    let is_null = fn_ctx.builder.build_is_null(header, "dyn_drop_null")?;
    fn_ctx
        .builder
        .build_conditional_branch(is_null, done_bb, body_bb)?;
    fn_ctx.builder.position_at_end(body_bb);

    let (obj, vt) = load_header(fn_ctx, header)?;
    let ptr_ty = fn_ctx.context.ptr_type(AddressSpace::default());

    // The drop wrapper lives in slot 0. The vtable's field count is
    // irrelevant to the slot address; the trait's own count keeps the GEP
    // in bounds.
    let vtable_ty = vtable_struct_ty(fn_ctx.context, trait_methods(trait_name).len() + 1);
    let slot = fn_ctx
        .builder
        .build_struct_gep(vtable_ty, vt, 0, "dyn_drop_slot")?;
    let wrapper = fn_ctx
        .builder
        .build_load(ptr_ty, slot, "dyn_drop_fn")?
        .into_pointer_value();
    let void_ptr_fn = fn_ctx.context.void_type().fn_type(&[ptr_ty.into()], false);
    fn_ctx
        .builder
        .build_indirect_call(void_ptr_fn, wrapper, &[obj.into()], "dyn_drop_call")?;

    let free_fn = fn_ctx
        .module
        .get_function("free")
        .ok_or_else(|| crate::CodegenError::LLVMError("free not declared".into()))?;
    fn_ctx
        .builder
        .build_call(free_fn, &[header.into()], "dyn_header_free")?;
    fn_ctx.builder.build_unconditional_branch(done_bb)?;
    fn_ctx.builder.position_at_end(done_bb);
    Ok(())
}

/// Installs every pending vtable's function pointers and emits the
/// corresponding drop wrappers. Called after all function bodies exist:
/// slot values are functions that may be defined later than the coercion
/// site that created the global.
pub(crate) fn finalize_trait_vtables<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
) -> Result<(), crate::CodegenError> {
    let pending = PENDING_VTABLES.with(|pending| pending.take());
    for (trait_name, class_name) in pending {
        let methods = trait_methods(&trait_name);
        let mut slots: Vec<BasicValueEnum<'_>> = Vec::with_capacity(methods.len() + 1);

        let wrapper = get_or_create_dyn_drop_wrapper(module, context, &trait_name, &class_name)?;
        slots.push(wrapper.as_global_value().as_pointer_value().into());

        for method in &methods {
            let fn_name = format!("{class_name}.{}", method.name);
            let function = module.get_function(&fn_name).ok_or_else(|| {
                crate::CodegenError::LLVMError(format!(
                    "vtable slot references undefined method `{fn_name}`"
                ))
            })?;
            slots.push(function.as_global_value().as_pointer_value().into());
        }

        let name = format!("__ntsc_vtable_{trait_name}_{class_name}");
        let global = module.get_global(&name).ok_or_else(|| {
            crate::CodegenError::LLVMError(format!("vtable `{name}` was never created"))
        })?;
        global.set_initializer(&context.const_struct(&slots, false));
    }
    Ok(())
}
