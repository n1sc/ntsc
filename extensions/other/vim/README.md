# Neutron for Vim

Requires Vim 8.2 with `+terminal`, plus
[`prabirshrestha/vim-lsp`](https://github.com/prabirshrestha/vim-lsp).

With vim-plug and a local checkout:

```vim
Plug 'prabirshrestha/vim-lsp'
Plug '/absolute/path/to/ntsc/extensions/other/vim'
```

Or add the directory to `runtimepath` before loading plugins. The adapter
registers the `.nt` filetype as `neutron` and registers one `ntsc` server during
vim-lsp setup. Remove any previous manual registration of the same server.
Installing this adapter deliberately gives Neutron precedence over Vim's
built-in N-Triples association for `.nt`.

```vim
let g:neutron_server_command = ['/absolute/path/to/ntsc', 'lsp']
let g:neutron_cli = '/absolute/path/to/ntsc'
```

For the standalone server use `g:neutron_server_command = ['ntsc-lsp']`.
`:NeutronBuild`, `:NeutronRun`, and `:NeutronTest` save buffers and start a
terminal at the nearest manifest. `:NeutronFormat` and `:NeutronRestart`
delegate to vim-lsp.

Use vim-lsp's native completion, `:LspHover`, `:LspDefinition`,
`:LspReferences`, `:LspRename`, `:LspSignatureHelp`, and diagnostics. Configure
your preferred completion/snippet UI separately; the adapter does not claim
support for editor features that vim-lsp does not implement.
