# Neutron editor integrations

These integrations all use **the same `ntsc lsp` server**, not independent
completion engines. Receiver-aware suggestions, cached Rust API metadata,
diagnostics, hover, definitions, references, rename, signature help, formatting,
and other advertised LSP capabilities therefore come from one implementation.
Their presentation depends on the editor's native LSP client; an editor that
does not implement a capability cannot expose it merely by installing a plugin.

| Editor | Integration | Minimum / dependency |
| --- | --- | --- |
| [VS Code](vscode/README.md) | Language-client extension, syntax, project tasks | VS Code 1.82; Node 22 for packaging |
| [Zed](zed/README.md) | Native Rust/WASM extension, shared Tree-sitter grammar, tasks | Zed with extension API 0.7 support |
| [Neovim](nvim/README.md) | Native LSP client, automatic completion, syntax, commands | Neovim 0.11 |
| [Emacs](emacs/README.md) | Major mode, native Eglot, project recognition, compilation | Emacs 29.1 |
| [Helix](other/helix/README.md) | Native LSP configuration, shared grammar, optional bindings | Helix with native LSP |
| [Sublime Text](other/sublime/README.md) | LSP client package, syntax, explicit workflows | Sublime Text 4 and the LSP package |
| [Vim](other/vim/README.md) | vim-lsp registration, syntax, terminal commands | Vim 8.2 with `+terminal`; vim-lsp |

## Shared behavior and workflow

1. Install the compiler and make `ntsc` available on the **editor's** `PATH`.
   The extensions do not bundle or automatically download compiler binaries.
2. Open a directory containing `neutron.toml` and edit `.nt` files.
3. The editor launches `ntsc lsp` over stdio. Completion and analysis do not
   build, download dependencies, run your program, or regenerate Rust adaptors.
4. Use **Build**, **Run**, or **Test** explicitly. These save sources and invoke
   `ntsc build`, `ntsc run`, or `ntsc test` at the project root. Zed and Helix
   use the opened worktree directory, so open the actual Neutron project root.
   VS Code, Neovim, Vim, Emacs, and the Sublime menu find the nearest manifest.
5. **Format** uses `textDocument/formatting`, not an editor-specific formatter.
   **Restart** reconnects the same language server after executable changes.
6. For Rust dependency completion, build the project explicitly once to create
   `.ntsc/<crate>/<crate>.export.json`. The server then reads that cache.

Use only one Neutron LSP integration in each editor. Remove a previous manual
`ntsc` LSP registration before enabling a supplied plugin.

Every integration supports configuring the executable. For `ntsc-lsp`, use an
empty argument list; for `ntsc`, use `["lsp"]`. The compiler command for
Build/Run/Test remains `ntsc`, even when the standalone server is used.
No integration turns on format-on-save globally or replaces unrelated
language clients/keybindings.

## Source of truth

[`shared/contract.json`](shared/contract.json) defines the language identity,
project marker, stdio command, and explicit compiler workflows.
[`shared/generate.py`](shared/generate.py) reads keywords from the compiler's
`TokenKind::keyword_lexeme` and generates client metadata and lexical syntax.
The shared Tree-sitter grammar is **lexical highlighting**, not a second
semantic parser. The LSP remains authoritative.

After editing shared metadata or compiler keywords:

```console
python3 extensions/shared/generate.py
npm ci --prefix extensions/shared/tree-sitter-neutron
npm run generate --prefix extensions/shared/tree-sitter-neutron
```

Generated parser C and headers are committed so Zed and Helix can compile the
grammar without requiring the grammar generator at install time.
The generated Tree-sitter support headers retain their upstream MIT notice in
`shared/tree-sitter-neutron/LICENSE.tree-sitter`.
Remote grammar manifests use the repository's `main` branch; before publishing
an editor-store release, pin both Zed and Helix to a commit containing the
generated grammar. Local development instructions do not require publishing
or committing your working tree.

## Verification

```console
arq test_extensions
npm ci --prefix extensions/vscode
npm test --prefix extensions/vscode
npm run package --prefix extensions/vscode
npm ci --prefix extensions/shared/tree-sitter-neutron
npm test --prefix extensions/shared/tree-sitter-neutron
cargo clippy --manifest-path extensions/zed/Cargo.toml --all-targets -- -D warnings
rustup target add wasm32-wasip1
cargo build --manifest-path extensions/zed/Cargo.toml --target wasm32-wasip1 --release
```

The routine extension task always validates metadata and runs Node/Python
tests. Native Neovim, Vim, and Emacs checks run when those editors are installed;
skips are printed. Neovim's integration test starts the actual `ntsc-lsp`
binary and checks typed receiver suggestions and signature shadowing.
VS Code also has an extension-host smoke test in `vscode/test/host.js`; see its
README. These source integrations are not automatically published to stores.
