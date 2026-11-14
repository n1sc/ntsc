#!/bin/sh
set -eu

usage() {
    cat <<'EOF'
usage: publish-vscode.sh --version X.Y.Z --vsix PATH [--marketplace|--openvsx] [--dry-run]

Publishes the VSIX to the VS Code Marketplace and/or Open VSX. Requires
VSCE_PAT and OVSX_PAT for the selected targets.
EOF
}

version=
vsix=
target=both
dry_run=0

while [ "$#" -gt 0 ]; do
    case "$1" in
        --version) version=$2; shift 2 ;;
        --vsix) vsix=$2; shift 2 ;;
        --marketplace) target=marketplace; shift ;;
        --openvsx) target=openvsx; shift ;;
        --dry-run) dry_run=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
done

if [ -z "$version" ] || [ -z "$vsix" ]; then
    usage >&2
    exit 2
fi

if [ ! -f "$vsix" ]; then
    echo "VSIX not found: $vsix" >&2
    exit 1
fi

embedded=$(unzip -p "$vsix" extension/package.json \
    | python3 -B -c 'import json,sys; print(json.load(sys.stdin)["version"])')
if [ "$embedded" != "$version" ]; then
    echo "VSIX version $embedded does not match $version" >&2
    exit 1
fi

vsce() {
    if [ -x extensions/vscode/node_modules/.bin/vsce ]; then
        extensions/vscode/node_modules/.bin/vsce "$@"
    else
        npx --yes @vscode/vsce "$@"
    fi
}

published() {
    python3 -B -c 'import json,sys
try:
    print(json.load(sys.stdin).get("version", ""))
except Exception:
    print("")'
}

if [ "$target" != "openvsx" ]; then
    if [ "$dry_run" -eq 1 ]; then
        echo "+ vsce publish --packagePath $vsix"
    else
        [ -n "${VSCE_PAT:-}" ] || { echo "VSCE_PAT is required" >&2; exit 1; }
        current=$(vsce show n1sc.neutron --json 2>/dev/null | published || true)
        if [ "$current" = "$version" ]; then
            echo "VS Code Marketplace already has $version"
        else
            vsce publish --packagePath "$vsix" --pat "$VSCE_PAT"
        fi
    fi
fi

if [ "$target" != "marketplace" ]; then
    if [ "$dry_run" -eq 1 ]; then
        echo "+ ovsx publish $vsix"
    else
        [ -n "${OVSX_PAT:-}" ] || { echo "OVSX_PAT is required" >&2; exit 1; }
        current=$(npx --yes ovsx get n1sc.neutron --json 2>/dev/null | published || true)
        if [ "$current" = "$version" ]; then
            echo "Open VSX already has $version"
        else
            npx --yes ovsx publish "$vsix" -p "$OVSX_PAT"
        fi
    fi
fi
