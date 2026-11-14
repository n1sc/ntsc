if exists("g:loaded_neutron") | finish | endif
let g:loaded_neutron = 1

function! s:register_server() abort
  if !exists("*lsp#register_server")
    return
  endif
  let contract = neutron#contract#get()
  let default_command = [contract.server.command] + contract.server.args
  call lsp#register_server({
        \ "name": contract.server.id,
        \ "cmd": {server_info -> get(g:, "neutron_server_command", default_command)},
        \ "allowlist": [contract.language.id],
        \ "root_uri": {server_info -> neutron#root_uri()},
        \ })
endfunction

augroup neutron_lsp
  autocmd!
  autocmd User lsp_setup call s:register_server()
augroup END

command! NeutronBuild call neutron#workflow("build")
command! NeutronRun call neutron#workflow("run")
command! NeutronTest call neutron#workflow("test")
command! NeutronFormat LspDocumentFormatSync
command! NeutronRestart LspStopServer ntsc | LspStartServer ntsc
