set nocompatible
execute "set runtimepath^=" . fnameescape(getcwd() . "/extensions/other/vim")
filetype plugin on
syntax on
runtime plugin/neutron.vim
let directory = tempname() . " neutron editor"
call mkdir(directory . "/nested/src", "p")
call writefile(["[package]", 'entry = "main.nt"', 'output = "test"', 'compiler = ">=0.1.0"'], directory . "/neutron.toml")
call writefile(["[package]", 'entry = "src/main.nt"', 'output = "test"', 'compiler = ">=0.1.0"'], directory . "/nested/neutron.toml")
execute "edit " . fnameescape(directory . "/nested/src/main.nt")
call assert_equal("neutron", &filetype)
call assert_equal("# %s", &commentstring)
call assert_equal(4, &shiftwidth)
call assert_equal(directory . "/nested", neutron#root())
call assert_equal(["ntsc", "lsp"], [neutron#contract#get().server.command] + neutron#contract#get().server.args)
for command in ["NeutronBuild", "NeutronRun", "NeutronTest", "NeutronFormat", "NeutronRestart"]
  call assert_equal(2, exists(":" . command))
endfor
call delete(directory, "rf")
if !empty(v:errors)
  call writefile(v:errors, "/dev/stderr")
  cquit 1
endif
qa!
