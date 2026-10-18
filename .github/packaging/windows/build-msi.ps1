# Builds the ntsc MSI installer with the WiX toolset.
#
# Usage (from the repository root, with the MSVC environment active and the
# wix CLI on PATH):
#
#   pwsh .github/packaging/windows/build-msi.ps1 -Version 26.0.0b -MsiVersion 26.0.0
#
# Steps:
#   1. Compute the DLL closure of ntsc.exe with dumpbin /DEPENDENTS.
#   2. Copy ntsc.exe and its non-system DLLs (from the LLVM install dir) into
#      a staging directory.
#   3. Stage the bundled Windows linker: ld.lld.exe (from the LLVM bin dir),
#      the GNU-flavoured runtime (libntsc_runtime.a) and the MinGW import
#      libraries. Together they let `ntsc build` link without MSVC or gcc.
#   4. Generate the license RTF (from LICENSE), dlls.wxs (one component per
#      DLL) and tools.wxs (the bundled linker files).
#   5. Build the MSI with `wix build -ext WixToolset.UI.wixext`.
#
# The MSI bundles the LLVM DLLs, so no separate LLVM install is needed. The
# installer offers one optional feature, "Add ntsc to PATH", checked by
# default and toggleable from the feature-selection dialog.
param(
  [Parameter(Mandatory = $true)]
  [string]$Version,

  [Parameter(Mandatory = $true)]
  [string]$MsiVersion,

  [string]$SourceDir = "target\release",
  [string]$LlvmBin = "$env:ProgramFiles\LLVM\bin",
  [string]$UpgradeCode = "3B4A7F2E-1C9D-4E5A-8F6B-2A0C1D3E5F7B",

  # The GNU-flavoured runtime archive. Built for x86_64-pc-windows-gnu; the
  # MSVC runtime ntsc.exe links against cannot be consumed by ld.lld in MinGW
  # mode.
  [string]$GnuRuntimeLib = "target\x86_64-pc-windows-gnu\release\libntsc_runtime.a",
  [string]$PackageManager = "crates\ntsc-pkg\build\release\ntsc-pkg.exe",
  [string]$LanguageServer = "target\release\ntsc-lsp.exe",

  # Where the MinGW import libraries live. Defaults to the rustc toolchain's
  # x86_64-pc-windows-gnu lib dir (created by `rustup target add
  # x86_64-pc-windows-gnu`). A bundled subset is used first by the linker and
  # the full set is installed alongside.
  [string]$MingwLibDir = ""
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path (Join-Path $SourceDir "ntsc.exe"))) {
  throw "ntsc.exe not found in $SourceDir - build the release binary first"
}
if (-not (Test-Path $PackageManager)) {
  throw "ntsc-pkg.exe not found at $PackageManager - build the package manager first"
}
if (-not (Test-Path $LanguageServer)) {
  throw "ntsc-lsp.exe not found at $LanguageServer - build the language server first"
}
if (-not (Get-Command dumpbin -ErrorAction SilentlyContinue)) {
  throw "dumpbin not found - run inside the MSVC developer environment"
}
if (-not (Get-Command wix -ErrorAction SilentlyContinue)) {
  throw "wix not found - install it with: dotnet tool install --global wix"
}

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$Dist = Join-Path (Get-Location) "dist\win"
if (Test-Path $Dist) { Remove-Item -Recurse -Force $Dist }
New-Item -ItemType Directory -Path $Dist | Out-Null

# ── License RTF ──────────────────────────────────────────────────────────────
# The WixUI_FeatureTree dialog set shows a license-agreement dialog, which
# needs an RTF copy of the LICENSE file. Convert it (escaping backslashes and
# braces, translating newlines to paragraph breaks).
$LicensePath = Join-Path (Get-Location) "LICENSE"
if (-not (Test-Path $LicensePath)) {
  throw "LICENSE not found next to the script - run from the repository root"
}
$licenseText = Get-Content $LicensePath -Raw
$escaped = $licenseText.Replace("\", "\\").Replace("{", "\{").Replace("}", "\}")
$escaped = $escaped -replace "`r?`n", "\par " -replace "`t", "\tab "
$rtf = "{\rtf1\ansi\deff0{\fonttbl{\f0\fnil Courier New;}}\f0\fs18 " + $escaped + "}"
[System.IO.File]::WriteAllText((Join-Path $Dist "license.rtf"), $rtf)

# ── DLL closure ─────────────────────────────────────────────────────────────
$system = @(
  "KERNEL32.dll", "USER32.dll", "ADVAPI32.dll", "SHELL32.dll", "WS2_32.dll",
  "NTDLL.dll", "GDI32.dll", "OLE32.dll", "RPCRT4.dll", "SHLWAPI.dll",
  "COMDLG32.dll", "OLEAUT32.dll", "IMM32.dll", "MSVCRT.dll", "VERSION.dll",
  "WINMM.dll", "bcrypt.dll", "CRYPT32.dll", "WTSAPI32.dll", "SETUPAPI.dll",
  "HID.DLL", "uxtheme.dll", "DWMAPI.dll", "NETAPI32.dll", "UCRTBASE.dll"
) | ForEach-Object { $_.ToLower() }

$visited = @{}
$deps = @()

function Get-Deps([string]$Path) {
  $output = & dumpbin /DEPENDENTS $Path 2>$null
  foreach ($line in $output) {
    $name = $line.Trim()
    if ($name -notmatch "\.dll$" -or $visited.ContainsKey($name.ToLower())) { continue }
    if ($system -contains $name.ToLower()) { continue }
    $visited[$name.ToLower()] = $true
    $deps += $name
    $full = Join-Path $LlvmBin $name
    if (Test-Path $full) { Get-Deps $full }
  }
}

Get-Deps (Join-Path $SourceDir "ntsc.exe")
Get-Deps $PackageManager

Write-Host "Bundling $($deps.Count) DLL(s):"
$deps | ForEach-Object { Write-Host "  $_" }

# ── Stage files ─────────────────────────────────────────────────────────────
Copy-Item (Join-Path $SourceDir "ntsc.exe") $Dist
Copy-Item $PackageManager (Join-Path $Dist "ntsc-pkg.exe")
Copy-Item $LanguageServer (Join-Path $Dist "ntsc-lsp.exe")
foreach ($name in $deps) {
  $full = Join-Path $LlvmBin $name
  if (Test-Path $full) {
    Copy-Item $full (Join-Path $Dist $name)
  } else {
    Write-Warning "DLL $name not found in $LlvmBin - not bundled"
  }
}

# ── Stage the bundled linker and runtime ────────────────────────────────────
# ld.lld links NTSC programs from the MinGW emulation without MSVC or gcc.
$lld = Join-Path $LlvmBin "ld.lld.exe"
if (-not (Test-Path $lld)) {
  throw "ld.lld.exe not found in $LlvmBin"
}
Copy-Item $lld $Dist

if (-not (Test-Path $GnuRuntimeLib)) {
  throw "GNU runtime not found at $GnuRuntimeLib - build it with: cargo build -p ntsc-runtime --target x86_64-pc-windows-gnu --release"
}
Copy-Item $GnuRuntimeLib $Dist

if (-not $MingwLibDir) {
  $sysroot = (& rustc --print sysroot) -replace "`r?`n", ""
  $MingwLibDir = Join-Path $sysroot "lib\rustlib\x86_64-pc-windows-gnu\lib"
}
if (-not (Test-Path $MingwLibDir)) {
  throw "MinGW import libraries not found at $MingwLibDir - run: rustup target add x86_64-pc-windows-gnu"
}
$MingwStage = Join-Path $Dist "mingw"
New-Item -ItemType Directory -Path $MingwStage | Out-Null
$mingwFiles = Get-ChildItem $MingwLibDir -File |
  Where-Object { $_.Extension -in ".a", ".o" }
$mingwFiles | Copy-Item -Destination $MingwStage
Write-Host "Bundling $($mingwFiles.Count) MinGW import library file(s)"

# ── Generate dlls.wxs ────────────────────────────────────────────────────────
$lines = @(
  '<?xml version="1.0" encoding="UTF-8"?>',
  '<Wix xmlns="http://wixtoolset.org/schemas/v4/wxs">',
  '  <Fragment>',
  '    <ComponentGroup Id="Libraries" Directory="INSTALLBIN">'
)
foreach ($name in $deps) {
  $id = "lib." + ([System.IO.Path]::GetFileNameWithoutExtension($name))
  $lines += "      <Component Id=`"$id`" Guid=`"*`">"
  $lines += "        <File Id=`"$id.file`" Source=`"`$(var.SourceDir)\$name`" KeyPath=`"yes`" />"
  $lines += "      </Component>"
}
$lines += @(
  '    </ComponentGroup>',
  '  </Fragment>',
  '</Wix>'
)
$lines | Set-Content -Path (Join-Path $ScriptDir "dlls.wxs") -Encoding UTF8

# ── Generate tools.wxs ───────────────────────────────────────────────────────
# The bundled binaries and linker files go next to ntsc.exe (INSTALLBIN); the
# MinGW import libraries go into a mingw subdirectory (INSTALLMINGW).
$lines = @(
  '<?xml version="1.0" encoding="UTF-8"?>',
  '<Wix xmlns="http://wixtoolset.org/schemas/v4/wxs">',
  '  <Fragment>',
  '    <ComponentGroup Id="Tools" Directory="INSTALLBIN">'
)
foreach ($name in @("ntsc-lsp.exe", "ld.lld.exe", "libntsc_runtime.a")) {
  $id = "tool." + [System.IO.Path]::GetFileNameWithoutExtension($name)
  $lines += "      <Component Id=`"$id`" Guid=`"*`">"
  $lines += "        <File Id=`"$id.file`" Source=`"`$(var.SourceDir)\$name`" KeyPath=`"yes`" />"
  $lines += "      </Component>"
}
$lines += @(
  '    </ComponentGroup>',
  '    <ComponentGroup Id="MingwLibs" Directory="INSTALLMINGW">'
)
foreach ($file in $mingwFiles) {
  $id = "mingw." + ($file.Name -replace "[^A-Za-z0-9_.]", ".")
  $lines += "      <Component Id=`"$id`" Guid=`"*`">"
  $lines += "        <File Id=`"$id.file`" Source=`"`$(var.SourceDir)\mingw\$($file.Name)`" KeyPath=`"yes`" />"
  $lines += "      </Component>"
}
$lines += @(
  '    </ComponentGroup>',
  '  </Fragment>',
  '</Wix>'
)
$lines | Set-Content -Path (Join-Path $ScriptDir "tools.wxs") -Encoding UTF8

# ── Build the MSI ────────────────────────────────────────────────────────────
& wix build `
  (Join-Path $ScriptDir "ntsc.wxs") `
  (Join-Path $ScriptDir "dlls.wxs") `
  (Join-Path $ScriptDir "tools.wxs") `
  -arch x64 `
  -ext WixToolset.UI.wixext `
  -d "SourceDir=$Dist" `
  -d "MsiVersion=$MsiVersion" `
  -d "UpgradeCode=$UpgradeCode" `
  "-o" (Join-Path (Get-Location) "dist\ntsc-$Version.msi")
if ($LASTEXITCODE -ne 0) { throw "wix build failed" }

Write-Host "Built dist\ntsc-$Version.msi"
