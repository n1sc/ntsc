#!/bin/sh
set -eu

usage() {
    cat <<'EOF'
usage: publish-melpa.sh --version X.Y.Z [--dry-run]

Pushes the Emacs mirror and opens the one-time MELPA recipe PR when missing.
Requires GH_TOKEN. Override the fork owner with MELPA_FORK_OWNER (default n1sc).
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

mirror_args="--prefix extensions/emacs --repo n1sc/neutron-mode --version $version"
if [ "$dry_run" -eq 1 ]; then
    sh scripts/publish-mirror.sh $mirror_args --dry-run
    echo "+ add recipes/neutron-mode when missing"
    exit 0
fi

sh scripts/publish-mirror.sh $mirror_args

if gh api "repos/melpa/melpa/contents/recipes/neutron-mode" >/dev/null 2>&1; then
    echo "MELPA recipe already exists"
    exit 0
fi

fork_owner=${MELPA_FORK_OWNER:-n1sc}
branch="neutron-mode-$version"
work=$(mktemp -d "${TMPDIR:-/tmp}/neutron-melpa.XXXXXX")
trap 'rm -rf "$work"' EXIT

gh repo fork melpa/melpa --clone=false --remote=false >/dev/null 2>&1 || true
gh repo clone "$fork_owner/melpa" "$work/melpa" -- --depth=1
cd "$work/melpa"
git checkout -b "$branch"

cat > recipes/neutron-mode <<'EOF'
(neutron-mode
 :fetcher github
 :repo "n1sc/neutron-mode"
 :files ("neutron-mode.el" "neutron-data.el" "LICENSE"))
EOF

git add recipes/neutron-mode
git commit -m "Add neutron-mode"
git push --force origin "$branch"
gh pr create --repo melpa/melpa --head "$fork_owner:$branch" \
    --title "Add neutron-mode" \
    --body "Adds the Neutron Emacs major mode. The package is NPL-1.1, a permissive GPL-compatible license."
echo "opened MELPA recipe PR for $version"
