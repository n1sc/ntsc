//! Class and function metadata lookups (fields, method types, parents).

use super::*;

/// Map an NTSC type annotation to a `Ty` for codegen.
pub(crate) fn type_annotation_to_ty(ann: &Option<ntsc_ast::types::TypeAnnotation>) -> Ty {
    use ntsc_ast::types::TypeAnnotation;
    match ann {
        Some(TypeAnnotation::Int) => Ty::Int,
        Some(TypeAnnotation::Float) => Ty::Float,
        Some(TypeAnnotation::String) => Ty::String,
        Some(TypeAnnotation::Bool) => Ty::Bool,
        Some(TypeAnnotation::Array(element)) => {
            let elem_ty = element
                .as_ref()
                .map(|inner| type_annotation_to_ty(&Some(*inner.clone())))
                .unwrap_or(Ty::Any);
            Ty::Array(Box::new(elem_ty))
        }
        Some(TypeAnnotation::Object) => Ty::Object,
        Some(TypeAnnotation::Option(inner)) => {
            Ty::Option(Box::new(type_annotation_to_ty(&Some(*inner.clone()))))
        }
        Some(TypeAnnotation::Result { ok, err }) => Ty::Result {
            ok: Box::new(type_annotation_to_ty(&Some(*ok.clone()))),
            err: Box::new(type_annotation_to_ty(&Some(*err.clone()))),
        },
        Some(TypeAnnotation::View(inner, mutable)) => Ty::View(
            Box::new(type_annotation_to_ty(&Some(*inner.clone()))),
            *mutable,
        ),
        Some(TypeAnnotation::Any) => Ty::Any,
        Some(TypeAnnotation::Pointer) => Ty::Pointer,
        Some(TypeAnnotation::Slice(element)) => Ty::Slice(Box::new(
            element
                .as_deref()
                .map(|element| type_annotation_to_ty(&Some(element.clone())))
                .unwrap_or(Ty::Any),
        )),
        Some(TypeAnnotation::Own(inner)) => {
            Ty::Own(Box::new(type_annotation_to_ty(&Some(*inner.clone()))))
        }
        Some(TypeAnnotation::Ref(inner, mutable)) => Ty::Ref(
            Box::new(type_annotation_to_ty(&Some(*inner.clone()))),
            *mutable,
        ),
        Some(TypeAnnotation::RawPointer(inner, mutable)) => Ty::RawPointer(
            Box::new(type_annotation_to_ty(&Some(*inner.clone()))),
            *mutable,
        ),
        Some(TypeAnnotation::Named(token)) if token.lexeme() == "void" => Ty::Void,
        Some(TypeAnnotation::Named(token))
            if matches!(
                token.lexeme(),
                "byte"
                    | "c_char"
                    | "c_uchar"
                    | "c_short"
                    | "c_ushort"
                    | "c_int"
                    | "c_uint"
                    | "c_long"
                    | "c_ulong"
                    | "c_longlong"
                    | "c_ulonglong"
                    | "c_size_t"
            ) =>
        {
            Ty::Int
        }
        Some(TypeAnnotation::Named(token)) if matches!(token.lexeme(), "c_float" | "c_double") => {
            Ty::Float
        }
        Some(TypeAnnotation::Named(token)) => Ty::Class(token.lexeme().to_string()),
        Some(TypeAnnotation::Shared(inner)) => {
            Ty::Shared(Box::new(type_annotation_to_ty(&Some(*inner.clone()))))
        }
        Some(TypeAnnotation::ImplTrait(_)) => Ty::Any,
        Some(TypeAnnotation::Dyn(token)) => Ty::Dyn(token.lexeme().to_string()),
        Some(TypeAnnotation::Tuple(types)) => Ty::Tuple(
            types
                .iter()
                .map(|t| type_annotation_to_ty(&Some(t.clone())))
                .collect(),
        ),
        Some(TypeAnnotation::Chan(element)) => {
            Ty::Chan(Box::new(type_annotation_to_ty(&Some(*element.clone()))))
        }
        None => Ty::Any,
    }
}

pub(crate) fn function_return_ty(ret: &Option<ntsc_ast::types::ReturnType>) -> Ty {
    ret.as_ref()
        .map(|r| type_annotation_to_ty(&Some(r.ty.clone())))
        .unwrap_or(Ty::Void)
}

/// Map an extern declaration to its platform C ABI type. Neutron's ordinary
/// `int`/`float` types remain the 64-bit runtime values; C-style declarations
/// use the widths required by the foreign ABI.
pub(crate) fn ffi_type_to_llvm<'ctx>(
    annotation: &ntsc_ast::types::TypeAnnotation,
    c_style: bool,
    context: &'ctx Context,
) -> Option<inkwell::types::BasicTypeEnum<'ctx>> {
    use ntsc_ast::types::TypeAnnotation;
    if !c_style {
        return match annotation {
            TypeAnnotation::Int => Some(context.i32_type().as_basic_type_enum()),
            TypeAnnotation::Float => Some(context.f64_type().as_basic_type_enum()),
            TypeAnnotation::Bool => Some(context.i8_type().as_basic_type_enum()),
            TypeAnnotation::RawPointer(..) => Some(
                context
                    .ptr_type(AddressSpace::default())
                    .as_basic_type_enum(),
            ),
            TypeAnnotation::Named(token) if token.lexeme() == "void" => None,
            _ => {
                let ty = type_annotation_to_ty(&Some(annotation.clone()));
                (ty != Ty::Void).then(|| ty_to_llvm(&ty, context))
            }
        };
    }
    if c_style {
        if let TypeAnnotation::Named(token) = annotation {
            let value = match token.lexeme() {
                "void" => return None,
                "c_char" | "c_uchar" => context.i8_type().as_basic_type_enum(),
                "c_short" | "c_ushort" => context.i16_type().as_basic_type_enum(),
                "c_int" | "c_uint" => context.i32_type().as_basic_type_enum(),
                "c_long" | "c_ulong" | "c_longlong" | "c_ulonglong" | "c_size_t" => {
                    context.i64_type().as_basic_type_enum()
                }
                "c_float" => context.f32_type().as_basic_type_enum(),
                "c_double" => context.f64_type().as_basic_type_enum(),
                _ => ty_to_llvm(&type_annotation_to_ty(&Some(annotation.clone())), context),
            };
            return Some(value);
        }
        if matches!(annotation, TypeAnnotation::RawPointer(..)) {
            return Some(
                context
                    .ptr_type(AddressSpace::default())
                    .as_basic_type_enum(),
            );
        }
        return match annotation {
            TypeAnnotation::Int => Some(context.i32_type().as_basic_type_enum()),
            TypeAnnotation::Float => Some(context.f64_type().as_basic_type_enum()),
            TypeAnnotation::Bool => Some(context.i8_type().as_basic_type_enum()),
            _ => {
                let ty = type_annotation_to_ty(&Some(annotation.clone()));
                (ty != Ty::Void).then(|| ty_to_llvm(&ty, context))
            }
        };
    }
    let ty = type_annotation_to_ty(&Some(annotation.clone()));
    (ty != Ty::Void).then(|| ty_to_llvm(&ty, context))
}

pub(crate) fn ffi_function_type<'ctx>(
    context: &'ctx Context,
    params: &[ntsc_ast::expr::FunctionParam],
    return_type: &Option<ntsc_ast::types::ReturnType>,
    c_style: bool,
) -> inkwell::types::FunctionType<'ctx> {
    let param_types: Vec<inkwell::types::BasicMetadataTypeEnum<'ctx>> = params
        .iter()
        .filter_map(|param| {
            param
                .type_annotation
                .as_ref()
                .and_then(|annotation| ffi_type_to_llvm(annotation, c_style, context))
                .map(Into::into)
        })
        .collect();
    match return_type
        .as_ref()
        .and_then(|return_type| ffi_type_to_llvm(&return_type.ty, c_style, context))
    {
        Some(return_type) => return_type.fn_type(&param_types, false),
        None => context.void_type().fn_type(&param_types, false),
    }
}

/// Return the declared return type of a class method, if the class declares
/// one (resolved through the inheritance chain).
pub(crate) fn class_method_ret_ty(class_name: &str, method: &str) -> Option<Ty> {
    let declaring = class_method_declaring_class(class_name, method)?;
    CLASS_METHOD_TYPES.with(|map| {
        map.borrow()
            .get(&declaring)
            .and_then(|methods| methods.get(method))
            .cloned()
    })
}

pub(crate) fn function_declared_ret_ty(name: &str) -> Option<Ty> {
    FUNCTION_RETURN_TYPES.with(|map| map.borrow().get(name).cloned())
}

/// Return the declared parameter types of a user function (empty for
/// unregistered or no-arg functions). Used to distinguish owned parameters
/// (moves) from view parameters (borrows) at call sites.
pub(crate) fn function_declared_param_types(name: &str) -> Vec<Ty> {
    FUNCTION_PARAM_TYPES
        .with(|map| map.borrow().get(name).cloned())
        .unwrap_or_default()
}

/// Return the declared parameter types of a class method (excluding
/// `this`), resolved through the inheritance chain.
pub(crate) fn class_method_declared_param_types(class_name: &str, method: &str) -> Vec<Ty> {
    let declaring = match class_method_declaring_class(class_name, method) {
        Some(declaring) => declaring,
        None => return Vec::new(),
    };
    CLASS_METHOD_PARAM_TYPES
        .with(|map| {
            map.borrow()
                .get(&declaring)
                .and_then(|methods| methods.get(method))
                .cloned()
        })
        .unwrap_or_default()
}

/// Whether `name` is a user-declared class type. Mirrors
/// `module.get_struct_type(name).is_some()` but consults the module-free
/// `CLASS_FIELDS` registry, so escape analysis can recognize constructor
/// calls without a module handle. `CLASS_FIELDS` is populated for every
/// class (with an empty field list when it declares none) before any
/// function is emitted, so a present class always has an entry.
pub(crate) fn is_class_type(name: &str) -> bool {
    CLASS_FIELDS.with(|map| map.borrow().contains_key(name))
}

/// The base class of a class, if it declares `extends`.
pub(crate) fn class_parent(class_name: &str) -> Option<String> {
    CLASS_PARENTS.with(|map| map.borrow().get(class_name).cloned())
}

/// Every field of a class in layout order: base fields first (recursively),
/// then the class's own fields.
pub(crate) fn class_all_fields(class_name: &str) -> Vec<String> {
    let mut fields = Vec::new();
    if let Some(parent) = class_parent(class_name) {
        fields.extend(class_all_fields(&parent));
    }
    fields.extend(CLASS_FIELDS.with(|m| m.borrow().get(class_name).cloned().unwrap_or_default()));
    fields
}

/// The types of [`class_all_fields`], in the same order.
pub(crate) fn class_all_field_types(class_name: &str) -> Vec<Ty> {
    let mut tys = Vec::new();
    if let Some(parent) = class_parent(class_name) {
        tys.extend(class_all_field_types(&parent));
    }
    tys.extend(CLASS_FIELD_TYPES.with(|m| m.borrow().get(class_name).cloned().unwrap_or_default()));
    tys
}

/// The declared field initializers of [`class_all_fields`], in the same
/// order.
pub(crate) fn class_all_field_inits(class_name: &str) -> Vec<Option<Expr>> {
    let mut inits = Vec::new();
    if let Some(parent) = class_parent(class_name) {
        inits.extend(class_all_field_inits(&parent));
    }
    inits.extend(
        CLASS_FIELD_INITS.with(|m| m.borrow().get(class_name).cloned().unwrap_or_default()),
    );
    inits
}

/// The flattened layout index of `field` in `class_name`, considering
/// inherited base fields.
pub(crate) fn class_field_index(class_name: &str, field: &str) -> Option<usize> {
    class_all_fields(class_name).iter().position(|f| f == field)
}

/// The class in the inheritance chain that declares `method`. A class's own
/// methods shadow inherited ones of the same name.
pub(crate) fn class_method_declaring_class(class_name: &str, method: &str) -> Option<String> {
    let declared_here = CLASS_METHOD_TYPES.with(|map| {
        map.borrow()
            .get(class_name)
            .is_some_and(|methods| methods.contains_key(method))
    });
    if declared_here {
        return Some(class_name.to_string());
    }
    class_parent(class_name).and_then(|parent| class_method_declaring_class(&parent, method))
}
