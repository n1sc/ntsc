#!/bin/sh
set -eu

usage() {
    cat <<'EOF'
usage: publish-mirror.sh --prefix PREFIX --repo OWNER/REPO --version X.Y.Z [--force-retag] [--dry-run]

Splits PREFIX from the extensions-vX.Y.Z tag, pushes it to OWNER/REPO main, and
tags the mirror with vX.Y.Z. Requires GH_TOKEN and full git history.
EOF
}

prefix=
repo=
version=
force_retag=0
dry_run=0

while [ "$#" -gt 0 ]; do
    case "$1" in
        --prefix) prefix=$2; shift 2 ;;
        --repo) repo=$2; shift 2 ;;
        --version) version=$2; shift 2 ;;
        --force-retag) force_retag=1; shift ;;
        --dry-run) dry_run=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
done

if [ -z "$prefix" ] || [ -z "$repo" ] || [ -z "$version" ]; then
    usage >&2
    exit 2
fi

tag="extensions-v$version"
mirror_tag="v$version"
remote="https://github.com/$repo.git"

if [ "$dry_run" -eq 0 ] && [ -z "${GH_TOKEN:-}" ]; then
    echo "GH_TOKEN is required to push mirror repositories" >&2
    exit 1
fi

if [ "$(git rev-parse --is-shallow-repository)" = "true" ]; then
    echo "full git history is required for git subtree split" >&2
    exit 1
fi

if ! git rev-parse --verify --quiet "$tag^{commit}" >/dev/null; then
    echo "tag $tag does not exist" >&2
    exit 1
fi

split=$(git subtree split --prefix="$prefix" "$tag")

assert_entry() {
    if ! git ls-tree --name-only "$split" | grep -qx "$1"; then
        echo "mirror split for $prefix is missing $1" >&2
        exit 1
    fi
}

case "$prefix" in
    extensions/emacs)
        assert_entry neutron-mode.el
        assert_entry neutron-data.el
        assert_entry LICENSE
        ;;
    extensions/other/sublime)
        assert_entry plugin.py
        assert_entry contract.json
        assert_entry LSP-neutron.sublime-settings
        ;;
esac

if [ "$dry_run" -eq 1 ]; then
    echo "+ git push --force $remote $split:refs/heads/main"
    echo "+ git push $remote $split:refs/tags/$mirror_tag"
    echo "split commit: $split"
    exit 0
fi

if command -v gh >/dev/null 2>&1; then
    gh auth setup-git
fi

remote_tag=$(git ls-remote --tags "$remote" "refs/tags/$mirror_tag" | awk '{print $1}')
if [ -n "$remote_tag" ]; then
    if [ "$remote_tag" = "$split" ]; then
        echo "$repo $mirror_tag already points at $split"
        exit 0
    fi
    if [ "$force_retag" -eq 0 ]; then
        echo "$repo $mirror_tag already exists at $remote_tag; pass --force-retag to replace it" >&2
        exit 1
    fi
fi

git push --force "$remote" "$split:refs/heads/main"
git push "$remote" "$split:refs/tags/$mirror_tag"
echo "pushed $prefix to $repo main and tagged $mirror_tag"
