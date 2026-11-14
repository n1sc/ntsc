vim.bo.commentstring = "# %s"
vim.bo.expandtab = true
vim.bo.shiftwidth = 4
vim.bo.softtabstop = 4
vim.bo.omnifunc = "v:lua.vim.lsp.omnifunc"
vim.b.undo_ftplugin = "setlocal commentstring< expandtab< shiftwidth< softtabstop< omnifunc<"
