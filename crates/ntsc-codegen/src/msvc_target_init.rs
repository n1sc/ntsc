//! Link-time definitions for llvm-sys's target-initialization shims.
//!
//! llvm-sys builds these wrappers from the LLVM headers, which the MSVC
//! installation deliberately omits, but inkwell references them from
//! `Target::initialize_all` and `Target::initialize_native` regardless. The
//! compiler only emits x86 and AArch64 code, so the same entry points are
//! defined here for those two backends.

use llvm_sys::prelude::LLVMBool;
use llvm_sys::target;

#[unsafe(no_mangle)]
pub extern "C" fn LLVM_InitializeAllTargetInfos() {
    unsafe {
        target::LLVMInitializeX86TargetInfo();
        target::LLVMInitializeAArch64TargetInfo();
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn LLVM_InitializeAllTargets() {
    unsafe {
        target::LLVMInitializeX86Target();
        target::LLVMInitializeAArch64Target();
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn LLVM_InitializeAllTargetMCs() {
    unsafe {
        target::LLVMInitializeX86TargetMC();
        target::LLVMInitializeAArch64TargetMC();
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn LLVM_InitializeAllAsmPrinters() {
    unsafe {
        target::LLVMInitializeX86AsmPrinter();
        target::LLVMInitializeAArch64AsmPrinter();
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn LLVM_InitializeAllAsmParsers() {
    unsafe {
        target::LLVMInitializeX86AsmParser();
        target::LLVMInitializeAArch64AsmParser();
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn LLVM_InitializeAllDisassemblers() {
    unsafe {
        target::LLVMInitializeX86Disassembler();
        target::LLVMInitializeAArch64Disassembler();
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn LLVM_InitializeNativeTarget() -> LLVMBool {
    LLVM_InitializeAllTargets();
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn LLVM_InitializeNativeAsmPrinter() -> LLVMBool {
    LLVM_InitializeAllAsmPrinters();
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn LLVM_InitializeNativeAsmParser() -> LLVMBool {
    LLVM_InitializeAllAsmParsers();
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn LLVM_InitializeNativeDisassembler() -> LLVMBool {
    LLVM_InitializeAllDisassemblers();
    0
}
