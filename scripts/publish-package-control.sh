#!/bin/sh
set -eu

usage() {
    cat <<'EOF'
usage: publish-package-control.sh --version X.Y.Z [--dry-run]

Pushes the Sublime mirror and opens the one-time Package Control channel PR
when missing. Requires GH_TOKEN. Override the fork owner with
PACKAGE_CONTROL_FORK_OWNER (default n1sc).
EOF
}

version=
dry_run=0

while [ "$#" -gt 0 ]; do
    case "$1" in
        --version) version=$2; shift 2 ;;
        --dry-run) dry_run=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
done

if [ -z "$version" ]; then
    usage >&2
    exit 2
fi

mirror_args="--prefix extensions/other/sublime --repo n1sc/LSP-neutron --version $version"
if [ "$dry_run" -eq 1 ]; then
    sh scripts/publish-mirror.sh $mirror_args --dry-run
    echo "+ add repository/LSP-neutron.json when missing"
    exit 0
fi

sh scripts/publish-mirror.sh $mirror_args

if gh api "repos/wbond/package_control_channel/contents/repository/LSP-neutron.json" >/dev/null 2>&1; then
    echo "Package Control entry already exists"
    exit 0
fi

fork_owner=${PACKAGE_CONTROL_FORK_OWNER:-n1sc}
branch="lsp-neutron-$version"
work=$(mktemp -d "${TMPDIR:-/tmp}/neutron-package-control.XXXXXX")
trap 'rm -rf "$work"' EXIT

gh repo fork wbond/package_control_channel --clone=false --remote=false >/dev/null 2>&1 || true
gh repo clone "$fork_owner/package_control_channel" "$work/channel" -- --depth=1
cd "$work/channel"
git checkout -b "$branch"

cat > repository/LSP-neutron.json <<'EOF'
{
  "name": "LSP-neutron",
  "details": "https://github.com/n1sc/LSP-neutron",
  "releases": [
    {
      "sublime_text": ">=4000",
      "tags": true
    }
  ]
}
EOF

git add repository/LSP-neutron.json
git commit -m "Add LSP-neutron"
git push --force origin "$branch"
gh pr create --repo wbond/package_control_channel --head "$fork_owner:$branch" \
    --title "Add LSP-neutron" \
    --body "Adds the Neutron language package. The mirror repository publishes semantic-version tags for each release."
echo "opened Package Control channel PR for $version"
