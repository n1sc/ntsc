# Other popular editors

These adapters use the same language server as the primary editor extensions:

- [Helix](helix/README.md): native LSP, shared highlighting grammar, workflow bindings.
- [Sublime Text](sublime/README.md): package for Sublime's LSP client and Tools menu.
- [Vim](vim/README.md): vim-lsp registration, syntax, terminal workflows.

For another LSP editor, configure the canonical values from
[`../shared/contract.json`](../shared/contract.json): `neutron`, `.nt`,
`neutron.toml`, `ntsc lsp`, stdio, and UTF-16. Delegate language intelligence to
that server; do not duplicate its API lists or receiver inference.
