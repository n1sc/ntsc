# Neutron for Helix

Merge `languages.toml` into `~/.config/helix/languages.toml`. Do not replace
your existing language configuration. Copy `runtime/queries/neutron` to
`~/.config/helix/runtime/queries/neutron`.

The remote grammar needs a commit containing the shared generated parser.
For a local checkout before publication:

```console
python3 extensions/other/helix/prepare.py
```

Merge the printed local configuration instead. Then run:

```console
hx --grammar fetch
hx --grammar build
hx --health neutron
```

Open Helix **from the directory containing `neutron.toml`**. Native completion,
hover, navigation, rename, diagnostics, signature help, and `:format` all use
`ntsc lsp`. `:lsp-restart` restarts it.

Optionally merge `config.toml` into your existing Helix configuration. The
`Space n` prefix offers `b` Build, `r` Run, `t` Test, `f` Format, and `s` Restart.
Build/Run/Test use `:write-all` before invoking the compiler in Helix's current
directory. They are not executed on open or on save.

To use a standalone language server:

```toml
[language-server.ntsc]
command = "/path/to/ntsc-lsp"
args = []
```

If the compiler is not on PATH, also adjust the optional workflow bindings.
Helix exposes only the LSP capabilities implemented by that Helix version;
the shared grammar supplies lexical highlighting independently.
