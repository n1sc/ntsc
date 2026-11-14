# Neutron for Sublime Text

Requires Sublime Text 4 and the **LSP** package from Package Control.
From **Preferences: Browse Packages**, create a directory named exactly
`LSP-neutron` and copy this directory's contents into it. Do not install a
second custom `ntsc` LSP configuration.

The package registers `.nt` syntax and the `LSP-neutron` client. The LSP package
handles native completion, hover, navigation, references, rename, signatures,
diagnostics, actions, and formatting as supported by the installed version.

Override server settings with **Preferences: LSP Settings** / the client
settings file:

```json
{
  "command": ["/absolute/path/to/ntsc", "lsp"]
}
```

For the standalone server, use `"command": ["/path/to/ntsc-lsp"]`.
Set `"cli": "/absolute/path/to/ntsc"` in user `Neutron.sublime-settings` if
compiler commands need a different executable.

The **Tools → Neutron** menu offers Build, Run, Test, Format, and Restart.
The compiler menu commands find the nearest manifest, save open files, refuse
to run with unsaved file-backed buffers, and use an argv list in Sublime's
output panel. Open a source file to select a project in a multi-folder window.
The optional `.sublime-build` definitions assume the opened project folder is
the Neutron root; use the Tools menu for nested projects.
