# Language server and editor integration

The Neutron language server is included with the compiler. It provides
completion, hover, definitions, references, rename, diagnostics, signature
help, symbols, semantic tokens, and formatting for `.nt` files.

The online documentation is at
[ntsc.yasakei.dev/docs](https://ntsc.yasakei.dev/docs).
Unreleased editor improvements are documented under
[Next](https://ntsc.yasakei.dev/docs/next/reference/lsp); a numbered documentation
snapshot describes only its pinned compiler release.

## Start the server

Configure your editor's LSP client to run either command:

```console
ntsc lsp
```

```console
ntsc-lsp
```

Both speak LSP over standard input and output. **Do not start the server as a
background terminal command and expect an editor to discover it.** The editor
must launch it and own those streams. Log messages go to standard error;
standard output is reserved for protocol messages.

Use `ntsc --version` to check the compiler version. `ntsc-lsp` is a protocol
server, not a command-line version browser.

## Client configuration

Use these settings when configuring an editor client:

| Setting | Value |
| --- | --- |
| Executable and arguments | `ntsc`, `lsp`, or the standalone `ntsc-lsp` executable |
| Source extension | `.nt` |
| Suggested language/filetype ID | `neutron` |
| Project marker | `neutron.toml` |
| Transport | stdio |
| Position encoding | UTF-16 |
| Document synchronization | Incremental; full-document replacements are also accepted |

The compiler must be on the editor's `PATH`, not just the path available to an
interactive shell. Restart the language server after replacing its executable.
Run only one Neutron language server per editor workspace; two clients can
publish conflicting diagnostics and duplicate suggestions.

### Neovim 0.11 and later

The native LSP client can launch the server without a custom Neutron plugin:

```lua
vim.filetype.add({ extension = { nt = "neutron" } })

vim.lsp.config("ntsc", {
  cmd = { "ntsc", "lsp" },
  filetypes = { "neutron" },
  root_markers = { "neutron.toml", ".git" },
})

vim.lsp.enable("ntsc")
```

If Neutron is not on the editor's path, replace `"ntsc"` with its absolute
path. Configure your completion UI and snippet engine separately. The server
offers snippet syntax only when the client advertises snippet support.

### VS Code and other clients

Use a language-client extension that associates `.nt` files with Neutron and
launches `ntsc lsp` over stdio. VS Code does **not** launch arbitrary language
servers merely because an executable is named in an invented `settings.json`
key. Check the installed extension's documented executable setting.

Other LSP-capable editors use the same command, project marker, and file
association shown above.

## Project-aware completion

Open the directory containing `neutron.toml`. The server follows the configured
entry file and its Neutron imports, including vendored packages in `.ntsc/`.
An open editor buffer takes precedence over its saved file.

Member suggestions are based on the receiver:

```ntsc
class Counter {
    var int value

    fun read() -> int {
        return this.value;
    }
}

fun make_counter() -> Counter {
    return Counter();
}

fun main() {
    var counter = make_counter();
    say(counter.read())
}
```

Completion after `counter.` offers `Counter`'s members, not every method in
the workspace. The server tracks explicit annotations, constructors, declared
factory return types, chained receivers, inherited members, and lexical
shadowing. A value with an unknown type does not receive an invented member
list. An unawaited future is not treated as the object its async function
eventually returns.

Definitions and rename use the resolved receiver and declaration identity.
Two unrelated classes having a method named `read` does not make those methods
the same rename target. File-private declarations stay private.

Completion requests may recover an incomplete expression temporarily, but that
recovery does not modify your document or become a fabricated saved declaration.

## Rust dependencies and built projects

For a project using `use @crate`, first build it normally:

```console
ntsc build
```

The server reads the generated Rust API manifests:

```text
.ntsc/<crate>/<crate>.export.json
```

For example, after `use @ureq`, completion after `@ureq.` uses the project's
cached exported function names, parameter types, return types, async flags,
and documentation. Unsupported exports are not offered as callable functions.
Hover and signature help use that same metadata.

The language server **does not run Cargo, download dependencies, execute your
program, or regenerate adaptors in the background**. Missing or stale metadata
limits what it can verify. Rebuild after changing Rust dependency versions,
features, or adaptor configuration; then save a source file or restart the
server if your client does not support workspace file notifications.

Rust API declarations belong to the dependency. Rename is for Neutron source
symbols, not for rewriting generated Rust adaptor APIs.

## Large workspaces and changes

Unchanged imported files and Rust manifests are cached instead of being parsed
again on every edit. Source edits, saves, workspace changes, and supported file
notifications refresh the relevant information. Multiple edits in one
notification are applied in order using UTF-16 positions; stale document
versions are not allowed to replace newer buffers.

To bound memory use, disk-loaded Neutron imports are limited to 256 files and
4 MB per file. Rust manifests are limited to 32 MiB each. Open buffers remain
available independently of the disk-import cache.

## Troubleshooting

| Symptom | Check |
| --- | --- |
| No server starts | Run `ntsc --version` from the editor's environment and check its LSP log. |
| Rust suggestions are missing | Check `use @crate`, run `ntsc build`, and verify the project-local export manifest exists. |
| Imported names are missing | Check the configured entry, import paths, and `.ntsc/` package sources. |
| Suggestions describe an old API | Rebuild the project's adaptors and restart the server. |
| Duplicate diagnostics | Disable a second Neutron language-client extension or workspace server. |
| A giant import is omitted | Check the disk-file limits and the server's standard-error log. |

Editor analysis is not a replacement for the compiler's complete build,
ownership checks, generic lowering, and test suite. Unknown or incomplete
information should remain unknown rather than produce confidently wrong
suggestions. Before releasing a program, run `ntsc build` and `ntsc test`.

When reporting a server bug, include `ntsc --version`, the editor/client
version, a minimal source example, its `neutron.toml`, the request position,
and relevant server logs. Do not include tokens, credentials, or `.env` files.
