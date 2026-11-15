<p align="center"><img src="icons/icon.png" alt="Neutron" width="96" height="96"></p>

# Neutron for Zed

This is a native extension using Zed extension API 0.7. The language server is
the installed `ntsc lsp`; the extension never downloads compiler binaries.
Shared lexical Tree-sitter highlighting is separate from LSP semantics.

## Install a development build

Until the grammar is published in the repository, prepare a local development
extension:

```console
npm ci --prefix extensions/shared/tree-sitter-neutron
npm run generate --prefix extensions/shared/tree-sitter-neutron
python3 extensions/zed/prepare.py
```

The script prints a directory under `target/editors`. In Zed's command palette,
run **zed: install dev extension** and select that printed directory.
The script creates a throwaway, immutable local Git checkout for the grammar;
it **does not commit your repository** or edit your Zed settings.
Zed builds the Rust extension and grammar using its normal development tools.

Once the grammar is available remotely, the `extensions/zed` directory itself
can be installed as a dev extension. The release workflow pins the grammar
revision automatically; see [`../RELEASING.md`](../RELEASING.md).

## Configure and use

Open the directory containing `neutron.toml`. Completion, navigation, rename,
diagnostics, code actions, signature help, and format use the native LSP UI.
Use **task: spawn** to choose **Neutron: Build**, **Run**, or **Test**. These run
at `$ZED_WORKTREE_ROOT` and request Zed's native `save: all` behavior.

```json
{
  "lsp": {
    "ntsc": {
      "binary": {
        "path": "/absolute/path/to/ntsc",
        "arguments": ["lsp"]
      }
    }
  }
}
```

For the standalone server, use `"path": "/path/to/ntsc-lsp"` and
`"arguments": []`. If compiler workflows need an absolute executable path,
copy the task definitions into your project/user Zed task configuration and
replace their `command`; server binary settings do not change the compiler.
Use Zed's native **Format** and language-server restart controls.

```console
rustup target add wasm32-wasip1
cargo build --manifest-path extensions/zed/Cargo.toml --target wasm32-wasip1 --release
```

The release workflow verifies this target. Zed's registry may use
`wasm32-wasip2`; see [`../RELEASING.md`](../RELEASING.md) before changing it.

The editor crate has its own Cargo workspace/lockfile so it does not pull
Zed/WASM dependencies into the compiler's LLVM-linked workspace.

## License

The Zed extension is released under the MIT license. The Neutron compiler and
the shared Tree-sitter grammar remain under NPL-1.1.
