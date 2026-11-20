# Prepare the minimal LLVM C API installation used by Windows/MSVC builds.
param(
  [ValidatePattern('^23\.\d+\.\d+$')]
  [string]$Version = "23.1.3"
)

$ErrorActionPreference = "Stop"
if ($env:OS -ne "Windows_NT") {
  throw "This script prepares LLVM for x86-64 Windows/MSVC."
}

$root = Split-Path -Parent $PSScriptRoot
$prefix = $env:LLVM_SYS_231_PREFIX
if (-not $prefix) {
  # Outside target/: cargo clean empties it, and CI cache restores pre-clean
  # anything in it that is not a Cargo artifact.
  $prefix = Join-Path $root ".llvm\llvm-$Version-windows-x86_64"
  if (-not (Test-Path $prefix)) {
    $staging = Join-Path $root "target\llvm-download-$([Guid]::NewGuid().ToString('N'))"
    New-Item -ItemType Directory -Path $staging -Force | Out-Null
    try {
      $archiveRoot = "clang+llvm-$Version-x86_64-pc-windows-msvc"
      $archive = Join-Path $staging "llvm.tar.xz"
      Write-Host "Downloading LLVM $Version (cached after the first build)..."
      Invoke-WebRequest -UseBasicParsing -Uri "https://github.com/llvm/llvm-project/releases/download/llvmorg-$Version/$archiveRoot.tar.xz" -OutFile $archive
      $entries = & tar -tf $archive
      if ($LASTEXITCODE -ne 0) { throw "Could not list the LLVM archive." }
      $selected = @($entries | Where-Object {
        $_ -like "$archiveRoot/bin/*.dll" -or
        $_ -eq "$archiveRoot/bin/ld.lld.exe" -or
        $_ -eq "$archiveRoot/lib/LLVM-C.lib"
      })
      foreach ($required in @("bin/LLVM-C.dll", "bin/ld.lld.exe", "lib/LLVM-C.lib")) {
        if ($selected -notcontains "$archiveRoot/$required") {
          throw "LLVM archive is missing $required."
        }
      }
      $extracted = Join-Path $staging "extracted"
      New-Item -ItemType Directory -Path $extracted | Out-Null
      & tar -xf $archive -C $extracted --strip-components 1 @selected
      if ($LASTEXITCODE -ne 0) { throw "Could not extract the LLVM libraries." }
      New-Item -ItemType Directory -Path (Split-Path -Parent $prefix) -Force | Out-Null
      Move-Item -Path $extracted -Destination $prefix
    } finally {
      Remove-Item -LiteralPath $staging -Recurse -Force
    }
  }
}

$prefix = [IO.Path]::GetFullPath($prefix)
foreach ($required in @("lib\LLVM-C.lib", "bin\LLVM-C.dll")) {
  if (-not (Test-Path (Join-Path $prefix $required) -PathType Leaf)) {
    throw "LLVM C API library missing: $prefix\$required. Set LLVM_SYS_231_PREFIX to a valid LLVM 23 installation."
  }
}
$env:LLVM_SYS_231_PREFIX = $prefix
$env:Path = "$(Join-Path $prefix 'bin');$env:Path"
Write-Output $prefix
