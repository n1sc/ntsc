# Resolve the non-system DLL closure without bundling unrelated LLVM tools.
param(
  [Parameter(Mandatory = $true)]
  [string[]]$Binaries,
  [Parameter(Mandatory = $true)]
  [string]$LlvmBin
)

$ErrorActionPreference = "Stop"
$system = @(
  "KERNEL32.dll", "USER32.dll", "ADVAPI32.dll", "SHELL32.dll", "WS2_32.dll",
  "NTDLL.dll", "GDI32.dll", "OLE32.dll", "RPCRT4.dll", "SHLWAPI.dll",
  "COMDLG32.dll", "OLEAUT32.dll", "IMM32.dll", "MSVCRT.dll", "VERSION.dll",
  "WINMM.dll", "bcrypt.dll", "bcryptprimitives.dll", "CRYPT32.dll", "WTSAPI32.dll", "SETUPAPI.dll",
  "HID.DLL", "uxtheme.dll", "DWMAPI.dll", "NETAPI32.dll", "UCRTBASE.dll"
)
$dependencies = [System.Collections.Generic.SortedDictionary[string, string]]::new(
  [System.StringComparer]::OrdinalIgnoreCase
)

function Resolve-Dll([string]$Name, [string]$BinaryPath) {
  foreach ($directory in @((Split-Path -Parent $BinaryPath), $LlvmBin)) {
    $candidate = Join-Path $directory $Name
    if (Test-Path $candidate -PathType Leaf) {
      return (Get-Item $candidate).FullName
    }
  }
  if ($env:VCToolsRedistDir) {
    $redist = Join-Path $env:VCToolsRedistDir "x64"
    if (Test-Path $redist -PathType Container) {
      $directories = Get-ChildItem $redist -Directory |
        Where-Object { $_.Name -like "Microsoft.VC*.CRT" } |
        Sort-Object Name
      foreach ($directory in $directories) {
        $candidate = Join-Path $directory.FullName $Name
        if (Test-Path $candidate -PathType Leaf) {
          return (Get-Item $candidate).FullName
        }
      }
    }
  }
  throw "required DLL $Name for $BinaryPath was not found beside the binary, in $LlvmBin, or in the x64 MSVC redistributables"
}

function Collect-Dependencies([string]$BinaryPath) {
  $output = & dumpbin /DEPENDENTS $BinaryPath 2>$null
  if ($LASTEXITCODE -ne 0) {
    throw "dumpbin failed for $BinaryPath with exit code $LASTEXITCODE"
  }
  foreach ($line in $output) {
    $name = $line.Trim()
    if ($name -notmatch "^[A-Za-z0-9_.-]+\.dll$" -or $dependencies.ContainsKey($name)) { continue }
    if ($system -contains $name -or $name -match "^(api-ms-|ext-ms-)") { continue }
    $full = Resolve-Dll $name $BinaryPath
    $dependencies.Add($name, $full)
    Collect-Dependencies $full
  }
}

foreach ($binary in $Binaries) { Collect-Dependencies $binary }
foreach ($path in $dependencies.Values) { Get-Item $path }
