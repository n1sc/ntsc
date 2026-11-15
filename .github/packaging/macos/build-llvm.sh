#!/usr/bin/env bash
set -euo pipefail

VERSION="${1:?usage: build-llvm.sh VERSION PREFIX}"
PREFIX="${2:?usage: build-llvm.sh VERSION PREFIX}"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/ntsc-llvm.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

curl -fL --retry 3 \
  "https://github.com/llvm/llvm-project/releases/download/llvmorg-${VERSION}/llvm-project-${VERSION}.src.tar.xz" \
  -o "$WORK/source.tar.xz"
tar -xf "$WORK/source.tar.xz" -C "$WORK"

cmake -S "$WORK/llvm-project-${VERSION}.src/llvm" -B "$WORK/build" \
  -DCMAKE_BUILD_TYPE=Release \
  -DCMAKE_INSTALL_PREFIX="$PREFIX" \
  -DLLVM_BUILD_LLVM_DYLIB=ON \
  -DLLVM_LINK_LLVM_DYLIB=ON \
  '-DLLVM_DISTRIBUTION_COMPONENTS=LLVM;llvm-config;llvm-headers;cmake-exports' \
  -DLLVM_INCLUDE_TESTS=OFF \
  -DLLVM_INCLUDE_BENCHMARKS=OFF \
  -DLLVM_INCLUDE_EXAMPLES=OFF \
  -DLLVM_INCLUDE_DOCS=OFF
cmake --build "$WORK/build" --target install-distribution --parallel 3

"$PREFIX/bin/llvm-config" --version
"$PREFIX/bin/llvm-config" --link-shared --libnames
