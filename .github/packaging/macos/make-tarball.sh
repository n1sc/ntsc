#!/usr/bin/env bash
# Build a self-contained portable .tar.gz for the ntsc release binary on macOS.
#
# Usage: make-tarball.sh <version> [path-to-binary]
#
# Bundles everything `ntsc` and `ntsc build` need — the compiler, the runtime
# static library, and the full dylib closure (including Homebrew's llvm@22) —
# so it runs on a Mac with no LLVM install and no cargo toolchain. Extract it
# anywhere and run bin/ntsc.
#
# Layout:
#   ntsc-<version>-macos-<arch>/
#     bin/ntsc
#     bin/ntsc-lsp
#     lib/ntsc/libntsc_runtime.a
#     lib/ntsc/libLLVM.dylib + the rest of the dylib closure
#     share/man/man1/ntsc.1.gz
#     LICENSE
#
# Relocatability: bin/ntsc gets an LC_RPATH of @executable_path/../lib/ntsc and
# each bundled dylib is re-id'd to @rpath/<name> with an LC_RPATH of
# @loader_path, so the loader resolves the bundle's own libraries from inside
# the tree wherever it is extracted. Every Mach-O is ad-hoc re-signed after
# each install_name_tool edit, because editing a Mach-O invalidates its
# signature and Apple Silicon then refuses to load it.
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

NAME="ntsc-$VERSION-macos-$ARCH"
STAGE="dist/$NAME"
LIBDIR="$STAGE/lib/ntsc"
rm -rf "$STAGE"
mkdir -p "$STAGE/bin" "$LIBDIR" "$STAGE/share/man/man1"

install -m 755 "$BIN" "$STAGE/bin/ntsc"
install -m 755 "$PKG" "$STAGE/bin/ntsc-pkg"
install -m 755 "$LSP" "$STAGE/bin/ntsc-lsp"
install -m 644 "$RUNTIME" "$LIBDIR/libntsc_runtime.a"
gzip -9 -c "$MAN" > "$STAGE/share/man/man1/ntsc.1.gz"
install -m 644 LICENSE "$STAGE/LICENSE"

# ── Bundle the dylib closure ───────────────────────────────────────────────
# otool -L reports only direct dependencies, so recurse. Copy every non-system
# dylib into lib/ntsc, rewrite install names to @rpath/<name>, and give each a
# @loader_path rpath so its own siblings resolve from the same directory.
copy_dylibs() {
  local binary="$1"
  local dep name target
  while IFS= read -r dep; do
    case "$dep" in
      /System/* | /usr/lib/* | "") continue ;;
      "@rpath"* | "@executable_path"* | "@loader_path"*) continue ;;
    esac
    name="$(basename "$dep")"
    target="$LIBDIR/$name"
    if [ ! -e "$target" ]; then
      cp "$dep" "$target"
      chmod u+w "$target"
      install_name_tool -id "@rpath/$name" "$target" 2>/dev/null || true
      install_name_tool -add_rpath "@loader_path" "$target" 2>/dev/null || true
      copy_dylibs "$target"
      codesign --force --sign - "$target" 2>/dev/null || true
    fi
    install_name_tool -change "$dep" "@rpath/$name" "$binary" 2>/dev/null || true
  done < <(otool -L "$binary" | tail -n +2 | awk '{print $1}')
}

copy_dylibs "$STAGE/bin/ntsc"
copy_dylibs "$STAGE/bin/ntsc-pkg"

# The executable resolves @rpath against lib/ntsc.
install_name_tool -add_rpath "@executable_path/../lib/ntsc" "$STAGE/bin/ntsc"
codesign --force --sign - "$STAGE/bin/ntsc"
codesign --force --sign - "$STAGE/bin/ntsc-pkg"
codesign --force --sign - "$STAGE/bin/ntsc-lsp"

# ── Assemble the archive ───────────────────────────────────────────────────
tar -C dist -czf "dist/$NAME.tar.gz" "$NAME"
rm -rf "$STAGE"
echo "Built dist/$NAME.tar.gz"
