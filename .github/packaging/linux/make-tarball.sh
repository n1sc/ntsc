#!/usr/bin/env bash
# Build a self-contained portable .tar.gz for the ntsc release binary.
#
# Usage: make-tarball.sh <version> [path-to-binary]
#
# Unlike the .deb/.rpm (which depend on the system LLVM), this archive bundles
# everything `ntsc` and `ntsc build` need: the compiler, the runtime static
# library, and the full LLVM shared-library closure. Extract it anywhere and
# run bin/ntsc — no LLVM install and no cargo toolchain required.
#
# Layout:
#   ntsc-<version>-linux-<arch>/
#     bin/ntsc
#     bin/ntsc-lsp
#     lib/ntsc/libntsc_runtime.a
#     lib/ntsc/libLLVM.so.22.1 + the rest of the shared-library closure
#     share/man/man1/ntsc.1.gz
#     LICENSE
#
# Relocatability: bin/ntsc gets RUNPATH $ORIGIN/../lib/ntsc and each bundled
# .so gets RUNPATH $ORIGIN, so the loader resolves the bundle's own libraries
# (including libLLVM's own dependencies) from inside the tree wherever it is
# extracted.
#
# Caveat: the archive inherits the builder's glibc floor (it does NOT bundle
# libc/the loader — doing so breaks the binary), so build it on the oldest
# distro you want to support.
set -euo pipefail

VERSION="${1:?usage: make-tarball.sh <version> [binary]}"
BIN="${2:-target/release/ntsc}"
PKG="crates/ntsc-pkg/build/release/ntsc-pkg"
LSP="target/release/ntsc-lsp"
RUNTIME="target/release/libntsc_runtime.a"
ARCH="$(uname -m)"
ROOT="$(cd "$(dirname "$0")" && pwd)"
MAN="$ROOT/../ntsc.1"

for f in "$BIN" "$PKG" "$LSP" "$RUNTIME"; do
  if [ ! -f "$f" ]; then
    echo "error: $f not found - build it with: cargo build --release -p ntsc-cli -p ntsc-lsp -p ntsc-runtime" >&2
    exit 1
  fi
done
if ! command -v patchelf >/dev/null 2>&1; then
  echo "error: patchelf not found - install it (apt-get install patchelf)" >&2
  exit 1
fi

NAME="ntsc-$VERSION-linux-$ARCH"
STAGE="dist/$NAME"
rm -rf "$STAGE"
mkdir -p "$STAGE/bin" "$STAGE/lib/ntsc" "$STAGE/share/man/man1"

install -m 755 "$BIN" "$STAGE/bin/ntsc"
install -m 755 "$PKG" "$STAGE/bin/ntsc-pkg"
install -m 755 "$LSP" "$STAGE/bin/ntsc-lsp"
install -m 644 "$RUNTIME" "$STAGE/lib/ntsc/libntsc_runtime.a"
gzip -9 -c "$MAN" > "$STAGE/share/man/man1/ntsc.1.gz"
install -m 644 LICENSE "$STAGE/LICENSE"

# ── Bundle the shared-library closure ──────────────────────────────────────
# ldd prints the *flattened* transitive closure, so a single pass is enough
# (no recursion, unlike macOS otool). Bundle every resolved library EXCEPT the
# loader and the core system libraries: shipping a foreign libc/loader against
# the host kernel breaks the binary, so those must come from the host.
skip_lib() {
  case "$1" in
    ld-linux*|linux-vdso.so*|libc.so.*|libm.so.*|libpthread.so.*|\
libdl.so.*|librt.so.*|libgcc_s.so.*) return 0 ;;
    *) return 1 ;;
  esac
}

bundle_elf() {
  local binary="$1"
  while read -r name _arrow path _addr; do
    # ldd lines are either "name => /path (0x..)" or "/path (0x..)" (loader).
    if [ -z "${path:-}" ] || [ "${path:0:1}" != "/" ]; then
      path="$name"
    fi
    base="$(basename "$name")"
    skip_lib "$base" && continue
    [ -f "$path" ] || continue
    cp -L "$path" "$STAGE/lib/ntsc/$base"
  done < <(ldd "$binary")
}

bundle_elf "$STAGE/bin/ntsc"
bundle_elf "$STAGE/bin/ntsc-pkg"
bundle_elf "$STAGE/bin/ntsc-lsp"

# ── Make it relocatable ────────────────────────────────────────────────────
# $ORIGIN is a literal token the loader expands at run time; single-quote it so
# the shell does not touch it.
patchelf --set-rpath '$ORIGIN/../lib/ntsc' "$STAGE/bin/ntsc"
patchelf --set-rpath '$ORIGIN/../lib/ntsc' "$STAGE/bin/ntsc-pkg"
patchelf --set-rpath '$ORIGIN/../lib/ntsc' "$STAGE/bin/ntsc-lsp"
for so in "$STAGE"/lib/ntsc/*.so*; do
  [ -f "$so" ] || continue
  chmod u+w "$so"
  patchelf --set-rpath '$ORIGIN' "$so"
done

# ── Assemble the archive ───────────────────────────────────────────────────
tar -C dist -czf "dist/$NAME.tar.gz" "$NAME"
rm -rf "$STAGE"
echo "Built dist/$NAME.tar.gz"
