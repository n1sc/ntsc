#!/usr/bin/env bash
# End-to-end test for `ntsc pkg`.
#
# Serves a registry index from a temporary HTTP server, backs its two packages
# with real git repositories tagged `v*`, and drives a consumer project
# through install, build, run, update and remove. Nothing outside the
# temporary directory is touched and no network access is required.
#
# Usage: scripts/test-pkg.sh
#   NTSC=/path/to/ntsc      compiler under test (default: target/release/ntsc)
#   PKG=/path/to/ntsc-pkg   package manager under test (default: target/release/ntsc-pkg)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
NTSC="${NTSC:-$ROOT/target/release/ntsc}"
PKG="${PKG:-$ROOT/target/release/ntsc-pkg}"

if [[ ! -x "$NTSC" ]]; then
    echo "error: compiler not found at $NTSC (build it with: cargo build --release -p ntsc-cli)" >&2
    exit 1
fi
if [[ ! -x "$PKG" ]]; then
    echo "error: package manager not found at $PKG (build crates/ntsc-pkg first)" >&2
    exit 1
fi

# `ntsc pkg` runs the manager that sits beside the compiler.
BESIDE="$(dirname "$NTSC")/ntsc-pkg"
if [[ "$PKG" != "$BESIDE" ]]; then
    cp "$PKG" "$BESIDE"
fi

WORK="$(mktemp -d)"
SERVER_PID=""
cleanup() {
    if [[ -n "$SERVER_PID" ]]; then
        kill "$SERVER_PID" 2>/dev/null || true
        wait "$SERVER_PID" 2>/dev/null || true
    fi
    rm -rf "$WORK"
}
trap cleanup EXIT

REG="$WORK/registry"
PROJ="$WORK/project"
mkdir -p "$REG" "$PROJ/src"

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

# ── fixture packages ────────────────────────────────────────────────────────
make_repo() {
    local dir="$1" version="$2"
    git -C "$dir" init -q -b main
    git -C "$dir" add -A
    git -C "$dir" -c user.email=test@example.com -c user.name=test \
        commit -qm "release $version"
    git -C "$dir" tag "v$version"
}

mkdir -p "$REG/fixture"
cat > "$REG/fixture/neutron.toml" <<'EOF'
[package]
name = "fixture"
entry = "lib.nt"
output = "fixture"
EOF
cat > "$REG/fixture/lib.nt" <<'EOF'
fun greeting() -> string {
    return "hello from fixture 1.0.0"
}
EOF
make_repo "$REG/fixture" "1.0.0"

mkdir -p "$REG/outer"
cat > "$REG/outer/neutron.toml" <<'EOF'
[package]
name = "outer"
entry = "lib.nt"
output = "outer"

[dependencies]
fixture = "^1.0.0"
EOF
cat > "$REG/outer/lib.nt" <<'EOF'
use "fixture" as fixture

fun outer_greeting() -> string {
    return "outer wraps " + fixture.greeting()
}
EOF
make_repo "$REG/outer" "1.0.0"

write_index() {
    local fixture_versions="$1"
    cat > "$REG/index.json" <<EOF
{
  "version": 1,
  "generated": "2026-10-18T00:00:00Z",
  "packages": [
    {
      "name": "fixture",
      "description": "Test fixture package",
      "repo": "file://$REG/fixture",
      "license": "MIT",
      "keywords": ["test", "fixture"],
      "versions": [$fixture_versions]
    },
    {
      "name": "outer",
      "description": "Depends on the fixture package",
      "repo": "file://$REG/outer",
      "license": "MIT",
      "keywords": ["test"],
      "versions": ["1.0.0"]
    }
  ]
}
EOF
}
write_index '"1.0.0"'

# ── serve the index ─────────────────────────────────────────────────────────
PORT=$((18000 + $$ % 2000))
python3 -m http.server "$PORT" --bind 127.0.0.1 --directory "$REG" >/dev/null 2>&1 &
SERVER_PID=$!

URL="http://127.0.0.1:$PORT/index.json"
for _ in $(seq 1 100); do
    if curl -fsS "$URL" >/dev/null 2>&1; then
        break
    fi
    if ! kill -0 "$SERVER_PID" 2>/dev/null; then
        fail "registry server exited"
    fi
    sleep 0.1
done
curl -fsS "$URL" >/dev/null || fail "registry server did not start on port $PORT"

# ── consumer project ────────────────────────────────────────────────────────
cat > "$PROJ/neutron.toml" <<'EOF'
[package]
name = "consumer"
entry = "src/main.nt"
output = "consumer"

[dependencies]
outer = "^1.0.0"
EOF
cat > "$PROJ/src/main.nt" <<'EOF'
use "outer" as outer

fun main() -> int {
    say(outer.outer_greeting())
    return 0
}
EOF

cd "$PROJ"
export NTSC_REGISTRY="$URL"

echo "== install"
INSTALL_OUTPUT="$($NTSC pkg install 2>&1)"
echo "$INSTALL_OUTPUT"
grep -q 'neutron modules' <<<"$INSTALL_OUTPUT" || fail "install output has no module header"
grep -q 'ok outer v1.0.0' <<<"$INSTALL_OUTPUT" || fail "install output has no outer package status"
grep -q 'installed 2 packages' <<<"$INSTALL_OUTPUT" || fail "install output has no install summary"
[[ -f .ntsc/outer/lib.nt ]] || fail "outer was not vendored"
[[ -f .ntsc/fixture/lib.nt ]] || fail "transitive dependency was not vendored"
[[ ! -d .ntsc/outer/.git ]] || fail "the checkout's .git directory was vendored"
grep -q '^fixture = "1.0.0"$' neutron.lock || fail "fixture missing from the lockfile"
grep -q '^outer = "1.0.0"$' neutron.lock || fail "outer missing from the lockfile"

echo "== build"
OUTPUT="$("$NTSC" build --release 2>&1)"
echo "$OUTPUT"
"$PROJ/build/release/consumer" | tee "$WORK/run.txt"
grep -q 'outer wraps hello from fixture 1.0.0' "$WORK/run.txt" ||
    fail "unexpected program output"

echo "== update"
sed 's/1\.0\.0/1.1.0/' "$REG/fixture/lib.nt" > "$REG/fixture/lib.nt.new"
mv "$REG/fixture/lib.nt.new" "$REG/fixture/lib.nt"
git -C "$REG/fixture" add -A
git -C "$REG/fixture" -c user.email=test@example.com -c user.name=test \
    commit -qm "release 1.1.0"
git -C "$REG/fixture" tag "v1.1.0"
write_index '"1.0.0", "1.1.0"'

# A plain install reproduces the locked version.
"$NTSC" pkg install >/dev/null
grep -q '^fixture = "1.0.0"$' neutron.lock ||
    fail "install ignored the lockfile and upgraded fixture"

"$NTSC" pkg update >/dev/null
grep -q '^fixture = "1.1.0"$' neutron.lock ||
    fail "update did not move fixture to 1.1.0"
grep -q 'hello from fixture 1.1.0' .ntsc/fixture/lib.nt ||
    fail "update did not re-vendor the new version"

echo "== remove"
"$NTSC" pkg remove outer >/dev/null
[[ ! -d .ntsc/outer ]] || fail "remove left the package vendored"
[[ ! -d .ntsc/fixture ]] || fail "remove left a stale transitive dependency"
if grep -q '^outer = ' neutron.lock; then
    fail "remove left the package in the lockfile"
fi
if grep -q '^outer = ' neutron.toml; then
    fail "remove left the dependency in the manifest"
fi

echo "== errors"
if "$NTSC" pkg install missing-package >/dev/null 2>&1; then
    fail "installing an unknown package succeeded"
fi
if NTSC_REGISTRY="http://127.0.0.1:1/index.json" "$NTSC" pkg search fixture >"$WORK/err.txt" 2>&1; then
    fail "an unreachable registry did not fail"
fi
grep -q 'cannot reach' "$WORK/err.txt" || fail "unreachable registry message is empty"

echo "OK: package manager end-to-end"
