if vim.fn.has("nvim-0.11") == 0 then
  vim.notify("Neutron requires Neovim 0.11 or later", vim.log.levels.ERROR)
  return
end

vim.filetype.add({ extension = { nt = "neutron" } })
