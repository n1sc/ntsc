#!/usr/bin/env bash
# Build the .deb and .rpm packages for the ntsc release binary.
#
# Usage: build-deb-rpm.sh <version>
#
# The binary is already built at target/release/ntsc and links LLVM 23
# dynamically; the packages therefore declare a runtime dependency on the
# system LLVM (libLLVM-23 on Debian/Ubuntu, llvm-libs on Fedora/RHEL).
#
# Both packages also ship libntsc_runtime.a — the static archive `ntsc build`
# links every NTSC program against. It is a static library, so it adds no
# shared-library dependency; an installed ntsc finds it via the
# executable-relative <prefix>/lib{,64}/ntsc directory.
set -euo pipefail

VERSION="${1:?usage: build-deb-rpm.sh <version>}"
ROOT="$(cd "$(dirname "$0")" && pwd)"
PACKAGING="$ROOT/.."
BIN="target/release/ntsc"
PKG="crates/ntsc-pkg/build/release/ntsc-pkg"
LSP="target/release/ntsc-lsp"
RUNTIME="target/release/libntsc_runtime.a"

for f in "$BIN" "$PKG" "$LSP" "$RUNTIME"; do
  if [ ! -f "$f" ]; then
    echo "error: $f not found - build the release compiler, package manager, language server, and runtime first" >&2
    exit 1
  fi
done

# ── Shared staging area ────────────────────────────────────────────────────
STAGE="$PACKAGING/.build"
rm -rf "$STAGE"
mkdir -p "$STAGE"
install -m 755 "$BIN" "$STAGE/ntsc"
install -m 755 "$PKG" "$STAGE/ntsc-pkg"
install -m 755 "$LSP" "$STAGE/ntsc-lsp"
install -m 644 "$RUNTIME" "$STAGE/libntsc_runtime.a"
gzip -9 -c "$ROOT/../ntsc.1" > "$STAGE/ntsc.1.gz"

# ── Debian / Ubuntu (.deb) ────────────────────────────────────────────────
if command -v dpkg-deb >/dev/null 2>&1; then
  DEBROOT="$STAGE/debroot"
  mkdir -p "$DEBROOT/DEBIAN" "$DEBROOT/usr/bin" "$DEBROOT/usr/lib/ntsc" \
    "$DEBROOT/usr/share/man/man1"
  install -m 755 "$STAGE/ntsc" "$DEBROOT/usr/bin/ntsc"
  install -m 755 "$STAGE/ntsc-pkg" "$DEBROOT/usr/bin/ntsc-pkg"
  install -m 755 "$STAGE/ntsc-lsp" "$DEBROOT/usr/bin/ntsc-lsp"
  install -m 644 "$STAGE/libntsc_runtime.a" "$DEBROOT/usr/lib/ntsc/libntsc_runtime.a"
  install -m 644 "$STAGE/ntsc.1.gz" "$DEBROOT/usr/share/man/man1/ntsc.1.gz"
  sed "s/@VERSION@/$VERSION/g" "$ROOT/debian/control" > "$DEBROOT/DEBIAN/control"
  dpkg-deb --build --root-owner-group "$DEBROOT" "ntsc_${VERSION}_amd64.deb"
  echo "Built ntsc_${VERSION}_amd64.deb"
fi

# ── Fedora / RHEL (.rpm) ───────────────────────────────────────────────────
if command -v rpmbuild >/dev/null 2>&1; then
  RPMROOT="$STAGE/rpmbuild"
  mkdir -p "$RPMROOT"/{BUILD,RPMS,SOURCES,SPECS,SRPMS}
  sed "s/@VERSION@/$VERSION/g" "$ROOT/fedora/ntsc.spec" > "$RPMROOT/SPECS/ntsc.spec"
  rpmbuild -bb "$RPMROOT/SPECS/ntsc.spec" \
    --define "_topdir $RPMROOT" \
    --define "_sourcedir $STAGE" \
    --define "_prefix /usr" \
    --define "_bindir /usr/bin" \
    --define "_mandir /usr/share/man"
  cp "$RPMROOT"/RPMS/*/*.rpm .
  echo "Built $(basename "$(find "$RPMROOT"/RPMS -name '*.rpm' | head -1)")"
fi
