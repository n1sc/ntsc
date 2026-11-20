# Offline script tests: real archive extraction, mocked downloads and toolchain.
$ErrorActionPreference = "Stop"
$directory = Join-Path ([IO.Path]::GetTempPath()) "ntsc-windows-build-$([Guid]::NewGuid().ToString('N'))"
$originalPath = $env:Path
$originalPrefix = $env:LLVM_SYS_231_PREFIX
$originalOS = $env:OS
# Windows runners expose two tar.exe (System32 and Git); .Source on both is an array.
$tarExecutable = (Get-Command tar -CommandType Application | Select-Object -First 1).Source
$version = "23.1.3"
$archiveRoot = "clang+llvm-$version-x86_64-pc-windows-msvc"
$global:downloadCount = 0
$global:failDownload = $false
$global:failExtraction = $false
$global:failBuild = $false

function Assert-True($Condition, [string]$Message) {
  if (-not $Condition) { throw $Message }
}

function Assert-Throws([scriptblock]$Action, [string]$Pattern) {
  try { & $Action } catch {
    Assert-True ($_.Exception.Message -like $Pattern) "Unexpected error: $_"
    return
  }
  throw "Expected error matching: $Pattern"
}

function Invoke-WebRequest {
  param($Uri, $OutFile, [switch]$UseBasicParsing)
  $global:downloadCount++
  Assert-True ($Uri -like "https://github.com/llvm/llvm-project/releases/download/llvmorg-23.1.3/*") "Unexpected download URL."
  if ($global:failDownload) { throw "fixture download failure" }
  Copy-Item $global:fixtureArchive $OutFile
}

function tar {
  if ($global:failExtraction -and $args[0] -eq "-xf") {
    $global:LASTEXITCODE = 2
    return
  }
  & $tarExecutable @args
  $global:LASTEXITCODE = $LASTEXITCODE
}

function cl.exe {}

function dumpbin {
  $global:LASTEXITCODE = 0
  $name = Split-Path -Leaf $args[1]
  if ($name -in @("ntsc.exe", "ntsc-lsp.exe", "zlib.dll")) {
    "    KERNEL32.dll"
    "    LLVM-C.dll"
  } elseif ($name -eq "LLVM-C.dll") {
    "    zlib.dll"
  } else {
    throw "Unexpected dependency scan: $name"
  }
}

function rustc {
  $global:LASTEXITCODE = 0
  "host: x86_64-pc-windows-msvc"
}

function cargo {
  $global:LASTEXITCODE = 0
  if ($args[0] -eq "metadata") {
    @{ target_directory = $global:fixtureTarget } | ConvertTo-Json -Compress
  } elseif ($args[0] -eq "build") {
    if ($global:failBuild) {
      $global:LASTEXITCODE = 1
      return
    }
    Assert-True ($args -contains "--workspace") "Build must include the runtime and language server."
    Assert-True ($args -contains "x86_64-pc-windows-msvc") "Build must target Windows/MSVC."
    $profile = if ($args -contains "--release") { "release" } else { "debug" }
    $output = Join-Path $global:fixtureTarget "x86_64-pc-windows-msvc/$profile"
    New-Item -ItemType Directory -Path $output -Force | Out-Null
    Set-Content (Join-Path $output "ntsc.exe") "fixture"
    Set-Content (Join-Path $output "ntsc-lsp.exe") "fixture"
  } else {
    throw "Unexpected Cargo command: $args"
  }
}

try {
  $scripts = Join-Path $directory "scripts"
  New-Item -ItemType Directory -Path $scripts -Force | Out-Null
  Copy-Item "$PSScriptRoot/setup-windows-llvm.ps1", "$PSScriptRoot/build-windows.ps1", "$PSScriptRoot/windows-dll-dependencies.ps1" $scripts
  $fixture = Join-Path $directory "fixture"
  foreach ($file in @(
    "bin/LLVM-C.dll", "bin/zlib.dll", "bin/unrelated.dll", "bin/ld.lld.exe",
    "bin/llvm-config.exe", "include/llvm-c/Target.h",
    "lib/LLVM-C.lib", "lib/LLVMCore.lib"
  )) {
    $path = Join-Path $fixture "$archiveRoot/$file"
    New-Item -ItemType Directory -Path (Split-Path -Parent $path) -Force | Out-Null
    Set-Content $path "fixture"
  }
  $global:fixtureArchive = Join-Path $directory "fixture.tar.xz"
  & $tarExecutable -cJf $global:fixtureArchive -C $fixture $archiveRoot
  Assert-True ($LASTEXITCODE -eq 0) "Could not create fixture archive."
  $env:OS = "Windows_NT"
  $env:LLVM_SYS_231_PREFIX = ""
  $setup = Join-Path $scripts "setup-windows-llvm.ps1"
  $cachedPrefix = Join-Path $directory "target/llvm-$version-windows-x86_64"

  $global:failDownload = $true
  Assert-Throws { & $setup } "*fixture download failure*"
  Assert-True (-not (Test-Path $cachedPrefix)) "Failed download must not become a cache entry."
  Assert-True (@(Get-ChildItem (Join-Path $directory "target")).Count -eq 0) "Failed download left staging files."
  $global:failDownload = $false
  $global:failExtraction = $true
  Assert-Throws { & $setup } "*Could not extract*"
  Assert-True (-not (Test-Path $cachedPrefix)) "Failed extraction must not become a cache entry."
  Assert-True (@(Get-ChildItem (Join-Path $directory "target")).Count -eq 0) "Failed extraction left staging files."
  $global:failExtraction = $false

  $prefix = & $setup
  foreach ($required in @("bin/LLVM-C.dll", "bin/zlib.dll", "bin/ld.lld.exe", "lib/LLVM-C.lib")) {
    Assert-True (Test-Path (Join-Path $prefix $required)) "Missing extracted file: $required"
  }
  foreach ($excluded in @("include", "bin/llvm-config.exe", "lib/LLVMCore.lib")) {
    Assert-True (-not (Test-Path (Join-Path $prefix $excluded))) "Unnecessary development file extracted: $excluded"
  }
  $downloads = $global:downloadCount
  $env:LLVM_SYS_231_PREFIX = ""
  $reused = & $setup
  Assert-True ($reused -eq $prefix -and $global:downloadCount -eq $downloads) "Cached builds must not download LLVM again."
  Assert-True ($env:LLVM_SYS_231_PREFIX -eq $prefix) "Setup must export the LLVM prefix."

  $env:LLVM_SYS_231_PREFIX = Join-Path $directory "missing"
  Assert-Throws { & $setup } "*LLVM C API library missing*"
  Assert-True ($global:downloadCount -eq $downloads) "An invalid explicit prefix must not trigger downloads."
  $env:LLVM_SYS_231_PREFIX = $prefix

  $global:fixtureTarget = Join-Path $directory "custom cargo target"
  $build = Join-Path $scripts "build-windows.ps1"
  foreach ($configuration in @("Debug", "Release", "Debug")) {
    & $build -Configuration $configuration
    $output = Join-Path $global:fixtureTarget "x86_64-pc-windows-msvc/$($configuration.ToLowerInvariant())"
    Assert-True (Test-Path (Join-Path $output "LLVM-C.dll")) "LLVM DLL must be adjacent to the compiler."
    Assert-True (Test-Path (Join-Path $output "zlib.dll")) "LLVM dependency DLL must be adjacent to the compiler."
    Assert-True (-not (Test-Path (Join-Path $output "unrelated.dll"))) "Unrelated DLLs must not inflate the build."
  }
  $location = Get-Location
  $global:failBuild = $true
  Assert-Throws { & $build } "*Cargo build failed*"
  Assert-True ((Get-Location).Path -eq $location.Path) "Failed builds must restore the caller's working directory."

  $mingw = Join-Path $directory "rustlib"
  foreach ($name in @("crt2.o", "libmingw32.a", "libgcc.a", "libmingwex.a", "libmsvcrt.a")) {
    $path = Join-Path $mingw "self-contained/$name"
    New-Item -ItemType Directory -Path (Split-Path -Parent $path) -Force | Out-Null
    Set-Content $path "self-contained fixture"
  }
  Set-Content (Join-Path $mingw "libkernel32.a") "system import fixture"
  Set-Content (Join-Path $mingw "libgcc.a") "root duplicate"
  Set-Content (Join-Path $mingw "unrelated.a") "unrelated fixture"
  $stage = Join-Path $PSScriptRoot "../.github/packaging/windows/stage-mingw.ps1"
  $destination = Join-Path $directory "mingw bundle"
  $files = @(& $stage -LibraryDirectory $mingw -Destination $destination)
  Assert-True ($files.Count -eq 6) "Bundle must contain only CRT and allowlisted imports."
  Assert-True ((Get-Content (Join-Path $destination "libgcc.a")) -eq "self-contained fixture") "Prefer Rust's self-contained CRT over root duplicates."
  Assert-True (-not (Test-Path (Join-Path $destination "unrelated.a"))) "Do not package unrelated archives."
  Assert-Throws {
    & $stage -LibraryDirectory (Join-Path $directory "missing CRT") -Destination (Join-Path $directory "bad bundle")
  } "*Required MinGW CRT file missing*"
  Write-Host "Windows build script tests passed."
} finally {
  $env:Path = $originalPath
  $env:LLVM_SYS_231_PREFIX = $originalPrefix
  $env:OS = $originalOS
  Remove-Item -LiteralPath $directory -Recurse -Force
}

# The failure-path assertions leave $LASTEXITCODE set; pwsh -Command would
# otherwise report that as this script's exit code.
exit 0
