//! LLVM IR emission for NTSC.
//!
//! The emitter is split by concern; each submodule self-contains its section:
//! `runtime` (extern declarations), `typing` (type mapping), `module`
//! (entry points), `lookup` + `escape` + `function` (function metadata and
//! stack analysis), `async_sm` (state machines), `class`/`stmt`/`expr`/
//! `literal`/`binary`/`member`/`call`/`assign`/`control`/`exception` (language
//! constructs), `helper` (shared conversions), `array` (RC-backed ops),
//! `drop` (owned-value reclaim), `c_main` (the generated main), and `tests`.

use std::cell::RefCell;
use std::collections::HashMap;

type RustOriginKey = (String, String);
type RustOriginValue = (Option<String>, Option<String>);
use std::collections::HashSet;
use std::hash::{Hash, Hasher};

use inkwell::AddressSpace;
use inkwell::IntPredicate;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::types::{BasicMetadataTypeEnum, BasicType};

thread_local! {
    /// Class name → ordered field names (declaration order).
    static CLASS_FIELDS: RefCell<HashMap<String, Vec<String>>> = RefCell::new(HashMap::new());
}

thread_local! {
    /// Class name → ordered field types.
    ///
    /// Mirrors `CLASS_FIELDS` so member access can recover the exact type of
    /// an array/class field instead of reverse-mapping the LLVM type, which
    /// collapses every pointer field (arrays, strings, class instances) to
    /// `String`.
    static CLASS_FIELD_TYPES: RefCell<HashMap<String, Vec<Ty>>> = RefCell::new(HashMap::new());
}

thread_local! {
    /// Class name → per-field declared initializer (`None` when undeclared).
    ///
    /// Initializers run at construction, before `init`, so a constructor can
    /// still overwrite them.
    static CLASS_FIELD_INITS: RefCell<HashMap<String, Vec<Option<Expr>>>> =
        RefCell::new(HashMap::new());
}

thread_local! {
    /// Classes whose deep copy is currently being emitted.
    ///
    /// A class that (directly or transitively) holds a field of its own type
    /// would otherwise make copy recursion overflow the compiler stack;
    /// re-entry is reported as a codegen error instead.
    static CLASS_COPY_IN_PROGRESS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

thread_local! {
    /// Class name → `extends` base class name.
    ///
    /// Used to resolve inherited members and to lay out derived structs with
    /// the base fields first, so parent methods see a layout-compatible
    /// instance.
    static CLASS_PARENTS: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
}

thread_local! {
    /// Stdlib import alias → real module name, from `use strings as s`.
    ///
    /// The alias is bound as an object in the source, but native stdlib
    /// functions are named `ntsc_{module}_{fn}`, so codegen translates an
    /// aliased member call back to the real module name.
    static STDLIB_ALIASES: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
}

thread_local! {
    /// Rust-crate namespace (`crate` or its `as` alias) → real crate name,
    /// from `use @foo` / `use @foo as bar`. Member access `@foo.func` or
    /// `bar.func` dispatches to the adaptor symbol `ntsc_adaptor_<crate>_<fn>`.
    static RUST_CRATE_NAMESPACES: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
}

thread_local! {
    /// Bare name imported from a Rust crate via selective form
    /// `use (@func) = from @foo` → `func` resolves to
    /// `ntsc_adaptor_foo_func`.
    static RUST_SELECTIVE_IMPORTS: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
}

thread_local! {
    /// Rust-crate function → declared return label (`"bool"`, `"int"`,
    /// `"object[Regex]"`, ...), read from the crate's `*.export.json`
    /// manifest so a Rust `bool` return is a Neutron bool, not an (empty)
    /// string handle.
    static RUST_RETURN_TYPES: RefCell<HashMap<(String, String), String>> =
        RefCell::new(HashMap::new());
    static RUST_PATH_NAMES: RefCell<HashMap<(String, String), String>> =
        RefCell::new(HashMap::new());
    static RUST_ASYNC_TYPES: RefCell<HashMap<(String, String), bool>> =
        RefCell::new(HashMap::new());
    /// Owning adaptor crate for foreign-typed signatures, as
    /// `((crate, fn), (first-param origin, return origin))`: method
    /// dispatch and variable typing resolve through the origin, since
    /// handles live in one shared space.
    static RUST_ORIGINS: RefCell<HashMap<RustOriginKey, RustOriginValue>> =
        RefCell::new(HashMap::new());
}

type RustMethodKey = (String, String, String);
type RustMethodValue = (String, String);

thread_local! {
    /// Method dispatch for Rust object handles: (crate, type path,
    /// method) → (exported function name, return label). Built from the
    /// manifest entries whose first parameter is `object[T]`.
    static RUST_METHODS: RefCell<HashMap<RustMethodKey, RustMethodValue>> =
        RefCell::new(HashMap::new());
}

thread_local! {
    /// Variable name → (crate, type path) for variables currently holding
    /// a Rust object handle, enabling `re.is_match(...)` method syntax.
    static RUST_OBJECT_VARS: RefCell<HashMap<String, (String, String)>> =
        RefCell::new(HashMap::new());
}

/// Load the export manifests of every adaptor under `<project>/.ntsc/`
/// so `@crate.fn()` results are typed per their declared return types and
/// object handles can dispatch methods.
///
/// Call before `compile_program`/`emit_module`. Missing manifests leave the
/// tables empty, and every Rust call then falls back to the historical
/// string-handle assumption.
pub fn set_rust_export_types(project_dir: &std::path::Path) {
    RUST_RETURN_TYPES.with(|returns| {
        let mut returns = returns.borrow_mut();
        returns.clear();
        RUST_PATH_NAMES.with(|paths| paths.borrow_mut().clear());
        RUST_METHODS.with(|methods| {
            let mut methods = methods.borrow_mut();
            methods.clear();
            let deps_dir = project_dir.join(".ntsc");
            let Ok(entries) = std::fs::read_dir(&deps_dir) else {
                return;
            };
            for entry in entries.flatten() {
                let crate_dir = entry.path();
                if !crate_dir.is_dir() {
                    continue;
                }
                let Some(dir_name) = crate_dir.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                let manifest = crate_dir.join(format!("{dir_name}.export.json"));
                let Ok(content) = std::fs::read_to_string(&manifest) else {
                    continue;
                };
                for func in parse_export_functions(&content) {
                    let crate_name = normalized_crate_name(dir_name);
                    returns.insert(table_key(&crate_name, &func.name), func.ret.clone());
                    RUST_PATH_NAMES.with(|paths| {
                        paths
                            .borrow_mut()
                            .insert(table_key(&crate_name, &func.path), func.name.clone());
                    });
                    RUST_ASYNC_TYPES.with(|async_types| {
                        async_types
                            .borrow_mut()
                            .insert(table_key(&crate_name, &func.name), func.is_async);
                    });
                    RUST_ORIGINS.with(|origins| {
                        origins.borrow_mut().insert(
                            table_key(&crate_name, &func.name),
                            (
                                func.param0_origin.as_deref().map(normalized_crate_name),
                                func.ret_origin.as_deref().map(normalized_crate_name),
                            ),
                        );
                    });
                    // A first `object[T]` parameter marks a method
                    // bridge: `T_method` (with an optional dedup `_N`
                    // suffix) is callable as method syntax on a handle.
                    // Foreign receivers register under their origin, where
                    // variables holding such handles are typed.
                    if let Some(short) = func.param0.as_deref().and_then(object_type_short)
                        && let Some(method) = method_name_for(&func.name, short)
                        && let Some(type_path) = func.param0.as_deref().and_then(object_type_path)
                    {
                        let owner = func
                            .param0_origin
                            .as_deref()
                            .map(normalized_crate_name)
                            .unwrap_or_else(|| crate_name.clone());
                        methods.insert(
                            (owner, type_path, method.to_string()),
                            (func.name, func.ret),
                        );
                    }
                }
            }
        });
    });
}

/// The bare type inside an `object[T]` label's last path segment
/// (`Regex` for `object[bytes::Regex]`).
fn object_type_short(label: &str) -> Option<&str> {
    label
        .strip_prefix("object[")?
        .strip_suffix(']')?
        .rsplit("::")
        .next()
}

/// The full type path inside an `object[T]` label.
fn object_type_path(label: &str) -> Option<String> {
    Some(
        label
            .strip_prefix("object[")?
            .strip_suffix(']')?
            .to_string(),
    )
}

/// The Neutron element type of an `array[...]` manifest label
/// (`array[string]`, `option[array[int]]`, `result[array[bool]]`).
/// Collection bridges throw on `Err` and use the null handle for `None`,
/// so the wrapped forms lower to the same array type as the plain form.
pub(crate) fn rust_array_elem_ty(label: &str) -> Option<Ty> {
    let inner = label
        .strip_prefix("array[")
        .and_then(|s| s.strip_suffix(']'))
        .or_else(|| {
            label
                .strip_prefix("option[array[")
                .and_then(|s| s.strip_suffix("]]"))
        })
        .or_else(|| {
            label
                .strip_prefix("result[array[")
                .and_then(|s| s.strip_suffix("]]"))
        })?;
    Some(match inner {
        "string" => Ty::Array(Box::new(Ty::String)),
        "int" => Ty::Array(Box::new(Ty::Int)),
        "bool" => Ty::Array(Box::new(Ty::Bool)),
        "float" => Ty::Array(Box::new(Ty::Float)),
        _ => return None,
    })
}

/// The scalar payload type inside an `option[...]` return label.
pub(crate) fn rust_option_scalar_ty(label: &str) -> Option<Ty> {
    let inner = label.strip_prefix("option[")?.strip_suffix(']')?;
    Some(match inner {
        "int" => Ty::Int,
        "bool" => Ty::Bool,
        "float" => Ty::Float,
        _ => return None,
    })
}

/// Split an exported name `T_method` (optionally `_N`-suffixed) into its
/// method part, given the receiver type's short name.
fn method_name_for(exported: &str, short: &str) -> Option<String> {
    let rest = exported.strip_prefix(short)?.strip_prefix('_')?;
    // Strip a trailing dedup suffix (`is_match_2` -> `is_match`).
    if let Some((head, digits)) = rest.rsplit_once('_')
        && !digits.is_empty()
        && digits.chars().all(|c| c.is_ascii_digit())
    {
        return Some(head.to_string());
    }
    Some(rest.to_string())
}

/// One parsed bridge entry: exported name, call path, first-param label,
/// return label, async flag, and the owning crates of foreign-typed
/// first params and returns (`None` for local types).
#[derive(Debug, PartialEq)]
struct ExportFn {
    name: String,
    path: String,
    param0: Option<String>,
    ret: String,
    is_async: bool,
    param0_origin: Option<String>,
    ret_origin: Option<String>,
}

/// Extract bridge entries from one export manifest (`<crate>.export.json`,
/// the `ntsc-cargo` autogen standard).
///
/// The document is real JSON, so it is parsed as such: entries without
/// a `return` object (unsupported items) are skipped, a missing `path`
/// defaults to the name, and a missing `async` defaults to false.
/// Unknown fields and sections are ignored, and a malformed document
/// (or a missing `functions` array) yields no entries — callers then
/// fall back to the historical string-handle assumption.
fn parse_export_functions(content: &str) -> Vec<ExportFn> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(content) else {
        return Vec::new();
    };
    let Some(entries) = value.get("functions").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| {
            let name = entry.get("name")?.as_str()?;
            let returned = entry.get("return")?;
            let ret = returned.get("type")?.as_str()?;
            let path = entry
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(name);
            let first_param = entry
                .get("params")
                .and_then(serde_json::Value::as_array)
                .and_then(|params| params.first());
            let param0 = first_param
                .and_then(|param| param.get("type"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            let param0_origin = first_param
                .and_then(|param| param.get("origin"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            let is_async = entry
                .get("async")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            Some(ExportFn {
                name: name.to_string(),
                path: path.to_string(),
                param0,
                ret: ret.to_string(),
                is_async,
                param0_origin,
                ret_origin: returned
                    .get("origin")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
            })
        })
        .collect()
}

/// Whether an exported Rust function is asynchronous.
pub(crate) fn rust_async(crate_name: &str, fn_name: &str) -> bool {
    RUST_ASYNC_TYPES.with(|map| {
        map.borrow()
            .get(&table_key(crate_name, fn_name))
            .copied()
            .unwrap_or(false)
    })
}

/// The generated bridge name for a Rust path, if a manifest was loaded.
pub(crate) fn rust_path_name(crate_name: &str, path: &str) -> Option<String> {
    RUST_PATH_NAMES.with(|map| map.borrow().get(&table_key(crate_name, path)).cloned())
}

/// The declared return label of `crate_name.fn_name`, if a manifest was
/// loaded.
pub(crate) fn rust_return_label(crate_name: &str, fn_name: &str) -> Option<String> {
    RUST_RETURN_TYPES.with(|map| map.borrow().get(&table_key(crate_name, fn_name)).cloned())
}

/// The one place a crate name is spelled. A package may be hyphenated
/// (`mewsic-tui`) while the Neutron module that imports it is not
/// (`use @mewsic_tui`), and the generated bridge symbols normalize the
/// hyphen to an underscore. Every manifest table is keyed by the normalized
/// form on both insert and lookup, so any spelling resolves to the same
/// entry instead of silently missing and falling back to a handle type.
pub(crate) fn normalized_crate_name(name: &str) -> String {
    name.replace('-', "_")
}

/// The `(crate, name)` key shared by the manifest lookup tables.
fn table_key(crate_name: &str, name: &str) -> (String, String) {
    (normalized_crate_name(crate_name), name.to_string())
}

#[cfg(test)]
pub(crate) fn table_key_for_test(crate_name: &str, name: &str) -> (String, String) {
    table_key(crate_name, name)
}

/// The owning adaptor crate of `crate_name.fn_name`'s return value, when
/// the manifest marks it foreign. Variables holding such handles are
/// typed under the origin so method dispatch resolves there.
pub(crate) fn rust_return_origin(crate_name: &str, fn_name: &str) -> Option<String> {
    RUST_ORIGINS.with(|map| {
        map.borrow()
            .get(&table_key(crate_name, fn_name))
            .and_then(|(_, ret)| ret.clone())
    })
}

/// The exported bridge name for `receiver.method(...)` on a Rust object
/// handle of `type_path` from `crate_name`, plus its return label.
pub(crate) fn rust_method(
    crate_name: &str,
    type_path: &str,
    method: &str,
) -> Option<(String, String)> {
    RUST_METHODS.with(|map| {
        map.borrow()
            .get(&(
                normalized_crate_name(crate_name),
                type_path.to_string(),
                method.to_string(),
            ))
            .cloned()
    })
}

/// Record that `var_name` holds (or no longer holds, with `None`) a Rust
/// object handle.
pub(crate) fn set_rust_object_var(var_name: &str, object: Option<(String, String)>) {
    RUST_OBJECT_VARS.with(|vars| {
        let mut vars = vars.borrow_mut();
        match object {
            Some(entry) => {
                vars.insert(var_name.to_string(), entry);
            }
            None => {
                vars.remove(var_name);
            }
        }
    });
}

/// The Rust object type held by `var_name`, if tracked.
pub(crate) fn rust_object_var(var_name: &str) -> Option<(String, String)> {
    RUST_OBJECT_VARS.with(|vars| vars.borrow().get(var_name).cloned())
}

/// Adaptor symbol for retaining/releasing `crate_name`'s objects. Mirrors
/// the template's `crate_escaped` (`-` to `_`).
fn rust_object_ownership_symbol(crate_name: &str, op: &str) -> String {
    format!("ntsc_adaptor_{}_object_{op}", crate_name.replace('-', "_"))
}

/// Declare (on first use) and call `<crate>'s` object-release bridge for
/// `handle`. Null handles, stale ids, and foreign-crate ids are safe
/// no-ops inside the adaptor, so imprecise tracking can only leak, never
/// corrupt.
pub(crate) fn emit_object_release<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    crate_name: &str,
    handle: inkwell::values::IntValue<'ctx>,
) -> Result<(), crate::CodegenError> {
    let symbol = rust_object_ownership_symbol(crate_name, "release");
    let release = fn_ctx.module.get_function(&symbol).unwrap_or_else(|| {
        let void_ty = fn_ctx.context.void_type();
        let i64_ty = fn_ctx.context.i64_type();
        fn_ctx.module.add_function(
            &symbol,
            void_ty.fn_type(&[i64_ty.into()], false),
            Some(inkwell::module::Linkage::External),
        )
    });
    fn_ctx
        .builder
        .build_call(release, &[handle.into()], "object_release")?;
    Ok(())
}

/// Declare (on first use) and call `<crate>'s` object-retain bridge for
/// `handle`, returning the same handle for convenient chaining.
pub(crate) fn emit_object_retain<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    crate_name: &str,
    handle: inkwell::values::IntValue<'ctx>,
) -> Result<inkwell::values::IntValue<'ctx>, crate::CodegenError> {
    let symbol = rust_object_ownership_symbol(crate_name, "retain");
    let retain = fn_ctx.module.get_function(&symbol).unwrap_or_else(|| {
        let i64_ty = fn_ctx.context.i64_type();
        fn_ctx.module.add_function(
            &symbol,
            i64_ty.fn_type(&[i64_ty.into()], false),
            Some(inkwell::module::Linkage::External),
        )
    });
    let retained = fn_ctx
        .builder
        .build_call(retain, &[handle.into()], "object_retain")?;
    Ok(retained
        .try_as_basic_value()
        .basic()
        .unwrap_or_else(|| handle.into())
        .into_int_value())
}

/// Release the object held by `name`'s slot, if the name is tracked.
/// Safe to call unconditionally: untracked names, null handles, and stale
/// ids release as no-ops.
pub(crate) fn release_tracked_object<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    name: &str,
) -> Result<(), crate::CodegenError> {
    let Some((crate_name, _)) = rust_object_var(name) else {
        return Ok(());
    };
    let Some((ptr, _)) = fn_ctx.variables.get(name).map(|(p, t)| (*p, t.clone())) else {
        return Ok(());
    };
    let handle = fn_ctx
        .builder
        .build_load(fn_ctx.context.i64_type(), ptr, "tracked_obj")?
        .into_int_value();
    emit_object_release(fn_ctx, &crate_name, handle)
}

/// Release tracked objects declared in the block being exited (those in
/// the current map but absent — or differently-slotted — in `outer`).
/// Slots outlive the block, but their names do not: without this, a
/// block-local handle would survive until function exit at best, and a
/// shadowed outer slot would leak at worst. Null/stale releases no-op.
pub(crate) fn release_block_objects<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    outer: &HashMap<String, (PointerValue<'ctx>, Ty)>,
) -> Result<(), crate::CodegenError> {
    // No emission past a terminator (a `return`/`throw` inside the block
    // already routes through function-exit drops).
    if fn_ctx
        .builder
        .get_insert_block()
        .is_some_and(|block| block.get_terminator().is_some())
    {
        return Ok(());
    }
    let declared: Vec<String> = fn_ctx
        .variables
        .iter()
        .filter(|(name, (ptr, _))| outer.get(*name).is_none_or(|(o, _)| o != ptr))
        .map(|(name, _)| name.clone())
        .collect();
    for name in declared {
        release_tracked_object(fn_ctx, &name)?;
    }
    Ok(())
}

/// Callee kinds for object-argument ownership at call boundaries.
/// Synchronous calls always borrow (the caller outlives the call), so only
/// task boundaries — where the callee outlives the caller's scope — will
/// take and track ownership (wired up with the task-future drop glue when
/// the ownership milestone lands).
pub(crate) enum ObjectCallee {
    /// Rust bridges, methods, lambdas, direct calls: bridges own takes
    /// internally and borrow the rest; fresh temps die after the call,
    /// tracked variables are untouched borrows.
    Foreign,
}

/// Share caller-side object arguments with the callee. Every failure
/// direction is safe: releases are null/missing-safe no-ops, and an
/// untracked destination simply leaks like every handle did before this
/// machinery existed.
pub(crate) fn share_object_call_args<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    kind: &ObjectCallee,
    arguments: &[ntsc_ast::expr::Expr],
    arg_values: &[TypedValue<'ctx>],
) -> Result<(), crate::CodegenError> {
    use ntsc_ast::expr::Expr;
    // Spread elements break positional mapping; their handles keep the
    // historical leak rather than risk misattribution.
    if arguments
        .iter()
        .any(|arg| matches!(arg, Expr::Spread { .. }))
    {
        return Ok(());
    }
    for (arg, val) in arguments.iter().zip(arg_values) {
        // A fresh bridge result is temp-owned: release it after the call
        // (no-op when a takes-bridge consumed it first). Tracked variables
        // are untouched borrows here: the caller outlives a synchronous
        // call. (Task boundaries retain shares; wired with the future-drop
        // glue when the ownership milestone lands.)
        if matches!(kind, ObjectCallee::Foreign)
            && let Some((crate_name, _)) = crate::emit::call::fresh_object_call(arg)
        {
            let handle = crate::emit::call::coerce_to_i64(fn_ctx, val.value)?;
            emit_object_release(fn_ctx, &crate_name, handle)?;
        }
    }
    Ok(())
}
/// Adopt the object value of an assignment or declaration into `dest`'s
/// tracking, releasing what `dest` held and retaining shares.
///
/// - Fresh bridge result (`@crate.new()`): transfers the single reference;
///   no retain is needed and none is emitted.
/// - Tracked variable: shares, so retain first (self-assignment safe),
///   then release the old value, then track.
/// - Anything else: release the old value; tracking is left alone (stale
///   entries release as no-ops, while clearing could orphan an outer
///   scope's same-named slot).
pub(crate) fn adopt_object_assignment<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    dest: &str,
    value: &Expr,
) -> Result<(), crate::CodegenError> {
    if let Some((crate_name, type_path)) = call::fresh_object_call(value) {
        let consumed_destination = match value {
            Expr::Call { arguments, .. } => arguments.iter().any(
                |argument| matches!(argument, Expr::Variable { name } if name.lexeme() == dest),
            ),
            _ => false,
        };
        if !consumed_destination {
            release_tracked_object(fn_ctx, dest)?;
        }
        set_rust_object_var(dest, Some((crate_name, type_path)));
        return Ok(());
    }
    if let Expr::Variable { name: source } = value
        && let Some((crate_name, type_path)) = rust_object_var(source.lexeme())
    {
        if let Some((ptr, _)) = fn_ctx
            .variables
            .get(source.lexeme())
            .map(|(p, t)| (*p, t.clone()))
        {
            let handle = fn_ctx
                .builder
                .build_load(fn_ctx.context.i64_type(), ptr, "shared_obj")?
                .into_int_value();
            emit_object_retain(fn_ctx, &crate_name, handle)?;
        }
        release_tracked_object(fn_ctx, dest)?;
        set_rust_object_var(dest, Some((crate_name, type_path)));
        return Ok(());
    }
    // Plain value into a maybe-tracked slot: release the old reference.
    // The table entry (if stale) stays; its future releases no-op.
    release_tracked_object(fn_ctx, dest)?;
    Ok(())
}

thread_local! {
    /// Enum name → member name → i32 value. Enums are lowered to int
    /// constants; `Color.RED` resolves through this table.
    static ENUM_VALUES: RefCell<HashMap<String, HashMap<String, i32>>> =
        RefCell::new(HashMap::new());
}

thread_local! {
    /// Bare enum member name → i32 value.
    ///
    /// Allows `case North` and `say(North)` where the member is referenced
    /// without its enum qualifier; last declaration in program order wins.
    static ENUM_MEMBER_VALUES: RefCell<HashMap<String, i32>> = RefCell::new(HashMap::new());
}

thread_local! {
    /// `static const` variable name → declared type.
    ///
    /// Used by `emit_variable` to type a reference to a module-level
    /// constant emitted as an LLVM global.
    static STATIC_CONST_TYPES: RefCell<HashMap<String, Ty>> = RefCell::new(HashMap::new());
}

thread_local! {
    /// `static const` variable name → its literal initializer (kept for
    /// lazily building string handles on first use).
    static STATIC_CONST_INITS: RefCell<HashMap<String, Option<Expr>>> =
        RefCell::new(HashMap::new());
}

thread_local! {
    /// `static const` variable name → pre-evaluated compile-time value,
    /// populated by the type checker's const evaluator.
    static CONST_EVAL_VALUES: RefCell<HashMap<String, ntsc_typeck::ConstValue>> =
        RefCell::new(HashMap::new());
}

thread_local! {
    /// Class name → method name → declared return type.
    ///
    /// Used by the for-in iterator protocol to type the loop variable from
    /// the `get(i)` method's declared return type.
    static CLASS_METHOD_TYPES: RefCell<HashMap<String, HashMap<String, Ty>>> =
        RefCell::new(HashMap::new());
}

thread_local! {
    /// User-function name → declared return type.
    ///
    /// Call sites type the result from the annotation instead of inferring
    /// from the LLVM pointer return type, which would map any class pointer
    /// to `String`.
    static FUNCTION_RETURN_TYPES: RefCell<HashMap<String, Ty>> = RefCell::new(HashMap::new());
}

thread_local! {
    /// User-function name → the `(owning crate, type path)` of the fresh Rust
    /// object it hands back to its caller.
    ///
    /// A function that ends in `return @crate.new(...)` — directly, through a
    /// local, or through another object-returning function — transfers a
    /// single owned handle across the call boundary. Recording it lets a call
    /// site treat the user call exactly like a bridge call
    /// (see [`call::fresh_object_call`]): the handle is tracked in its
    /// destination and released at scope exit, or released after a borrowing
    /// bridge call, instead of leaking. Populated once, before any body is
    /// emitted, so a caller resolves regardless of definition order.
    static USER_OBJECT_RETURNS: RefCell<HashMap<String, (String, String)>> =
        RefCell::new(HashMap::new());
}

/// Record every free function's fresh-object return (replacing any prior set).
pub(crate) fn set_user_object_returns(map: HashMap<String, (String, String)>) {
    USER_OBJECT_RETURNS.with(|m| *m.borrow_mut() = map);
}

/// The `(crate, type path)` of the fresh object `name` returns, if it does.
pub(crate) fn user_function_object_return(name: &str) -> Option<(String, String)> {
    USER_OBJECT_RETURNS.with(|m| m.borrow().get(name).cloned())
}

thread_local! {
    /// User-function name → declared parameter types.
    ///
    /// Decides whether a call argument is moved (owned parameter) or borrowed
    /// (view parameter).
    static FUNCTION_PARAM_TYPES: RefCell<HashMap<String, Vec<Ty>>> = RefCell::new(HashMap::new());
}

thread_local! {
    /// Names of declared foreign functions. Their LLVM signatures may use
    /// C-width primitives even though type checking exposes Neutron values.
    static FFI_FUNCTIONS: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

pub(crate) fn is_ffi_function(name: &str) -> bool {
    FFI_FUNCTIONS.with(|functions| functions.borrow().contains(name))
}

thread_local! {
    /// Class name → method name → declared parameter types (excluding `this`).
    static CLASS_METHOD_PARAM_TYPES: RefCell<HashMap<String, HashMap<String, Vec<Ty>>>> =
        RefCell::new(HashMap::new());
}

use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, FunctionValue, PointerValue};

use ntsc_ast::expr::{Expr, LiteralValue};
use ntsc_ast::stmt::{Program, Stmt};
use ntsc_ast::token::TokenKind;
use ntsc_typeck::Ty;

/// Evaluate an expression that must be a compile-time int constant (used for
/// explicit `enum Member = value` values). Returns `None` for anything else.
fn const_int_expr_value(expr: &Expr) -> Option<i32> {
    match expr {
        Expr::Literal {
            value: LiteralValue::Number(n),
            ..
        } => n.parse::<i64>().ok().and_then(|v| i32::try_from(v).ok()),
        Expr::Unary { op, right } if op.lexeme() == "-" => {
            const_int_expr_value(right).map(|v| v.wrapping_neg())
        }
        Expr::Grouping { expression, .. } => const_int_expr_value(expression),
        _ => None,
    }
}

// ── Type-tracked value ──────────────────────────────────────────────────

#[derive(Clone)]
/// A value with its NTSC type information, keeping the language-level type
/// alongside the LLVM representation.
pub(crate) struct TypedValue<'ctx> {
    value: BasicValueEnum<'ctx>,
    ntsc_type: Ty,
}

impl<'ctx> TypedValue<'ctx> {
    fn new(value: BasicValueEnum<'ctx>, ntsc_type: Ty) -> Self {
        Self { value, ntsc_type }
    }
}

// ── Per-function state ──────────────────────────────────────────────────

pub(crate) struct FunctionContext<'ctx, 'm> {
    function: FunctionValue<'ctx>,
    builder: &'m Builder<'ctx>,
    entry_builder: &'m Builder<'ctx>,
    entry_bb: inkwell::basic_block::BasicBlock<'ctx>,
    module: &'m Module<'ctx>,
    variables: HashMap<String, (PointerValue<'ctx>, Ty)>,
    /// Class elements whose ownership was transferred into tuple locals.
    /// Tuples are LLVM aggregates, so this metadata is needed when a tuple
    /// is stored and destructured later.
    tuple_class_elements: HashMap<String, HashSet<usize>>,
    return_type: Ty,
    context: &'ctx Context,
    break_targets: Vec<inkwell::basic_block::BasicBlock<'ctx>>,
    continue_targets: Vec<inkwell::basic_block::BasicBlock<'ctx>>,
    loop_owned_slots: Vec<HashSet<String>>,
    /// Variable names visible before each enclosing loop, innermost last.
    /// A name bound inside the loop but absent here was declared by the
    /// loop itself: its slot is re-stored every iteration, so the previous
    /// value must be released on the way back around. A name that was
    /// already visible keeps its final value for the code after the loop
    /// and must not be touched here, even if the loop assigned it.
    loop_outer_names: Vec<HashSet<String>>,

    /// Exception-handler blocks of enclosing `try`/`retry`, innermost last.
    /// A pending exception branches to the innermost block; when empty it
    /// unwinds to the function's exception-return block.
    exception_targets: Vec<inkwell::basic_block::BasicBlock<'ctx>>,

    /// The block a pending exception branches to when no enclosing
    /// `try`/`retry` handles it: it drops all owned locals and returns a
    /// default value. Created lazily on first use.
    exception_return_bb: Option<inkwell::basic_block::BasicBlock<'ctx>>,

    /// Whether calls emit pending-exception checks.
    exception_checks: bool,

    /// Bitcast future struct pointer and its struct type when emitting an
    /// async poll function (async locals live in its fields, so they survive
    /// suspension).
    future_base: Option<(PointerValue<'ctx>, inkwell::types::StructType<'ctx>)>,

    /// Field index per async local name. When present, `alloca` becomes a GEP
    /// into the future struct (a deterministic field, stable across poll
    /// resumes) instead of a stack alloca.
    async_fields: Option<HashMap<String, u32>>,

    /// The layout of the async future currently being emitted. Present only
    /// while emitting an async poll body: it lets the statement emitter lower
    /// `await`/channel-op statements to suspension points wherever they appear
    /// (including inside control flow).
    async_layout: Option<&'m async_sm::AsyncLayout>,

    /// The next suspension point's ordinal, consumed in emission order as the
    /// poll body lowers each `await`/channel op. Matches the order
    /// `build_async_layout` assigns state indices.
    async_suspend_ordinal: u32,

    /// The resume block created for each suspension point, indexed by ordinal.
    /// The poll switch routes resume state `ordinal + 1` to
    /// `async_resume_blocks[ordinal]`.
    async_resume_blocks: Vec<inkwell::basic_block::BasicBlock<'ctx>>,

    /// Cached future-field GEPs, keyed by field index. Populated in the poll
    /// entry block (which dominates every resume block) so a field pointer
    /// used after a suspension is always dominated by its definition.
    future_field_cache: HashMap<u32, PointerValue<'ctx>>,

    /// Locals that escape analysis proved can be stack-allocated instead of
    /// heap-allocated.
    stack_allocated: HashSet<String>,

    /// Local `var x = ClassName(...)` constructions whose instance never
    /// escapes or is aliased within the function. Their owned fields are
    /// reclaimed by the class drop thunk at scope exit.
    class_drops: HashSet<String>,

    /// Class constructions whose only escaping use is a `return`. On the
    /// normal path the returned instance is moved to the caller and must not
    /// be dropped; on the exception-return path nothing is handed to the
    /// caller, so these instances are dropped there to avoid leaking the whole
    /// tree when a function throws after constructing a value it would have
    /// returned.
    exception_class_drops: HashSet<String>,

    /// Names of `var`-declared locals (or owned parameters) whose static
    /// type is a heap array or heap string: the slot is zero-initialized,
    /// the previous value is dropped before an overwrite, and the current
    /// value is dropped at function exit. Moving an owned value into another
    /// slot nulls the source so it is never dropped twice. View parameters
    /// and `for-in` loop variables are borrowed and never appear here.
    owned_slots: HashSet<String>,

    /// Owned slots that a re-declaration or block exit displaced, so they can
    /// no longer be reached through `variables`. Their values are still live
    /// and dropped at function exit alongside the slots still bound by name.
    shadowed_owned_slots: Vec<(PointerValue<'ctx>, Ty)>,

    /// Maps `Expr::AsyncBlock` spans to their generated anonymous function
    /// names. Populated during async poll emission for standalone blocks and
    /// `wait_any`/`wait_all` argument resolution.
    pub(crate) block_span_to_name: Option<HashMap<usize, String>>,

    /// The annotated slot type of a `var` initializer currently being
    /// emitted, if any. `chan.new` reads it to size the channel's
    /// element-ownership flag; it must be reset right after the initializer
    /// is emitted.
    pub(crate) expected_ty: Option<Ty>,
}

impl<'ctx, 'm> FunctionContext<'ctx, 'm> {
    fn new(
        function: FunctionValue<'ctx>,
        builder: &'m Builder<'ctx>,
        entry_builder: &'m Builder<'ctx>,
        entry_bb: inkwell::basic_block::BasicBlock<'ctx>,
        module: &'m Module<'ctx>,
        return_type: Ty,
        context: &'ctx Context,
    ) -> Self {
        Self {
            function,
            builder,
            entry_builder,
            entry_bb,
            module,
            variables: HashMap::new(),
            tuple_class_elements: HashMap::new(),
            return_type,
            context,
            break_targets: Vec::new(),
            continue_targets: Vec::new(),
            loop_owned_slots: Vec::new(),
            loop_outer_names: Vec::new(),
            exception_targets: Vec::new(),
            exception_return_bb: None,
            exception_checks: true,
            future_base: None,
            async_fields: None,
            async_layout: None,
            async_suspend_ordinal: 0,
            async_resume_blocks: Vec::new(),
            future_field_cache: HashMap::new(),
            stack_allocated: HashSet::new(),
            class_drops: HashSet::new(),
            exception_class_drops: HashSet::new(),
            owned_slots: HashSet::new(),
            shadowed_owned_slots: Vec::new(),
            block_span_to_name: None,
            expected_ty: None,
        }
    }

    fn current_exception_handler(&mut self) -> inkwell::basic_block::BasicBlock<'ctx> {
        // The innermost enclosing `try`/`retry` handler, or the function-level
        // exception-return block, created on first use.
        if let Some(bb) = self.exception_targets.last() {
            return *bb;
        }
        if let Some(bb) = self.exception_return_bb {
            return bb;
        }
        let bb = self
            .context
            .append_basic_block(self.function, "exception.return");
        self.exception_return_bb = Some(bb);
        bb
    }

    /// Check the runtime's pending-exception flag after a call that may
    /// throw: on pending, branch to the current exception handler; otherwise
    /// continue in a fresh block so the flag is not re-read by later
    /// instructions. A no-op inside async state machines.
    fn emit_pending_exception_check(&mut self) -> Result<(), crate::CodegenError> {
        if !self.exception_checks {
            return Ok(());
        }
        let handler = self.current_exception_handler();
        let pending_fn = self
            .module
            .get_function("ntsc_exception_pending")
            .ok_or_else(|| {
                crate::CodegenError::LLVMError("ntsc_exception_pending not declared".into())
            })?;
        let pending = self
            .builder
            .build_call(pending_fn, &[], "exc_pending")?
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let active = self.builder.build_int_compare(
            inkwell::IntPredicate::NE,
            pending,
            self.context.i8_type().const_zero(),
            "exc_active",
        )?;
        let continue_bb = self
            .context
            .append_basic_block(self.function, "exc.continue");
        self.builder
            .build_conditional_branch(active, handler, continue_bb)?;
        self.builder.position_at_end(continue_bb);
        Ok(())
    }

    fn push_loop_targets(
        &mut self,
        break_target: inkwell::basic_block::BasicBlock<'ctx>,
        continue_target: inkwell::basic_block::BasicBlock<'ctx>,
    ) {
        self.break_targets.push(break_target);
        self.continue_targets.push(continue_target);
        self.loop_owned_slots.push(self.owned_slots.clone());
        self.loop_outer_names
            .push(self.variables.keys().cloned().collect());
    }

    fn pop_loop_targets(&mut self) {
        self.break_targets.pop();
        self.continue_targets.pop();
        self.loop_owned_slots.pop();
        self.loop_outer_names.pop();
    }

    fn drop_loop_locals(&mut self) -> Result<(), crate::CodegenError> {
        let Some(outer) = self.loop_outer_names.last().cloned() else {
            return Ok(());
        };
        let mut names: Vec<_> = self.owned_slots.difference(&outer).cloned().collect();
        names.sort();
        for name in names {
            if let Some((ptr, ty)) = self
                .variables
                .get(&name)
                .map(|(ptr, ty)| (*ptr, ty.clone()))
            {
                emit_drop_slot_value(self, ptr, &ty)?;
                self.builder
                    .build_store(ptr, default_llvm_value(&ty, self.context))?;
            }
        }
        // Tracked adaptor objects are held apart from `owned_slots`, so a
        // loop-local `object[T]` reaches the back-edge (or a `break`/`continue`
        // that skips its block's own release) still owning its handle. Release
        // those declared since loop entry and null their slots, so the next
        // iteration's re-adopt and the function-exit release both no-op on a
        // stale handle instead of leaking or double-freeing.
        let mut tracked: Vec<String> = self
            .variables
            .keys()
            .filter(|name| !outer.contains(*name) && rust_object_var(name).is_some())
            .cloned()
            .collect();
        tracked.sort();
        for name in tracked {
            release_tracked_object(self, &name)?;
            if let Some((ptr, ty)) = self
                .variables
                .get(&name)
                .map(|(ptr, ty)| (*ptr, ty.clone()))
            {
                self.builder
                    .build_store(ptr, default_llvm_value(&ty, self.context))?;
            }
        }
        Ok(())
    }

    fn emit_break(&mut self) -> Result<(), crate::CodegenError> {
        let target = match self.break_targets.last() {
            Some(target) => *target,
            None => {
                return Err(crate::CodegenError::LLVMError(
                    "`break` outside of a loop".into(),
                ));
            }
        };
        self.drop_loop_locals()?;
        self.builder.build_unconditional_branch(target)?;
        Ok(())
    }

    fn emit_continue(&mut self) -> Result<(), crate::CodegenError> {
        let target = match self.continue_targets.last() {
            Some(target) => *target,
            None => {
                return Err(crate::CodegenError::LLVMError(
                    "`continue` outside of a loop".into(),
                ));
            }
        };
        self.drop_loop_locals()?;
        self.builder.build_unconditional_branch(target)?;
        Ok(())
    }

    /// Allocate space for a local variable at the very start of the entry block.
    ///
    /// The alloca is inserted before the entry block's first instruction so
    /// it is never placed after a terminator (e.g. when a variable is
    /// declared inside a loop body). Inside an async poll function, a
    /// top-level local is instead a field of the future struct (a GEP), so it
    /// survives suspension.
    fn alloca(&mut self, name: &str, ty: &Ty) -> Result<PointerValue<'ctx>, crate::CodegenError> {
        if let Some(fields) = &self.async_fields
            && let Some(index) = fields.get(name).copied()
        {
            return self.future_field(index);
        }
        let llvm_ty = ty_to_llvm(ty, self.context);
        match self.entry_bb.get_first_instruction() {
            Some(first) => self.entry_builder.position_before(&first),
            None => self.entry_builder.position_at_end(self.entry_bb),
        }
        let slot = self.entry_builder.build_alloca(llvm_ty, name)?;

        // Every slot a drop path can read starts null, so dropping a slot
        // that was never assigned is a safe no-op. Leaving a kind out of
        // `ty_is_owned_handle` would leave its slot `undef` until the first
        // store: a debug build reads whatever a fresh stack page holds, but
        // `mem2reg` turns the pre-store load into poison and the drop
        // thunk's null check can fold either way, freeing garbage handles.
        if ty_is_owned_handle(ty) {
            let zero = default_llvm_value(ty, self.context);
            self.entry_builder.build_store(slot, zero)?;
        }
        Ok(slot)
    }

    /// Allocate a raw LLVM-typed stack slot in the entry block. Used by
    /// escape analysis to place a stack-allocated class object; not used
    /// inside async poll functions.
    fn alloca_llvm(
        &mut self,
        name: &str,
        llvm_ty: inkwell::types::BasicTypeEnum<'ctx>,
    ) -> Result<PointerValue<'ctx>, crate::CodegenError> {
        match self.entry_bb.get_first_instruction() {
            Some(first) => self.entry_builder.position_before(&first),
            None => self.entry_builder.position_at_end(self.entry_bb),
        }
        Ok(self.entry_builder.build_alloca(llvm_ty, name)?)
    }

    fn future_field(&self, index: u32) -> Result<PointerValue<'ctx>, crate::CodegenError> {
        if let Some(ptr) = self.future_field_cache.get(&index) {
            return Ok(*ptr);
        }
        let (base, struct_ty) = self.future_base.ok_or_else(|| {
            crate::CodegenError::LLVMError("future field access outside an async poll".into())
        })?;
        self.builder
            .build_struct_gep(struct_ty, base, index, "future_field")
            .map_err(|e| crate::CodegenError::LLVMError(format!("future field GEP: {e}")))
    }

    /// Build and cache every future-field GEP in the block the builder is
    /// currently positioned in. The poll body calls this from the entry block
    /// so the cached pointers dominate every resume block reached by the
    /// dispatch switch.
    fn cache_future_fields(&mut self, field_count: u32) -> Result<(), crate::CodegenError> {
        let (base, struct_ty) = self.future_base.ok_or_else(|| {
            crate::CodegenError::LLVMError("future field access outside an async poll".into())
        })?;
        for index in 0..field_count {
            let ptr = self
                .builder
                .build_struct_gep(struct_ty, base, index, "future_field")
                .map_err(|e| crate::CodegenError::LLVMError(format!("future field GEP: {e}")))?;
            self.future_field_cache.insert(index, ptr);
        }
        Ok(())
    }

    /// Define a new variable in the current scope.
    ///
    /// A re-declaration in an inner block takes over the name, leaving the
    /// slot it displaced unreachable by name; if that slot owned a heap
    /// value it moves to `shadowed_owned_slots` so the exit-time drop still
    /// reclaims it instead of leaking it.
    fn define_var(&mut self, name: &str, ptr: PointerValue<'ctx>, ty: Ty) {
        if let Some((old_ptr, old_ty)) = self.variables.get(name)
            && *old_ptr != ptr
            && self.owned_slots.contains(name)
        {
            self.shadowed_owned_slots.push((*old_ptr, old_ty.clone()));
        }
        self.variables.insert(name.to_string(), (ptr, ty));
    }

    /// The bindings currently in scope, handed back to `end_block_scope`.
    fn begin_block_scope(&self) -> HashMap<String, (PointerValue<'ctx>, Ty)> {
        self.variables.clone()
    }

    /// Leave a block: names it declared go out of scope and the bindings they
    /// displaced become visible again. The slots themselves outlive the block
    /// (entry-block allocas) and their values are reclaimed at function exit,
    /// so an owned slot leaving scope moves to `shadowed_owned_slots`
    /// (unreachable by name, still dropped) exactly like one a re-declaration
    /// displaced.
    fn end_block_scope(
        &mut self,
        outer: HashMap<String, (PointerValue<'ctx>, Ty)>,
    ) -> Result<(), crate::CodegenError> {
        let declared: Vec<(String, PointerValue<'ctx>, Ty)> = self
            .variables
            .iter()
            .filter(|(name, (ptr, _))| outer.get(*name).is_none_or(|(o, _)| o != ptr))
            .map(|(name, (ptr, ty))| (name.clone(), *ptr, ty.clone()))
            .collect();
        // A block inside a loop body re-executes every iteration: a slot it
        // binds is re-stored each time, so deferring its value to the
        // function-exit drop frees only the last iteration and leaks the
        // rest. Release here instead. The slot is nulled, so the exit-time
        // drop of the shadowed entry below is a safe no-op, and so is a
        // `break`/`continue` that already released through `drop_loop_locals`.
        let release_now = !self.loop_owned_slots.is_empty()
            && self
                .builder
                .get_insert_block()
                .and_then(|block| block.get_terminator())
                .is_none();
        for (name, ptr, ty) in declared {
            if !self.owned_slots.contains(&name) {
                continue;
            }
            if release_now {
                emit_drop_slot_value(self, ptr, &ty)?;
                self.builder
                    .build_store(ptr, default_llvm_value(&ty, self.context))?;
            }
            self.shadowed_owned_slots.push((ptr, ty));
            match outer.get(&name) {
                Some((outer_ptr, _)) => {
                    // The declaration displaced an outer slot of the same
                    // name (`define_var` moved it to `shadowed_owned_slots`);
                    // it is reachable by name again, so drop it through the
                    // name and leave exactly one owner for each slot.
                    if let Some(at) = self
                        .shadowed_owned_slots
                        .iter()
                        .position(|(p, _)| p == outer_ptr)
                    {
                        self.shadowed_owned_slots.remove(at);
                    }
                }

                None => {
                    // Nothing outside the block owns this name any more.
                    self.owned_slots.remove(&name);
                }
            }
        }
        self.variables = outer;
        Ok(())
    }

    fn lookup_var(&self, name: &str) -> Option<(PointerValue<'ctx>, &Ty)> {
        self.variables.get(name).map(|(ptr, ty)| (*ptr, ty))
    }

    /// Register `name` as an owned slot if its type owns a heap allocation
    /// (array, string, object, or shared box). View parameters, `this`, and
    /// scalars are never owned. Class slots are owned only when escape
    /// analysis proved the instance is not aliased; a borrowed class value
    /// (an element read, a field read) must never be dropped.
    fn mark_owned_if_heap(&mut self, name: &str, ty: &Ty) {
        let heap_owned = matches!(
            ty,
            Ty::Array(_)
                | Ty::String
                | Ty::Object
                | Ty::Shared(_)
                | Ty::Chan(_)
                | Ty::Option(_)
                | Ty::Result { .. }
                | Ty::Pointer
                | Ty::Slice(_)
                | Ty::Own(_)
                | Ty::Dyn(_)
        ) || (matches!(ty, Ty::Class(_)) && self.class_drops.contains(name));
        if heap_owned {
            self.owned_slots.insert(name.to_string());
        }
    }

    /// Null the stack slot of an owned variable after its value was moved
    /// elsewhere, so the exit-time drop is a no-op. Shared boxes are never
    /// moved (only retained) and class values use reference semantics
    /// (`var y = x` aliases `x`), so those slots are left untouched. Option
    /// and result slots *are* nulled: both are owned cells, so a move that
    /// left the slot intact would let both the destination and this scope's
    /// exit free the same cell. A `dyn` fat pointer owns its header, so it
    /// is nulled on move for the same reason. A `chan` slot holds one
    /// reference of a counted handle: `return ch` hands that reference to
    /// the caller, so the slot must not release it again.
    fn null_var_slot(&mut self, name: &str) {
        if let Some((ptr, ty)) = self.lookup_var(name)
            && matches!(
                ty,
                Ty::Array(_)
                    | Ty::String
                    | Ty::Object
                    | Ty::Chan(_)
                    | Ty::Option(_)
                    | Ty::Dyn(_)
                    | Ty::Result { .. }
            )
        {
            let _ = self
                .builder
                .build_store(ptr, default_llvm_value(ty, self.context));
        }
    }
}

pub(crate) mod array;
pub(crate) mod assign;
pub(crate) mod async_sm;
pub(crate) mod binary;
pub(crate) mod c_main;
pub(crate) mod call;
pub(crate) mod class;
pub(crate) mod control;
pub(crate) mod drop;
pub(crate) mod dyn_obj;
pub(crate) mod escape;
pub(crate) mod exception;
pub(crate) mod expr;
pub(crate) mod function;
pub(crate) mod helper;
pub(crate) mod literal;
pub(crate) mod lookup;
pub(crate) mod member;
pub(crate) mod module;
pub(crate) mod result_cell;
pub(crate) mod runtime;
pub(crate) mod stmt;
#[cfg(test)]
pub(crate) mod tests;
pub(crate) mod typing;

pub(crate) use array::*;
pub(crate) use assign::*;
pub(crate) use async_sm::*;
pub(crate) use binary::*;
pub(crate) use c_main::*;
pub(crate) use call::*;
pub(crate) use class::*;
pub(crate) use control::*;
pub(crate) use drop::*;
pub(crate) use escape::*;
pub(crate) use exception::*;
pub(crate) use expr::*;
pub(crate) use function::*;
pub(crate) use helper::*;
pub(crate) use literal::*;
pub(crate) use lookup::*;
pub(crate) use member::*;
pub use module::emit_module;
pub(crate) use result_cell::*;
pub(crate) use runtime::*;
pub(crate) use stmt::*;
pub(crate) use typing::*;

#[cfg(test)]
mod manifest_tests {
    use super::parse_export_functions;

    #[test]
    fn manifest_entries_parse_with_async_and_paths() {
        let manifest = r#"{
  "schema_version": 3,
  "module": "demo",
  "functions": [
    {
      "name": "request_async",
      "path": "request_async",
      "status": "typed",
      "params": [{ "name": "a0", "tier": 1, "type": "string" }],
      "return": { "tier": 1, "type": "string" },
      "symbol": "ntsc_adaptor_demo_request_async",
      "async": true
    },
    {
      "name": "request",
      "path": "request",
      "status": "typed",
      "params": [{ "name": "a0", "tier": 1, "type": "string" }],
      "return": { "tier": 1, "type": "string" },
      "symbol": "ntsc_adaptor_demo_request",
      "async": false
    },
    {
      "name": "broken",
      "path": "broken",
      "status": "unsupported",
      "reason": "no bridge"
    }
  ],
  "callbacks": []
}"#;

        let entries = parse_export_functions(manifest);
        // The unsupported entry (no `return`) is skipped, not fatal.
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "request_async");
        assert_eq!(entries[0].param0.as_deref(), Some("string"));
        assert_eq!(entries[0].ret, "string");
        assert!(entries[0].is_async);
        assert_eq!(entries[0].param0_origin, None);
        assert_eq!(entries[1].name, "request");
        assert!(!entries[1].is_async);
    }

    #[test]
    fn manifest_parsing_tolerates_hostile_and_missing_shapes() {
        // Escaped quotes inside strings must not confuse the reader, and
        // missing `path`/`async` default sanely.
        let manifest = r#"{
  "functions": [
    {
      "name": "greet",
      "status": "typed",
      "params": [],
      "return": { "tier": 1, "type": "string" },
      "symbol": "s",
      "doc": "says \"hi\""
    }
  ]
}"#;
        let entries = parse_export_functions(manifest);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "greet");
        assert_eq!(entries[0].path, "greet");
        assert_eq!(entries[0].param0, None);
        assert_eq!(entries[0].ret, "string");
        assert!(!entries[0].is_async);

        assert!(parse_export_functions("not json").is_empty());
        assert!(parse_export_functions("{}").is_empty());
        assert!(parse_export_functions(r#"{"functions": []}"#).is_empty());
    }

    #[test]
    fn manifest_origins_route_foreign_types() {
        let manifest = r#"{
  "schema_version": 3,
  "module": "consumer",
  "functions": [
    {
      "name": "register",
      "path": "serenity::register",
      "status": "typed",
      "params": [{ "name": "a0", "tier": 1, "type": "object[client::ClientBuilder]", "origin": "serenity" }],
      "return": { "tier": 1, "type": "object[client::ClientBuilder]", "origin": "serenity" },
      "symbol": "ntsc_adaptor_consumer_register",
      "async": false
    }
  ],
  "callbacks": []
}"#;
        let entries = parse_export_functions(manifest);
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].param0.as_deref(),
            Some("object[client::ClientBuilder]")
        );
        assert_eq!(entries[0].param0_origin.as_deref(), Some("serenity"));
        assert_eq!(entries[0].ret_origin.as_deref(), Some("serenity"));
    }
}
