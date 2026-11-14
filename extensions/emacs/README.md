# Neutron for Emacs

Requires Emacs 29.1 or later, including its built-in Eglot client.

```elisp
(add-to-list 'load-path "/absolute/path/to/ntsc/extensions/emacs")
(require 'neutron-mode)
```

Opening `.nt` files selects `neutron-mode`, recognizes the nearest
`neutron.toml` as an Emacs project, and starts Eglot. Native completion-at-point,
Eldoc signature/hover help, Xref definition/references, Flymake diagnostics,
Eglot rename/code actions, and formatting use the shared server.

```elisp
(setq neutron-server-command '("/absolute/path/to/ntsc" "lsp"))
(setq neutron-cli-command "/absolute/path/to/ntsc")
```

For the standalone server, use `neutron-server-command '("ntsc-lsp")`.
Set `neutron-enable-eglot` to `nil` before loading the mode to start Eglot
manually. Do not enable another Neutron `lsp-mode` client in the same buffers.

| Command | Mode key |
| --- | --- |
| `neutron-build` | `C-c C-b` |
| `neutron-run` | `C-c C-r` |
| `neutron-test` | `C-c C-t` |
| `neutron-format` | `C-c C-f` |
| `neutron-restart` | `M-x neutron-restart` |

Compiler workflows prompt to save modified project buffers and refuse to run
when project buffers remain unsaved. Output uses Emacs' compilation buffer.
Eglot support varies by Emacs version; for example, semantic-token and inlay
hint presentation is not identical to VS Code.

```console
emacs -Q --batch -L extensions/emacs -l extensions/tests/emacs.el
```
