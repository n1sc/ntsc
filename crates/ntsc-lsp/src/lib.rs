//! Language server (LSP) for the Neutron (NTSC) programming language.
//!
//! The server speaks LSP over stdio and exposes the usual capabilities:
//! diagnostics, completion, hover, go-to-definition, references, rename,
//! document and workspace symbols, signature help, formatting, folding
//! ranges, semantic tokens, inlay hints, document highlights, and quick-fix
//! code actions.

pub mod analysis;
pub mod convert;
pub mod formatting;
pub mod server;
pub mod stdlib;

pub use analysis::{FileAnalysis, Symbol, SymbolKind, analyze};
pub use server::run_stdio;
