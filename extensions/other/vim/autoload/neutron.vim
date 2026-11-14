function! neutron#root() abort
  let directory = expand("%:p:h")
  let marker = neutron#contract#get().language.projectMarker
  while !empty(directory)
    if filereadable(directory . "/" . marker)
      return directory
    endif
    let parent = fnamemodify(directory, ":h")
    if parent ==# directory
      break
    endif
    let directory = parent
  endwhile
  return ""
endfunction

function! neutron#root_uri() abort
  let root = neutron#root()
  return lsp#utils#path_to_uri(empty(root) ? expand("%:p:h") : root)
endfunction

function! neutron#workflow(action) abort
  let root = neutron#root()
  let contract = neutron#contract#get()
  if empty(root)
    echoerr "Neutron: open a project containing " . contract.language.projectMarker
    return
  endif
  if !has_key(contract.workflows, a:action)
    echoerr "Neutron: unknown workflow " . a:action
    return
  endif
  wall
  botright new
  let command = [get(g:, "neutron_cli", contract.cli)] + contract.workflows[a:action]
  let job = term_start(command, {"cwd": root, "curwin": 1})
  if job <= 0
    echoerr "Neutron: compiler could not be started"
  endif
endfunction
