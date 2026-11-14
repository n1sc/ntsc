local M = {}
local contract = require("neutron.contract")
local options

local function project_root()
  local name = vim.api.nvim_buf_get_name(0)
  local root = vim.fs.root(name ~= "" and name or vim.fn.getcwd(), contract.marker)
  if not root then
    error("Neutron: open a project containing " .. contract.marker)
  end
  return root
end

local function workflow(action)
  local arguments = contract.workflows[action]
  local root = project_root()
  vim.cmd("wall")
  local command = { options.cli }
  vim.list_extend(command, arguments)
  vim.cmd("botright new")
  local job = vim.fn.jobstart(command, { term = true, cwd = root })
  if job <= 0 then
    vim.notify("Neutron: failed to launch " .. options.cli, vim.log.levels.ERROR)
    return
  end
  vim.cmd("startinsert")
end

function M.setup(configuration)
  options = vim.tbl_deep_extend("force", {
    cmd = contract.cmd,
    cli = contract.cli,
    completion = true,
  }, configuration or {})
  vim.filetype.add({ extension = { nt = "neutron" } })
  vim.lsp.config(contract.server_id, {
    cmd = options.cmd,
    filetypes = { "neutron" },
    root_markers = { contract.marker },
    workspace_required = false,
    on_attach = function(client, buffer)
      if options.completion and client:supports_method("textDocument/completion") then
        vim.lsp.completion.enable(true, client.id, buffer, { autotrigger = true })
      end
      if client:supports_method("textDocument/inlayHint") then
        vim.lsp.inlay_hint.enable(true, { bufnr = buffer })
      end
    end,
  })
  vim.lsp.enable(contract.server_id)
  for action in pairs(contract.workflows) do
    local command = "Neutron" .. action:sub(1, 1):upper() .. action:sub(2)
    vim.api.nvim_create_user_command(command, function()
      local ok, err = pcall(workflow, action)
      if not ok then
        vim.notify(tostring(err), vim.log.levels.ERROR)
      end
    end, { force = true, desc = "Neutron: " .. action })
  end
  vim.api.nvim_create_user_command("NeutronFormat", function()
    vim.lsp.buf.format({ name = contract.server_id, async = false })
  end, { force = true })
  vim.api.nvim_create_user_command("NeutronRestart", function()
    vim.lsp.enable(contract.server_id, false)
    vim.lsp.enable(contract.server_id, true)
  end, { force = true })
end

return M
