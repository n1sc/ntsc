//! NTSC type to LLVM type mapping and default values.

use super::*;

/// Map an NTSC type to its LLVM representation.
///
/// All runtime-registry-backed values travel as opaque i64 handles: strings,
/// arrays, option cells, shared boxes, JSON objects, and the dynamic
/// `any`/`nil` values that may hold them. Class instances stay raw
/// `malloc`ed structs, so they remain pointers. `void` maps to i8 only for
/// storage purposes (e.g. storing a return default); it never carries data.
/// A view of a value has the same representation as the underlying value
/// itself — a handle for registry-backed types, a plain copy for scalars.
pub(crate) fn ty_to_llvm<'ctx>(
    ty: &Ty,
    context: &'ctx Context,
) -> inkwell::types::BasicTypeEnum<'ctx> {
    let i64 = context.i64_type();
    match ty {
        Ty::Int => i64.as_basic_type_enum(),
        Ty::Float => context.f64_type().as_basic_type_enum(),
        Ty::Bool => context.bool_type().as_basic_type_enum(),

        Ty::String => i64.as_basic_type_enum(),
        Ty::Void => context.i8_type().as_basic_type_enum(),
        Ty::Nil => i64.as_basic_type_enum(),
        Ty::Array(_) => i64.as_basic_type_enum(),
        Ty::Option(_) => i64.as_basic_type_enum(),
        Ty::Result { .. } => i64.as_basic_type_enum(),
        Ty::Object => i64.as_basic_type_enum(),
        Ty::Function { .. } => context
            .ptr_type(AddressSpace::default())
            .as_basic_type_enum(),

        Ty::Class(_) => context
            .ptr_type(AddressSpace::default())
            .as_basic_type_enum(),
        Ty::Any => i64.as_basic_type_enum(),
        Ty::Pointer => i64.as_basic_type_enum(),
        Ty::Slice(_) => i64.as_basic_type_enum(),
        Ty::Chan(_) => i64.as_basic_type_enum(),

        Ty::Shared(_) => i64.as_basic_type_enum(),

        Ty::View(inner, _) => ty_to_llvm(inner, context),
        // An owning allocation, a reference, a raw pointer, and a trait
        // object are all a machine address of the pointee's storage.
        Ty::Own(_) | Ty::Ref(..) | Ty::RawPointer(..) | Ty::Dyn(_) => context
            .ptr_type(AddressSpace::default())
            .as_basic_type_enum(),
        Ty::Tuple(elements) => {
            let fields: Vec<inkwell::types::BasicTypeEnum> =
                elements.iter().map(|e| ty_to_llvm(e, context)).collect();
            let ll_ty = context.struct_type(&fields, false);
            ll_ty.as_basic_type_enum()
        }
    }
}

/// Whether `ty`'s LLVM representation is a raw pointer (rather than an
/// i64 handle). Only class instances and function references travel as
/// pointers; everything else is a registry handle or a scalar.
pub(crate) fn ty_is_llvm_pointer(ty: &Ty) -> bool {
    match ty {
        Ty::Class(_) | Ty::Function { .. } | Ty::Dyn(_) => true,
        Ty::Own(_) | Ty::Ref(..) | Ty::RawPointer(..) => true,
        Ty::View(inner, _) => ty_is_llvm_pointer(inner),
        _ => false,
    }
}

pub(crate) fn default_llvm_value<'ctx>(ty: &Ty, context: &'ctx Context) -> BasicValueEnum<'ctx> {
    match ty {
        Ty::Int => context.i64_type().const_zero().into(),
        Ty::Float => context.f64_type().const_zero().into(),
        Ty::Bool => context.bool_type().const_zero().into(),
        Ty::View(inner, _) => default_llvm_value(inner, context),
        Ty::Own(_) | Ty::Ref(..) | Ty::RawPointer(..) => context
            .ptr_type(AddressSpace::default())
            .const_null()
            .into(),
        Ty::String
        | Ty::Nil
        | Ty::Array(_)
        | Ty::Option(_)
        | Ty::Result { .. }
        | Ty::Object
        | Ty::Any
        | Ty::Shared(_)
        | Ty::Pointer
        | Ty::Slice(_)
        | Ty::Chan(_)
        | Ty::Void => context.i64_type().const_zero().into(),
        Ty::Tuple(elements) => {
            let ll_ty = ty_to_llvm(&Ty::Tuple(elements.clone()), context);
            if let inkwell::types::BasicTypeEnum::StructType(st) = ll_ty {
                st.const_zero().into()
            } else {
                context.i64_type().const_zero().into()
            }
        }
        Ty::Function { .. } | Ty::Class(_) | Ty::Dyn(_) => context
            .ptr_type(AddressSpace::default())
            .const_null()
            .into(),
    }
}

/// Target ABI storage size, with a nonzero allocation even for empty aggregates.
pub(crate) fn llvm_allocation_size(
    module: &Module<'_>,
    llvm_type: inkwell::types::BasicTypeEnum<'_>,
) -> Result<u64, crate::CodegenError> {
    if llvm_type.size_of().is_none() {
        return Err(crate::CodegenError::LLVMError(format!(
            "cannot allocate unsized type `{llvm_type}`"
        )));
    }
    let layout = module.get_data_layout();
    let layout = layout.as_str().to_str().map_err(|error| {
        crate::CodegenError::LLVMError(format!("invalid target data layout: {error}"))
    })?;
    let target_data = inkwell::targets::TargetData::create(layout);
    Ok(target_data.get_abi_size(&llvm_type).max(1))
}
