<p align="center"><img src="icons/icon.png" alt="Neutron" width="96" height="96"></p>

# Neutron for VS Code

## Build and install

From the repository root:

```console
npm ci --prefix extensions/vscode
npm run package --prefix extensions/vscode
code --install-extension extensions/vscode/neutron-0.1.0.vsix
```

Packaging uses Node 22 or later. Install NTSC separately. Open a trusted,
local Neutron project; the extension does not execute a server or compiler
in restricted/untrusted or virtual workspaces.

The packaging tool's transitive `minimatch` is pinned to a patched release
to avoid the glob-pattern denial-of-service advisory affecting older versions.

## Use

Open a `.nt` file. The extension launches one language client for the VS Code
window and passes the workspace folders to the shared server. Nested projects
and multi-root workspaces do not start overlapping completion providers.

The command palette offers **Neutron: Build**, **Run**, **Test**, **Restart
Language Server**, and **Show Language Server Output**. Build/Run/Test save
sources and use the nearest `neutron.toml`. Use native **Format Document**,
Go to Definition, Find References, Rename Symbol, signature help, inlay hints,
code actions, and the Problems panel. Snippet support is negotiated by the
language client, rather than advertising fabricated editor-specific APIs.
Neutron's default disables VS Code's word-only suggestions so unrelated words
from the buffer do not masquerade as typed member completions. You can override
this language-specific preference in your own settings.

```json
{
  "neutron.serverPath": "/absolute/path/to/ntsc",
  "neutron.serverArgs": ["lsp"],
  "neutron.cliPath": "/absolute/path/to/ntsc"
}
```

To use the standalone server, set `neutron.serverPath` to `ntsc-lsp` and
`neutron.serverArgs` to `[]`. Server settings are window/workspace-wide;
`neutron.cliPath` can be set per workspace folder for tasks.

User tasks can use:

```json
{
  "version": "2.0.0",
  "tasks": [
    { "type": "neutron", "action": "build", "project": ".", "label": "Neutron build" }
  ]
}
```

Task executables and arguments use `ProcessExecution`, not shell concatenation,
so paths containing spaces are preserved.

## Test

```console
cargo build -p ntsc-lsp
npm test --prefix extensions/vscode
code --wait --user-data-dir /tmp/neutron-vscode-test \
  --extensions-dir /tmp/neutron-vscode-test-extensions \
  --extensionDevelopmentPath "$PWD/extensions/vscode" \
  --extensionTestsPath "$PWD/extensions/vscode/test/host.js" \
  --disable-workspace-trust --skip-welcome
cat /tmp/neutron-vscode-host-result.json
```

The isolated extension-host test checks actual receiver completion, absence of
unrelated members, command registration, and reconnection. For normal work,
**do not disable workspace trust**.
