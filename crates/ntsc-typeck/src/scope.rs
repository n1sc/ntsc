//! Scoped symbol table for name resolution.

use std::collections::HashMap;

use crate::ty::Ty;

/// A nested scope for tracking variable and function types.
pub struct SymbolTable {
    /// Stack of scopes. The last element is the innermost scope.
    scopes: Vec<HashMap<String, Symbol>>,
}

struct Symbol {
    ty: Ty,
    stdlib_module: Option<String>,
}

impl SymbolTable {
    /// Create a new symbol table with a global scope.
    pub fn new() -> Self {
        Self {
            scopes: vec![HashMap::new()],
        }
    }

    /// Enter a new nested scope.
    pub fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    /// Exit the current scope, discarding its bindings.
    pub fn pop_scope(&mut self) {
        if self.scopes.len() > 1 {
            self.scopes.pop();
        }
    }

    /// Define a name in the current (innermost) scope.
    ///
    /// Returns `Err` if the name is already defined in the same scope.
    pub fn define(&mut self, name: &str, ty: Ty) -> Result<(), String> {
        let scope = self.scopes.last_mut().expect("no scope on stack");
        if scope.contains_key(name) {
            return Err(format!("`{name}` is already defined in this scope"));
        }
        scope.insert(
            name.to_string(),
            Symbol {
                ty,
                stdlib_module: None,
            },
        );
        Ok(())
    }

    pub fn define_stdlib_module(&mut self, name: &str, module: &str) -> Result<(), String> {
        self.define(name, Ty::Object)?;
        if let Some(symbol) = self.scopes.last_mut().and_then(|scope| scope.get_mut(name)) {
            symbol.stdlib_module = Some(module.to_string());
        }
        Ok(())
    }

    pub fn lookup_stdlib_module(&self, name: &str) -> Option<&str> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name))
            .and_then(|symbol| symbol.stdlib_module.as_deref())
    }

    /// Look up a name, searching from innermost scope outward.
    pub fn lookup(&self, name: &str) -> Option<&Ty> {
        self.lookup_depth(name).map(|(_, ty)| ty)
    }

    /// The nesting depth of the current scope (0 is the global scope).
    pub fn depth(&self) -> usize {
        self.scopes.len() - 1
    }

    /// Look up a name and report which scope depth it was resolved in.
    ///
    /// A name defined in the global scope resolves at depth 0; a name
    /// defined in the current scope resolves at `self.depth()`.
    pub fn lookup_depth(&self, name: &str) -> Option<(usize, &Ty)> {
        for (index, scope) in self.scopes.iter().enumerate().rev() {
            if let Some(symbol) = scope.get(name) {
                return Some((index, &symbol.ty));
            }
        }
        None
    }

    /// Check if a name exists in the current (innermost) scope only.
    #[allow(dead_code)]
    pub fn has_in_current_scope(&self, name: &str) -> bool {
        // Same name in inner scope is allowed (shadowing from outer).
        self.scopes
            .last()
            .is_some_and(|scope| scope.contains_key(name))
    }
}

impl Default for SymbolTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn define_and_lookup() {
        let mut table = SymbolTable::new();
        table.define("x", Ty::Int).unwrap();
        assert_eq!(table.lookup("x"), Some(&Ty::Int));
    }

    #[test]
    fn shadow_in_inner_scope() {
        let mut table = SymbolTable::new();
        table.define("x", Ty::Int).unwrap();
        table.push_scope();
        table.define("x", Ty::String).unwrap();
        assert_eq!(table.lookup("x"), Some(&Ty::String));
        table.pop_scope();
        assert_eq!(table.lookup("x"), Some(&Ty::Int));
    }

    #[test]
    fn undefined_returns_none() {
        let table = SymbolTable::new();
        assert_eq!(table.lookup("nope"), None);
    }

    #[test]
    fn duplicate_in_same_scope_fails() {
        let mut table = SymbolTable::new();
        assert!(table.define("x", Ty::Int).is_ok());
        assert!(table.define("x", Ty::Float).is_err());
    }

    #[test]
    fn same_name_in_different_scopes_ok() {
        let mut table = SymbolTable::new();
        table.define("x", Ty::Int).unwrap();
        table.push_scope();

        assert!(table.define("x", Ty::String).is_ok());
        table.pop_scope();
    }
}
