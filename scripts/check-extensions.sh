#!/bin/sh
set -eu

python3 -B scripts/extensions-version.py check
python3 -B -m unittest discover -s extensions/tests -p 'test_*.py'
node --test extensions/vscode/test/*.test.js

if command -v nvim >/dev/null 2>&1; then
    cargo build -p ntsc-lsp
    nvim --headless -u NONE -l extensions/tests/nvim.lua
else
    echo "SKIP: native Neovim check (Neovim 0.11+ not installed)"
fi

if command -v vim >/dev/null 2>&1; then
    vim -Nu NONE -n -es -S extensions/tests/vim.vim
else
    echo "SKIP: native Vim check (Vim not installed)"
fi

if command -v emacs >/dev/null 2>&1; then
    emacs -Q --batch -L extensions/emacs -l extensions/tests/emacs.el
else
    echo "SKIP: native Emacs check (Emacs 29+ not installed)"
fi
