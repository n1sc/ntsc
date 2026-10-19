//! LLVM context management and module emission.

use std::cell::Cell;
use std::sync::{Mutex, MutexGuard, Once};

use inkwell::OptimizationLevel;
use inkwell::module::Module;
pub use inkwell::targets::TargetMachine;
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetTriple,
};

/// LLVM's target registry initialization is not thread-safe, so it must run
/// exactly once per process even when codegen runs from several threads.
static INIT_LLVM: Once = Once::new();

pub fn init_llvm() {
    INIT_LLVM.call_once(|| {
        // Fail fast when the linked system libLLVM.so is not the LLVM major
        // the inkwell C-API bindings were generated for: the mismatch is not
        // reported at build or link time, and the ABI skew corrupts memory
        // inside the pass pipeline instead of failing loudly.
        let (mut major, mut minor, mut patch) = (0u32, 0u32, 0u32);
        unsafe {
            llvm_sys::core::LLVMGetVersion(&mut major, &mut minor, &mut patch);
        }
        assert_eq!(
            major, EXPECTED_LLVM_MAJOR,
            "linked LLVM runtime is {major}.{minor}.{patch}, but ntsc-codegen was built for the \
             LLVM {EXPECTED_LLVM_MAJOR} C API (inkwell `llvm23-1-force-dynamic`). A mismatched system \
             libLLVM.so silently corrupts memory inside the pass pipeline: install LLVM \
             {EXPECTED_LLVM_MAJOR} and point LLVM_SYS_231_PREFIX at it (see \
             docs/reference/contributing.md)."
        );
        Target::initialize_all(&InitializationConfig::default());
    });
}

/// The LLVM major version the inkwell C-API bindings were generated for.
/// Must match the `llvm23-1-force-dynamic` feature in `Cargo.toml`.
const EXPECTED_LLVM_MAJOR: u32 = 23;

/// Serializes every operation that touches LLVM's shared global state.
///
/// LLVM's target registry, pass manager, and object-file backend share global
/// state that must not be accessed concurrently. The production compiler is
/// single-threaded; the lock also keeps the parallel test suite safe.
static CODGEN_LOCK: Mutex<()> = Mutex::new(());

thread_local! {
    static CODGEN_LOCK_DEPTH: Cell<usize> = const { Cell::new(0) };
}

/// Reentrant RAII guard for [`CODGEN_LOCK`].
///
/// The outermost acquisition holds the mutex; nested acquisitions on the same
/// thread only bump the thread-local depth counter. The `guard` field is never
/// read — it exists to pin the mutex release to the outermost guard's `Drop`.
pub(crate) struct CodegenLockGuard {
    #[allow(dead_code)]
    guard: Option<MutexGuard<'static, ()>>,
}

impl CodegenLockGuard {
    /// Acquire the codegen lock, nesting if the current thread already holds it.
    pub(crate) fn acquire() -> Self {
        let depth = CODGEN_LOCK_DEPTH.with(Cell::get);
        if depth > 0 {
            CODGEN_LOCK_DEPTH.with(|c| c.set(depth + 1));
            CodegenLockGuard { guard: None }
        } else {
            let guard = CODGEN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            CODGEN_LOCK_DEPTH.with(|c| c.set(1));
            CodegenLockGuard { guard: Some(guard) }
        }
    }
}

impl Drop for CodegenLockGuard {
    fn drop(&mut self) {
        let depth = CODGEN_LOCK_DEPTH.with(Cell::get);
        debug_assert!(depth > 0, "unbalanced codegen lock acquisition");
        CODGEN_LOCK_DEPTH.with(|c| c.set(depth - 1));
    }
}

pub fn create_target_machine(
    triple: &str,
    opt_level: OptimizationLevel,
) -> Result<TargetMachine, crate::CodegenError> {
    let _guard = CodegenLockGuard::acquire();

    let target_triple = TargetTriple::create(triple);
    let target = Target::from_triple(&target_triple).map_err(|e| {
        crate::CodegenError::LLVMError(format!("invalid target triple `{triple}`: {e}"))
    })?;

    target
        .create_target_machine(
            &target_triple,
            "generic",
            "",
            opt_level,
            RelocMode::PIC,
            CodeModel::Default,
        )
        .ok_or_else(|| {
            crate::CodegenError::LLVMError(format!(
                "failed to create target machine for `{triple}`"
            ))
        })
}

pub fn write_object_file(
    module: &Module<'_>,
    target_machine: &TargetMachine,
    path: &std::path::Path,
) -> Result<(), crate::CodegenError> {
    let _guard = CodegenLockGuard::acquire();

    target_machine
        .write_to_file(module, FileType::Object, path)
        .map_err(|e| crate::CodegenError::LLVMError(format!("failed to write object file: {e}")))
}

/// Run the IR optimization pipeline over a module before codegen.
///
/// `TargetMachine::write_to_file` only runs the backend, so the alloca-based
/// slots the codegen emits would stay as stack memory unless the IR-level
/// passes (mem2reg etc.) run explicitly here. The pass set is deliberately
/// conservative and deterministic — no vectorization or unrolling — so the
/// emitted IR and final object stay predictable and easy to debug.
pub fn run_optimization_passes(
    module: &Module<'_>,
    target_machine: &TargetMachine,
) -> Result<(), crate::CodegenError> {
    let _guard = CodegenLockGuard::acquire();

    let passes = "mem2reg,instcombine,simplifycfg,sccp,dce,gvn";
    let options = inkwell::passes::PassBuilderOptions::create();
    options.set_verify_each(true);
    options.set_loop_unrolling(false);
    options.set_loop_vectorization(false);
    options.set_loop_slp_vectorization(false);
    module
        .run_passes(passes, target_machine, options)
        .map_err(|e| {
            crate::CodegenError::LLVMError(format!("optimization pass pipeline failed: {e}"))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count_allocas(function: inkwell::values::FunctionValue) -> usize {
        function
            .get_basic_blocks()
            .iter()
            .flat_map(|block| block.get_instructions())
            .filter(|instruction| {
                instruction.get_opcode() == inkwell::values::InstructionOpcode::Alloca
            })
            .count()
    }

    /// Replicates the slot pattern the codegen emits for every local: a loop
    /// induction variable and accumulator in allocas.
    fn build_loop_function<'ctx>(
        context: &'ctx inkwell::context::Context,
    ) -> (Module<'ctx>, inkwell::values::FunctionValue<'ctx>) {
        let module = context.create_module("opt_test");
        let i64_ty = context.i64_type();
        let fn_ty = i64_ty.fn_type(&[i64_ty.into()], false);
        let function = module.add_function("loop", fn_ty, None);
        let builder = context.create_builder();
        let zero = i64_ty.const_zero();
        let one = i64_ty.const_int(1, false);
        let n = function
            .get_first_param()
            .expect("n param")
            .into_int_value();

        let entry = context.append_basic_block(function, "entry");
        let loop_bb = context.append_basic_block(function, "loop");
        let body = context.append_basic_block(function, "body");
        let exit = context.append_basic_block(function, "exit");

        builder.position_at_end(entry);
        let i_slot = builder.build_alloca(i64_ty, "i").unwrap();
        let sum_slot = builder.build_alloca(i64_ty, "sum").unwrap();
        builder.build_store(i_slot, zero).unwrap();
        builder.build_store(sum_slot, zero).unwrap();
        builder.build_unconditional_branch(loop_bb).unwrap();

        builder.position_at_end(loop_bb);
        let i = builder
            .build_load(i64_ty, i_slot, "i")
            .unwrap()
            .into_int_value();
        let cont = builder
            .build_int_compare(inkwell::IntPredicate::ULT, i, n, "cont")
            .unwrap();
        builder.build_conditional_branch(cont, body, exit).unwrap();

        builder.position_at_end(body);
        let sum = builder
            .build_load(i64_ty, sum_slot, "sum")
            .unwrap()
            .into_int_value();
        let next_sum = builder.build_int_add(sum, i, "next_sum").unwrap();
        builder.build_store(sum_slot, next_sum).unwrap();
        let next_i = builder.build_int_add(i, one, "next_i").unwrap();
        builder.build_store(i_slot, next_i).unwrap();
        builder.build_unconditional_branch(loop_bb).unwrap();

        builder.position_at_end(exit);
        let result = builder
            .build_load(i64_ty, sum_slot, "result")
            .unwrap()
            .into_int_value();
        builder.build_return(Some(&result)).unwrap();

        (module, function)
    }

    /// Guards against a regression where the pass pipeline silently stops
    /// running: mem2reg must promote the loop slots to SSA registers.
    #[test]
    fn optimization_pipeline_promotes_loop_slots_to_ssa() {
        super::init_llvm();
        let context = inkwell::context::Context::create();
        let (module, function) = build_loop_function(&context);
        let target_machine =
            super::create_target_machine(crate::host_triple(), OptimizationLevel::Aggressive)
                .expect("target machine");

        let before = count_allocas(function);
        assert!(
            before >= 2,
            "loop slots should start in allocas, got {before}"
        );

        run_optimization_passes(&module, &target_machine).expect("pass pipeline must succeed");
        assert!(module.verify().is_ok(), "optimized module must stay valid");

        let after = count_allocas(function);
        assert_eq!(
            after, 0,
            "mem2reg must promote the loop slots to SSA, got {after} allocas"
        );
    }
}
