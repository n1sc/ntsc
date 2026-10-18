# Packaging

This directory contains the recipes that produce distributable installers for
the `ntsc` compiler. CI (`.github/workflows/release.yml`) builds the release
binary on each platform and invokes these scripts whenever a `v*` tag is
pushed, then attaches the artifacts to the GitHub Release.

These recipes are versioned with the rest of the repository. The release job
checks out the tree on a fresh runner and runs them from there, so a change
here only takes effect once it is merged.

The compiler links LLVM 22 **dynamically**. The native Linux packages built
from source (`.deb`, `.rpm`, and the AUR `PKGBUILD`) declare a runtime
dependency on the system LLVM; the macOS `.dmg` and the Windows `.msi` ship the
LLVM libraries alongside the binary. The portable `.tar.gz` archives (one per
platform and CPU arch) and the prebuilt Arch package (`.pkg.tar.zst`) go further
and bundle the LLVM libraries **and** the ntsc runtime, so they run with no LLVM
install and no toolchain.

Every package — native or tarball — ships `libntsc_runtime.a`, the static
archive `ntsc build` links each NTSC program against. Beside `ntsc` itself,
each package installs `ntsc-pkg` (the package manager) and `ntsc-lsp` (the
language server editors launch for `*.nt` files), so both are on `PATH`
without a separate install. An installed `ntsc`
locates it relative to its own executable (beside the binary on Windows and in
the macOS bundle; in the sibling `lib/ntsc` or, on Fedora, `lib64/ntsc`
directory on Unix prefixes), so no workspace checkout or `cargo` is needed at
build time.

## Layout

```
packaging/
├── ntsc.1                   roff man page installed by every package
├── README.md
├── linux/
│   ├── build-deb-rpm.sh     builds the .deb and .rpm from target/release
│   ├── make-tarball.sh      self-contained .tar.gz (ldd closure + patchelf)
│   ├── make-arch-pkg.sh     prebuilt .pkg.tar.zst (wraps the tarball; makepkg)
│   ├── debian/control       Debian/Ubuntu package metadata (@VERSION@)
│   ├── fedora/ntsc.spec     Fedora/RHEL spec file (@VERSION@)
│   ├── arch/PKGBUILD        Arch/AUR source recipe (builds from source)
│   └── arch/PKGBUILD-bin    Arch/AUR prebuilt recipe (installs the tarball)
├── homebrew/
│   └── ntsc.rb              Homebrew formula template (tap: n1sc/homebrew-ntsc)
├── scoop/
│   └── ntsc.json            Scoop manifest template (bucket: n1sc/scoop-ntsc)
├── macos/
│   ├── make-dmg.sh          .app bundle + dylib closure + .dmg
│   └── make-tarball.sh      self-contained .tar.gz (dylib closure + rpath)
└── windows/
    ├── ntsc.wxs             main WiX v4 source (binary + PATH feature)
    ├── build-msi.ps1        computes DLL closure, bundles ld.lld + runtime,
    │                        generates dlls.wxs and tools.wxs, builds MSI
    ├── make-tarball.ps1     self-contained .tar.gz (reuses build-msi staging)
    ├── dlls.wxs             generated at build time, not committed
    └── tools.wxs            generated at build time, not committed
```

## Platform notes

### Debian / Ubuntu (.deb)

`build-deb-rpm.sh` stages the binary and a gzipped man page, then runs
`dpkg-deb`. The generated `debian/control` declares a dependency on
`libLLVM-22`, the Debian/Ubuntu name for the LLVM 22 runtime (Fedora's is
`llvm-libs`), so each distro family is packaged against its own runtime.

Artifact: `ntsc_<version>_amd64.deb`

Install with: `sudo dpkg -i ntsc_<version>_amd64.deb`

### Fedora / RHEL (.rpm)

`build-deb-rpm.sh` runs `rpmbuild` against `fedora/ntsc.spec`, which requires
`llvm-libs >= 22`.

Artifact: `ntsc-<version>-1.x86_64.rpm`

Install with: `sudo dnf install ntsc-<version>-1.x86_64.rpm`

### Arch Linux (AUR `ntsc` / `ntsc-bin` + prebuilt .pkg.tar.zst)

Arch gets three routes to the same compiler:

**Source recipe (`arch/PKGBUILD`, AUR `ntsc`)** — downloads the `v<version>`
tarball and builds with the system toolchain, so `makedepends` lists `cargo`,
`rust`, and `llvm`, and the runtime dependency is `llvm-libs`. `build()`
compiles both `ntsc-cli` and `ntsc-runtime`, and `package()` installs the
runtime archive to `/usr/lib/ntsc/libntsc_runtime.a`.

Install with: `yay -S ntsc`

**Prebuilt recipe (`arch/PKGBUILD-bin`, AUR `ntsc-bin`)** — installs the
self-contained release tarball, which already bundles the full LLVM closure and
the runtime. Because LLVM is bundled it depends only on `glibc` and `gcc-libs`,
so it keeps working when rolling Arch bumps LLVM to a new soname — unlike a
package linked against the system `llvm-libs`.

Install with: `yay -S ntsc-bin`

**Release artifact (`make-arch-pkg.sh` → `.pkg.tar.zst`)** — the same package
built from the tarball this run produced, attached to the GitHub Release for
offline installs. `makepkg` only runs on Arch and refuses to run as root, so CI
builds it inside an `archlinux` container as an unprivileged user; it must run
after `make-tarball.sh`, whose output tree it repackages. `options=('!strip')`
keeps `makepkg` from stripping `libntsc_runtime.a` (which would gut its symbol
table and break linking).

Install with: `sudo pacman -U ntsc-bin-<version>-1-x86_64.pkg.tar.zst`

Artifact: `ntsc-bin-<version>-1-x86_64.pkg.tar.zst`

### macOS (.dmg)

`make-dmg.sh` wraps the binary in a `ntsc.app` bundle, recursively copies the
non-system dylib closure (including Homebrew's `llvm@22` libraries) into
`Contents/Frameworks`, rewrites install names to `@rpath`, ad-hoc code signs,
and packs everything into a `UDZO` .dmg with an Applications symlink.

The bundle is a CLI tool, not a GUI app; dragging it into `/Applications` and
running `./ntsc.app/Contents/MacOS/ntsc` works, or symlink it onto your PATH.

Artifact: `ntsc-<version>-macos-<arch>.dmg` (`arm64` or `x86_64`)

### Windows (.msi)

`build-msi.ps1` runs `dumpbin /DEPENDENTS` to compute the DLL closure of
`ntsc.exe`, copies the non-system DLLs (from the LLVM install directory) into
the MSI, and emits a `dlls.wxs` fragment. It also stages a **self-contained
Windows link path** so `ntsc build` works on a machine with no compiler
installed:

- `ld.lld.exe` (from the LLVM bin directory) is placed next to `ntsc.exe`;
- the GNU-flavoured runtime (`libntsc_runtime.a`, built with
  `cargo build -p ntsc-runtime --target x86_64-pc-windows-gnu --release`) is
  bundled, because `ld.lld` in MinGW mode cannot consume the MSVC-flavoured
  archive;
- the MinGW import libraries (from the rustc toolchain's
  `x86_64-pc-windows-gnu` lib dir, created by
  `rustup target add x86_64-pc-windows-gnu`) are installed into a `mingw`
  subdirectory.

`tools.wxs` declares these files. At runtime `ntsc` prefers the bundled
`ld.lld` (found next to the executable) over `link.exe`/`gcc` on PATH, so an
MSVC or MinGW installation is no longer required for linking. The GNU-flavoured
runtime is only built for the installer; developer machines with `link.exe`
still use the MSVC-flavoured `ntsc_runtime.lib`.

`wix build` then produces an installer that installs to
`%ProgramFiles%\ntsc\bin` and supports upgrades via the `UpgradeCode`.

Because the LLVM DLLs are bundled, Windows needs no separate LLVM
installation. The installer (WixUI feature-selection dialogs) offers one
optional feature: **Add ntsc to PATH**, checked by default and toggleable at
install time, or later via Programs & Features -> Change. Unchecking it skips
the environment change entirely. The license-agreement dialog shows an RTF
conversion of `LICENSE`, generated by the script.

Requires the wix CLI: `dotnet tool install --global wix`.

Artifact: `ntsc-<version>.msi`

### Portable tarball (.tar.gz, all platforms)

Each platform also produces a **self-contained** `.tar.gz` that bundles the
compiler, the runtime archive, and the full LLVM shared-library closure, so it
runs with no LLVM install and no `cargo` toolchain. Extract it anywhere and run
`bin/ntsc` (or add the directory to `PATH`).

Artifacts:

- `ntsc-<version>-linux-x86_64.tar.gz`
- `ntsc-<version>-macos-arm64.tar.gz`
- `ntsc-<version>-macos-x86_64.tar.gz`
- `ntsc-<version>-windows-x86_64.tar.gz`

The Unix archives use a `bin/` + `lib/ntsc/` prefix layout and are made
relocatable so the bundled libraries resolve wherever the tree is extracted:

- **Linux** (`linux/make-tarball.sh`): `ldd` prints the flattened closure, so
  one pass copies every non-system library into `lib/ntsc`. `patchelf` sets
  `RUNPATH` to `$ORIGIN/../lib/ntsc` on the binary and `$ORIGIN` on each bundled
  `.so`. The loader and core system libraries (libc, libm, the loader,
  libgcc_s, …) are deliberately **not** bundled — a foreign libc against the
  host kernel breaks the binary — so the archive inherits the **glibc floor of
  the build machine**. Build it on the oldest distro you want to support
  (CI uses `ubuntu-latest`).
- **macOS** (`macos/make-tarball.sh`): reuses the recursive `otool -L` /
  `install_name_tool` closure walk from `make-dmg.sh`, rewriting install names
  to `@rpath/<name>` with an `@executable_path/../lib/ntsc` rpath on the binary
  and `@loader_path` on each dylib. Every Mach-O is ad-hoc re-signed after each
  edit, because editing a Mach-O invalidates its signature.
- **Windows** (`windows/make-tarball.ps1`): reuses the self-contained tree
  `build-msi.ps1` already stages at `dist\win` (ntsc.exe, DLL closure,
  `ld.lld.exe`, `libntsc_runtime.a`, `mingw\`), so it **must run after
  `build-msi.ps1`**. The layout is intentionally flat — `ntsc` resolves both
  its bundled linker and the runtime from its own directory — so it is not
  reshaped into `bin/` + `lib/`.

## Distribution channels

After the GitHub Release is created, three jobs in `release.yml` push the
manifests below so an installed copy upgrades through its package manager. They
run only for a `v*` tag push (never for a branch `workflow_dispatch`) and only
while `PUBLISH` is `"true"`:

| Channel | Template | Rendered into | Secret |
| --- | --- | --- | --- |
| Homebrew | `homebrew/ntsc.rb` | `Formula/ntsc.rb` in `n1sc/homebrew-ntsc` | `HOMEBREW_TAP_TOKEN` |
| Scoop | `scoop/ntsc.json` | `bucket/ntsc.json` in `n1sc/scoop-ntsc` | `SCOOP_BUCKET_TOKEN` |
| AUR | `linux/arch/PKGBUILD`, `linux/arch/PKGBUILD-bin` | `ntsc.git`, `ntsc-bin.git` | `AUR_SSH_KEY` |

`@VERSION@`, `@SHA256@`, `@SHA256_MACOS_ARM@` and `@SHA256_MACOS_INTEL@` are
substituted from the tag and the checksums of the artifacts this run built, so
the templates in this directory are never installable as-is. Each job refuses
to publish if a placeholder is left over, if the expected artifact is missing,
or if the release asset is not yet downloadable.

The Homebrew formula installs the macOS tarball's tree under `libexec` and
writes `bin` shims that exec the real binaries — the compiler resolves its
runtime and bundled LLVM through `@executable_path/../lib/ntsc`, so it must run
from a complete tree. The Scoop manifest extracts the Windows tarball flat and
exposes `ntsc`, `ntsc-pkg` and `ntsc-lsp`; its `checkver` block tracks new tags.
The AUR push needs a `.SRCINFO` generated by `makepkg`, so CI runs that inside
an `archlinux` container as an unprivileged user, and commits as the
`AUR_MAINTAINER` identity (the AUR rejects unregistered email addresses).

## Building locally

Each recipe takes the version as its first argument. The native packages and
tarballs expect the release compiler and language server, the package-manager
binary at `crates/ntsc-pkg/build/release/ntsc-pkg` (or `.exe`), and the
runtime at `target/release/libntsc_runtime.a`:

```bash
cargo build --release -p ntsc-cli -p ntsc-lsp -p ntsc-runtime
(cd crates/ntsc-pkg && ../../target/release/ntsc build --release)

# Linux (.deb/.rpm require dpkg-deb/rpmbuild; the tarball requires patchelf;
# the .pkg.tar.zst requires makepkg and so only builds on Arch):
bash .github/packaging/linux/build-deb-rpm.sh 0.1.0
bash .github/packaging/linux/make-tarball.sh 0.1.0
bash .github/packaging/linux/make-arch-pkg.sh 0.1.0   # Arch only; after make-tarball.sh

# macOS (requires hdiutil, otool, install_name_tool, codesign):
bash .github/packaging/macos/make-dmg.sh 0.1.0
bash .github/packaging/macos/make-tarball.sh 0.1.0

# Windows (requires the wix CLI and the MSVC developer environment; the
# bundled linker additionally needs the GNU runtime and MinGW import libs).
# make-tarball.ps1 reuses build-msi.ps1's staging, so run it afterwards:
rustup target add x86_64-pc-windows-gnu
cargo build -p ntsc-runtime --target x86_64-pc-windows-gnu --release
pwsh .github/packaging/windows/build-msi.ps1 -Version 0.1.0 -MsiVersion 0.1.0
pwsh .github/packaging/windows/make-tarball.ps1 -Version 0.1.0
```
