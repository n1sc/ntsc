# Releasing editor integrations

Editor extensions release independently from the compiler. Compiler tags use
`v*`; extension tags use `extensions-v*`. The workflows are separate:
`.github/workflows/release.yml` and `.github/workflows/extensions-release.yml`.

## Version source

`extensions/shared/release.json` is the single source of truth for the shared
version and the grammar pin. `scripts/extensions-version.py check` validates
every manifest; `prepare` updates them.

## Prepare a release

Commit grammar changes first. The release commit cannot reference itself, so
`prepare` pins the current `HEAD` (the parent of the release commit).

```console
VERSION=0.2.0 arq extensions_prepare
git diff
git add extensions
git commit -m "chore(extensions): release 0.2.0"
git tag -a extensions-v0.2.0 -m "Neutron editor extensions 0.2.0"
git push origin HEAD
git push origin extensions-v0.2.0
```

Set `GRAMMAR_REV=<sha>` to pin a different commit. `prepare` refuses a dirty
`extensions/shared/tree-sitter-neutron` subtree.

## Verification

### Artifact-only testing

Manual runs default to `dry_run=true`. To test an untagged branch:

```console
gh workflow run extensions-release.yml --ref main -f ref=main -F dry_run=true
```

Replace `main` with the branch to test. Verification and packaging use the
resolved commit, and `extensions-dist` is available in the run's Actions
artifacts. No GitHub Release, registry PR, or mirror push is created.
Branch dry runs allow the development grammar pin; actual releases still
require the strict commit pin and matching `extensions-vX.Y.Z` tag.
To test an existing release tag, pass `-f tag=extensions-vX.Y.Z` instead.
Do not push a new release tag just to test: tag pushes publish normally.

The workflow runs:

- `scripts/extensions-version.py check --strict-grammar --expect-tag`
- `extensions/shared/generate.py --check`
- `scripts/check-extensions.sh` (contracts, Node, native Neovim/Vim/Emacs)
- VS Code packaging, Tree-sitter regeneration, and the Zed WASM build
- archive contents and `SHA256SUMS`

The GitHub Release is created only after verification and packaging succeed.

## Job graph

```
meta -> verify -> package -> release -> publish (environment: registry-publish)
```

`publish` is the only job that pushes to registries. It waits for a required
reviewer on the `registry-publish` environment, so no registry PR or mirror push
happens without human approval.

## One-time setup

Create mirror repos and forks:

- `n1sc/neutron-mode` (Emacs mirror)
- `n1sc/LSP-neutron` (Sublime mirror; the name is required by
  `Packages/LSP-neutron/contract.json`)
- forks of `zed-industries/extensions`, `melpa/melpa`, and
  `wbond/package_control_channel`

Create the protected environment `registry-publish` with required reviewers and
a tag rule for `extensions-v*`. Add environment secrets:

- `VSCE_PAT` (VS Code Marketplace)
- `OVSX_PAT` (Open VSX)
- `REGISTRY_GITHUB_TOKEN` (classic PAT with `public_repo` for mirrors and PRs)

Confirm the `n1sc` publisher/namespace and the Zed extension ID `neutron`.

## Registry behavior

- VS Code Marketplace and Open VSX publish the VSIX; an existing version is
  skipped.
- Zed opens or updates a PR against `zed-industries/extensions` using the
  release commit as a submodule and `path = "extensions/zed"`.
- MELPA pushes the Emacs mirror and opens the recipe PR once; later releases
  only push a new mirror tag.
- Package Control pushes the Sublime mirror and opens the channel entry once;
  later releases only push a new mirror tag.

## Rollback

- Delete the GitHub Release if the artifacts are wrong.
- Marketplace versions are immutable; bump the version instead of retrying.
- Never move a published `extensions-v*` tag or a mirror tag. Publish a new
  version.
- Close the registry PR if it has not merged.

## Troubleshooting

- `prepare` refuses a dirty grammar subtree: commit grammar changes first.
- `git subtree split` fails on a shallow clone: fetch full history.
- `pnpm sort-extensions` is missing: install Node 22 and run
  `corepack enable pnpm`.
- Zed's docs mention `wasm32-wasip2`; the release verification currently builds
  `wasm32-wasip1`. If Zed requires `wasip2`, update `extensions.yml`,
  `extensions/zed/README.md`, and `extensions/README.md` together.
