#!/bin/sh
set -eu

usage() {
    cat <<'EOF'
usage: publish-zed.sh --version X.Y.Z [--dry-run]

Opens or updates a PR against zed-industries/extensions for the release tag.
Requires GH_TOKEN and a fork. Override the fork with ZED_FORK_OWNER (default
n1sc) or ZED_FORK_REPO (default OWNER/extensions).
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

tag="extensions-v$version"
fork_owner=${ZED_FORK_OWNER:-n1sc}
fork_repo=${ZED_FORK_REPO:-$fork_owner/extensions}
upstream="zed-industries/extensions"
branch="neutron-v$version"

if [ "$dry_run" -eq 0 ] && [ -z "${GH_TOKEN:-}" ]; then
    echo "GH_TOKEN is required to open the Zed registry PR" >&2
    exit 1
fi

if ! git rev-parse --verify --quiet "$tag^{commit}" >/dev/null; then
    echo "tag $tag does not exist" >&2
    exit 1
fi

if [ "$dry_run" -eq 1 ]; then
    cat <<EOF
+ fork/clone $fork_repo
+ git checkout -B $branch upstream/main
+ git submodule add https://github.com/n1sc/ntsc.git extensions/neutron
+ git -C extensions/neutron checkout $tag
+ extensions.toml:
[neutron]
submodule = "extensions/neutron"
path = "extensions/zed"
version = "$version"
+ pnpm sort-extensions
+ gh pr create --repo $upstream --head $fork_owner:$branch
EOF
    exit 0
fi

work=$(mktemp -d "${TMPDIR:-/tmp}/neutron-zed-registry.XXXXXX")
trap 'rm -rf "$work"' EXIT

if command -v gh >/dev/null 2>&1; then
    gh auth setup-git
fi

gh repo fork "$upstream" --clone=false --remote=false >/dev/null 2>&1 || true
gh repo clone "$fork_repo" "$work/extensions" -- --depth=1

cd "$work/extensions"
git remote add upstream "https://github.com/$upstream.git" 2>/dev/null || true
git fetch upstream main
git checkout -B "$branch" upstream/main

if [ -d extensions/neutron ]; then
    git -C extensions/neutron fetch --tags origin
else
    git submodule add https://github.com/n1sc/ntsc.git extensions/neutron
fi
git -C extensions/neutron checkout "$tag"

python3 -B - "$version" <<'PY'
import sys
from pathlib import Path

version = sys.argv[1]
path = Path("extensions.toml")
block = (
    "[neutron]\n"
    'submodule = "extensions/neutron"\n'
    'path = "extensions/zed"\n'
    f'version = "{version}"\n'
)
output = []
inside = False
replaced = False
for line in path.read_text().splitlines(keepends=True):
    if line.strip() == "[neutron]":
        if not replaced:
            output.append(block)
            replaced = True
        inside = True
        continue
    if inside and line.startswith("["):
        inside = False
    if not inside:
        output.append(line)
if not replaced:
    output.append("\n" + block)
path.write_text("".join(output))
PY

if command -v corepack >/dev/null 2>&1; then
    corepack enable pnpm >/dev/null 2>&1 || true
fi
if ! command -v pnpm >/dev/null 2>&1; then
    echo "pnpm is required to sort the Zed extension registry" >&2
    exit 1
fi
pnpm install --frozen-lockfile
pnpm sort-extensions

git add .gitmodules extensions/neutron extensions.toml
if git diff --cached --quiet; then
    echo "Zed registry metadata is already current for $version"
else
    git commit -m "Add Neutron $version"
    git push --force origin "$branch"
fi

existing=$(gh pr list --repo "$upstream" --head "$fork_owner:$branch" --json number --jq '.[0].number // empty')
if [ -n "$existing" ]; then
    gh pr comment "$existing" --repo "$upstream" --body "Updated to Neutron $version."
    echo "updated Zed registry PR #$existing"
else
    gh pr create --repo "$upstream" --head "$fork_owner:$branch" \
        --title "Add Neutron extension $version" \
        --body "Adds the Neutron language extension from $tag. The submodule is pinned to the release commit and the grammar revision is pinned in extensions/zed/extension.toml."
    echo "opened Zed registry PR for $version"
fi
