#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

for target in x86_64-unknown-linux-gnu aarch64-apple-darwin x86_64-pc-windows-msvc; do
  case "$target" in
    *-msvc) expected=no-llvm-linking ;;
    *) expected=force-dynamic ;;
  esac
  tree="$(cargo tree --workspace --all-features --target "$target" --edges features --invert llvm-sys --prefix none)"
  preferences="$(printf '%s\n' "$tree" |
    sed -n 's/^llvm-sys feature "\(force-dynamic\|force-static\|prefer-dynamic\|prefer-static\|no-llvm-linking\)"$/\1/p' |
    sort -u)"
  if [ "$preferences" != "$expected" ]; then
    printf 'error: %s must select %s, found: %s\n' "$target" "$expected" "$preferences" >&2
    exit 1
  fi
  printf '%s: %s\n' "$target" "$expected"
done
