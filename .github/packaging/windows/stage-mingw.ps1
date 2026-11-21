# Stage only libraries the bundled linker uses, including Rust's self-contained CRT.
param(
  [Parameter(Mandatory = $true)]
  [string]$LibraryDirectory,
  [Parameter(Mandatory = $true)]
  [string]$Destination
)

$ErrorActionPreference = "Stop"
$manifest = Join-Path $PSScriptRoot "../../../crates/ntsc-codegen/windows-link-libraries.txt"
if (-not (Test-Path $manifest -PathType Leaf)) {
  $manifest = Join-Path $PSScriptRoot "windows-link-libraries.txt"
}
$names = @("crt2.o") + @(Get-Content $manifest | ForEach-Object { "lib$_.a" })
$searchDirectories = @((Join-Path $LibraryDirectory "self-contained"), $LibraryDirectory)
New-Item -ItemType Directory -Path $Destination -Force | Out-Null
foreach ($name in $names) {
  $source = $null
  foreach ($directory in $searchDirectories) {
    $candidate = Join-Path $directory $name
    if (Test-Path $candidate -PathType Leaf) {
      $source = $candidate
      break
    }
  }
  if ($source) {
    Copy-Item $source $Destination
  } elseif ($name -in @("crt2.o", "libmingw32.a", "libgcc.a", "libmingwex.a", "libmsvcrt.a")) {
    throw "Required MinGW CRT file missing: $name (searched $LibraryDirectory and self-contained)."
  }
}
Get-ChildItem $Destination -File | Sort-Object Name
