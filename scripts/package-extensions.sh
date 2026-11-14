#!/bin/sh
set -eu

usage() {
    cat <<'EOF'
usage: package-extensions.sh --version X.Y.Z --out DIR [--vsix PATH] [--wasm PATH] [--dry-run]

Builds every editor extension artifact into DIR and writes extensions-manifest.json
and SHA256SUMS. Pass --vsix/--wasm to reuse artifacts produced by CI.
EOF
}

version=
out=
vsix=
wasm=
dry_run=0

while [ "$#" -gt 0 ]; do
    case "$1" in
        --version) version=$2; shift 2 ;;
        --out) out=$2; shift 2 ;;
        --vsix) vsix=$2; shift 2 ;;
        --wasm) wasm=$2; shift 2 ;;
        --dry-run) dry_run=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
done

if [ -z "$version" ] || [ -z "$out" ]; then
    usage >&2
    exit 2
fi

expected=$(python3 -B -c 'import json; print(json.load(open("extensions/shared/release.json"))["version"])')
if [ "$version" != "$expected" ]; then
    echo "version $version does not match extensions/shared/release.json ($expected)" >&2
    exit 1
fi

run() {
    if [ "$dry_run" -eq 1 ]; then
        printf '+ %s\n' "$*"
    else
        "$@"
    fi
}

require_command() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "required command not found: $1" >&2
        exit 1
    fi
}

require_command tar
require_command unzip
require_command sha256sum
require_command python3

if [ "$dry_run" -eq 1 ]; then
    tmp="<tmp>"
else
    tmp=$(mktemp -d "${TMPDIR:-/tmp}/neutron-extensions.XXXXXX")
    trap 'rm -rf "$tmp"' EXIT
fi

# VS Code
if [ -z "$vsix" ]; then
    run npm ci --prefix extensions/vscode
    run npm run package --prefix extensions/vscode
    vsix="extensions/vscode/neutron-$version.vsix"
fi
run cp "$vsix" "$tmp/neutron-$version.vsix"

# Zed
if [ -z "$wasm" ]; then
    run cargo build --locked --manifest-path extensions/zed/Cargo.toml --target wasm32-wasip1 --release
    wasm="extensions/zed/target/wasm32-wasip1/release/neutron_zed.wasm"
fi
run cp "$wasm" "$tmp/neutron-zed-$version.wasm"

# Archives
run tar -czf "$tmp/neutron-zed-$version.tar.gz" \
    --exclude=target --exclude=grammars --exclude='*.wasm' -C extensions/zed .
run tar -czf "$tmp/neutron-nvim-$version.tar.gz" \
    --exclude=__pycache__ --exclude='*.pyc' -C extensions/nvim .
run tar -czf "$tmp/neutron-vim-$version.tar.gz" \
    --exclude=__pycache__ --exclude='*.pyc' -C extensions/other/vim .
run tar -czf "$tmp/neutron-emacs-$version.tar.gz" \
    --exclude=__pycache__ --exclude='*.pyc' --exclude='*.elc' -C extensions/emacs .
run tar -czf "$tmp/neutron-helix-$version.tar.gz" \
    --exclude=__pycache__ --exclude='*.pyc' -C extensions/other/helix .
run tar -czf "$tmp/neutron-extensions-$version.tar.gz" \
    --exclude=node_modules --exclude=target --exclude=grammars --exclude=dist --exclude=__pycache__ \
    --exclude='*.pyc' --exclude='*.vsix' --exclude='*.elc' --exclude='*.wasm' \
    --exclude='*.so' --exclude=.git -C extensions .

run python3 -B scripts/zip-directory.py "$tmp/LSP-neutron-$version.zip" extensions/other/sublime
run cp "$tmp/LSP-neutron-$version.zip" "$tmp/neutron-sublime-$version.zip"

if [ "$dry_run" -eq 0 ]; then
    tar_list() {
        tar -tzf "$1" | sed 's|^\./||'
    }

    zip_list() {
        unzip -Z1 "$1" | sed 's|^\./||'
    }

    require_tar() {
        if ! tar_list "$1" | grep -qx "$2"; then
            echo "missing $2 in $1" >&2
            exit 1
        fi
    }

    require_zip() {
        if ! zip_list "$1" | grep -qx "$2"; then
            echo "missing $2 in $1" >&2
            exit 1
        fi
    }

    forbid_tar() {
        if tar_list "$1" | grep -Eq '(^|/)(node_modules|target|__pycache__)/|\.vsix$|\.elc$'; then
            echo "forbidden entry in $1" >&2
            exit 1
        fi
    }

    embedded=$(unzip -p "$tmp/neutron-$version.vsix" extension/package.json \
        | python3 -B -c 'import json,sys; print(json.load(sys.stdin)["version"])')
    if [ "$embedded" != "$version" ]; then
        echo "VSIX version $embedded does not match $version" >&2
        exit 1
    fi
    if ! unzip -Z1 "$tmp/neutron-$version.vsix" | grep -qx 'extension/syntaxes/neutron.tmLanguage.json'; then
        echo "VSIX is missing its grammar" >&2
        exit 1
    fi

    require_tar "$tmp/neutron-zed-$version.tar.gz" extension.toml
    require_tar "$tmp/neutron-zed-$version.tar.gz" Cargo.toml
    require_tar "$tmp/neutron-zed-$version.tar.gz" src/lib.rs
    require_tar "$tmp/neutron-zed-$version.tar.gz" languages/neutron/config.toml
    require_tar "$tmp/neutron-nvim-$version.tar.gz" plugin/neutron.lua
    require_tar "$tmp/neutron-nvim-$version.tar.gz" lua/neutron/init.lua
    require_tar "$tmp/neutron-nvim-$version.tar.gz" lua/neutron/contract.lua
    require_tar "$tmp/neutron-nvim-$version.tar.gz" syntax/neutron.vim
    require_tar "$tmp/neutron-vim-$version.tar.gz" plugin/neutron.vim
    require_tar "$tmp/neutron-vim-$version.tar.gz" autoload/neutron.vim
    require_tar "$tmp/neutron-vim-$version.tar.gz" autoload/neutron/contract.vim
    require_tar "$tmp/neutron-vim-$version.tar.gz" syntax/neutron.vim
    require_tar "$tmp/neutron-emacs-$version.tar.gz" neutron-mode.el
    require_tar "$tmp/neutron-emacs-$version.tar.gz" neutron-data.el
    require_tar "$tmp/neutron-emacs-$version.tar.gz" LICENSE
    require_tar "$tmp/neutron-helix-$version.tar.gz" languages.toml
    require_tar "$tmp/neutron-helix-$version.tar.gz" runtime/queries/neutron/highlights.scm
    require_zip "$tmp/LSP-neutron-$version.zip" plugin.py
    require_zip "$tmp/LSP-neutron-$version.zip" contract.json
    require_zip "$tmp/LSP-neutron-$version.zip" LSP-neutron.sublime-settings
    require_zip "$tmp/LSP-neutron-$version.zip" Neutron.tmLanguage
    forbid_tar "$tmp/neutron-extensions-$version.tar.gz"

    python3 -B - "$tmp" "$version" <<'PY'
import json
import subprocess
import sys
from pathlib import Path

out = Path(sys.argv[1])
version = sys.argv[2]
release = json.loads(Path("extensions/shared/release.json").read_text())
commit = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
artifacts = sorted(
    path.name
    for path in out.iterdir()
    if path.name not in {"SHA256SUMS", "extensions-manifest.json"}
)
manifest = {
    "version": version,
    "tag": f"extensions-v{version}",
    "commit": commit,
    "grammar": release["grammar"],
    "artifacts": artifacts,
}
(out / "extensions-manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
PY

    (
        cd "$tmp"
        rm -f SHA256SUMS
        sha256sum -- * > SHA256SUMS
        sha256sum -c SHA256SUMS
    )

    rm -rf "$out"
    mv "$tmp" "$out"
    tmp=
    echo "extension artifacts written to $out"
else
    echo "+ write extensions-manifest.json and SHA256SUMS"
    echo "+ move $tmp to $out"
fi
