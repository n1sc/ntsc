use std::env;
use std::fs;
use std::io;
use std::path::PathBuf;

fn main() -> io::Result<()> {
    println!("cargo:rerun-if-env-changed=LLVM_SYS_231_PREFIX");
    let target_environment = env::var("CARGO_CFG_TARGET_ENV")
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    if target_environment != "msvc" {
        return Ok(());
    }

    let prefix = env::var_os("LLVM_SYS_231_PREFIX").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "LLVM_SYS_231_PREFIX must point to the LLVM development archive on MSVC",
        )
    })?;
    let prefix = PathBuf::from(prefix);
    let library_directory = prefix.join("lib");
    let import_library = library_directory.join("LLVM-C.lib");
    let runtime_library = prefix.join("bin").join("LLVM-C.dll");
    for path in [&import_library, &runtime_library] {
        let metadata = fs::metadata(path).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("LLVM C API library {}: {error}", path.display()),
            )
        })?;
        if !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("LLVM C API library is not a file: {}", path.display()),
            ));
        }
        println!("cargo:rerun-if-changed={}", path.display());
    }

    println!(
        "cargo:rustc-link-search=native={}",
        library_directory.display()
    );
    println!("cargo:rustc-link-lib=dylib=LLVM-C");
    Ok(())
}
