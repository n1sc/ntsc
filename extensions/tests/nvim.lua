local root = vim.fn.getcwd()
vim.opt.runtimepath:prepend(root .. "/extensions/nvim")
vim.cmd("filetype plugin on")
vim.cmd("syntax on")

local directory = vim.fn.tempname() .. " neutron editor"
vim.fn.mkdir(directory .. "/src", "p")
vim.fn.writefile({ '[package]', 'entry = "src/main.nt"', 'output = "editor-test"' }, directory .. "/neutron.toml")
local file = directory .. "/src/main.nt"
local source = "class Item { var int count; fun ping() -> int { return 1; } } class Other { fun unrelated() {} } fun make() -> Item { return Item(); } fun main() { var item = make(); item. }"
vim.fn.writefile({ source }, file)
require("neutron").setup({ cmd = { root .. "/target/debug/ntsc-lsp" }, completion = false })
vim.cmd.edit(vim.fn.fnameescape(file))

local function check(condition, message)
  if not condition then
    error(message)
  end
end

local ok, failure = pcall(function()
  check(vim.bo.filetype == "neutron", "filetype registration failed")
  check(vim.bo.commentstring == "# %s", "comment behavior differs")
  check(vim.bo.shiftwidth == 4, "indentation differs")
  check(vim.wait(15000, function()
    local running = vim.lsp.get_clients({ bufnr = 0, name = "ntsc" })
    return #running == 1 and running[1].initialized
  end), "language server did not attach")
  local client = vim.lsp.get_clients({ bufnr = 0, name = "ntsc" })[1]
  check(client.offset_encoding == "utf-16", "wrong position encoding")
  check(client.config.root_dir == directory, "wrong project root")

  local function request(method, offset)
    vim.api.nvim_win_set_cursor(0, { 1, offset })
    local params = vim.lsp.util.make_position_params(0, client.offset_encoding)
    local response, err = client:request_sync(method, params, 10000, vim.api.nvim_get_current_buf())
    check(response ~= nil, tostring(err))
    check(response.err == nil, vim.inspect(response.err))
    return response.result
  end

  local completion_offset = assert(source:find("item.", 1, true)) + #"item." - 1
  local completions = request("textDocument/completion", completion_offset)
  local labels = {}
  for _, item in ipairs(completions.items or completions) do
    labels[item.label] = true
  end
  check(labels.count and labels.ping, "factory receiver members missing")
  check(not labels.unrelated, "unrelated class leaked into suggestions")

  source = "fun exposed(int value) {} fun main() { var exposed = 1; exposed(); }"
  vim.api.nvim_buf_set_lines(0, 0, -1, false, { source })
  local offset = assert(source:find("exposed();", 1, true)) + #"exposed(" - 1
  local signature = request("textDocument/signatureHelp", offset)
  check(signature == nil or signature == vim.NIL, "shadowed function received a guessed signature")

  source = "fun exposed(int value) {} fun main() { exposed(1); }"
  vim.api.nvim_buf_set_lines(0, 0, -1, false, { source })
  offset = assert(source:find("exposed(1)", 1, true)) + #"exposed(" - 1
  signature = request("textDocument/signatureHelp", offset)
  check(signature and signature.signatures[1].label:find("value", 1, true), "real signature missing")

  local commands = vim.api.nvim_get_commands({})
  for _, name in ipairs({ "NeutronBuild", "NeutronRun", "NeutronTest", "NeutronFormat", "NeutronRestart" }) do
    check(commands[name] ~= nil, name .. " was not registered")
  end
end)

for _, client in ipairs(vim.lsp.get_clients({ name = "ntsc" })) do
  client:stop(true)
end
vim.fn.delete(directory, "rf")
if not ok then
  io.stderr:write(tostring(failure) .. "\n")
  vim.cmd("cquit 1")
end
print("Neovim client integration passed")
vim.cmd("qa!")
