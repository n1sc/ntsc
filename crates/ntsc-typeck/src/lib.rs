//! Type checker for the NTSC language.
//!
//! Performs name resolution and bidirectional type checking on the parsed AST.

mod const_eval;
mod diag;
mod generics;
mod names;
mod ownership;
mod resolve;
mod returns;
mod scope;
mod ty;
mod warnings;

pub use const_eval::ConstValue;
pub use generics::{TraitMethodInfo, TraitObjectInfo, prepare_program, take_trait_object_tables};
pub use names::{ResolveError, resolve_program};
pub use resolve::{
    TypeError, chan_recv_element_type, check_program, go_captures, take_const_values,
};
pub use ty::Ty;
pub use warnings::{Warning, lint_program};
