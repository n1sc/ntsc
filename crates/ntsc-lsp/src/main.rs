//! `ntsc-lsp` — the Neutron language server.
//!
//! Speaks LSP over stdio:
//!
//! ```console
//! $ ntsc-lsp
//! ```
//!
//! Configure the editor to launch this binary for `*.nt` files (or run
//! `ntsc lsp`, which starts the same server).

fn main() {
    std::process::exit(ntsc_lsp::run_stdio());
}
