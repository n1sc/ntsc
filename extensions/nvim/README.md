# Neutron for Neovim

Requires Neovim 0.11 or later; no third-party LSP/completion plugin is required.

With `lazy.nvim` and a checked-out repository:

```lua
{
  dir = "/absolute/path/to/ntsc/extensions/nvim",
  ft = "neutron",
  init = function()
    vim.filetype.add({ extension = { nt = "neutron" } })
  end,
  config = function()
    require("neutron").setup()
  end,
}
```

Without a plugin manager:

```lua
vim.opt.runtimepath:append("/absolute/path/to/ntsc/extensions/nvim")
require("neutron").setup({
  cmd = { "ntsc", "lsp" },
  cli = "ntsc",
})
```

Set `cmd = { "/path/to/ntsc-lsp" }` for the standalone server. Set
`completion = false` if your existing completion UI manages LSP completion.
Do not also register `ntsc` through another `lspconfig` configuration.

`:NeutronBuild`, `:NeutronRun`, and `:NeutronTest` save buffers and open a
terminal at the nearest manifest. `:NeutronFormat` uses LSP formatting;
`:NeutronRestart` restarts the client.

The plugin enables native automatic completion and inlay hints when advertised
by the server. Use native `vim.lsp.buf.hover`, `definition`, `references`,
`rename`, `signature_help`, and `code_action`, and `vim.diagnostic`.
It intentionally does not override your LSP navigation keymaps.
