#!/usr/bin/env bash
# Build a drag-and-drop .dmg for the ntsc release binary.
#
# Usage: make-dmg.sh <version> [path-to-binary]
#
# The binary links LLVM 23 dynamically via Homebrew's llvm@23. The script
# packages the binary into an .app bundle and copies the full dylib closure
# into Contents/Frameworks, rewriting install names to @rpath so the bundle
# is self-contained. It is ad-hoc code-signed so it runs on Apple Silicon.
set -euo pipefail

VERSION="${1:?usage: make-dmg.sh <version> [binary]}"
BIN="${2:-target/release/ntsc}"
PKG="crates/ntsc-pkg/build/release/ntsc-pkg"
LSP="target/release/ntsc-lsp"
RUNTIME="target/release/libntsc_runtime.a"
# Both macOS arches are built, and a release cannot hold two assets with the
# same name, so the disk image carries its arch the way the tarball does.
ARCH="$(uname -m)"
DMG="dist/ntsc-$VERSION-macos-$ARCH.dmg"

APP="dist/ntsc.app"
FRAMEWORKS="$APP/Contents/Frameworks"
MACOS="$APP/Contents/MacOS"
DMGDIR="dist/dmg"

for f in "$BIN" "$PKG" "$LSP" "$RUNTIME"; do
  if [ ! -f "$f" ]; then
    echo "error: $f not found - build the release compiler, package manager, language server, and runtime first" >&2
    exit 1
  fi
done

# Clean only this script's own outputs, not the whole dist/ directory, so a
# tarball or other artifact already written to dist/ is not clobbered.
rm -rf "$APP" "$DMGDIR" "$DMG"
mkdir -p "$FRAMEWORKS" "$MACOS" "$DMGDIR"

install -m 755 "$BIN" "$MACOS/ntsc"
install -m 755 "$PKG" "$MACOS/ntsc-pkg"
install -m 755 "$LSP" "$MACOS/ntsc-lsp"

# The static archive `ntsc build` links every NTSC program against. It goes in
# Contents/lib/ntsc/ — NOT beside the binary in Contents/MacOS/ — because
# `codesign --force --sign -` on the whole .app treats every file under
# Contents/MacOS/ as nested executable code needing its own signature, and an
# ar archive is not signable Mach-O (it fails with "code object is not signed
# at all"). Files elsewhere in the bundle are sealed as resources instead. ntsc
# resolves it from Contents/MacOS/ntsc via its executable-relative ../lib/ntsc
# candidate, so no CLI change is needed.
mkdir -p "$APP/Contents/lib/ntsc"
install -m 644 "$RUNTIME" "$APP/Contents/lib/ntsc/libntsc_runtime.a"

# ── Info.plist ─────────────────────────────────────────────────────────────
cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key>
  <string>ntsc</string>
  <key>CFBundleDisplayName</key>
  <string>NTSC</string>
  <key>CFBundleIdentifier</key>
  <string>dev.neutron.ntsc</string>
  <key>CFBundleVersion</key>
  <string>$VERSION</string>
  <key>CFBundleShortVersionString</key>
  <string>$VERSION</string>
  <key>CFBundleExecutable</key>
  <string>ntsc</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
</dict>
</plist>
EOF

# ── Bundle the dylib closure ───────────────────────────────────────────────
# Copy every non-system dylib referenced by the binary and its dependencies
# into Contents/Frameworks and rewrite install names to @rpath/<name>.
copy_dylibs() {
  local binary="$1"
  local dep name target
  while IFS= read -r dep; do
    case "$dep" in
      /System/* | /usr/lib/* | "") continue ;;
      "@rpath"* | "@executable_path"* | "@loader_path"*) continue ;;
    esac
    name="$(basename "$dep")"
    target="$FRAMEWORKS/$name"
    if [ ! -e "$target" ]; then
      cp "$dep" "$target"
      install_name_tool -id "@rpath/$name" "$target" 2>/dev/null || true
      copy_dylibs "$target"
    fi
    install_name_tool -change "$dep" "@rpath/$name" "$binary" 2>/dev/null || true
  done < <(otool -L "$binary" | tail -n +2 | awk '{print $1}')
}

copy_dylibs "$MACOS/ntsc"
copy_dylibs "$MACOS/ntsc-pkg"

# Ad-hoc signature so macOS accepts the bundle.
codesign --force --sign - "$MACOS/ntsc-pkg"
codesign --force --sign - "$MACOS/ntsc-lsp"
codesign --force --sign - "$APP"

# ── Assemble the .dmg ──────────────────────────────────────────────────────
cp -R "$APP" "$DMGDIR/"
ln -s /Applications "$DMGDIR/Applications"
hdiutil create -volname "ntsc $VERSION" -srcfolder "$DMGDIR" -ov \
  -format UDZO "$DMG"

echo "Built $DMG"
