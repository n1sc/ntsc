import json
import shutil
import subprocess
import tempfile
import unittest
import xml.etree.ElementTree as ET
import zipfile
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
PACKAGING = ROOT / ".github/packaging"


class ExtensionPackagingTests(unittest.TestCase):
    def test_packages_into_a_missing_parent_directory(self):
        version = json.loads((ROOT / "extensions/shared/release.json").read_text())["version"]
        with tempfile.TemporaryDirectory(prefix="ntsc extension packages ") as directory:
            fixture = Path(directory)
            vsix = fixture / "neutron.vsix"
            with zipfile.ZipFile(vsix, "w") as archive:
                archive.writestr("extension/package.json", json.dumps({"version": version}))
                archive.writestr("extension/syntaxes/neutron.tmLanguage.json", "{}")
            wasm = fixture / "neutron.wasm"
            wasm.write_bytes(b"\0asm\x01\0\0\0")
            output = fixture / "missing parent/dist/extensions"
            result = subprocess.run(
                [
                    "sh", "scripts/package-extensions.sh",
                    "--version", version, "--out", str(output),
                    "--vsix", str(vsix), "--wasm", str(wasm),
                ],
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=False,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            manifest = json.loads((output / "extensions-manifest.json").read_text())
            self.assertEqual(manifest["version"], version)
            self.assertEqual(len(manifest["artifacts"]), 10)
            subprocess.run(
                ["sha256sum", "-c", "SHA256SUMS"], cwd=output,
                check=True, capture_output=True,
            )


class WindowsPackagingTests(unittest.TestCase):
    def test_template_uses_legal_wix_identifiers(self):
        tree = ET.parse(PACKAGING / "windows/ntsc.wxs")
        for element in tree.iter():
            if "Id" in element.attrib:
                self.assertRegex(element.attrib["Id"], r"^[A-Za-z_][A-Za-z0-9_.]*$")
        sources = [element.attrib.get("Source", "") for element in tree.iter()]
        self.assertIn(r"$(var.SourceDir)\ntsc-pkg.exe", sources)

    @unittest.skipUnless(shutil.which("pwsh"), "PowerShell is not installed")
    def test_generated_ids_and_transitive_dll_staging(self):
        with tempfile.TemporaryDirectory(prefix="ntsc windows packages ") as directory:
            fixture = Path(directory)
            scripts = fixture / "scripts"
            scripts.mkdir()
            for name in ("build-msi.ps1", "ntsc.wxs"):
                shutil.copyfile(PACKAGING / "windows" / name, scripts / name)
            shutil.copyfile(ROOT / "LICENSE", fixture / "LICENSE")
            source = fixture / "source"
            llvm = fixture / "llvm"
            mingw = fixture / "mingw"
            for path in (source, llvm, mingw):
                path.mkdir()
            for name in ("ntsc.exe", "ntsc-pkg.exe", "ntsc-lsp.exe", "libntsc_runtime.a"):
                (source / name).write_bytes(b"fixture")
            for name in ("LLVM-C.dll", "support-with-hyphen.dll", "ld.lld.exe"):
                (llvm / name).write_bytes(b"fixture")
            (mingw / "libkernel32.a").write_bytes(b"fixture")
            harness = fixture / "test.ps1"
            harness.write_text(r'''
$ErrorActionPreference = "Stop"
function dumpbin {
  param($Option, $Binary)
  if ([System.IO.Path]::GetFileName($Binary) -eq "LLVM-C.dll") {
    "support-with-hyphen.dll"
  } elseif ([System.IO.Path]::GetExtension($Binary) -eq ".exe") {
    "LLVM-C.dll"
  }
  "KERNEL32.dll"
}
function wix {
  foreach ($path in $args | Where-Object { $_ -like "*.wxs" }) {
    [xml]$document = Get-Content $path -Raw
    foreach ($element in $document.SelectNodes("//*[@Id]")) {
      if ($element.Id -notmatch "^[A-Za-z_][A-Za-z0-9_.]*$") {
        throw "invalid WiX identifier: $($element.Id)"
      }
    }
  }
  $global:LASTEXITCODE = 0
}
& ./scripts/build-msi.ps1 -Version 0.1.0 -MsiVersion 0.1.0 `
  -SourceDir ./source -LlvmBin ./llvm -MingwLibDir ./mingw `
  -GnuRuntimeLib ./source/libntsc_runtime.a `
  -PackageManager ./source/ntsc-pkg.exe -LanguageServer ./source/ntsc-lsp.exe
''')
            result = subprocess.run(
                ["pwsh", "-NoProfile", "-File", str(harness)],
                cwd=fixture, text=True, capture_output=True, check=False,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            for name in ("LLVM-C.dll", "support-with-hyphen.dll", "ntsc-lsp.exe"):
                self.assertTrue((fixture / "dist/win" / name).is_file(), name)
            for name in ("dlls.wxs", "tools.wxs"):
                tree = ET.parse(scripts / name)
                for element in tree.iter():
                    if "Id" in element.attrib:
                        self.assertRegex(element.attrib["Id"], r"^[A-Za-z_][A-Za-z0-9_.]*$")


class IntelMacPackagingTests(unittest.TestCase):
    def test_builds_and_caches_a_shared_llvm_distribution(self):
        workflow = (ROOT / ".github/workflows/release.yml").read_text()
        restore = workflow.split("      - name: Restore LLVM 23 (macOS Intel)\n", 1)[1]
        self.assertIn("if: matrix.name == 'macos-x86_64'", restore)
        self.assertIn("uses: actions/cache/restore@v4", restore)
        self.assertIn("uses: actions/cache/save@v4", restore)
        self.assertIn("bash .github/packaging/macos/build-llvm.sh", restore)
        self.assertNotIn("brew install", restore.split("      # The win64", 1)[0])
        script = (PACKAGING / "macos/build-llvm.sh").read_text()
        self.assertIn("-DLLVM_BUILD_LLVM_DYLIB=ON", script)
        self.assertIn("-DLLVM_LINK_LLVM_DYLIB=ON", script)
        self.assertIn("--target install-distribution", script)
        self.assertNotIn("LLVM_TARGETS_TO_BUILD=Native", script)
        self.assertIn("llvm-project-${VERSION}.src.tar.xz", script)


if __name__ == "__main__":
    unittest.main()
