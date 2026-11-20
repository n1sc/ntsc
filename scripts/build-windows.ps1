# Run from an x64 Visual Studio Developer PowerShell with stable Rust installed.
param(
  [ValidateSet("Debug", "Release")]
  [string]$Configuration = "Release"
)

$ErrorActionPreference = "Stop"
if (-not (Get-Command cl.exe -ErrorAction SilentlyContinue)) {
  throw "Open x64 Developer PowerShell for Visual Studio (C++ Build Tools and Windows SDK required), then run this script again."
}

Push-Location (Split-Path -Parent $PSScriptRoot)
try {
  $hostInfo = & rustc -vV
  if ($LASTEXITCODE -ne 0) { throw "Install stable Rust from https://rustup.rs first." }
  if ($hostInfo -notcontains "host: x86_64-pc-windows-msvc") {
    throw "Use the stable x86_64-pc-windows-msvc Rust toolchain."
  }
  $llvm = & "$PSScriptRoot/setup-windows-llvm.ps1"
  $metadata = & cargo metadata --no-deps --format-version 1
  if ($LASTEXITCODE -ne 0) { throw "Could not determine Cargo's output directory." }
  $targetDirectory = ($metadata | ConvertFrom-Json).target_directory
  $arguments = @("build", "--workspace", "--target", "x86_64-pc-windows-msvc")
  if ($Configuration -eq "Release") { $arguments += "--release" }
  & cargo @arguments
  if ($LASTEXITCODE -ne 0) { throw "Cargo build failed." }

  $output = Join-Path $targetDirectory "x86_64-pc-windows-msvc\$($Configuration.ToLowerInvariant())"
  $binaries = @((Join-Path $output "ntsc.exe"), (Join-Path $output "ntsc-lsp.exe"))
  $dependencies = & "$PSScriptRoot/windows-dll-dependencies.ps1" -Binaries $binaries -LlvmBin (Join-Path $llvm "bin")
  foreach ($dependency in $dependencies) {
    $destination = Join-Path $output $dependency.Name
    if ($dependency.FullName -ne $destination) {
      Copy-Item $dependency.FullName $destination
    }
  }
  Write-Host "Built $output\ntsc.exe (LLVM DLLs included)."
} finally {
  Pop-Location
}
