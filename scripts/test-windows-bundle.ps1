# Exercise the normal-user package with no Rust, MSVC, MinGW, or LLVM on PATH.
param(
  [string]$BundleDirectory = "dist/win"
)

$ErrorActionPreference = "Stop"
$bundle = (Resolve-Path $BundleDirectory).Path
$compiler = Join-Path $bundle "ntsc.exe"
$fixture = Join-Path $PSScriptRoot "fixtures/windows-bundle.nt"
$directory = Join-Path ([IO.Path]::GetTempPath()) "ntsc-user-smoke-$([Guid]::NewGuid().ToString('N'))"
$originalPath = $env:Path
$originalPrefix = $env:LLVM_SYS_231_PREFIX
$originalLib = $env:LIB
New-Item -ItemType Directory -Path $directory | Out-Null
Push-Location $directory
try {
  $env:Path = "$env:SystemRoot\System32"
  $env:LLVM_SYS_231_PREFIX = ""
  $env:LIB = ""
  foreach ($name in @("ntsc.exe", "ld.lld.exe")) {
    & (Join-Path $bundle $name) --version
    if ($LASTEXITCODE -ne 0) { throw "Bundled $name cannot run without developer tools." }
  }
  & (Join-Path $bundle "ntsc-pkg.exe") --help
  if ($LASTEXITCODE -ne 0) { throw "Bundled package manager cannot run without developer tools." }
  if (-not (Test-Path (Join-Path $bundle "ntsc-lsp.exe") -PathType Leaf)) {
    throw "Bundled language server is missing."
  }
  & $compiler init hello
  if ($LASTEXITCODE -ne 0) { throw "Bundled compiler cannot scaffold a project." }
  Push-Location hello
  try {
    $sizes = @()
    foreach ($profile in @("debug", "release")) {
      $arguments = @("run")
      if ($profile -eq "release") { $arguments += "--release" }
      $output = & $compiler @arguments 2>&1
      if ($LASTEXITCODE -ne 0) { throw "Standalone $profile build/run failed: $output" }
      if (($output | Out-String) -notmatch "Hello, World!") { throw "Unexpected hello output: $output" }
      if (($output | Out-String) -match "Building runtime") { throw "Package must reuse its bundled runtime." }
      $binary = Get-Item "build/$profile/hello.exe"
      $bytes = [IO.File]::ReadAllBytes($binary.FullName)
      $peOffset = [BitConverter]::ToInt32($bytes, 0x3c)
      if ([BitConverter]::ToUInt32($bytes, $peOffset + 16) -ne 0) {
        throw "$profile binary contains a COFF symbol table despite stripping."
      }
      $sizes += "hello ($profile): $($binary.Length) bytes"
    }
    Copy-Item $fixture "src/main.nt" -Force
    $output = & $compiler run --release 2>&1
    if ($LASTEXITCODE -ne 0) { throw "Standalone standard-library build/run failed: $output" }
    $text = $output | Out-String
    foreach ($expected in @(
      "bundle-worker: 42",
      "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
      "bundle-archive-ok", "bundle-http-ok"
    )) {
      if (-not $text.Contains($expected)) { throw "Missing expected output: $expected`n$text" }
    }
    if ($text -match "Building runtime") { throw "Standard library must not require Cargo." }
    $sizes += "stdlib (release): $((Get-Item 'build/release/hello.exe').Length) bytes"
    $bundleBytes = (Get-ChildItem $bundle -Recurse -File | Measure-Object Length -Sum).Sum
    $sizes += "installed bundle: $bundleBytes bytes"
    $sizes | ForEach-Object { Write-Host $_ }
    if ($env:GITHUB_STEP_SUMMARY) {
      @("### Windows standalone binary sizes") + $sizes | Add-Content $env:GITHUB_STEP_SUMMARY
    }
  } finally {
    Pop-Location
  }
} finally {
  Pop-Location
  $env:Path = $originalPath
  $env:LLVM_SYS_231_PREFIX = $originalPrefix
  $env:LIB = $originalLib
  Remove-Item -LiteralPath $directory -Recurse -Force
}
