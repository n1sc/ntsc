//! Type resolution and checking over the AST.

use std::collections::{HashMap, HashSet};

use ntsc_ast::expr::{Expr, LiteralValue};
use ntsc_ast::span::Span;
use ntsc_ast::stmt::{MatchCase, MatchPattern, Program, Stmt};
use ntsc_ast::token::{Token, TokenKind};
use ntsc_ast::types::TypeAnnotation;

use crate::const_eval::{ConstEvaluator, ConstValue};
use crate::names::resolve_program;
use crate::returns::can_fall_through;
use crate::scope::SymbolTable;
use crate::ty::Ty;

// Async function name -> element types of its `|>` channel receives, in
// order of appearance. Populated during type checking and read by codegen so
// a receive can type its freshly-bound receiver variable. Keyed by the
// receive's operator span (unique per site), so receives inside `go { }`
// blocks resolve even though they are checked under the enclosing function.
std::thread_local! {
    static CHAN_RECV_TYPES: std::cell::RefCell<HashMap<usize, Ty>> =
        std::cell::RefCell::new(HashMap::new());
}

/// Returns the element type of the `|>` receive (or `for v in chan` loop)
/// at the given operator/channel span start. Used by codegen to type the
/// receiver variable of each channel receive it lowers.
pub fn chan_recv_element_type(span_start: usize) -> Ty {
    CHAN_RECV_TYPES
        .with(|map| map.borrow().get(&span_start).cloned())
        .unwrap_or(Ty::Any)
}

/// The name of a `go` callee for diagnostics (`worker` from `go worker(..)`).
fn name_of_callee(callee: &Expr) -> String {
    match callee {
        Expr::Variable { name } => name.lexeme().to_string(),
        _ => "go".into(),
    }
}

impl TypeChecker {
    /// Reject a `go` argument whose type cannot cross a thread boundary.
    /// Views borrow the spawner's frame; `shared` refcounts are not
    /// synchronized across goroutines.
    fn reject_thread_unsafe(&mut self, ty: &Ty, span: Span, callee: &str) {
        match ty {
            Ty::View(..) => self.errors.push(TypeError {
                code: Some("NTSC-E0501"),
                help: Some(
                    "a borrow may not outlive the goroutine; move or copy the value instead"
                        .into(),
                ),
                message: format!(
                    "cannot pass `{ty}` to `go {callee}`: a view cannot cross into a goroutine"
                ),
                span,
            }),
            Ty::Shared(..) => self.errors.push(TypeError {
                code: Some("NTSC-E0501"),
                help: Some(
                    "`shared` reference counts are not synchronized across goroutines; send the value through a channel instead".into(),
                ),
                message: format!(
                    "cannot pass `{ty}` to `go {callee}`: unsynchronized reference counting"
                ),
                span,
            }),
            _ => {}
        }
    }
}

// Captured outer locals per `go { ... }` block, keyed by the `go`
// keyword's span start. Codegen turns each entry into the parameters of
// the block's synthesized goroutine function; the list is deterministic
// (sorted by name) and only contains variables of the enclosing function.
std::thread_local! {
    static GO_CAPTURES: std::cell::RefCell<HashMap<usize, Vec<(String, Ty)>>> =
        std::cell::RefCell::new(HashMap::new());
}

/// Returns the locals a `go { ... }` block captures, sorted by name.
pub fn go_captures(keyword_span_start: usize) -> Vec<(String, Ty)> {
    GO_CAPTURES.with(|map| {
        map.borrow()
            .get(&keyword_span_start)
            .cloned()
            .unwrap_or_default()
    })
}

/// Names bound anywhere inside a `go` block (any depth): those never count
/// as captures of the enclosing function.
pub(crate) fn collect_go_bindings(stmts: &[Stmt], bound: &mut HashSet<String>) {
    for stmt in stmts {
        match stmt {
            Stmt::Var { name, .. } | Stmt::ForIn { variable: name, .. } => {
                bound.insert(name.lexeme().to_string());
            }
            Stmt::ForAwait { variable, .. } => {
                bound.insert(variable.lexeme().to_string());
            }
            Stmt::For {
                init: Some(init), ..
            } => {
                if let Stmt::Var { name, .. } = init.as_ref() {
                    bound.insert(name.lexeme().to_string());
                }
            }
            Stmt::ChanRecvFor { variable, .. } => {
                bound.insert(variable.lexeme().to_string());
            }
            Stmt::Destructure { names, .. } => {
                for name in names {
                    bound.insert(name.lexeme().to_string());
                }
            }
            Stmt::Block { statements, .. } => collect_go_bindings(statements, bound),
            Stmt::If {
                then_branch,
                elif_branches,
                else_branch,
                ..
            } => {
                collect_go_bindings(std::slice::from_ref(then_branch), bound);
                for branch in elif_branches {
                    collect_go_bindings(std::slice::from_ref(&branch.body), bound);
                }
                if let Some(else_branch) = else_branch {
                    collect_go_bindings(std::slice::from_ref(else_branch), bound);
                }
            }
            Stmt::While { body, .. } | Stmt::DoWhile { body, .. } => {
                collect_go_bindings(std::slice::from_ref(body), bound)
            }
            Stmt::Try {
                catch_var,
                catch_block,
                ..
            } => {
                if let Some(catch_var) = catch_var {
                    bound.insert(catch_var.lexeme().to_string());
                }
                if let Some(catch_block) = catch_block {
                    collect_go_bindings(std::slice::from_ref(catch_block), bound);
                }
            }
            _ => {}
        }
    }
}

thread_local! {
    /// Evaluated constant values produced by the type checker, keyed by
    /// variable name. Codegen reads these to emit folded constants.
    static CONST_EVAL_VALUES: std::cell::RefCell<HashMap<String, ConstValue>> =
        std::cell::RefCell::new(HashMap::new());
}

/// Take the evaluated constant values out of thread-local storage.
/// Called by codegen after `check_program` to consume the results.
pub fn take_const_values() -> HashMap<String, ConstValue> {
    CONST_EVAL_VALUES.with(|cell| cell.borrow_mut().drain().collect())
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// A type error with source location.
pub struct TypeError {
    pub message: String,
    pub span: Span,

    /// Diagnostic error-code family. `None` means the default type-check code.
    pub code: Option<&'static str>,

    /// A one-sentence remedy rendered as a `help:` line under the error
    /// (e.g. where to add `copy(...)`). Absent when there is nothing
    /// actionable to suggest.
    pub help: Option<String>,
}

impl std::fmt::Display for TypeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.span, self.message)
    }
}

impl std::error::Error for TypeError {}

/// Check a program for type errors.
///
/// Returns `Ok(())` if the program is well-typed, or a list of all type
/// errors found.
pub fn check_program(program: &Program) -> Result<(), Vec<TypeError>> {
    if !program.statements.iter().any(needs_generic_preparation) {
        return check_prepared_program(program);
    }
    let prepared = crate::generics::prepare_program(program)?;
    check_prepared_program(&prepared)
}

fn needs_generic_preparation(statement: &Stmt) -> bool {
    match statement {
        Stmt::Trait { .. } | Stmt::Impl { .. } => true,
        Stmt::Function { generic_params, .. }
        | Stmt::Class { generic_params, .. }
        | Stmt::Enum { generic_params, .. } => !generic_params.is_empty(),
        Stmt::TypeAlias { .. } => true,
        _ => false,
    }
}

pub(crate) fn check_prepared_program(program: &Program) -> Result<(), Vec<TypeError>> {
    if let Err(errors) = resolve_program(program) {
        return Err(errors
            .into_iter()
            .map(|error| TypeError {
                code: None,
                help: None,
                message: error.message,
                span: error.span,
            })
            .collect());
    }
    let mut checker = TypeChecker::new();
    checker.check_program(program);
    if !checker.errors.is_empty() {
        return Err(checker.errors);
    }
    let mut ownership = crate::ownership::OwnershipChecker::new();
    ownership.check_program(program);
    if ownership.errors.is_empty() {
        Ok(())
    } else {
        Err(ownership.errors)
    }
}

/// The declared members of one class, used to type member access.
struct ClassInfo {
    /// The `extends` parent, if any.
    parent: Option<String>,

    /// Declared field types by name.
    fields: HashMap<String, Ty>,

    /// Method names. Their signatures are not modelled here, so a method
    /// reference stays `any` and its call is unchecked.
    methods: HashSet<String>,

    /// Operator method names to their declared return types.
    /// Populated for methods whose name is an operator symbol (+, -, etc.).
    operator_returns: HashMap<String, Ty>,
}

/// The type checker state.
struct TypeChecker {
    symbols: SymbolTable,
    errors: Vec<TypeError>,

    /// The expected return type of the current function, if inside one.
    current_return_type: Option<Ty>,

    /// Whether we are currently checking a class body (fields may be
    /// uninitialized declarations with only a type annotation).
    in_class_body: bool,

    /// Names of top-level async functions. Calling one without `await` is
    /// an error, and `await` is only valid inside an async function.
    async_fns: HashSet<String>,
    ffi_functions: HashSet<String>,

    /// Nesting depth of async function bodies currently being checked.
    async_depth: usize,

    /// Name of the async function whose body is currently being checked, used
    /// to key the receiver-element-type registry codegen reads for `|>`.
    current_fn_name: Option<String>,

    unsafe_depth: usize,

    /// Names of enum members, which resolve to `int` constants.
    enum_members: HashSet<String>,

    /// Names of `static const` variables; they are immutable and require a
    /// compile-time constant initializer.
    consts: HashSet<String>,

    /// Scope depths at which each enclosing function body starts. A
    /// variable resolved at a shallower depth (but not the global scope)
    /// inside a lambda is a capture, which lambdas cannot perform.
    capture_bases: Vec<usize>,

    /// Declared members of every class in the program, by class name.
    classes: HashMap<String, ClassInfo>,

    /// Compile-time constant evaluator — folds arithmetic, resolves
    /// references to earlier constants, and evaluates pure functions.
    const_eval: ConstEvaluator,
}

impl TypeChecker {
    fn new() -> Self {
        let mut symbols = SymbolTable::new();

        // Pre-declare built-in `say` function.
        symbols
            .define(
                "say",
                Ty::Function {
                    params: vec![Ty::String],
                    return_type: Box::new(Ty::Void),
                },
            )
            .expect("global scope should be empty");

        // Pre-declare `wait_any` / `wait_all` concurrent combinators.
        for name in ["wait_any", "wait_all"] {
            symbols
                .define(
                    name,
                    Ty::Function {
                        params: vec![Ty::Any, Ty::Any],
                        return_type: Box::new(Ty::Any),
                    },
                )
                .expect("global scope should be empty");
        }

        // `async` is a reserved keyword and cannot be imported with `use`, so
        // it stays always in scope.
        symbols
            .define("async", Ty::Object)
            .expect("global scope should be empty");

        // `chan.new(...)` — the virtual-task channel constructor. The
        // element type comes from the target slot, so the constructor call
        // itself is only arity-checked.
        symbols
            .define("chan", Ty::Object)
            .expect("global scope should be empty");

        Self {
            symbols,
            errors: Vec::new(),
            current_return_type: None,
            in_class_body: false,
            async_fns: HashSet::new(),
            ffi_functions: HashSet::new(),
            async_depth: 0,
            current_fn_name: None,
            unsafe_depth: 0,
            enum_members: HashSet::new(),
            consts: HashSet::new(),
            capture_bases: Vec::new(),
            classes: HashMap::new(),
            const_eval: ConstEvaluator::new(),
        }
    }

    /// Reports classes that reach themselves through their `extends` chain.
    /// One diagnostic is emitted per cycle, naming the full chain, rather
    /// than one per class involved in it.
    fn check_inheritance_cycles(&mut self, program: &Program) {
        let parents: HashMap<&str, &Token> = program
            .statements
            .iter()
            .filter_map(|stmt| match stmt {
                Stmt::Class {
                    name,
                    parent: Some(parent),
                    ..
                } => Some((name.lexeme(), parent)),
                _ => None,
            })
            .collect();

        let mut reported: HashSet<&str> = HashSet::new();
        for stmt in &program.statements {
            let Stmt::Class {
                name,
                parent: Some(parent),
                ..
            } = stmt
            else {
                continue;
            };
            let start = name.lexeme();
            if reported.contains(start) {
                continue;
            }

            let mut chain = vec![start];
            let mut current = parent.lexeme();
            // Walk up from `start` looking for a return to `start`. Chains
            // that end at a root, at an undefined parent, or in a cycle
            // that does not include `start` are left for their own class to
            // report.
            while current != start {
                let Some(next) = parents.get(current) else {
                    break;
                };
                if chain.contains(&current) {
                    break;
                }
                chain.push(current);
                current = next.lexeme();
            }
            if current != start {
                continue;
            }

            chain.push(start);
            reported.extend(chain.iter().copied());
            self.errors.push(TypeError {
                code: None,
                help: None,
                message: format!(
                    "class `{start}` cannot inherit from itself (cycle: {})",
                    chain.join(" -> ")
                ),
                span: parent.span,
            });
        }
    }

    fn check_program(&mut self, program: &Program) {
        // Reject cyclic inheritance (`class A extends A`, mutual cycles)
        // before anything else: the code generator walks the parent chain
        // recursively to lay out fields and resolve inherited methods, and
        // a cycle would make that recursion overflow the compiler's stack.
        self.check_inheritance_cycles(program);

        // Types must be visible before signatures so class declarations
        // can refer to each other regardless of source order.
        for stmt in &program.statements {
            let (name, ty) = match stmt {
                Stmt::Class { name, .. } | Stmt::Enum { name, .. } => {
                    (name, Ty::Class(name.lexeme().into()))
                }
                _ => continue,
            };
            if let Err(message) = self.symbols.define(name.lexeme(), ty) {
                self.errors.push(TypeError {
                    code: None,
                    help: None,
                    message,
                    span: name.span,
                });
            }
        }

        // Register enum member names as `int` constants so bare references
        // (`case North`) type-check.
        for stmt in &program.statements {
            if let Stmt::Enum { members, .. } = stmt {
                for member in members {
                    self.enum_members.insert(member.name.lexeme().to_string());
                }
            }
        }

        self.collect_class_members(program);

        // Register all top-level functions before checking any bodies so
        // calls are independent of declaration order.
        for stmt in &program.statements {
            let (name, params, return_type) = match stmt {
                Stmt::Function {
                    name,
                    params,
                    return_type,
                    ..
                }
                | Stmt::AsyncFunction {
                    name,
                    params,
                    return_type,
                    ..
                } => (name, params, return_type),
                Stmt::ExternFunction {
                    name,
                    params,
                    return_type,
                    ..
                } => (name, params, return_type),
                _ => continue,
            };
            self.require_function_signature(name, params, return_type);
            let param_tys: Vec<Ty> = params
                .iter()
                .map(|p| self.resolve_annotation(p.type_annotation.as_ref()))
                .collect();
            let ret_ty = return_type
                .as_ref()
                .map(|r| self.resolve_annotation(Some(&r.ty)))
                .unwrap_or(Ty::Void);
            let fn_ty = Ty::Function {
                params: param_tys,
                return_type: Box::new(ret_ty),
            };
            if let Err(msg) = self.symbols.define(name.lexeme(), fn_ty) {
                self.errors.push(TypeError {
                    code: None,
                    help: None,
                    message: msg,
                    span: name.span,
                });
            }
            if matches!(stmt, Stmt::AsyncFunction { .. }) {
                self.async_fns.insert(name.lexeme().to_string());
            }
            if matches!(stmt, Stmt::ExternFunction { .. }) {
                self.ffi_functions.insert(name.lexeme().to_string());
            }
        }
        for stmt in &program.statements {
            let Stmt::ExternBlock { functions, .. } = stmt else {
                continue;
            };
            for function in functions {
                let Stmt::ExternFunction {
                    name,
                    params,
                    return_type,
                    ..
                } = function
                else {
                    continue;
                };
                self.require_function_signature(name, params, return_type);
                let param_tys: Vec<Ty> = params
                    .iter()
                    .map(|p| self.resolve_annotation(p.type_annotation.as_ref()))
                    .collect();
                let ret_ty = return_type
                    .as_ref()
                    .map(|r| self.resolve_annotation(Some(&r.ty)))
                    .unwrap_or(Ty::Void);
                let fn_ty = Ty::Function {
                    params: param_tys,
                    return_type: Box::new(ret_ty),
                };
                if let Err(message) = self.symbols.define(name.lexeme(), fn_ty) {
                    self.errors.push(TypeError {
                        code: None,
                        help: None,
                        message,
                        span: name.span,
                    });
                }
                self.ffi_functions.insert(name.lexeme().to_string());
            }
        }

        // Second pass: check bodies.
        for stmt in &program.statements {
            self.check_statement(stmt);
        }
    }

    /// Records every class's fields and methods before any body is
    /// checked, so a member access is typed regardless of declaration
    /// order.
    ///
    /// A field's type comes from its annotation, or from a literal
    /// initializer when there is no annotation, which is exactly how the
    /// code generator decides the field's slot type. Anything else stays
    /// `any` so the two never disagree about what a slot holds.
    fn collect_class_members(&mut self, program: &Program) {
        for stmt in &program.statements {
            let Stmt::Class {
                name, parent, body, ..
            } = stmt
            else {
                continue;
            };
            let mut info = ClassInfo {
                parent: parent.as_ref().map(|p| p.lexeme().to_string()),
                fields: HashMap::new(),
                methods: HashSet::new(),
                operator_returns: HashMap::new(),
            };
            for member in body {
                match member {
                    Stmt::Var {
                        name: field,
                        type_annotation,
                        initializer,
                        ..
                    } => {
                        let ty = if type_annotation.is_some() {
                            self.resolve_annotation(type_annotation.as_ref())
                        } else {
                            match initializer {
                                Some(Expr::Literal { value, .. }) => match value {
                                    // A `nil` initializer says nothing about
                                    // the field's type, only that it starts
                                    // empty.
                                    LiteralValue::Nil => Ty::Any,
                                    other => self.literal_type(other),
                                },
                                _ => Ty::Any,
                            }
                        };
                        info.fields.insert(field.lexeme().to_string(), ty);
                    }
                    Stmt::Function {
                        name: method,
                        return_type,
                        ..
                    }
                    | Stmt::AsyncFunction {
                        name: method,
                        return_type,
                        ..
                    } => {
                        let lexeme = method.lexeme().to_string();
                        info.methods.insert(lexeme.clone());
                        if is_operator_name(&lexeme) {
                            let ret = self.resolve_annotation(return_type.as_ref().map(|r| &r.ty));
                            info.operator_returns.insert(lexeme, ret);
                        }
                    }
                    _ => {}
                }
            }
            self.classes.insert(name.lexeme().to_string(), info);
        }
    }

    /// Whether `class_name` (or one of its `extends` parents) declares a field
    /// named `property`. Methods do not count as fields.
    fn class_declares_field(&self, class_name: &str, property: &str) -> bool {
        let mut current = Some(class_name);
        let mut seen: HashSet<&str> = HashSet::new();
        while let Some(name) = current {
            if !seen.insert(name) {
                break;
            }
            let Some(info) = self.classes.get(name) else {
                return false;
            };
            if info.fields.contains_key(property) {
                return true;
            }
            if info.methods.contains(property) {
                return false;
            }
            current = info.parent.as_deref();
        }
        false
    }

    /// Every declared field and method name of `class_name` (including
    /// inherited ones), for "did you mean" suggestions.
    fn class_member_names(&self, class_name: &str) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        let mut current = Some(class_name);
        let mut seen: HashSet<&str> = HashSet::new();
        while let Some(name) = current {
            if !seen.insert(name) {
                break;
            }
            let Some(info) = self.classes.get(name) else {
                break;
            };
            names.extend(info.fields.keys().cloned());
            names.extend(info.methods.iter().cloned());
            current = info.parent.as_deref();
        }
        names
    }

    /// The declared type of `property` read off an instance of `class_name`,
    /// walking the `extends` chain. `None` when the class is unknown, when
    /// the property is a method, or when no declaration names it — those
    /// keep the old `any` behavior instead of becoming an error.
    fn class_field_ty(&self, class_name: &str, property: &str) -> Option<Ty> {
        let mut current = Some(class_name);

        let mut seen: HashSet<&str> = HashSet::new();
        // A cyclic `extends` chain is reported by
        // `check_inheritance_cycles` and checking continues, so this walk
        // has to terminate on its own.
        while let Some(name) = current {
            if !seen.insert(name) {
                return None;
            }
            let info = self.classes.get(name)?;
            if let Some(ty) = info.fields.get(property) {
                return Some(ty.clone());
            }
            if info.methods.contains(property) {
                return None;
            }
            current = info.parent.as_deref();
        }
        None
    }

    /// The declared type of a member read, or `Ty::Any` when it is not known.
    fn member_ty(&mut self, object: &Expr, property: &Token) -> Ty {
        let object_ty = self.check_expression(object);
        object_ty
            .as_ref()
            .and_then(base_class_name)
            .and_then(|class| self.class_field_ty(class, property.lexeme()))
            .unwrap_or(Ty::Any)
    }

    /// Look up the declared return type of an operator method on `class_name`,
    /// walking the `extends` chain. Returns `None` when the class does not
    /// define the operator.
    fn lookup_operator_return(&self, class_name: &str, op: &str) -> Option<Ty> {
        let mut current = Some(class_name);
        let mut seen: HashSet<&str> = HashSet::new();
        while let Some(name) = current {
            if !seen.insert(name) {
                break;
            }
            if let Some(info) = self.classes.get(name) {
                if let Some(ret) = info.operator_returns.get(op) {
                    return Some(ret.clone());
                }
                current = info.parent.as_deref();
            } else {
                break;
            }
        }
        None
    }

    /// Check a destructuring match arm: the variant must exist on the
    /// scrutinee's type (result cells expose `Ok` / `Err`), and the binder
    /// is scoped to the arm body with the payload's type.
    fn check_pattern_case(
        &mut self,
        case: &MatchCase,
        pattern: &MatchPattern,
        scrutinee_ty: Option<Ty>,
    ) {
        let variant = pattern.variant.lexeme();
        let payload_ty = match (&scrutinee_ty, variant) {
            (Some(Ty::Result { ok, err: _ }), "Ok") => Some((**ok).clone()),
            (Some(Ty::Result { ok: _, err }), "Err") => Some((**err).clone()),
            (Some(other), _) => {
                self.errors.push(TypeError {
                    code: None,
                    help: None,
                    message: format!("`{other}` has no variant `{variant}` to match here"),
                    span: pattern.variant.span,
                });
                None
            }
            // Scrutinee type unknown (earlier errors): accept either
            // variant and let the binder stay unchecked.
            (None, "Ok" | "Err") => None,
            (None, _) => {
                return;
            }
        };

        if let Some(binding) = &pattern.binding {
            self.symbols.push_scope();
            let binder_ty = payload_ty.unwrap_or(Ty::Any);
            if let Err(msg) = self.symbols.define(binding.lexeme(), binder_ty) {
                self.errors.push(TypeError {
                    code: None,
                    help: None,
                    message: msg,
                    span: binding.span,
                });
            }
            self.check_statement(&case.body);
            self.symbols.pop_scope();
        } else {
            self.check_statement(&case.body);
        }
    }

    fn check_statement(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Function {
                name,
                params,
                return_type,
                body,
                ..
            } => {
                self.check_function(name.lexeme(), params, return_type, body, name.span, false);
            }
            Stmt::AsyncFunction {
                name,
                params,
                return_type,
                body,
            } => {
                self.check_function(name.lexeme(), params, return_type, body, name.span, true);
            }
            Stmt::ExternFunction { .. } | Stmt::ExternBlock { .. } => {}
            Stmt::Test { body, .. } => {
                // A test block is a no-argument, void function.
                self.check_function("", &[], &None, body, Span::dummy(), false);
            }
            Stmt::Var {
                name,
                type_annotation,
                initializer,
                view,
                is_const,
                ..
            } => {
                self.check_var(name, type_annotation, initializer, view, *is_const);
            }
            Stmt::Block { statements, .. } => {
                self.symbols.push_scope();
                for stmt in statements {
                    self.check_statement(stmt);
                }
                self.symbols.pop_scope();
            }
            Stmt::If {
                condition,
                then_branch,
                elif_branches,
                else_branch,
            } => {
                self.check_condition(condition);
                self.check_statement(then_branch);
                for branch in elif_branches {
                    self.check_condition(&branch.condition);
                    self.check_statement(&branch.body);
                }
                if let Some(else_branch) = else_branch {
                    self.check_statement(else_branch);
                }
            }
            Stmt::While { condition, body } => {
                self.check_condition(condition);
                self.check_statement(body);
            }
            Stmt::DoWhile { body, condition } => {
                self.check_statement(body);
                self.check_condition(condition);
            }
            Stmt::For {
                init,
                condition,
                update,
                body,
            } => {
                self.symbols.push_scope();
                if let Some(init) = init {
                    self.check_statement(init);
                }
                if let Some(condition) = condition {
                    self.check_condition(condition);
                }
                if let Some(update) = update {
                    let _ = self.check_expression(update);
                }
                self.check_statement(body);
                self.symbols.pop_scope();
            }
            Stmt::ForIn {
                variable,
                iterable,
                body,
            } => {
                let iterable_ty = self.check_expression(iterable);
                let elem_ty = match &iterable_ty {
                    Some(Ty::Array(inner)) => (**inner).clone(),
                    Some(Ty::String) => Ty::String,
                    _ => Ty::Any,
                };
                self.symbols.push_scope();
                if let Err(msg) = self.symbols.define(variable.lexeme(), elem_ty) {
                    self.errors.push(TypeError {
                        code: None,
                        help: None,
                        message: msg,
                        span: variable.span,
                    });
                }
                self.check_statement(body);
                self.symbols.pop_scope();
            }
            Stmt::ForAwait {
                variable,
                producer,
                body,
            } => {
                let _producer_ty = self.check_expression(producer);
                self.symbols.push_scope();
                if let Err(msg) = self.symbols.define(variable.lexeme(), Ty::Any) {
                    self.errors.push(TypeError {
                        code: None,
                        help: None,
                        message: msg,
                        span: variable.span,
                    });
                }
                self.check_statement(body);
                self.symbols.pop_scope();
            }
            Stmt::ChanRecvFor {
                variable,
                channel,
                body,
            } => {
                let channel_ty = self.check_expression(channel);
                let element_ty = match &channel_ty {
                    Some(Ty::Chan(element)) => (**element).clone(),
                    _ => {
                        if let Some(other) = &channel_ty {
                            self.errors.push(TypeError {
                                code: None,
                                help: Some(
                                    "only a channel can be iterated with `for v in chan`".into(),
                                ),
                                message: format!(
                                    "cannot iterate over `{other}`; expected a channel"
                                ),
                                span: channel.span(),
                            });
                        }
                        Ty::Any
                    }
                };
                self.symbols.push_scope();
                if let Err(msg) = self.symbols.define(variable.lexeme(), element_ty.clone()) {
                    self.errors.push(TypeError {
                        code: None,
                        help: None,
                        message: msg,
                        span: variable.span,
                    });
                }
                CHAN_RECV_TYPES.with(|map| {
                    map.borrow_mut()
                        .insert(channel.span().start, element_ty.clone());
                });
                self.check_statement(body);
                self.symbols.pop_scope();
            }
            Stmt::Go {
                call,
                block,
                keyword_span,
            } => {
                // `go worker(args)` runs an async function's future on the
                // scheduler; the bare call is legal here (no `await`). Every
                // argument crosses a thread boundary: views may not outlive
                // the spawner and `shared` refcounts are unsynchronized.
                match call {
                    Expr::Call {
                        callee, arguments, ..
                    } if matches!(callee.as_ref(), Expr::Variable { name }
                            if self.async_fns.contains(name.lexeme())) =>
                    {
                        for argument in arguments {
                            if let Some(ty) = self.check_expression(argument) {
                                self.reject_thread_unsafe(
                                    &ty,
                                    argument.span(),
                                    &name_of_callee(callee.as_ref()),
                                );
                            }
                        }
                    }
                    Expr::Literal {
                        value: LiteralValue::Nil,
                        ..
                    } if block.is_some() => {}
                    _ => {
                        self.errors.push(TypeError {
                            code: Some("NTSC-E0501"),
                            help: Some(
                                "spawn an async function: `go worker(args)`, or run a block: `go { ... }`"
                                    .into(),
                            ),
                            message: "`go` requires a call to an async function or a block".into(),
                            span: call.span(),
                        });
                    }
                }
                if let Some(block) = block {
                    let mut uses = HashSet::new();
                    for stmt in block {
                        crate::ownership::collect_stmt_uses(stmt, &mut uses);
                    }
                    let mut bound = HashSet::new();
                    collect_go_bindings(block, &mut bound);
                    let mut captures: Vec<(String, Ty)> = uses
                        .into_iter()
                        .filter(|name| !bound.contains(name))
                        .filter_map(|name| {
                            let (depth, ty) = self.symbols.lookup_depth(&name)?;
                            if depth == 0 || matches!(ty, Ty::Function { .. }) {
                                return None;
                            }
                            if let Ty::View(..) = &ty {
                                self.errors.push(TypeError {
                                    code: Some("NTSC-E0501"),
                                    help: Some(
                                        "a borrow may not outlive the goroutine; move or copy the value instead".into(),
                                    ),
                                    message: format!(
                                        "cannot capture view `{name}` in a `go` block: a borrow cannot cross into a goroutine"
                                    ),
                                    span: *keyword_span,
                                });
                                return None;
                            }
                            if let Ty::Shared(..) = &ty {
                                self.errors.push(TypeError {
                                    code: Some("NTSC-E0501"),
                                    help: Some(
                                        "`shared` reference counts are not synchronized across goroutines; send the value through a channel instead".into(),
                                    ),
                                    message: format!(
                                        "cannot capture shared `{name}` in a `go` block: unsynchronized reference counting"
                                    ),
                                    span: *keyword_span,
                                });
                                return None;
                            }
                            Some((name, ty.clone()))
                        })
                        .collect();
                    captures.sort_by(|a, b| a.0.cmp(&b.0));
                    GO_CAPTURES.with(|map| {
                        map.borrow_mut().insert(keyword_span.start, captures);
                    });
                    self.symbols.push_scope();
                    for stmt in block {
                        self.check_statement(stmt);
                    }
                    self.symbols.pop_scope();
                }
            }
            Stmt::Return { value } => {
                if let Some(expr) = value {
                    let expr_ty = self.check_expression(expr);
                    if matches!(&expr_ty, Some(ty) if is_borrow_ty(ty)) {
                        self.errors.push(TypeError {
                            code: None,
                            message: "cannot return a borrow; it is scoped to this function and dies with it".into(),
                            help: Some("wrap the value: `return copy(value)` hands back an owned copy".into()),
                            span: expr.span(),
                        });
                    } else if let Some(expected) = &self.current_return_type
                        && let Some(actual) = &expr_ty
                        && !self.assignable(expected, actual)
                    {
                        self.errors.push(TypeError {
                            code: None,
                            help: None,
                            message: format!(
                                "return type mismatch: expected `{expected}`, got `{actual}`"
                            ),
                            span: expr.span(),
                        });
                    }
                } else if let Some(expected) = &self.current_return_type
                    && *expected != Ty::Void
                {
                    self.errors.push(TypeError {
                        code: None,
                        help: None,
                        message: format!("return type mismatch: expected `{expected}`, got `void`"),
                        span: Span::dummy(),
                    });
                }
            }
            Stmt::Expression { expression } => {
                let _ = self.check_expression(expression);
            }
            Stmt::Say { expression, .. } => {
                let arg_ty = self.check_expression(expression);
                if let Some(ty) = &arg_ty
                    && !Ty::String.is_assignable_from(ty)
                {
                    self.errors.push(TypeError {
                        code: None,
                        help: None,
                        message: format!("`say` expects a string, got `{ty}`"),
                        span: expression.span(),
                    });
                }
            }
            Stmt::Match {
                expression,
                cases,
                default_case,
            } => {
                let scrutinee_ty = self.check_expression(expression);
                for case in cases {
                    match &case.pattern {
                        Some(pattern) => {
                            self.check_pattern_case(case, pattern, scrutinee_ty.clone())
                        }
                        None => {
                            let _ = self.check_expression(&case.value);
                            self.check_statement(&case.body);
                        }
                    }
                }
                if let Some(default) = default_case {
                    self.check_statement(default);
                }
            }
            Stmt::Try {
                try_block,
                catch_var,
                catch_block,
                finally_block,
                ..
            } => {
                self.check_statement(try_block);
                if let (Some(var), Some(catch)) = (catch_var, catch_block) {
                    self.symbols.push_scope();
                    // The catch variable holds the exception message.
                    if let Err(msg) = self.symbols.define(var.lexeme(), Ty::String) {
                        self.errors.push(TypeError {
                            code: None,
                            help: None,
                            message: msg,
                            span: var.span,
                        });
                    }
                    self.check_statement(catch);
                    self.symbols.pop_scope();
                } else if let Some(catch) = catch_block {
                    self.check_statement(catch);
                }
                if let Some(finally) = finally_block {
                    self.check_statement(finally);
                }
            }
            Stmt::Throw { value } => {
                let _ = self.check_expression(value);
            }
            Stmt::Retry {
                count,
                body,
                catch_var,
                catch_block,
            } => {
                let count_ty = self.check_expression(count);
                if let Some(ty) = &count_ty
                    && !Ty::Int.is_assignable_from(ty)
                {
                    self.errors.push(TypeError {
                        code: None,
                        help: None,
                        message: format!("`retry` count must be `int`, got `{ty}`"),
                        span: count.span(),
                    });
                }
                self.check_statement(body);
                if let (Some(var), Some(catch)) = (catch_var, catch_block) {
                    self.symbols.push_scope();
                    // The catch variable holds the last exception message.
                    if let Err(msg) = self.symbols.define(var.lexeme(), Ty::String) {
                        self.errors.push(TypeError {
                            code: None,
                            help: None,
                            message: msg,
                            span: var.span,
                        });
                    }
                    self.check_statement(catch);
                    self.symbols.pop_scope();
                } else if let Some(catch) = catch_block {
                    self.check_statement(catch);
                }
            }
            Stmt::Class {
                name, parent, body, ..
            } => {
                // Check parent exists if specified.
                if let Some(parent_token) = parent
                    && self.symbols.lookup(parent_token.lexeme()).is_none()
                {
                    self.errors.push(TypeError {
                        code: None,
                        help: None,
                        message: format!("parent class `{}` is not defined", parent_token.lexeme()),
                        span: parent_token.span,
                    });
                }

                // Check class body in a new scope with `this`.
                self.symbols.push_scope();
                let _ = self.symbols.define("this", Ty::Class(name.lexeme().into()));
                let prev_in_class = self.in_class_body;
                self.in_class_body = true;
                for member in body {
                    self.check_statement(member);
                }
                self.in_class_body = prev_in_class;
                self.symbols.pop_scope();
            }
            Stmt::Enum { .. } | Stmt::TypeAlias { .. } => {}
            Stmt::Trait { .. } | Stmt::Impl { .. } => {}
            Stmt::Break { .. } | Stmt::Continue { .. } => {}
            Stmt::Unsafe { body } => {
                self.unsafe_depth += 1;
                self.check_statement(body);
                self.unsafe_depth -= 1;
            }
            Stmt::Quiet { body, .. } => {
                self.check_statement(body);
            }
            Stmt::Destructure {
                is_array,
                is_tuple,
                names,
                initializer,
                ..
            } => {
                let init_ty = self.check_expression(initializer);
                if matches!(&init_ty, Some(ty) if is_borrow_ty(ty)) {
                    self.errors.push(TypeError {
                        code: None,
                        help: None,
                        message: "cannot destructure a view; destructuring moves values out of their source".into(),
                        span: initializer.span(),
                    });
                }

                if *is_tuple {
                    // Tuple destructuring: each binding gets the
                    // corresponding element type.
                    if let Some(Ty::Tuple(element_tys)) = &init_ty {
                        for (position, name) in names.iter().enumerate() {
                            let bound_ty = if position < element_tys.len() {
                                element_tys[position].clone()
                            } else {
                                Ty::Any
                            };
                            if let Err(msg) = self.symbols.define(name.lexeme(), bound_ty.clone()) {
                                self.errors.push(TypeError {
                                    code: None,
                                    help: None,
                                    message: msg,
                                    span: name.span,
                                });
                            }
                        }
                    } else {
                        let bound_ty = Ty::Any;
                        for name in names {
                            if let Err(msg) = self.symbols.define(name.lexeme(), bound_ty.clone()) {
                                self.errors.push(TypeError {
                                    code: None,
                                    help: None,
                                    message: msg,
                                    span: name.span,
                                });
                            }
                        }
                    }
                    return;
                }

                // Each bound name is a new variable. Array destructuring
                // hands out the source's element type; object
                // destructuring reads fields whose types are not tracked,
                // so those stay `any`.
                let bound_ty = match (is_array, &init_ty) {
                    (true, Some(Ty::Array(element))) => (**element).clone(),
                    _ => Ty::Any,
                };
                for name in names {
                    if let Err(msg) = self.symbols.define(name.lexeme(), bound_ty.clone()) {
                        self.errors.push(TypeError {
                            code: None,
                            help: None,
                            message: msg,
                            span: name.span,
                        });
                    }
                }
            }
            Stmt::Use {
                library,
                is_file_path: false,
                alias,
                imported_symbols,
                ..
            } if imported_symbols.is_empty() => {
                // `use strings as s` binds the alias `s`; bare imports bind the
                // module name itself. Either way it is an object namespace.
                // Rust crates (`use @serde_json`) behave identically.
                let name = alias
                    .as_ref()
                    .map_or_else(|| library.lexeme(), |a| a.lexeme());
                if self.symbols.lookup(name).is_none() {
                    self.symbols.define(name, Ty::Object).ok();
                }
            }
            // `use "file.nt" as arm` binds `arm` as an object-like namespace
            // whose members (`arm.func`) type as unknowns, like stdlib.
            Stmt::Use {
                is_file_path: true,
                alias: Some(alias),
                imported_symbols,
                ..
            } if imported_symbols.is_empty() => {
                let name = alias.lexeme();
                if self.symbols.lookup(name).is_none() {
                    self.symbols.define(name, Ty::Object).ok();
                }
            }
            Stmt::Use {
                imported_symbols, ..
            } if !imported_symbols.is_empty() => {
                for symbol in imported_symbols {
                    let name = symbol.lexeme();
                    if self.symbols.lookup(name).is_none() {
                        self.symbols.define(name, Ty::Any).ok();
                    }
                }
            }
            Stmt::Use { .. } => {}
        }
    }

    fn check_function(
        &mut self,
        name: &str,
        params: &[ntsc_ast::expr::FunctionParam],
        return_type: &Option<ntsc_ast::types::ReturnType>,
        body: &[Stmt],
        span: Span,
        is_async: bool,
    ) {
        let expected_return = match return_type {
            Some(r) => {
                let resolved = self.resolve_annotation(Some(&r.ty));
                if is_borrow_ty(&resolved) {
                    self.errors.push(TypeError {
                        code: None,
                        help: None,
                        message: format!(
                            "functions cannot return a borrow (`{resolved}`); a borrow cannot outlive the value it points at — return an owned value instead"
                        ),
                        span: r.arrow_span,
                    });
                }
                resolved
            }
            None => Ty::Void,
        };

        self.symbols.push_scope();
        self.capture_bases.push(self.symbols.depth());
        let prev_return_type = self.current_return_type.replace(expected_return.clone());
        let prev_async_depth = self.async_depth;
        if is_async {
            self.async_depth += 1;
            CHAN_RECV_TYPES.with(|map| map.borrow_mut().clear());
            self.current_fn_name = Some(name.to_string());
        }

        if is_async {
            for stmt in body {
                self.validate_await_placement(stmt, true);
                self.validate_chan_placement(stmt, true);
            }
        }

        // Register parameters.
        for param in params {
            let param_ty = self.resolve_annotation(param.type_annotation.as_ref());
            if let Err(msg) = self.symbols.define(param.name.lexeme(), param_ty) {
                self.errors.push(TypeError {
                    code: None,
                    help: None,
                    message: msg,
                    span: param.name.span,
                });
            }
        }

        if is_async {
            for stmt in body {
                self.validate_await_placement(stmt, true);
                self.validate_chan_placement(stmt, true);
            }
        }

        for stmt in body {
            self.check_statement(stmt);
        }

        if expected_return != Ty::Void && can_fall_through(body) {
            self.errors.push(TypeError {
                code: None,
                help: Some(format!("add a `return` on every path of function `{name}`")),
                message: format!(
                    "function `{name}` may fall through without returning `{expected_return}`"
                ),
                span,
            });
        }

        // Store function body for const evaluation.
        // Only non-async functions with simple bodies (single return) are
        // evaluated at build time.
        if !is_async && !name.is_empty() {
            let param_names: Vec<String> =
                params.iter().map(|p| p.name.lexeme().to_string()).collect();
            self.const_eval
                .fn_bodies
                .insert(name.to_string(), body.to_vec());
            self.const_eval
                .fn_params
                .insert(name.to_string(), param_names);
        }

        self.async_depth = prev_async_depth;
        self.current_return_type = prev_return_type;
        self.current_fn_name = None;
        self.capture_bases.pop();
        self.symbols.pop_scope();
    }

    fn validate_await_placement(&mut self, stmt: &Stmt, _at_top_level: bool) {
        match stmt {
            Stmt::Expression { expression }
            | Stmt::Var {
                initializer: Some(expression),
                ..
            }
            | Stmt::Return {
                value: Some(expression),
                ..
            } => self.validate_await_expression(expression),
            Stmt::Say { expression, .. } | Stmt::Throw { value: expression } => {
                self.validate_await_expression(expression)
            }
            Stmt::Block { statements, .. } => {
                for inner in statements {
                    self.validate_await_placement(inner, false);
                }
            }
            Stmt::If {
                condition,
                then_branch,
                elif_branches,
                else_branch,
            } => {
                self.validate_await_expression(condition);
                self.validate_await_placement(then_branch, false);
                for branch in elif_branches {
                    self.validate_await_expression(&branch.condition);
                    self.validate_await_placement(&branch.body, false);
                }
                if let Some(else_branch) = else_branch {
                    self.validate_await_placement(else_branch, false);
                }
            }
            Stmt::While { condition, body } => {
                self.validate_await_expression(condition);
                self.validate_await_placement(body, false);
            }
            Stmt::DoWhile { body, condition } => {
                self.validate_await_placement(body, false);
                self.validate_await_expression(condition);
            }
            Stmt::For {
                init,
                condition,
                update,
                body,
            } => {
                if let Some(init) = init {
                    self.validate_await_placement(init, false);
                }
                if let Some(condition) = condition {
                    self.validate_await_expression(condition);
                }
                if let Some(update) = update {
                    self.validate_await_expression(update);
                }
                self.validate_await_placement(body, false);
            }
            Stmt::ForIn { iterable, body, .. } => {
                self.validate_await_expression(iterable);
                self.validate_await_placement(body, false);
            }
            Stmt::ForAwait { producer, body, .. } => {
                self.validate_await_expression(producer);
                self.validate_await_placement(body, false);
            }
            Stmt::Match {
                expression,
                cases,
                default_case,
            } => {
                self.validate_await_expression(expression);
                for case in cases {
                    self.validate_await_expression(&case.value);
                    if let Some(guard) = &case.guard {
                        self.validate_await_expression(guard);
                    }
                    self.validate_await_placement(&case.body, false);
                }
                if let Some(default_case) = default_case {
                    self.validate_await_placement(default_case, false);
                }
            }
            Stmt::Try {
                try_block,
                catch_block,
                finally_block,
                ..
            } => {
                self.validate_await_placement(try_block, false);
                if let Some(catch_block) = catch_block {
                    self.validate_await_placement(catch_block, false);
                }
                if let Some(finally_block) = finally_block {
                    self.validate_await_placement(finally_block, false);
                }
            }
            Stmt::Retry {
                count,
                body,
                catch_block,
                ..
            } => {
                self.validate_await_expression(count);
                self.validate_await_placement(body, false);
                if let Some(catch_block) = catch_block {
                    self.validate_await_placement(catch_block, false);
                }
            }
            Stmt::ChanRecvFor { channel, body, .. } => {
                self.validate_await_expression(channel);
                self.validate_await_placement(body, false);
            }
            _ => {}
        }
    }

    fn validate_await_expression(&mut self, expression: &Expr) {
        if let Expr::Await { .. } = expression {
            return;
        }
        if let Expr::Assign { value, .. } = expression
            && matches!(value.as_ref(), Expr::Await { .. })
        {
            return;
        }
        if let Some(span) = find_await(expression) {
            self.errors.push(TypeError {
                code: None,
                help: None,
                message: "await must be a statement-level call".into(),
                span,
            });
        }
    }

    fn validate_chan_placement(&mut self, stmt: &Stmt, _at_top_level: bool) {
        match stmt {
            Stmt::Expression { expression } => self.validate_chan_expression(expression),
            Stmt::Block { statements, .. } => {
                for inner in statements {
                    self.validate_chan_placement(inner, false);
                }
            }
            Stmt::If {
                condition,
                then_branch,
                elif_branches,
                else_branch,
            } => {
                self.validate_chan_expression(condition);
                self.validate_chan_placement(then_branch, false);
                for branch in elif_branches {
                    self.validate_chan_expression(&branch.condition);
                    self.validate_chan_placement(&branch.body, false);
                }
                if let Some(else_branch) = else_branch {
                    self.validate_chan_placement(else_branch, false);
                }
            }
            Stmt::While { condition, body } | Stmt::DoWhile { condition, body } => {
                self.validate_chan_expression(condition);
                self.validate_chan_placement(body, false);
            }
            Stmt::For {
                init,
                condition,
                update,
                body,
            } => {
                if let Some(init) = init {
                    self.validate_chan_placement(init, false);
                }
                if let Some(condition) = condition {
                    self.validate_chan_expression(condition);
                }
                if let Some(update) = update {
                    self.validate_chan_expression(update);
                }
                self.validate_chan_placement(body, false);
            }
            Stmt::ForIn { iterable, body, .. }
            | Stmt::ForAwait {
                producer: iterable,
                body,
                ..
            } => {
                self.validate_chan_expression(iterable);
                self.validate_chan_placement(body, false);
            }
            Stmt::Match {
                expression,
                cases,
                default_case,
            } => {
                self.validate_chan_expression(expression);
                for case in cases {
                    self.validate_chan_expression(&case.value);
                    if let Some(guard) = &case.guard {
                        self.validate_chan_expression(guard);
                    }
                    self.validate_chan_placement(&case.body, false);
                }
                if let Some(default_case) = default_case {
                    self.validate_chan_placement(default_case, false);
                }
            }
            Stmt::Try {
                try_block,
                catch_block,
                finally_block,
                ..
            } => {
                self.validate_chan_placement(try_block, false);
                if let Some(catch_block) = catch_block {
                    self.validate_chan_placement(catch_block, false);
                }
                if let Some(finally_block) = finally_block {
                    self.validate_chan_placement(finally_block, false);
                }
            }
            Stmt::Retry {
                count,
                body,
                catch_block,
                ..
            } => {
                self.validate_chan_expression(count);
                self.validate_chan_placement(body, false);
                if let Some(catch_block) = catch_block {
                    self.validate_chan_placement(catch_block, false);
                }
            }
            Stmt::ChanRecvFor { channel, body, .. } => {
                self.validate_chan_expression(channel);
                self.validate_chan_placement(body, false);
            }
            _ => {}
        }
    }

    fn validate_chan_expression(&mut self, expression: &Expr) {
        if find_chan_in_expr(expression)
            && !matches!(expression, Expr::ChanSend { .. } | Expr::ChanRecv { .. })
        {
            self.errors.push(TypeError {
                code: None,
                help: None,
                message: "channel send/receive must be a statement-level expression".into(),
                span: expression.span(),
            });
        }
    }

    fn require_function_signature(
        &mut self,
        name: &ntsc_ast::token::Token,
        params: &[ntsc_ast::expr::FunctionParam],
        return_type: &Option<ntsc_ast::types::ReturnType>,
    ) {
        for param in params {
            if param.type_annotation.is_none() {
                self.errors.push(TypeError {
                    code: None,
                    help: None,
                    message: format!(
                        "parameter `{}` requires an explicit type annotation",
                        param.name.lexeme()
                    ),
                    span: param.name.span,
                });
            }
            self.reject_dynamic_annotation(param.type_annotation.as_ref(), param.name.span);
            self.require_known_annotation(param.type_annotation.as_ref(), param.name.span);
            self.validate_shared_annotation(param.type_annotation.as_ref(), param.name.span);
            self.validate_no_view_storage_annotation(
                param.type_annotation.as_ref(),
                param.name.span,
            );
        }
        // Missing return type defaults to `void` (see LANGUAGE.md §6.3).
        if return_type.is_none() {
        } else if let Some(return_type) = return_type {
            self.reject_dynamic_annotation(Some(&return_type.ty), name.span);
            self.require_known_annotation(Some(&return_type.ty), name.span);
            self.validate_shared_annotation(Some(&return_type.ty), name.span);
            self.validate_no_view_storage_annotation(Some(&return_type.ty), name.span);
        }
    }

    /// Reject `shared T` annotations whose inner type cannot be shared.
    fn validate_shared_annotation(&mut self, annotation: Option<&TypeAnnotation>, span: Span) {
        let Some(annotation) = annotation else {
            return;
        };
        match annotation {
            TypeAnnotation::Shared(inner) => {
                let resolved = self.resolve_annotation(Some(inner));
                if !resolved.viewable() {
                    self.errors.push(TypeError {
                        code: None,
                        help: None,
                        message: format!(
                            "cannot share a value of type `{resolved}`; `shared` requires a heap type (string, array, object, class, any)"
                        ),
                        span,
                    });
                }
            }
            TypeAnnotation::Array(Some(element)) | TypeAnnotation::Option(element) => {
                self.validate_shared_annotation(Some(element), span);
            }
            TypeAnnotation::Result { ok, err } => {
                self.validate_shared_annotation(Some(ok), span);
                self.validate_shared_annotation(Some(err), span);
            }
            _ => {}
        }
    }

    /// Whether `name` was resolved at a scope belonging to an enclosing
    /// function rather than the current function or the global scope.
    fn is_captured(&self, name: &str, depth: usize) -> bool {
        if name == "_" || self.capture_bases.is_empty() {
            return false;
        }
        depth > 0 && depth < *self.capture_bases.last().expect("checked non-empty")
    }

    /// Reject annotations that store a view inside an owned container
    /// (`array[view T]`, `option[view T]`, `shared view T`), which would
    /// require the container to own a borrow it cannot enforce.
    fn validate_no_view_storage_annotation(
        &mut self,
        annotation: Option<&TypeAnnotation>,
        span: Span,
    ) {
        let Some(annotation) = annotation else {
            return;
        };
        match annotation {
            TypeAnnotation::Array(Some(inner)) | TypeAnnotation::Option(inner) => {
                let inner_ty = self.resolve_annotation(Some(inner));
                if matches!(inner_ty, Ty::View(..)) {
                    let container = if matches!(annotation, TypeAnnotation::Array(_)) {
                        "array"
                    } else {
                        "option"
                    };
                    self.errors.push(TypeError {
                        code: None,
                        help: None,
                        message: format!(
                            "cannot store a view inside `{container}[{inner_ty}]`; views cannot live inside arrays or options",
                        ),
                        span,
                    });
                }
                self.validate_no_view_storage_annotation(Some(inner), span);
            }
            TypeAnnotation::Result { ok, err } => {
                for inner in [ok, err] {
                    let inner_ty = self.resolve_annotation(Some(inner));
                    if matches!(inner_ty, Ty::View(..)) {
                        self.errors.push(TypeError {
                            code: None,
                            help: None,
                            message: "cannot store a view inside `result[.., ..]`; views cannot live inside results".to_string(),
                            span,
                        });
                    }
                    self.validate_no_view_storage_annotation(Some(inner), span);
                }
            }
            TypeAnnotation::Shared(inner) => {
                let inner_ty = self.resolve_annotation(Some(inner));
                if matches!(inner_ty, Ty::View(..)) {
                    self.errors.push(TypeError {
                        code: None,
                        help: None,
                        message: format!(
                            "cannot store a view inside `shared {inner_ty}`; shared values cannot hold views",
                        ),
                        span,
                    });
                }
                self.validate_no_view_storage_annotation(Some(inner), span);
            }
            _ => {}
        }
    }

    fn reject_dynamic_annotation(&mut self, annotation: Option<&TypeAnnotation>, span: Span) {
        if matches!(annotation, Some(TypeAnnotation::Any)) {
            self.errors.push(TypeError {
                code: None,
                help: None,
                message: "`any` is not supported in statically typed NTSC".into(),
                span,
            });
        }
        match annotation {
            Some(TypeAnnotation::Array(Some(element))) | Some(TypeAnnotation::Option(element)) => {
                self.reject_dynamic_annotation(Some(element), span);
            }
            Some(TypeAnnotation::Result { ok, err }) => {
                self.reject_dynamic_annotation(Some(ok), span);
                self.reject_dynamic_annotation(Some(err), span);
            }
            _ => {}
        }
    }

    fn require_known_annotation(&mut self, annotation: Option<&TypeAnnotation>, span: Span) {
        match annotation {
            Some(TypeAnnotation::Named(name)) if name.lexeme() == "void" => {}
            Some(TypeAnnotation::Named(name))
                if matches!(
                    name.lexeme(),
                    "byte"
                        | "c_char"
                        | "c_uchar"
                        | "c_short"
                        | "c_ushort"
                        | "c_int"
                        | "c_uint"
                        | "c_long"
                        | "c_ulong"
                        | "c_longlong"
                        | "c_ulonglong"
                        | "c_size_t"
                        | "c_float"
                        | "c_double"
                ) => {}
            Some(TypeAnnotation::Named(name))
                if !matches!(self.symbols.lookup(name.lexeme()), Some(Ty::Class(_))) =>
            {
                self.errors.push(TypeError {
                    code: None,
                    help: None,
                    message: format!("unknown type `{}`", name.lexeme()),
                    span,
                });
            }
            Some(TypeAnnotation::Array(Some(element))) | Some(TypeAnnotation::Option(element)) => {
                self.require_known_annotation(Some(element), span);
            }
            Some(TypeAnnotation::Result { ok, err }) => {
                self.require_known_annotation(Some(ok), span);
                self.require_known_annotation(Some(err), span);
            }
            _ => {}
        }
    }

    fn check_var(
        &mut self,
        name: &ntsc_ast::token::Token,
        type_annotation: &Option<TypeAnnotation>,
        initializer: &Option<Expr>,
        view: &Option<ntsc_ast::types::ViewMutability>,
        is_const: bool,
    ) {
        self.reject_dynamic_annotation(type_annotation.as_ref(), name.span);
        self.require_known_annotation(type_annotation.as_ref(), name.span);
        self.validate_shared_annotation(type_annotation.as_ref(), name.span);
        self.validate_no_view_storage_annotation(type_annotation.as_ref(), name.span);

        let mut const_folded_val: Option<ConstValue> = None;
        if is_const {
            self.consts.insert(name.lexeme().to_string());
            if let Some(init) = initializer {
                if self.const_eval.is_constant_expr(init) {
                    // Evaluate at compile time.
                    if let Some(val) = self.const_eval.eval(init) {
                        const_folded_val = Some(val.clone());
                        let const_name = name.lexeme().to_string();
                        CONST_EVAL_VALUES.with(|cell| {
                            cell.borrow_mut().insert(const_name.clone(), val.clone());
                        });
                        // Register in the evaluator so later constants can
                        // reference this one.
                        self.const_eval.constants.insert(const_name, val);
                    }
                } else {
                    self.errors.push(TypeError {
                        code: None,
                        help: None,
                        message: format!(
                            "`static const` variable `{}` requires a constant expression initializer",
                            name.lexeme()
                        ),
                        span: name.span,
                    });
                }
            }
        }

        if self.async_depth > 0 && type_annotation.is_none() {
            // Async locals live in the future struct, whose field types
            // are fixed at compile time, so an unannotated variable must
            // have a statically inferable type (a literal or an awaited
            // async call).
            let inferable = initializer
                .as_ref()
                .is_some_and(|init| matches!(init, Expr::Literal { .. } | Expr::Await { .. }));
            if !inferable {
                self.errors.push(TypeError {
                    code: None,
                    help: None,
                    message: format!(
                        "variable `{}` in an async function requires an explicit type annotation",
                        name.lexeme()
                    ),
                    span: name.span,
                });
            }
        }
        let declared_ty = self.resolve_annotation(type_annotation.as_ref());
        let init_ty = if let Some(val) = const_folded_val {
            Some(match val {
                ConstValue::Int(_) => Ty::Int,
                ConstValue::Float(_) => Ty::Float,
                ConstValue::Bool(_) => Ty::Bool,
                ConstValue::String(_) => Ty::String,
            })
        } else {
            initializer
                .as_ref()
                .and_then(|expr| self.check_expression(expr))
        };

        // A view declaration stores a borrow, so a view-typed initializer is
        // allowed there (and only there).
        if view.is_none()
            && let Some(Ty::View(..)) = &init_ty
        {
            self.errors.push(TypeError {
                code: None,
                message: format!(
                    "cannot store a view in variable `{}`; views are block-scoped and cannot outlive their source",
                    name.lexeme()
                ),
                help: Some("store an owned value instead: `var T name = copy(source)`".into()),
                span: name.span,
            });
        }

        // View declarations must have a source to borrow.
        if view.is_some() && initializer.is_none() {
            self.errors.push(TypeError {
                code: None,
                help: None,
                message: format!(
                    "view variable `{}` must be initialized with a value to borrow",
                    name.lexeme()
                ),
                span: name.span,
            });
        }

        let final_ty = match (&type_annotation, &init_ty, view) {
            // `view var` / `view mut var`: the declared type is a view of
            // the annotated type, or of the initializer's inferred type.
            (_, _, Some(_)) => {
                let inner = if matches!(declared_ty, Ty::Any) {
                    match init_ty.clone().unwrap_or(Ty::Any) {
                        // A view of a shared value borrows the pointee,
                        // not the box.
                        Ty::Shared(inner) => *inner,
                        other => other,
                    }
                } else {
                    declared_ty.clone()
                };
                if !inner.viewable() && !matches!(inner, Ty::Any) {
                    self.errors.push(TypeError {
                        code: None,
                        help: None,
                        message: format!(
                            "cannot take a view of type `{inner}`; views require a heap type (string, array, object, class, shared value)"
                        ),
                        span: name.span,
                    });
                }

                if let Some(init_ty) = &init_ty {
                    let borrowed = match init_ty {
                        // A view of a shared value borrows the pointee, so
                        // compare against what is inside the box rather
                        // than the box itself — otherwise `view var v = s`
                        // on a `shared array[int]` reads as an
                        // `array<int>`/`shared array[int]` mismatch and
                        // every borrow of a shared value is rejected.
                        Ty::Shared(pointee) => pointee.as_ref(),
                        other => other,
                    };
                    if !inner.is_assignable_from(borrowed) {
                        self.errors.push(TypeError {
                            code: None,
                            help: None,
                            message: format!(
                                "type mismatch: expected a view of `{inner}`, got `{init_ty}`"
                            ),
                            span: name.span,
                        });
                    }
                }
                Ty::View(
                    Box::new(inner),
                    *view == Some(ntsc_ast::types::ViewMutability::Mutable),
                )
            }
            (Some(_), Some(init_ty), _) => {
                // Both declared and initialized — check compatibility.
                if !self.assignable(&declared_ty, init_ty) {
                    self.errors.push(TypeError {
                        code: None,
                        help: None,
                        message: format!(
                            "type mismatch: expected `{declared_ty}`, got `{init_ty}`"
                        ),
                        span: name.span,
                    });
                }
                declared_ty
            }
            (Some(_), None, _) => {
                // Class fields may be declared without an initializer;
                // they are zero-initialized by the runtime.
                if self.in_class_body {
                    declared_ty
                } else {
                    if view.is_none() {
                        self.errors.push(TypeError {
                            code: None,
                            help: None,
                            message: format!("variable `{}` must be initialized", name.lexeme()),
                            span: name.span,
                        });
                    }
                    declared_ty
                }
            }
            (None, Some(ty), _) => ty.clone(),
            (None, None, _) => {
                if view.is_none() {
                    self.errors.push(TypeError {
                        code: None,
                        help: None,
                        message: format!("variable `{}` must be initialized", name.lexeme()),
                        span: name.span,
                    });
                }
                Ty::Any
            }
        };

        if let Err(msg) = self.symbols.define(name.lexeme(), final_ty) {
            self.errors.push(TypeError {
                code: None,
                help: None,
                message: msg,
                span: name.span,
            });
        }
    }

    fn check_condition(&mut self, expr: &Expr) {
        let ty = self.check_expression(expr);
        if let Some(ty) = &ty
            && !matches!(ty, Ty::Bool | Ty::Any)
        {
            self.errors.push(TypeError {
                code: None,
                help: None,
                message: format!("condition must be `bool`, got `{ty}`"),
                span: expr.span(),
            });
        }
    }

    /// Assignability including trait-object coercions: `dyn P` accepts a
    /// class instance whose class implements `P` (directly or through a
    /// supertrait), and same-trait dyn values move between dyn slots.
    fn assignable(&self, expected: &Ty, actual: &Ty) -> bool {
        if expected.is_assignable_from(actual) {
            return true;
        }
        match (expected, actual) {
            (Ty::Dyn(expected_trait), Ty::Dyn(actual_trait)) => expected_trait == actual_trait,
            (Ty::Dyn(trait_name), Ty::Class(class_name)) => {
                crate::generics::implementation_exists(trait_name, class_name)
            }
            (Ty::Own(expected_inner), Ty::Own(actual_inner)) => {
                self.assignable(expected_inner, actual_inner)
            }
            (Ty::Own(inner), _) => self.assignable(inner, actual),
            _ => false,
        }
    }

    fn check_expression(&mut self, expr: &Expr) -> Option<Ty> {
        match expr {
            Expr::Literal { value, .. } => Some(self.literal_type(value)),
            Expr::Variable { name } => {
                let lex = name.lexeme().trim_start_matches('@');
                match self.symbols.lookup_depth(lex) {
                    Some((depth, ty)) => {
                        if self.is_captured(lex, depth) {
                            self.errors.push(TypeError {
                                code: None,
                                help: None,
                                message: format!(
                                    "lambda cannot capture outer variable `{}`; pass it as a parameter or use a global value",
                                    lex
                                ),
                                span: name.span,
                            });
                        }
                        Some(ty.clone())
                    }
                    None => {
                        if lex == "_" {
                            return Some(Ty::Any);
                        }
                        if self.enum_members.contains(lex) {
                            return Some(Ty::Int);
                        }
                        self.errors.push(TypeError {
                            code: None,
                            help: None,
                            message: format!("undefined variable `{}`", lex),
                            span: name.span,
                        });
                        Some(Ty::Any)
                    }
                }
            }
            Expr::Binary { left, op, right } => self.check_binary(left, op, right),
            Expr::Unary { op, right } => self.check_unary(op, right),
            Expr::PostfixUnary { op, left } => self.check_postfix_unary(op, left),
            Expr::Grouping { expression, .. } => self.check_expression(expression),
            Expr::Call {
                callee, arguments, ..
            } => self.check_call(callee, arguments),
            Expr::Assign { name, value } => {
                let value_ty = self.check_expression(value);

                if self.consts.contains(name.lexeme()) {
                    self.errors.push(TypeError {
                        code: None,
                        help: None,
                        message: format!(
                            "cannot assign to `static const` variable `{}`; constants are immutable",
                            name.lexeme()
                        ),
                        span: name.span,
                    });
                }

                // Assigning a view into a variable stores a borrow in a
                // slot that may outlive the source, exactly like declaring
                // one does. The declaration form is caught above; without
                // this the same escape slips through as a plain assignment.
                if matches!(&value_ty, Some(ty) if is_borrow_ty(ty))
                    && !matches!(
                        self.symbols.lookup(name.lexeme()),
                        Some(Ty::View(..) | Ty::Ref(..)) | None
                    )
                {
                    self.errors.push(TypeError {
                        code: None,
                        message: format!(
                            "cannot assign a view to variable `{}`; views are block-scoped and cannot outlive their source",
                            name.lexeme()
                        ),
                        help: Some("assign an owned value instead: `name = copy(source)`".into()),
                        span: name.span,
                    });
                }
                if let Some((depth, declared)) = self.symbols.lookup_depth(name.lexeme()) {
                    if self.is_captured(name.lexeme(), depth) {
                        self.errors.push(TypeError {
                            code: None,
                            help: None,
                            message: format!(
                                "lambda cannot capture outer variable `{}`; pass it as a parameter or use a global value",
                                name.lexeme()
                            ),
                            span: name.span,
                        });
                    }
                    if let Some(vt) = &value_ty
                        && !self.assignable(declared, vt)
                    {
                        self.errors.push(TypeError {
                            code: None,
                            help: None,
                            message: format!(
                                "cannot assign `{vt}` to variable of type `{declared}`"
                            ),
                            span: name.span,
                        });
                    }
                    Some(declared.clone())
                } else {
                    if self.enum_members.contains(name.lexeme()) {
                        return Some(Ty::Int);
                    }
                    self.errors.push(TypeError {
                        code: None,
                        help: None,
                        message: format!("undefined variable `{}`", name.lexeme()),
                        span: name.span,
                    });
                    Some(Ty::Any)
                }
            }
            Expr::Ternary {
                condition,
                then_branch,
                else_branch,
            } => {
                self.check_condition(condition);
                let then_ty = self.check_expression(then_branch);
                let else_ty = self.check_expression(else_branch);
                match (then_ty, else_ty) {
                    (Some(a), Some(b)) => {
                        if a == b {
                            Some(a)
                        } else {
                            Some(Ty::Any)
                        }
                    }
                    (Some(ty), None) | (None, Some(ty)) => Some(ty),
                    _ => None,
                }
            }
            Expr::Propagate {
                value,
                question_span,
            } => {
                let value_ty = self.check_expression(value);
                match value_ty {
                    Some(Ty::Result { ok, err }) => match self.current_return_type.clone() {
                        Some(Ty::Result {
                            ok: fn_ok,
                            err: fn_err,
                        }) => {
                            let err_compatible =
                                self.assignable(&fn_err, &err) || matches!(*fn_err, Ty::String);
                            if !err_compatible {
                                self.errors.push(TypeError {
                                    code: None,
                                    help: None,
                                    message: format!(
                                        "cannot propagate error of type `{err}` from a function returning `{}`",
                                        Ty::Result {
                                            ok: fn_ok.clone(),
                                            err: fn_err.clone(),
                                        }
                                    ),
                                    span: *question_span,
                                });
                            }
                            // The propagated expression yields the operand's Ok
                            // payload; only an early Err return carries the
                            // function's result shape.
                            Some(*ok)
                        }
                        Some(other) => {
                            self.errors.push(TypeError {
                                code: None,
                                help: None,
                                message: format!(
                                    "`?` can only be used in a function returning a `result`, got `{other}`"
                                ),
                                span: *question_span,
                            });
                            Some(*ok)
                        }
                        None => {
                            self.errors.push(TypeError {
                                code: None,
                                help: None,
                                message: "`?` can only be used inside a function".into(),
                                span: *question_span,
                            });
                            Some(*ok)
                        }
                    },
                    Some(Ty::Any) => Some(Ty::Any),
                    Some(other) => {
                        self.errors.push(TypeError {
                            code: None,
                            help: None,
                            message: format!("`?` requires a `result` value, got `{other}`"),
                            span: *question_span,
                        });
                        Some(Ty::Any)
                    }
                    None => None,
                }
            }
            Expr::Lambda {
                params,
                return_type,
                body,
                span,
            } => {
                self.symbols.push_scope();
                self.capture_bases.push(self.symbols.depth());
                let param_tys: Vec<Ty> = params
                    .iter()
                    .map(|p| self.resolve_annotation(p.type_annotation.as_ref()))
                    .collect();
                for param in params {
                    let ty = self.resolve_annotation(param.type_annotation.as_ref());
                    self.validate_no_view_storage_annotation(
                        param.type_annotation.as_ref(),
                        param.name.span,
                    );
                    let _ = self.symbols.define(param.name.lexeme(), ty);
                }
                let ret = return_type
                    .as_ref()
                    .map(|r| self.resolve_annotation(Some(&r.ty)))
                    .unwrap_or(Ty::Void);
                let saved_return = self.current_return_type.take();
                self.current_return_type = Some(ret.clone());

                let saved_async_depth = self.async_depth;
                // A lambda is a separate sync function: `await` is not
                // allowed inside it even if the enclosing function is
                // async.
                self.async_depth = 0;
                for stmt in body {
                    self.check_statement(stmt);
                }
                if ret != Ty::Void && can_fall_through(body) {
                    self.errors.push(TypeError {
                        code: None,
                        help: Some("add a `return` on every path of the lambda".into()),
                        message: format!("lambda may fall through without returning `{ret}`"),
                        span: *span,
                    });
                }
                self.async_depth = saved_async_depth;
                self.current_return_type = saved_return;
                self.capture_bases.pop();
                self.symbols.pop_scope();
                Some(Ty::Function {
                    params: param_tys,
                    return_type: Box::new(ret),
                })
            }
            Expr::ArrayLiteral { elements, .. } => {
                // Skip spread elements when determining the element type
                // so a leading `...[1, 2, 3]` does not make the array
                // `[any]`.
                let first = elements
                    .iter()
                    .find(|e| !matches!(e, Expr::Spread { .. }))
                    .or_else(|| elements.first());
                if let Some(first) = first {
                    let elem_ty = self.check_expression(first);
                    for elem in elements {
                        let current_ty = self.check_expression(elem);
                        if matches!(&current_ty, Some(ty) if is_borrow_ty(ty)) {
                            // An element slot is owned by the container,
                            // which outlives the enclosing block, so a
                            // borrow stored there can dangle.
                            self.errors.push(TypeError {
                                code: None,
                                help: None,
                                message:
                                    "cannot store a view in an array; arrays own their elements"
                                        .into(),
                                span: elem.span(),
                            });
                        }
                        if let (Some(expected), Some(actual)) = (&elem_ty, &current_ty)
                            && !matches!(elem, Expr::Spread { .. })
                            && !expected.is_assignable_from(actual)
                        {
                            self.errors.push(TypeError {
                                code: None,
                                help: None,
                                message: format!(
                                    "array element type mismatch: expected `{expected}`, got `{actual}`"
                                ),
                                span: elem.span(),
                            });
                        }
                    }
                    Some(Ty::Array(Box::new(elem_ty.unwrap_or(Ty::Any))))
                } else {
                    Some(Ty::Array(Box::new(Ty::Any)))
                }
            }
            Expr::ObjectLiteral { properties, .. } => {
                for property in properties {
                    if let Some(value_ty) = self.check_expression(&property.value)
                        && is_borrow_ty(&value_ty)
                    {
                        // A field belongs to the instance, which can
                        // outlive the block the borrow was taken in, so a
                        // view may not be stored there.
                        self.errors.push(TypeError {
                            code: None,
                            help: None,
                            message: format!(
                                "cannot store a view in object property `{}`; objects own their properties",
                                property.key
                            ),
                            span: property.key_span,
                        });
                    }
                }
                Some(Ty::Object)
            }
            Expr::Member { object, property } => Some(self.member_ty(object, property)),
            Expr::OptionalMember { object, property } => Some(self.member_ty(object, property)),
            Expr::IndexGet { object, index } => {
                let obj_ty = self.check_expression(object);
                self.check_index(index);
                match obj_ty {
                    Some(Ty::Array(inner)) | Some(Ty::Slice(inner)) => Some(*inner),
                    _ => Some(Ty::Any),
                }
            }
            Expr::IndexSet {
                object,
                index,
                value,
            } => {
                let obj_ty = self.check_expression(object);
                self.check_index(index);
                let val_ty = self.check_expression(value);

                if matches!(&val_ty, Some(ty) if is_borrow_ty(ty)) {
                    self.errors.push(TypeError {
                        code: None,
                        message:
                            "cannot store a view in an array element; arrays own their elements"
                                .into(),
                        help: Some("store an owned value instead: `arr[i] = copy(source)`".into()),
                        span: value.span(),
                    });
                }
                if let Some(Ty::Array(inner)) = obj_ty
                    && let Some(vt) = &val_ty
                    && !inner.is_assignable_from(vt)
                {
                    // The slot has the element's declared type, so storing
                    // another type there would make later reads of that
                    // element lie.
                    self.errors.push(TypeError {
                        code: None,
                        help: None,
                        message: format!("array element type is `{inner}`, got `{vt}`"),
                        span: value.span(),
                    });
                }
                val_ty
            }
            Expr::TupleLiteral { elements, .. } => {
                let tys: Vec<Ty> = elements
                    .iter()
                    .map(|e| self.check_expression(e).unwrap_or(Ty::Any))
                    .collect();
                Some(Ty::Tuple(tys))
            }
            Expr::TupleIndex {
                object,
                index,
                dot_span,
            } => {
                let obj_ty = self.check_expression(object);
                match obj_ty {
                    Some(Ty::Tuple(tys)) => {
                        if *index < tys.len() {
                            Some(tys[*index].clone())
                        } else {
                            self.errors.push(TypeError {
                                code: None,
                                message: format!(
                                    "tuple index `{index}` is out of range (tuple has {} element{})",
                                    tys.len(),
                                    if tys.len() == 1 { "" } else { "s" }
                                ),
                                help: None,
                                span: *dot_span,
                            });
                            Some(Ty::Any)
                        }
                    }
                    _ => {
                        self.errors.push(TypeError {
                            code: None,
                            message: "cannot index into a non-tuple value with a numeric index"
                                .into(),
                            help: Some(
                                "use `var (a, b) = expr` to destructure, or `.field` for objects"
                                    .into(),
                            ),
                            span: *dot_span,
                        });
                        Some(Ty::Any)
                    }
                }
            }
            Expr::MemberSet {
                object,
                property,
                value,
            } => {
                let object_ty = self.check_expression(object);
                let val_ty = self.check_expression(value);

                if matches!(&val_ty, Some(ty) if is_borrow_ty(ty)) {
                    self.errors.push(TypeError {
                        code: None,
                        message: format!(
                            "cannot store a view in field `{}`; the instance owns its fields",
                            property.lexeme()
                        ),
                        help: Some(
                            "store an owned value instead: `obj.field = copy(source)`".into(),
                        ),
                        span: property.span,
                    });
                }

                if let Some(declared) = object_ty
                    .as_ref()
                    .and_then(base_class_name)
                    .and_then(|class| self.class_field_ty(class, property.lexeme()))
                    && let Some(vt) = &val_ty
                    && !declared.is_assignable_from(vt)
                {
                    // The slot has the field's declared type, so storing
                    // another type there would make later reads of that
                    // field lie.
                    self.errors.push(TypeError {
                        code: None,
                        help: None,
                        message: format!(
                            "field `{}` has type `{declared}`, got `{vt}`",
                            property.lexeme()
                        ),
                        span: property.span,
                    });
                }
                val_ty
            }
            Expr::Await {
                callee,
                arguments,
                span,
            } => self.check_await(callee, arguments, *span),
            Expr::AsyncBlock { body, .. } => {
                self.symbols.push_scope();
                for stmt in body {
                    self.check_statement(stmt);
                }
                self.symbols.pop_scope();
                Some(Ty::Any)
            }
            Expr::ChanSend {
                channel,
                value,
                op_span,
            } => {
                if self.async_depth == 0 {
                    self.errors.push(TypeError {
                        code: None,
                        help: None,
                        message: "a channel send is only allowed inside an async function".into(),
                        span: *op_span,
                    });
                }
                let channel_ty = self.check_expression(channel);
                let value_ty = self.check_expression(value);
                match (channel_ty, value_ty) {
                    (Some(Ty::Chan(element)), Some(actual))
                        if self.assignable(&element, &actual) =>
                    {
                        // send moves the value into the channel; returns the
                        // unit type.
                    }
                    (Some(Ty::Chan(element)), Some(actual)) => self.errors.push(TypeError {
                        code: None,
                        help: Some(format!("expected `{}`, got `{actual}`", element)),
                        message: format!("cannot send `{actual}` into `chan[{}]`", element),
                        span: *op_span,
                    }),
                    (Some(other), _) => self.errors.push(TypeError {
                        code: None,
                        help: Some("only a channel can receive a send".into()),
                        message: format!("cannot send into `{other}`; expected a channel"),
                        span: *op_span,
                    }),
                    _ => {}
                }
                Some(Ty::Void)
            }
            Expr::ChanRecv {
                receiver,
                channel,
                op_span,
                ..
            } => {
                if self.async_depth == 0 {
                    self.errors.push(TypeError {
                        code: None,
                        help: None,
                        message: "a channel receive is only allowed inside an async function"
                            .into(),
                        span: receiver.span,
                    });
                }
                let channel_ty = self.check_expression(channel);
                let element_ty = match &channel_ty {
                    Some(Ty::Chan(element)) => (**element).clone(),
                    _ => {
                        if let Some(other) = &channel_ty {
                            self.errors.push(TypeError {
                                code: None,
                                help: Some("only a channel can be received from".into()),
                                message: format!(
                                    "cannot receive from `{other}`; expected a channel"
                                ),
                                span: channel.span(),
                            });
                        }
                        Ty::Any
                    }
                };
                if let Err(msg) = self.symbols.define(receiver.lexeme(), element_ty.clone()) {
                    self.errors.push(TypeError {
                        code: None,
                        help: None,
                        message: msg,
                        span: receiver.span,
                    });
                }
                CHAN_RECV_TYPES.with(|map| {
                    map.borrow_mut().insert(op_span.start, element_ty.clone());
                });
                Some(element_ty)
            }
            Expr::Close { channel, keyword } => {
                let channel_ty = self.check_expression(channel);
                if let Some(other) = channel_ty
                    && !matches!(&other, Ty::Chan(_))
                {
                    self.errors.push(TypeError {
                        code: None,
                        help: Some("only a channel can be closed".into()),
                        message: format!("cannot close `{other}`; expected a channel"),
                        span: *keyword,
                    });
                }
                Some(Ty::Void)
            }
            Expr::View {
                target,
                mutable,
                keyword,
            } => {
                let target_ty = self.check_expression(target);
                match target_ty {
                    // A view of a shared value borrows the pointee, not
                    // the box.
                    Some(Ty::Shared(inner)) if inner.viewable() => Some(Ty::View(inner, *mutable)),
                    Some(inner) if inner.viewable() => Some(Ty::View(Box::new(inner), *mutable)),
                    Some(inner) => {
                        // A view of a plain value is not allowed: scalars
                        // cannot be borrowed.
                        self.errors.push(TypeError {
                            code: None,
                            help: None,
                            message: format!(
                                "cannot view a value of type `{inner}`; views require a heap type (string, array, object, class)"
                            ),
                            span: *keyword,
                        });
                        Some(inner)
                    }
                    None => None,
                }
            }
            Expr::Copy { expression, .. } => {
                let inner = self.check_expression(expression);

                match inner {
                    // `copy` dereferences a view and deep-copies the
                    // pointee; otherwise it returns the same owned type.
                    Some(Ty::View(inner_ty, _)) => Some(*inner_ty),

                    // `copy` of a shared value deep-copies the pointee
                    // into an independent owned value.
                    Some(Ty::Shared(inner_ty)) => Some(*inner_ty),

                    // Copying a window materializes an owned array of the
                    // elements it spans.
                    Some(Ty::Slice(element)) => Some(Ty::Array(element)),
                    Some(ty) => Some(ty),
                    None => None,
                }
            }
            Expr::Borrow {
                target, mutable, ..
            } => {
                let target_ty = self.check_expression(target).unwrap_or(Ty::Any);

                // Borrowing a place that already holds an address (an owning
                // allocation, another reference, a shared handle, or a view)
                // yields a reference to the pointee, not to the handle.
                let pointee = match target_ty {
                    Ty::Own(inner) | Ty::Ref(inner, _) | Ty::Shared(inner) | Ty::View(inner, _) => {
                        *inner
                    }
                    other => other,
                };
                Some(Ty::Ref(Box::new(pointee), *mutable))
            }
            Expr::RawDeref { target, star } => {
                if self.unsafe_depth == 0 {
                    self.errors.push(TypeError {
                        code: None,
                        help: None,
                        message: "raw pointer dereference requires an `unsafe` block".into(),
                        span: *star,
                    });
                }
                let target_ty = self.check_expression(target).unwrap_or(Ty::Any);
                match target_ty {
                    Ty::RawPointer(inner, _) => Some(*inner),
                    Ty::Any => Some(Ty::Any),
                    other => {
                        self.errors.push(TypeError {
                            code: None,
                            help: None,
                            message: format!("cannot dereference `{other}` as a raw pointer"),
                            span: *star,
                        });
                        Some(Ty::Any)
                    }
                }
            }
            Expr::RawDerefSet {
                target,
                value,
                star,
            } => {
                if self.unsafe_depth == 0 {
                    self.errors.push(TypeError {
                        code: None,
                        help: None,
                        message: "raw pointer dereference requires an `unsafe` block".into(),
                        span: *star,
                    });
                }
                let target_ty = self.check_expression(target).unwrap_or(Ty::Any);
                let value_ty = self.check_expression(value).unwrap_or(Ty::Any);
                match target_ty {
                    Ty::RawPointer(inner, true) if inner.is_assignable_from(&value_ty) => {
                        Some(*inner)
                    }
                    Ty::RawPointer(_, false) => {
                        self.errors.push(TypeError {
                            code: None,
                            help: None,
                            message: "cannot write through `*const` pointer".into(),
                            span: *star,
                        });
                        Some(value_ty)
                    }
                    other => {
                        self.errors.push(TypeError {
                            code: None,
                            help: None,
                            message: format!("cannot write through `{other}`"),
                            span: *star,
                        });
                        Some(value_ty)
                    }
                }
            }
            Expr::This { .. } => self.symbols.lookup("this").cloned(),
            Expr::Spread { value, .. } => {
                if let Expr::ArrayLiteral { elements, .. } = &**value
                    && let Some(first) = elements.first()
                {
                    return self.check_expression(first);
                }
                let _ = self.check_expression(value);
                Some(Ty::Any)
            }
            Expr::StructLiteral {
                class_name,
                fields,
                update,
                ..
            } => {
                let class_name_str = class_name.lexeme().to_string();
                if !self
                    .symbols
                    .lookup(&class_name_str)
                    .is_some_and(|t| matches!(t, Ty::Class(_)))
                {
                    self.errors.push(TypeError {
                        code: None,
                        help: None,
                        message: format!(
                            "`{}` is not a class; struct literals require a class name",
                            class_name_str
                        ),
                        span: class_name.span,
                    });
                } else if self.classes.contains_key(&class_name_str) {
                    // Collected up front so no borrow of the class table
                    // spans the rest of the arm.
                    let member_names = self.class_member_names(&class_name_str);
                    let mut seen = HashSet::new();
                    for field in fields {
                        if !self.class_declares_field(&class_name_str, &field.key) {
                            // Offer the closest declared field or method when
                            // the key looks like a typo.
                            let suggestion = crate::names::closest_match(
                                &field.key,
                                member_names.iter().map(String::as_str),
                            );
                            let help =
                                suggestion.map(|candidate| format!("did you mean `{candidate}`?"));
                            self.errors.push(TypeError {
                                code: None,
                                message: format!(
                                    "struct literal for `{class_name_str}` has no field `{}`",
                                    field.key
                                ),
                                help,
                                span: field.key_span,
                            });
                        }
                        if !seen.insert(field.key.clone()) {
                            self.errors.push(TypeError {
                                code: None,
                                help: None,
                                message: format!(
                                    "struct literal for `{class_name_str}` sets field `{}` twice",
                                    field.key
                                ),
                                span: field.key_span,
                            });
                        }
                        let _ = self.check_expression(&field.value);
                    }
                    if let Some(update) = update {
                        let update_ty = self.check_expression(update).unwrap_or(Ty::Any);
                        match &update_ty {
                            Ty::Class(name) if name == &class_name_str => {}
                            Ty::Class(name) => {
                                self.errors.push(TypeError {
                                    code: None,
                                    help: None,
                                    message: format!(
                                        "struct update `..` requires an instance of `{class_name_str}`, \
                                         but `{name}` was given",
                                    ),
                                    span: update.span(),
                                });
                            }
                            _ => {
                                self.errors.push(TypeError {
                                    code: None,
                                    help: None,
                                    message: format!(
                                        "struct update `..` requires a class instance, not `{}`",
                                        update_ty
                                    ),
                                    span: update.span(),
                                });
                            }
                        }
                    }
                }
                Some(Ty::Class(class_name_str))
            }
        }
    }

    /// Check `await callee(args)`.
    ///
    /// The callee must be an async function (a top-level `async fun`), or
    /// the built-in `async.sleep` suspender. Returns the awaited value's
    /// type.
    fn check_await(&mut self, callee: &Expr, arguments: &[Expr], span: Span) -> Option<Ty> {
        if self.async_depth == 0 {
            self.errors.push(TypeError {
                code: None,
                help: None,
                message: "await is only allowed inside an async function".into(),
                span,
            });
        }
        for argument in arguments {
            self.check_expression(argument);
        }
        match callee {
            Expr::Member { object, property }
                if matches!(object.as_ref(), Expr::Variable { name } if name.lexeme() == "async")
                    && property.lexeme() == "sleep" =>
            {
                if arguments.len() != 1 {
                    self.errors.push(TypeError {
                        code: None,
                        help: None,
                        message: format!(
                            "async.sleep expects 1 argument(s), got {}",
                            arguments.len()
                        ),
                        span,
                    });
                } else if let Some(ty) = self.check_expression(&arguments[0])
                    && !matches!(ty, Ty::Int | Ty::Float | Ty::Any)
                {
                    self.errors.push(TypeError {
                        code: None,
                        help: None,
                        message: format!("async.sleep expects an `int` duration, got `{ty}`"),
                        span: arguments[0].span(),
                    });
                }
                Some(Ty::Void)
            }
            Expr::Member { object, property }
                if matches!(object.as_ref(), Expr::Variable { name } if name.lexeme() == "net")
                    && property.lexeme() == "recv_line_async" =>
            {
                // Awaiting a socket read: the runtime tries the read first and
                // parks the goroutine only when the socket has nothing pending,
                // so a handler never holds a worker waiting on a client.
                if arguments.len() != 1 {
                    self.errors.push(TypeError {
                        code: None,
                        help: None,
                        message: format!(
                            "net.recv_line_async expects 1 argument (a socket), got {}",
                            arguments.len()
                        ),
                        span,
                    });
                }
                for argument in arguments {
                    let _ = self.check_expression(argument);
                }
                Some(Ty::String)
            }
            Expr::Member { object, property }
                if matches!(object.as_ref(), Expr::Variable { name } if name.lexeme() == "net")
                    && property.lexeme() == "accept_async" =>
            {
                // Awaiting an offloaded accept: the runtime runs the blocking
                // `accept` on the worker pool and the goroutine parks until a
                // client socket is ready, so an accept loop never pins a
                // worker thread.
                if arguments.len() != 1 {
                    self.errors.push(TypeError {
                        code: None,
                        help: None,
                        message: format!(
                            "net.accept_async expects 1 argument (a listener), got {}",
                            arguments.len()
                        ),
                        span,
                    });
                }
                for argument in arguments {
                    let _ = self.check_expression(argument);
                }
                Some(Ty::Int)
            }
            Expr::Member { object, property }
                if matches!(object.as_ref(), Expr::Variable { name } if name.lexeme() == "http")
                    && property.lexeme().ends_with("_async") =>
            {
                // Awaiting an offloaded http future: the runtime runs the
                // blocking request on the worker pool and the goroutine
                // parks until the response handle is ready.
                let expected_arity = match property.lexeme() {
                    "get_async" | "delete_async" | "head_async" => 1,
                    _ => 2,
                };
                if arguments.len() != expected_arity {
                    self.errors.push(TypeError {
                        code: None,
                        help: None,
                        message: format!(
                            "http.{} expects {expected_arity} argument(s), got {}",
                            property.lexeme(),
                            arguments.len()
                        ),
                        span,
                    });
                }
                for argument in arguments {
                    let _ = self.check_expression(argument);
                }
                Some(Ty::String)
            }
            Expr::Member { object, property } if matches!(object.as_ref(), Expr::Variable { name } if name.lexeme().starts_with('@')) =>
            {
                // Awaiting a Rust-crate bridge call (`@serenity.Client_start(client)`):
                // the adaptor runs the future on a real runtime and the
                // goroutine parks on its async-op poll, so any `@crate.fn`
                // call is awaitable. The result type is resolved during
                // codegen from the bridge's return label.
                Some(Ty::Any)
            }
            Expr::Variable { name } => {
                let callee_name = name.lexeme();
                if !self.async_fns.contains(callee_name) {
                    self.errors.push(TypeError {
                        code: None,
                        help: None,
                        message: "await requires a call to an async function".to_string(),
                        span,
                    });
                    return Some(Ty::Any);
                }
                let ret_ty = match self.symbols.lookup(callee_name) {
                    Some(Ty::Function { return_type, .. }) => (**return_type).clone(),
                    _ => Ty::Any,
                };
                self.check_function_call_args(callee_name, arguments, span);
                Some(ret_ty)
            }
            Expr::AsyncBlock { body, .. } => {
                self.symbols.push_scope();
                for stmt in body {
                    self.check_statement(stmt);
                }
                self.symbols.pop_scope();
                Some(Ty::Any)
            }
            other => {
                let _ = self.check_expression(other);
                self.errors.push(TypeError {
                    code: None,
                    help: None,
                    message: "await requires a call to an async function".into(),
                    span,
                });
                Some(Ty::Any)
            }
        }
    }

    fn check_function_call_args(&mut self, callee_name: &str, arguments: &[Expr], span: Span) {
        let params = match self.symbols.lookup(callee_name) {
            Some(Ty::Function { params, .. }) => params.clone(),
            _ => return,
        };
        if params.len() != arguments.len() {
            self.errors.push(TypeError {
                code: None,
                help: None,
                message: format!(
                    "`{callee_name}` expects {} argument(s), got {}",
                    params.len(),
                    arguments.len()
                ),
                span,
            });
            return;
        }
        for (param_ty, argument) in params.iter().zip(arguments) {
            let arg_ty = self.check_expression(argument);
            if let Some(arg_ty) = arg_ty
                && !param_ty.is_assignable_from(&arg_ty)
            {
                self.errors.push(TypeError {
                    code: None,
                    help: None,
                    message: format!(
                        "argument type mismatch: expected `{param_ty}`, got `{arg_ty}`"
                    ),
                    span: argument.span(),
                });
            }
        }
    }

    fn check_binary(
        &mut self,
        left: &Expr,
        op: &ntsc_ast::token::Token,
        right: &Expr,
    ) -> Option<Ty> {
        let left_ty = self.check_expression(left);
        let right_ty = self.check_expression(right);

        match (&op.kind, &left_ty, &right_ty) {
            // Arithmetic: int op int → int, float op float → float
            (
                TokenKind::Plus
                | TokenKind::Minus
                | TokenKind::Star
                | TokenKind::Slash
                | TokenKind::Percent,
                Some(Ty::Int),
                Some(Ty::Int),
            ) => Some(Ty::Int),
            (
                TokenKind::Plus
                | TokenKind::Minus
                | TokenKind::Star
                | TokenKind::Slash
                | TokenKind::Percent,
                Some(Ty::Float),
                Some(Ty::Float),
            ) => Some(Ty::Float),

            // String concatenation: string + string → string
            (TokenKind::Plus, Some(Ty::String), Some(Ty::String)) => Some(Ty::String),

            // String concatenation with a scalar: string + int/float/bool
            // → string
            (TokenKind::Plus, Some(Ty::String), Some(Ty::Int | Ty::Float | Ty::Bool)) => {
                Some(Ty::String)
            }
            (TokenKind::Plus, Some(Ty::Int | Ty::Float | Ty::Bool), Some(Ty::String)) => {
                Some(Ty::String)
            }

            // Operator overloading: when at least one operand is a class
            // type, look up the operator method and use its declared return
            // type. Must come before the generic "same type → bool" arm so
            // class types with custom operators are intercepted first.
            (op_tok, Some(l), Some(r))
                if binary_op_method_name(op_tok).is_some()
                    && (base_class_name(l).is_some() || base_class_name(r).is_some()) =>
            {
                if let Some(method_name) = binary_op_method_name(op_tok) {
                    let class_name = base_class_name(l).or_else(|| base_class_name(r));
                    if let Some(class) = class_name {
                        if let Some(ret) = self.lookup_operator_return(class, method_name) {
                            return Some(ret);
                        }
                        self.errors.push(TypeError {
                            code: None,
                            help: None,
                            message: format!(
                                "type `{}` does not implement binary operator `{}`",
                                l,
                                op_lexeme(op_tok)
                            ),
                            span: op.span,
                        });
                        return Some(Ty::Any);
                    }
                }
                Some(Ty::Any)
            }

            // Comparison: same type → bool
            (
                TokenKind::EqualEqual
                | TokenKind::BangEqual
                | TokenKind::Greater
                | TokenKind::GreaterEqual
                | TokenKind::Less
                | TokenKind::LessEqual,
                Some(a),
                Some(b),
            ) if a == b => Some(Ty::Bool),

            // Equality against `nil`: `option[T]` values may be compared
            // with `nil` (a nullness test) and with other `option[T]`
            // values (a pointer identity test), both yielding a bool.
            (
                TokenKind::EqualEqual | TokenKind::BangEqual,
                Some(Ty::Nil | Ty::Option(_)),
                Some(Ty::Nil | Ty::Option(_)),
            ) => Some(Ty::Bool),

            // Logical: bool op bool → bool
            (
                TokenKind::And | TokenKind::Or | TokenKind::AndSym | TokenKind::OrSym,
                Some(Ty::Bool),
                Some(Ty::Bool),
            ) => Some(Ty::Bool),

            // Any propagation
            (
                TokenKind::Plus
                | TokenKind::Minus
                | TokenKind::Star
                | TokenKind::Slash
                | TokenKind::Percent
                | TokenKind::EqualEqual
                | TokenKind::BangEqual
                | TokenKind::Greater
                | TokenKind::GreaterEqual
                | TokenKind::Less
                | TokenKind::LessEqual
                | TokenKind::And
                | TokenKind::Or
                | TokenKind::AndSym
                | TokenKind::OrSym,
                Some(Ty::Any),
                _,
            )
            | (
                TokenKind::Plus
                | TokenKind::Minus
                | TokenKind::Star
                | TokenKind::Slash
                | TokenKind::Percent
                | TokenKind::EqualEqual
                | TokenKind::BangEqual
                | TokenKind::Greater
                | TokenKind::GreaterEqual
                | TokenKind::Less
                | TokenKind::LessEqual
                | TokenKind::And
                | TokenKind::Or
                | TokenKind::AndSym
                | TokenKind::OrSym,
                _,
                Some(Ty::Any),
            ) => Some(Ty::Any),

            // Type mismatch
            (op_tok, Some(l), Some(r)) if is_arithmetic(op_tok) || is_logical(op_tok) => {
                self.errors.push(TypeError {
                    code: None,
                    help: None,
                    message: format!(
                        "binary operator `{}` cannot apply to `{l}` and `{r}`",
                        op_lexeme(op_tok)
                    ),
                    span: op.span,
                });
                Some(Ty::Any)
            }
            _ => Some(Ty::Any),
        }
    }

    fn check_unary(&mut self, op: &ntsc_ast::token::Token, right: &Expr) -> Option<Ty> {
        let right_ty = self.check_expression(right);
        match (&op.kind, &right_ty) {
            (TokenKind::Minus, Some(Ty::Int)) => Some(Ty::Int),
            (TokenKind::Minus, Some(Ty::Float)) => Some(Ty::Float),

            // Unary operator overloading for class types. Must come before
            // the generic `Bang → bool` and `Minus → error` arms.
            (TokenKind::Minus, Some(ty)) if base_class_name(ty).is_some() => {
                if let Some(class) = base_class_name(ty)
                    && let Some(ret) = self.lookup_operator_return(class, "-")
                {
                    return Some(ret);
                }
                self.errors.push(TypeError {
                    code: None,
                    help: None,
                    message: format!("type `{}` does not implement unary operator `-`", ty),
                    span: op.span,
                });
                Some(Ty::Any)
            }
            (TokenKind::Bang, Some(ty)) if base_class_name(ty).is_some() => {
                if let Some(class) = base_class_name(ty)
                    && let Some(ret) = self.lookup_operator_return(class, "!")
                {
                    return Some(ret);
                }
                // No `!` operator defined — fall back to bool negation.
                Some(Ty::Bool)
            }

            (TokenKind::Bang, _) => Some(Ty::Bool),
            (TokenKind::Minus, Some(Ty::Any)) => Some(Ty::Any),
            (TokenKind::Minus, Some(ty)) => {
                self.errors.push(TypeError {
                    code: None,
                    help: None,
                    message: format!("cannot negate `{ty}`"),
                    span: op.span,
                });
                Some(Ty::Any)
            }
            _ => Some(Ty::Any),
        }
    }

    fn check_postfix_unary(&mut self, op: &ntsc_ast::token::Token, left: &Expr) -> Option<Ty> {
        let left_ty = self.check_expression(left);
        match (&op.kind, &left_ty) {
            (TokenKind::PlusPlus | TokenKind::MinusMinus, Some(Ty::Int)) => Some(Ty::Int),
            (TokenKind::PlusPlus | TokenKind::MinusMinus, Some(Ty::Float)) => Some(Ty::Float),
            (TokenKind::PlusPlus | TokenKind::MinusMinus, Some(Ty::Any)) => Some(Ty::Any),
            (TokenKind::PlusPlus | TokenKind::MinusMinus, Some(ty)) => {
                self.errors.push(TypeError {
                    code: None,
                    help: None,
                    message: format!("cannot apply `{}` to `{ty}`", op.lexeme()),
                    span: op.span,
                });
                Some(Ty::Any)
            }
            _ => Some(Ty::Any),
        }
    }

    /// Type-check the builtin combinators dispatched as methods on
    /// `result[.., ..]` and `option[T]` receivers: error handling helpers
    /// (`unwrap_or`, `and_then`, ...) and the Option→Result bridge
    /// (`ok_or`, `ok_or_else`). `object_ty` is the already-checked receiver
    /// type; the caller guarantees it is a result or option.
    fn check_builtin_combinator(
        &mut self,
        property: &Token,
        arguments: &[Expr],
        object_ty: Option<Ty>,
    ) -> Option<Ty> {
        let arity_error = |expected: usize| -> TypeError {
            TypeError {
                code: None,
                help: None,
                message: format!(
                    "{} expects {expected} argument(s), got {}",
                    property.lexeme(),
                    arguments.len()
                ),
                span: property.span,
            }
        };
        let function_ty = |ty: &Option<Ty>| -> Option<(Vec<Ty>, Box<Ty>)> {
            match ty {
                Some(Ty::Function {
                    params,
                    return_type,
                }) => Some((params.clone(), return_type.clone())),
                _ => None,
            }
        };
        let mismatch = |expected: &str, actual: &Ty| -> TypeError {
            TypeError {
                code: None,
                help: None,
                message: format!("argument type mismatch: expected `{expected}`, got `{actual}`"),
                span: arguments.first().map(|a| a.span()).unwrap_or(property.span),
            }
        };
        match (&object_ty, property.lexeme()) {
            (
                Some(Ty::Result {
                    ok: recv_ok,
                    err: _recv_err,
                }),
                "unwrap_or",
            ) => {
                if arguments.len() != 1 {
                    self.errors.push(arity_error(1));
                }
                if let Some(default_ty) = self.check_expression(arguments.first()?)
                    && !self.assignable(recv_ok, &default_ty)
                {
                    let recv_ok = (**recv_ok).clone();
                    self.errors
                        .push(mismatch(&format!("{recv_ok}"), &default_ty));
                }
                Some((**recv_ok).clone())
            }
            (
                Some(Ty::Result {
                    ok: recv_ok,
                    err: recv_err,
                }),
                "map",
            ) => {
                if arguments.len() != 1 {
                    self.errors.push(arity_error(1));
                }
                let argument_ty = self.check_expression(arguments.first()?);
                match function_ty(&argument_ty) {
                    Some((params, return_type)) if params.len() == 1 => {
                        if !self.assignable(&params[0], recv_ok) {
                            let params0 = params[0].clone();
                            self.errors
                                .push(mismatch(&format!("{}", **recv_ok), &params0));
                        }
                        Some(Ty::Result {
                            ok: return_type,
                            err: recv_err.clone(),
                        })
                    }
                    _ => {
                        self.errors.push(TypeError {
                            code: None,
                            help: None,
                            message: format!(
                                "{} expects a one-parameter function, got `{}`",
                                property.lexeme(),
                                argument_ty.map(|t| t.label()).unwrap_or_else(|| "?".into())
                            ),
                            span: property.span,
                        });
                        Some(Ty::Any)
                    }
                }
            }
            (
                Some(Ty::Result {
                    ok: recv_ok,
                    err: recv_err,
                }),
                "and_then" | "or_else",
            ) => {
                if arguments.len() != 1 {
                    self.errors.push(arity_error(1));
                }
                let argument_ty = self.check_expression(arguments.first()?);
                // `and_then` receives the Ok payload; `or_else` receives the
                // Err payload. Both must return a result.
                let parameter = if property.lexeme() == "and_then" {
                    (**recv_ok).clone()
                } else {
                    (**recv_err).clone()
                };
                match function_ty(&argument_ty) {
                    Some((params, return_type)) if params.len() == 1 => {
                        if !self.assignable(&params[0], &parameter) {
                            self.errors
                                .push(mismatch(&format!("{parameter}"), &params[0]));
                        }
                        Some(return_type.as_ref().clone())
                    }
                    _ => {
                        self.errors.push(TypeError {
                            code: None,
                            help: None,
                            message: format!(
                                "{} expects a one-parameter function returning a `result`, got `{}`",
                                property.lexeme(),
                                argument_ty.map(|t| t.label()).unwrap_or_else(|| "?".into())
                            ),
                            span: property.span,
                        });
                        Some(Ty::Any)
                    }
                }
            }
            (Some(Ty::Option(inner)), "ok_or") => {
                if arguments.len() != 1 {
                    self.errors.push(arity_error(1));
                }
                let err_ty = self
                    .check_expression(arguments.first()?)
                    .unwrap_or(Ty::String);
                Some(Ty::Result {
                    ok: inner.clone(),
                    err: Box::new(err_ty),
                })
            }
            (Some(Ty::Option(inner)), "ok_or_else") => {
                if arguments.len() != 1 {
                    self.errors.push(arity_error(1));
                }
                let argument_ty = self.check_expression(arguments.first()?);
                match function_ty(&argument_ty) {
                    Some((params, return_type)) if params.is_empty() => Some(Ty::Result {
                        ok: inner.clone(),
                        err: return_type,
                    }),
                    _ => {
                        self.errors.push(TypeError {
                            code: None,
                            help: None,
                            message: format!(
                                "ok_or_else expects a zero-parameter function, got `{}`",
                                argument_ty.map(|t| t.label()).unwrap_or_else(|| "?".into())
                            ),
                            span: property.span,
                        });
                        Some(Ty::Any)
                    }
                }
            }
            _ => None,
        }
    }

    fn check_call(&mut self, callee: &Expr, arguments: &[Expr]) -> Option<Ty> {
        let is_ffi_call =
            matches!(callee, Expr::Variable { name } if self.ffi_functions.contains(name.lexeme()));
        if let Expr::Variable { name } = callee
            && self.ffi_functions.contains(name.lexeme())
            && self.unsafe_depth == 0
        {
            self.errors.push(TypeError {
                code: None,
                help: Some(
                    "wrap the call in `unsafe { ... }` after checking its C preconditions".into(),
                ),
                message: format!("FFI call `{}` requires an `unsafe` block", name.lexeme()),
                span: callee.span(),
            });
        }

        // An async function call without `await` would start the future
        // but never poll it to completion, so it is rejected here.
        if let Expr::Variable { name } = callee
            && self.async_fns.contains(name.lexeme())
        {
            self.errors.push(TypeError {
                code: None,
                help: None,
                message: format!("async function `{}` must be awaited", name.lexeme()),
                span: callee.span(),
            });
        }

        if let Expr::Member { object, property } = callee
            && let Expr::Variable { name } = object.as_ref()
            && name.lexeme() == "slices"
        {
            // The element type flows from the sliced value, so these
            // signatures are resolved against the first argument.
            let first = arguments.first().map(|a| self.check_expression(a));
            let element = match first.flatten() {
                Some(Ty::Array(element)) | Some(Ty::Slice(element)) => *element,
                _ => Ty::Any,
            };
            for argument in arguments.iter().skip(1) {
                let _ = self.check_expression(argument);
            }
            let (arity, result) = match property.lexeme() {
                "of" | "sub" => (3, Ty::Slice(Box::new(element))),
                "length" => (1, Ty::Int),
                "get" => (2, element),
                "set" => (3, Ty::Bool),
                "to_array" => (1, Ty::Array(Box::new(element))),
                "fill" => (2, Ty::Bool),
                "copy_from" | "equal" => (2, Ty::Bool),
                other => {
                    const SLICES_FUNCTIONS: [&str; 9] = [
                        "of",
                        "sub",
                        "length",
                        "get",
                        "set",
                        "to_array",
                        "fill",
                        "copy_from",
                        "equal",
                    ];
                    let suggestion = crate::names::closest_match(other, SLICES_FUNCTIONS);
                    self.errors.push(TypeError {
                        code: None,
                        message: format!("unknown function `slices.{other}`"),
                        help: suggestion.map(|candidate| format!("did you mean `{candidate}`?")),
                        span: callee.span(),
                    });
                    return Some(Ty::Any);
                }
            };
            if arity != arguments.len() {
                self.errors.push(TypeError {
                    code: None,
                    help: None,
                    message: format!(
                        "slices.{} expects {arity} argument(s), got {}",
                        property.lexeme(),
                        arguments.len()
                    ),
                    span: callee.span(),
                });
            }
            return Some(result);
        }

        if let Expr::Member { object, property } = callee
            && let Expr::Variable { name } = object.as_ref()
            && name.lexeme() == "memory"
        {
            if property.lexeme() == "raw_address" {
                if self.unsafe_depth == 0 {
                    self.errors.push(TypeError {
                        code: None,
                        help: None,
                        message: "`memory.raw_address` requires an `unsafe` block".into(),
                        span: callee.span(),
                    });
                }
                if arguments.len() != 1 {
                    self.errors.push(TypeError {
                        code: None,
                        help: None,
                        message: format!(
                            "memory.raw_address expects 1 argument(s), got {}",
                            arguments.len()
                        ),
                        span: callee.span(),
                    });
                    return Some(Ty::Any);
                }
                let argument_ty = self.check_expression(&arguments[0]).unwrap_or(Ty::Any);
                return Some(match argument_ty {
                    Ty::Ref(inner, mutable) => Ty::RawPointer(inner, mutable),
                    other => {
                        self.errors.push(TypeError {
                            code: None,
                            help: None,
                            message: format!(
                                "cannot take the raw address of `{other}`; expected a reference"
                            ),
                            span: arguments[0].span(),
                        });
                        Ty::Any
                    }
                });
            }
            let (params, result) = match property.lexeme() {
                "alloc" => (vec![Ty::Int], Ty::Pointer),
                "offset" => (vec![Ty::Pointer, Ty::Int], Ty::Pointer),
                "clone" => (vec![Ty::Pointer], Ty::Pointer),
                "drop" => (vec![Ty::Pointer], Ty::Void),
                "load8" | "load64" => (vec![Ty::Pointer], Ty::Int),
                "store8" | "store64" => (vec![Ty::Pointer, Ty::Int], Ty::Bool),
                _ => return Some(Ty::Any),
            };
            if params.len() != arguments.len() {
                self.errors.push(TypeError {
                    code: None,
                    help: None,
                    message: format!(
                        "memory.{} expects {} argument(s), got {}",
                        property.lexeme(),
                        params.len(),
                        arguments.len()
                    ),
                    span: callee.span(),
                });
            }
            for (argument, parameter) in arguments.iter().zip(params.iter()) {
                if let Some(argument_ty) = self.check_expression(argument)
                    && !self.assignable(parameter, &argument_ty)
                {
                    self.errors.push(TypeError {
                        code: None,
                        help: None,
                        message: format!(
                            "argument type mismatch: expected `{parameter}`, got `{argument_ty}`"
                        ),
                        span: argument.span(),
                    });
                }
            }
            return Some(result);
        }

        if let Expr::Variable { name } = callee
            && name.lexeme() == "alloc"
        {
            if arguments.len() != 1 {
                self.errors.push(TypeError {
                    code: None,
                    help: None,
                    message: format!("alloc expects 1 argument(s), got {}", arguments.len()),
                    span: callee.span(),
                });
                return Some(Ty::Any);
            }
            let value_ty = self.check_expression(&arguments[0]).unwrap_or(Ty::Any);
            return Some(Ty::Own(Box::new(value_ty)));
        }
        if let Expr::Variable { name } = callee
            && (name.lexeme() == "Ok" || name.lexeme() == "Err")
        {
            if arguments.len() != 1 {
                self.errors.push(TypeError {
                    code: None,
                    help: None,
                    message: format!(
                        "{} expects 1 argument(s), got {}",
                        name.lexeme(),
                        arguments.len()
                    ),
                    span: callee.span(),
                });
                return Some(Ty::Any);
            }
            let value_ty = self.check_expression(&arguments[0]).unwrap_or(Ty::Any);
            // The unspecified side defaults so standalone construction stays
            // concrete; `any` keeps it assignable to any matching slot.
            return Some(if name.lexeme() == "Ok" {
                Ty::Result {
                    ok: Box::new(value_ty),
                    err: Box::new(Ty::Any),
                }
            } else {
                Ty::Result {
                    ok: Box::new(Ty::Any),
                    err: Box::new(value_ty),
                }
            });
        }

        if let Expr::Member { object, property } = callee
            && matches!(
                property.lexeme(),
                "unwrap_or" | "map" | "and_then" | "or_else" | "ok_or" | "ok_or_else"
            )
        {
            // Only result/option receivers take the builtin dispatch;
            // anything else keeps the ordinary member-call flow so class
            // methods may reuse these names.
            let object_ty = self.check_expression(object);
            if matches!(object_ty, Some(Ty::Result { .. } | Ty::Option(_))) {
                return self.check_builtin_combinator(property, arguments, object_ty);
            }
        }

        let callee_ty = self.check_expression(callee);
        match callee_ty {
            Some(Ty::Function {
                params,
                return_type,
            }) => {
                let has_spread = arguments.iter().any(|a| matches!(a, Expr::Spread { .. }));
                if params.len() != arguments.len() && !has_spread {
                    self.errors.push(TypeError {
                        code: None,
                        help: None,
                        message: format!(
                            "function expects {} argument(s), got {}",
                            params.len(),
                            arguments.len()
                        ),
                        span: callee.span(),
                    });
                }
                for (argument, parameter) in arguments.iter().zip(params.iter()) {
                    if matches!(argument, Expr::Spread { .. }) {
                        continue;
                    }
                    if let Some(argument_ty) = self.check_expression(argument) {
                        // A view may only flow into a view-typed
                        // parameter; passing it where an owned value is
                        // expected would copy or move borrowed data.
                        if matches!(argument_ty, Ty::View(..)) && !matches!(parameter, Ty::View(..))
                        {
                            self.errors.push(TypeError {
                                code: None,
                                message: format!(
                                    "cannot pass a view where an owned `{parameter}` is expected"
                                ),
                                help: Some(
                                    "pass `copy(argument)` to hand the call its own value".into(),
                                ),
                                span: argument.span(),
                            });
                        } else if !(is_ffi_call && ffi_pointer_compatible(parameter, &argument_ty))
                            && !self.assignable(parameter, &argument_ty)
                        {
                            self.errors.push(TypeError {
                                code: None,
                                help: None,
                                message: format!(
                                    "argument type mismatch: expected `{parameter}`, got `{argument_ty}`"
                                ),
                                span: argument.span(),
                            });
                        }
                    }
                }
                for argument in arguments.iter().skip(params.len()) {
                    let _ = self.check_expression(argument);
                }
                Some(*return_type)
            }
            Some(Ty::Any) => Some(Ty::Any),
            Some(Ty::Class(class_name)) => {
                // Class constructor call: `ClassName(...)`.
                for argument in arguments {
                    let _ = self.check_expression(argument);
                }
                Some(Ty::Class(class_name))
            }
            Some(other) => {
                self.errors.push(TypeError {
                    code: None,
                    help: None,
                    message: format!("`{other}` is not callable"),
                    span: callee.span(),
                });
                Some(Ty::Any)
            }
            None => None,
        }
    }

    fn literal_type(&self, value: &LiteralValue) -> Ty {
        match value {
            LiteralValue::Nil => Ty::Nil,
            LiteralValue::Bool(_) => Ty::Bool,
            LiteralValue::Number(n) => {
                if n.contains('.') {
                    Ty::Float
                } else {
                    Ty::Int
                }
            }
            LiteralValue::String(_) => Ty::String,
        }
    }

    fn check_index(&mut self, index: &Expr) {
        if let Some(index_ty) = self.check_expression(index)
            && !Ty::Int.is_assignable_from(&index_ty)
        {
            self.errors.push(TypeError {
                code: None,
                help: None,
                message: format!("index must be `int`, got `{index_ty}`"),
                span: index.span(),
            });
        }
    }

    fn resolve_annotation(&self, ann: Option<&TypeAnnotation>) -> Ty {
        match ann {
            Some(TypeAnnotation::Int) => Ty::Int,
            Some(TypeAnnotation::Float) => Ty::Float,
            Some(TypeAnnotation::String) => Ty::String,
            Some(TypeAnnotation::Bool) => Ty::Bool,
            Some(TypeAnnotation::Array(Some(element))) => {
                Ty::Array(Box::new(self.resolve_annotation(Some(element))))
            }
            Some(TypeAnnotation::Array(None)) => Ty::Array(Box::new(Ty::Any)),
            Some(TypeAnnotation::Object) => Ty::Object,
            Some(TypeAnnotation::Option(inner)) => {
                Ty::Option(Box::new(self.resolve_annotation(Some(inner))))
            }
            Some(TypeAnnotation::Result { ok, err }) => Ty::Result {
                ok: Box::new(self.resolve_annotation(Some(ok))),
                err: Box::new(self.resolve_annotation(Some(err))),
            },
            Some(TypeAnnotation::View(inner, mutable)) => {
                Ty::View(Box::new(self.resolve_annotation(Some(inner))), *mutable)
            }
            Some(TypeAnnotation::Shared(inner)) => {
                let inner = self.resolve_annotation(Some(inner));
                Ty::Shared(Box::new(inner))
            }
            Some(TypeAnnotation::Any) => Ty::Any,
            Some(TypeAnnotation::Pointer) => Ty::Pointer,
            Some(TypeAnnotation::Slice(element)) => Ty::Slice(Box::new(
                element
                    .as_deref()
                    .map(|element| self.resolve_annotation(Some(element)))
                    .unwrap_or(Ty::Any),
            )),
            Some(TypeAnnotation::Own(inner)) => {
                Ty::Own(Box::new(self.resolve_annotation(Some(inner))))
            }
            Some(TypeAnnotation::Ref(inner, mutable)) => {
                Ty::Ref(Box::new(self.resolve_annotation(Some(inner))), *mutable)
            }
            Some(TypeAnnotation::RawPointer(inner, mutable)) => {
                Ty::RawPointer(Box::new(self.resolve_annotation(Some(inner))), *mutable)
            }
            Some(TypeAnnotation::Named(token)) if token.lexeme() == "void" => Ty::Void,
            Some(TypeAnnotation::Named(token))
                if matches!(
                    token.lexeme(),
                    "byte"
                        | "c_char"
                        | "c_uchar"
                        | "c_short"
                        | "c_ushort"
                        | "c_int"
                        | "c_uint"
                        | "c_long"
                        | "c_ulong"
                        | "c_longlong"
                        | "c_ulonglong"
                        | "c_size_t"
                ) =>
            {
                Ty::Int
            }
            Some(TypeAnnotation::Named(token))
                if matches!(token.lexeme(), "c_float" | "c_double") =>
            {
                Ty::Float
            }
            Some(TypeAnnotation::Named(token)) => Ty::Class(token.lexeme().to_string()),
            // Rewritten to the concrete class before checking runs.
            Some(TypeAnnotation::ImplTrait(_)) => Ty::Any,
            Some(TypeAnnotation::Dyn(token)) => Ty::Dyn(token.lexeme().to_string()),
            Some(TypeAnnotation::Tuple(types)) => Ty::Tuple(
                types
                    .iter()
                    .map(|t| self.resolve_annotation(Some(t)))
                    .collect(),
            ),
            Some(TypeAnnotation::Chan(element)) => {
                Ty::Chan(Box::new(self.resolve_annotation(Some(element))))
            }
            None => Ty::Any,
        }
    }
}

/// The class an instance-typed expression names, seen through views.
/// `None` for anything else — a module name, an object literal, a
/// scalar.
/// Whether a type is a borrow: a `view` or a `&T` / `&mut T` reference.
/// Neither owns its referent, so neither may be stored where it can outlive
/// the value it points at.
fn is_borrow_ty(ty: &Ty) -> bool {
    matches!(ty, Ty::View(..) | Ty::Ref(..))
}

fn ffi_pointer_compatible(parameter: &Ty, argument: &Ty) -> bool {
    matches!(
        (parameter, argument),
        (Ty::RawPointer(_, parameter_mut), Ty::RawPointer(_, argument_mut))
            if !*parameter_mut || *argument_mut
    )
}

fn base_class_name(ty: &Ty) -> Option<&str> {
    match ty {
        Ty::Class(name) => Some(name.as_str()),
        Ty::View(inner, _) => base_class_name(inner),

        // An owning allocation and a reference both address the instance, so
        // a field read reaches through them.
        Ty::Own(inner) | Ty::Ref(inner, _) => base_class_name(inner),
        _ => None,
    }
}

fn is_arithmetic(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Plus
            | TokenKind::Minus
            | TokenKind::Star
            | TokenKind::Slash
            | TokenKind::Percent
    )
}

fn is_logical(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::And | TokenKind::Or | TokenKind::AndSym | TokenKind::OrSym
    )
}

fn op_lexeme(kind: &TokenKind) -> &'static str {
    match kind {
        TokenKind::Plus => "+",
        TokenKind::Minus => "-",
        TokenKind::Star => "*",
        TokenKind::Slash => "/",
        TokenKind::Percent => "%",
        TokenKind::EqualEqual => "==",
        TokenKind::BangEqual => "!=",
        TokenKind::Greater => ">",
        TokenKind::GreaterEqual => ">=",
        TokenKind::Less => "<",
        TokenKind::LessEqual => "<=",
        TokenKind::And | TokenKind::AndSym => "&&",
        TokenKind::Or | TokenKind::OrSym => "||",
        _ => "?",
    }
}

/// Whether a method name is an operator symbol that may be overloaded.
fn is_operator_name(name: &str) -> bool {
    matches!(
        name,
        "+" | "-" | "*" | "/" | "%" | "!" | "==" | "!=" | "<" | "<=" | ">" | ">="
    )
}

/// Map a binary operator token kind to the operator method name used for
/// overloading (the lexeme stored as a method name on the class).
fn binary_op_method_name(kind: &TokenKind) -> Option<&'static str> {
    match kind {
        TokenKind::Plus => Some("+"),
        TokenKind::Minus => Some("-"),
        TokenKind::Star => Some("*"),
        TokenKind::Slash => Some("/"),
        TokenKind::Percent => Some("%"),
        TokenKind::EqualEqual => Some("=="),
        TokenKind::BangEqual => Some("!="),
        TokenKind::Less => Some("<"),
        TokenKind::LessEqual => Some("<="),
        TokenKind::Greater => Some(">"),
        TokenKind::GreaterEqual => Some(">="),
        _ => None,
    }
}

/// Returns the span of the first `await` expression found in `expr`,
/// without descending into lambda bodies (a lambda is a separate sync
/// function).
fn find_await(expr: &Expr) -> Option<Span> {
    match expr {
        Expr::Await { span, .. } => Some(*span),
        Expr::AsyncBlock { .. } => None,
        Expr::Lambda { .. } => None,
        Expr::Literal { .. } | Expr::Variable { .. } | Expr::This { .. } => None,
        Expr::Binary { left, right, .. }
        | Expr::IndexGet {
            object: left,
            index: right,
        }
        | Expr::IndexSet {
            object: left,
            index: right,
            ..
        } => find_await(left).or_else(|| find_await(right)),
        Expr::Unary { right, .. }
        | Expr::PostfixUnary { left: right, .. }
        | Expr::Grouping {
            expression: right, ..
        }
        | Expr::Member { object: right, .. }
        | Expr::OptionalMember { object: right, .. }
        | Expr::MemberSet { object: right, .. }
        | Expr::Assign { value: right, .. }
        | Expr::Spread { value: right, .. }
        | Expr::View { target: right, .. }
        | Expr::Copy {
            expression: right, ..
        } => find_await(right),
        Expr::Borrow { target: right, .. } | Expr::RawDeref { target: right, .. } => {
            find_await(right)
        }
        Expr::RawDerefSet { target, value, .. } => find_await(target).or_else(|| find_await(value)),
        Expr::Call {
            callee, arguments, ..
        } => find_await(callee).or_else(|| arguments.iter().find_map(find_await)),
        Expr::Ternary {
            condition,
            then_branch,
            else_branch,
        } => find_await(condition)
            .or_else(|| find_await(then_branch))
            .or_else(|| find_await(else_branch)),
        Expr::ObjectLiteral { properties, .. } => properties
            .iter()
            .find_map(|property| find_await(&property.value)),
        Expr::ArrayLiteral { elements, .. } => elements.iter().find_map(find_await),
        Expr::StructLiteral { fields, update, .. } => fields
            .iter()
            .find_map(|field| find_await(&field.value))
            .or_else(|| update.as_ref().and_then(|u| find_await(u))),
        Expr::Propagate { value, .. } => find_await(value),
        Expr::TupleLiteral { elements, .. } => elements.iter().find_map(find_await),
        Expr::TupleIndex { object, .. } => find_await(object),
        Expr::ChanSend { channel, value, .. } => find_await(channel).or_else(|| find_await(value)),
        Expr::ChanRecv { channel, .. } => find_await(channel),
        Expr::Close { channel, .. } => find_await(channel),
    }
}

/// Returns `true` if a channel send/receive suspension point is reachable
/// inside `expr`.
fn find_chan_in_expr(expr: &Expr) -> bool {
    match expr {
        Expr::ChanSend { .. } | Expr::ChanRecv { .. } => true,
        Expr::Close { channel, .. } => find_chan_in_expr(channel),
        Expr::Binary { left, right, .. }
        | Expr::IndexGet {
            object: left,
            index: right,
        }
        | Expr::IndexSet {
            object: left,
            index: right,
            ..
        } => find_chan_in_expr(left) || find_chan_in_expr(right),
        Expr::Unary { right, .. }
        | Expr::PostfixUnary { left: right, .. }
        | Expr::Grouping {
            expression: right, ..
        }
        | Expr::Member { object: right, .. }
        | Expr::OptionalMember { object: right, .. }
        | Expr::MemberSet { object: right, .. }
        | Expr::Assign { value: right, .. }
        | Expr::Spread { value: right, .. }
        | Expr::View { target: right, .. }
        | Expr::Copy {
            expression: right, ..
        }
        | Expr::Borrow { target: right, .. }
        | Expr::RawDeref { target: right, .. } => find_chan_in_expr(right),
        Expr::RawDerefSet { target, value, .. } => {
            find_chan_in_expr(target) || find_chan_in_expr(value)
        }
        Expr::Call {
            callee, arguments, ..
        } => find_chan_in_expr(callee) || arguments.iter().any(find_chan_in_expr),
        Expr::Ternary {
            condition,
            then_branch,
            else_branch,
        } => {
            find_chan_in_expr(condition)
                || find_chan_in_expr(then_branch)
                || find_chan_in_expr(else_branch)
        }
        Expr::ObjectLiteral { properties, .. } => {
            properties.iter().any(|p| find_chan_in_expr(&p.value))
        }
        Expr::ArrayLiteral { elements, .. } => elements.iter().any(find_chan_in_expr),
        Expr::StructLiteral { fields, update, .. } => {
            fields.iter().any(|f| find_chan_in_expr(&f.value))
                || update.as_ref().is_some_and(|u| find_chan_in_expr(u))
        }
        Expr::Propagate { value, .. } | Expr::TupleIndex { object: value, .. } => {
            find_chan_in_expr(value)
        }
        Expr::TupleLiteral { elements, .. } => elements.iter().any(find_chan_in_expr),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ntsc_lexer::tokenize;
    use ntsc_parser::parse;

    // ── go: callable, thread-boundary args, capture classification ───────

    #[test]
    fn go_requires_an_async_fn_call_or_block() {
        let errors =
            check_source("fun helper() { }\nasync fun main() { go helper() }").unwrap_err();
        assert!(
            errors.iter().any(|e| e
                .message
                .contains("`go` requires a call to an async function")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn go_accepts_an_async_fn_call() {
        check_source("async fun worker(int n) { say(\"w\") }\nasync fun main() { go worker(1) }")
            .expect("an async fn call is a legal go spawn");
    }

    #[test]
    fn go_rejects_view_argument() {
        let errors = check_source(
            "async fun worker(string s) { }\nasync fun main() { var string msg = \"hi\"\n view var v = msg\n go worker(v) }",
        )
        .unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("a view cannot cross into a goroutine")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn go_rejects_shared_argument() {
        let errors = check_source(
            "async fun worker(shared string s) { }\nasync fun main() { shared string g = \"hi\"\n go worker(g) }",
        )
        .unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("unsynchronized reference counting")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn go_rejects_view_capture() {
        let errors = check_source(
            "async fun main() { var string msg = \"hi\"\n view var v = msg\n go { say(v) } }",
        )
        .unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("cannot capture view `v`")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn go_rejects_shared_capture() {
        let errors = check_source("async fun main() { shared string g = \"hi\"\n go { say(g) } }")
            .unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("cannot capture shared `g`")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn go_scalar_and_channel_captures_are_allowed() {
        check_source(
            "async fun main() { var chan[string] words = chan.new(2)\n var int base = 40\n go { say(\"capture \" + base) }\n go { for w in words { say(w) } }\n await async.sleep(1) }",
        )
        .expect("scalar and channel captures are safe");
    }

    #[test]
    fn go_owned_capture_moves_out_of_the_caller() {
        let errors = check_source(
            "async fun main() { var string msg = \"hi\"\n go { say(msg) }\n say(msg) }",
        )
        .unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("use of moved value: `msg`")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn channel_send_moves_the_value() {
        let errors = check_source(
            "async fun main() { var chan[string] ch = chan.new(1)\n var string m = \"x\"\n m |> ch\n say(m) }",
        )
        .unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("use of moved value: `m`")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn channel_receive_binds_an_owned_receiver() {
        check_source(
            "async fun main() { var chan[string] ch = chan.new(2)\n \"ping\" |> ch\n x <| ch\n say(x) }",
        )
        .expect("a receive binds a fresh owned receiver");
    }

    fn check_source(source: &str) -> Result<(), Vec<TypeError>> {
        let tokens = tokenize(source);
        let program = parse(&tokens).map_err(|errs| {
            errs.into_iter()
                .map(|e| TypeError {
                    code: None,
                    help: None,
                    message: e.message,
                    span: e.span,
                })
                .collect::<Vec<_>>()
        })?;
        check_program(&program)
    }

    #[test]
    fn var_with_annotation_and_matching_init() {
        assert!(check_source("var int x = 1").is_ok());
    }

    #[test]
    fn raw_pointer_deref_requires_unsafe() {
        let errs = check_source("fun f() { var *mut int p = 0\n *p = 1 }").unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.message.contains("requires an `unsafe` block"))
        );
    }

    #[test]
    fn raw_pointer_deref_is_allowed_inside_unsafe() {
        assert!(check_source(
            "use memory\nfun f() { var int x = 1\n var &mut int w = &mut x\n unsafe { var *mut int p = memory.raw_address(w)\n *p = 2 } }"
        )
        .is_ok());
    }

    #[test]
    fn ffi_calls_require_unsafe() {
        let errors = check_source(
            r#"extern "C" fun abs(int value) -> int
fun main() -> int { return abs(-1) }"#,
        )
        .expect_err("FFI calls outside unsafe must be rejected");
        assert!(errors.iter().any(|error| {
            error.message.contains("FFI call `abs`") && error.message.contains("unsafe")
        }));
    }

    #[test]
    fn ffi_calls_are_allowed_inside_unsafe() {
        check_source(
            r#"extern "C" { int abs(int value); }
fun main() -> int { unsafe { return abs(-1) } }"#,
        )
        .expect("FFI calls inside unsafe should type-check");
    }

    #[test]
    fn raw_address_preserves_pointee_type() {
        let errs = check_source(
            "use memory\nclass Packet { var int id }\nfun f() { var own Packet packet = alloc(Packet())\n var &mut Packet write = &mut packet\n unsafe { var *mut int raw = memory.raw_address(write) } }",
        )
        .unwrap_err();
        assert!(errs.iter().any(|e| e.message.contains("type mismatch")));
    }

    #[test]
    fn var_with_annotation_and_wrong_init() {
        let errs = check_source("var int x = \"hello\"").unwrap_err();
        assert!(errs.iter().any(|e| e.message.contains("type mismatch")));
    }

    #[test]
    fn var_inferred_from_init() {
        assert!(check_source("var x = 42").is_ok());
    }

    #[test]
    fn view_var_declaration_typechecks() {
        assert!(check_source("fun f() { var xs = [1, 2]; view var r = xs; }").is_ok());
        assert!(check_source("fun f() { var xs = [1, 2]; view mut var m = xs; }").is_ok());
        assert!(check_source("fun f() { var xs = [1, 2]; view var array[int] r = xs; }").is_ok());
    }

    #[test]
    fn view_var_requires_source() {
        let errs = check_source("fun f() { view var r = [1, 2]; }").unwrap_err();
        assert!(errs.iter().any(|e| e.message.contains("temporary value")));
    }

    #[test]
    fn view_var_of_scalar_is_rejected() {
        let errs = check_source("fun f() { var x = 1; view var r = x; }").unwrap_err();
        assert!(!errs.is_empty());
    }

    #[test]
    fn plain_var_cannot_store_a_view() {
        let errs = check_source("fun f() { var xs = [1, 2]; var r = view(xs); }").unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.message.contains("cannot store a view"))
        );
    }

    #[test]
    fn var_inferred_string() {
        assert!(check_source("var x = \"hello\"").is_ok());
    }

    #[test]
    fn function_params_and_return() {
        assert!(check_source("fun add(int a, int b) -> int { return a + b }").is_ok());
    }

    #[test]
    fn function_return_type_mismatch() {
        let errs = check_source("fun bad(int a) -> int { return \"hello\" }").unwrap_err();

        assert!(!errs.is_empty());
    }

    #[test]
    fn non_void_function_must_return_on_every_path() {
        let errors = check_source(
            "use sys\nuse arrays\nfun main() -> int {\n    var args = sys.args()\n    if (arrays.length(args) < 2) { return 1 }\n    if (arrays.get(args, 1) == \"init\") { say(\"testing...\") }\n}",
        )
        .unwrap_err();
        assert!(
            errors
                .iter()
                .any(|error| error.message.contains("may fall through")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn non_void_function_accepts_exhaustive_if_returns() {
        check_source(
            "fun classify(bool ready) -> int { if (ready) { return 1 } else { return 0 } }",
        )
        .expect("every branch returns");
    }

    #[test]
    fn non_void_function_rejects_return_only_inside_a_loop() {
        let errors = check_source("fun first(bool ready) -> int { while (ready) { return 1 } }")
            .unwrap_err();
        assert!(
            errors
                .iter()
                .any(|error| error.message.contains("may fall through")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn non_void_function_accepts_an_unconditional_returning_loop() {
        check_source("fun forever() -> int { while (true) { return 1 } }")
            .expect("the loop always enters and returns");
    }

    #[test]
    fn non_void_function_accepts_return_through_finally() {
        check_source("fun guarded() -> int { try { return 1 } finally { say(\"done\") } }")
            .expect("finally runs before the return completes");
    }

    #[test]
    fn unreachable_fallback_returns_remain_accepted() {
        check_source("fun f() -> int { return 1\n return 0 }")
            .expect("unreachable fallbacks do not change return coverage");
    }

    #[test]
    fn non_void_lambda_must_return_on_every_path() {
        let errors =
            check_source("var f = fun(bool ready) -> int { if (ready) { return 1 } }").unwrap_err();
        assert!(
            errors
                .iter()
                .any(|error| error.message.contains("lambda may fall through")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn condition_must_be_bool() {
        let errs = check_source("if (1) { }").unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.message.contains("condition must be `bool`"))
        );
    }

    #[test]
    fn condition_bool_is_ok() {
        assert!(check_source("if (true) { }").is_ok());
    }

    #[test]
    fn arithmetic_int() {
        assert!(check_source("var int x = 1 + 2 * 3").is_ok());
    }

    #[test]
    fn arithmetic_type_mismatch() {
        let errs = check_source("1 - \"hello\"").unwrap_err();
        assert!(errs.iter().any(|e| e.message.contains("cannot apply")));
    }

    #[test]
    fn undefined_variable() {
        let errs = check_source("var x = y").unwrap_err();
        assert!(errs.iter().any(|e| e.message.contains("undefined")));
    }

    #[test]
    fn duplicate_definition() {
        let errs = check_source("var int x = 1\nvar int x = 2").unwrap_err();
        assert!(errs.iter().any(|e| e.message.contains("already defined")));
    }

    #[test]
    fn say_with_string() {
        assert!(check_source("say(\"hello\")").is_ok());
    }

    #[test]
    fn say_with_non_string_warns() {
        let errs = check_source("say(42)").unwrap_err();
        assert!(errs.iter().any(|e| e.message.contains("expects a string")));
    }

    #[test]
    fn while_condition_must_be_bool() {
        let errs = check_source("while (1) { }").unwrap_err();
        assert!(errs.iter().any(|e| e.message.contains("condition")));
    }

    #[test]
    fn while_condition_bool_ok() {
        assert!(check_source("while (true) { }").is_ok());
    }

    #[test]
    fn nested_scopes() {
        let src = "var int x = 1\n{ var int x = 2 }";
        assert!(check_source(src).is_ok());
    }

    #[test]
    fn multiple_errors_collected() {
        let src = "var int x = \"a\"\nif (1) { }";
        let errs = check_source(src).unwrap_err();
        assert!(errs.len() >= 2);
    }

    #[test]
    fn lambda_type() {
        assert!(check_source("var f = fun(int x) -> int { return x }").is_ok());
    }

    #[test]
    fn unknown_parent_class() {
        let errs = check_source("class Dog extends Unknown { }").unwrap_err();
        assert!(errs.iter().any(|e| e.message.contains("not defined")));
    }

    #[test]
    fn class_self_reference() {
        assert!(check_source("class Dog { }").is_ok());
    }

    #[test]
    fn direct_inheritance_cycle_is_rejected() {
        let errs = check_source("class A extends A { }").unwrap_err();
        assert!(
            errs.iter().any(|e| e.message.contains("cannot inherit")),
            "expected an inheritance-cycle error, got {errs:?}"
        );
    }

    #[test]
    fn mutual_inheritance_cycle_is_rejected() {
        let errs = check_source("class A extends B { } class B extends A { }").unwrap_err();
        assert!(
            errs.iter().any(|e| e.message.contains("cannot inherit")),
            "expected an inheritance-cycle error, got {errs:?}"
        );
    }

    #[test]
    fn three_way_inheritance_cycle_is_rejected() {
        let errs =
            check_source("class A extends B { } class B extends C { } class C extends A { }")
                .unwrap_err();
        assert!(
            errs.iter().any(|e| e.message.contains("cannot inherit")),
            "expected an inheritance-cycle error, got {errs:?}"
        );
    }

    #[test]
    fn acyclic_inheritance_chain_is_accepted() {
        assert!(
            check_source("class Base { } class Mid extends Base { } class Leaf extends Mid { }")
                .is_ok()
        );
    }

    #[test]
    fn functions_require_complete_signatures() {
        let errors = check_source("fun id(value) { return value }").unwrap_err();
        assert!(
            errors
                .iter()
                .any(|error| error.message.contains("parameter `value` requires"))
        );
    }

    #[test]
    fn missing_return_type_defaults_to_void() {
        assert!(check_source("fun empty() { }").is_ok());
        assert!(check_source("fun main() { say(\"hi\") }").is_ok());
    }

    #[test]
    fn rejects_dynamic_annotations_and_uninitialized_variables() {
        let errors = check_source("var any value = 1\nvar int later").unwrap_err();
        assert!(
            errors
                .iter()
                .any(|error| error.message.contains("`any` is not supported"))
        );
        assert!(errors.iter().any(|error| {
            error
                .message
                .contains("variable `later` must be initialized")
        }));
    }

    #[test]
    fn rejects_implicit_numeric_conversion() {
        let errors = check_source("var float value = 1 + 2.0").unwrap_err();
        assert!(
            errors
                .iter()
                .any(|error| error.message.contains("cannot apply to `int` and `float`"))
        );
    }

    #[test]
    fn checks_function_argument_count_and_types() {
        let errors = check_source(
            "fun add(int left, int right) -> int { return left + right }\nadd(1, \"two\", 3)",
        )
        .unwrap_err();
        assert!(
            errors
                .iter()
                .any(|error| error.message.contains("expects 2 argument(s), got 3"))
        );
        assert!(
            errors
                .iter()
                .any(|error| error.message.contains("argument type mismatch"))
        );
    }

    #[test]
    fn checks_array_homogeneity_and_index_type() {
        let errors = check_source("var values = [1, \"two\"]\nvalues[true]").unwrap_err();
        assert!(
            errors
                .iter()
                .any(|error| error.message.contains("array element type mismatch"))
        );
        assert!(
            errors
                .iter()
                .any(|error| error.message.contains("index must be `int`"))
        );
    }

    #[test]
    fn named_types_are_available_to_function_signatures() {
        let source = "fun identity(Person person) -> Person { return person }\nclass Person { }";
        assert!(check_source(source).is_ok());
    }

    #[test]
    fn rejects_unknown_named_types() {
        let errors =
            check_source("fun identity(Missing value) -> Missing { return value }").unwrap_err();
        assert!(
            errors
                .iter()
                .any(|error| error.message.contains("unknown type `Missing`"))
        );
    }

    #[test]
    fn checks_explicit_array_and_option_types() {
        let source =
            "class Person { }\nvar array[int] values = [1, 2]\nvar option[Person] owner = nil";
        assert!(check_source(source).is_ok());

        let errors = check_source("var array[int] values = [1, \"two\"]").unwrap_err();
        assert!(
            errors
                .iter()
                .any(|error| error.message.contains("type mismatch"))
        );
    }

    #[test]
    fn async_function_with_top_level_awaits_checks() {
        let source = "async fun work() {\n    await async.sleep(10)\n    var msg = \"done\"\n    return\n}\nasync fun main() {\n    await work()\n}";
        assert!(check_source(source).is_ok());
    }

    #[test]
    fn await_is_only_allowed_inside_async_functions() {
        let errors = check_source("fun sync_work() { await async.sleep(10) }").unwrap_err();
        assert!(errors.iter().any(|error| {
            error
                .message
                .contains("await is only allowed inside an async")
        }));
    }

    #[test]
    fn async_function_must_be_awaited() {
        let errors = check_source("async fun work() { }\nasync fun main() { work() }").unwrap_err();
        assert!(
            errors
                .iter()
                .any(|error| error.message.contains("must be awaited"))
        );
    }

    #[test]
    fn await_requires_an_async_function() {
        let errors =
            check_source("fun helper() { }\nasync fun main() { await helper() }").unwrap_err();
        assert!(errors.iter().any(|error| {
            error
                .message
                .contains("await requires a call to an async function")
        }));
    }

    #[test]
    fn await_outside_statement_boundaries_is_rejected() {
        check_source("async fun work() { }\nasync fun main() {\n    if (true) { await work() }\n}")
            .expect("await in a control-flow statement is valid");

        let errors = check_source("async fun work() { }\nasync fun main() { await work() + 1 }")
            .unwrap_err();
        assert!(errors.iter().any(|error| {
            error
                .message
                .contains("await must be a statement-level call")
        }));
    }

    #[test]
    fn try_and_throw_are_allowed_inside_async_bodies() {
        // throw inside async bodies is accepted: it sets the
        // thread-local pending flag and the caller's exception check
        // catches it.
        check_source("async fun main() {\n    throw \"timeout\"\n}").unwrap();
    }

    #[test]
    fn async_sleep_checks_its_duration_argument() {
        let errors = check_source("async fun main() { await async.sleep(\"soon\") }").unwrap_err();
        assert!(errors.iter().any(|error| {
            error
                .message
                .contains("async.sleep expects an `int` duration")
        }));

        let errors = check_source("async fun main() { await async.sleep(1, 2) }").unwrap_err();
        assert!(
            errors
                .iter()
                .any(|error| error.message.contains("async.sleep expects 1 argument"))
        );
    }

    #[test]
    fn return_inside_control_flow_is_allowed_in_async_bodies() {
        check_source("async fun main() -> int {\n    if (true) { return 1 }\n    return 1\n}")
            .expect("nested returns are lowered through the async future result");

        assert!(
            check_source(
                "fun helper() -> int { return 7 }\nasync fun main() -> int { return helper() }"
            )
            .is_ok()
        );
    }

    #[test]
    fn await_as_variable_initializer_and_return_value_is_allowed() {
        let source = "async fun count() -> int { return 1 }\nasync fun main() -> int {\n    var int value = await count()\n    return await count()\n}";
        assert!(check_source(source).is_ok());
    }

    #[test]
    fn async_locals_without_an_annotation_must_have_literal_or_await_init() {
        let source =
            "async fun main() {\n    var count = 1 + 2\n}\nfun helper() -> int { return 0 }";
        let errors = check_source(source).unwrap_err();
        assert!(errors.iter().any(|error| {
            error
                .message
                .contains("requires an explicit type annotation")
        }));

        let source = "async fun main() { var text = \"hi\"\n var total = await async.sleep(1) }";
        assert!(check_source(source).is_ok());
    }

    #[test]
    fn lambda_cannot_capture_an_outer_variable() {
        let errors =
            check_source("fun f() { var x = 1\n var g = fun() -> int { return x } }").unwrap_err();
        assert!(errors.iter().any(|error| {
            error
                .message
                .contains("lambda cannot capture outer variable `x`")
        }));

        let errors = check_source(
            "fun f() { var xs = [1, 2]\n view var r = xs\n var g = fun() -> int { return r[0] } }",
        )
        .unwrap_err();
        assert!(errors.iter().any(|error| {
            error
                .message
                .contains("lambda cannot capture outer variable `r`")
        }));
    }

    #[test]
    fn lambda_cannot_assign_to_an_outer_variable() {
        let errors =
            check_source("fun f() { var x = 1\n var g = fun() -> void { x = 2 } }").unwrap_err();
        assert!(errors.iter().any(|error| {
            error
                .message
                .contains("lambda cannot capture outer variable `x`")
        }));
    }

    #[test]
    fn lambda_may_use_globals_params_and_its_own_locals() {
        assert!(check_source(
            "fun helper() -> int { return 7 }\nvar f = fun(int x) -> int { var y = x + 1; return helper() + y }"
        )
        .is_ok());
    }

    #[test]
    fn nested_lambda_cannot_capture_the_outer_lambdas_scope() {
        let errors =
            check_source("var g = fun(int x) -> void {\n    var h = fun() -> int { return x }\n}")
                .unwrap_err();
        assert!(errors.iter().any(|error| {
            error
                .message
                .contains("lambda cannot capture outer variable `x`")
        }));
    }

    #[test]
    fn array_literal_cannot_store_a_view() {
        let errors =
            check_source("fun f() { var xs = [1, 2]\n var arr = [view(xs)] }").unwrap_err();
        assert!(
            errors
                .iter()
                .any(|error| { error.message.contains("cannot store a view in an array") })
        );
    }

    #[test]
    fn assignment_cannot_store_a_view_in_an_owned_variable() {
        let errors = check_source(
            "fun f() { var array[int] outer = [0]\n var xs = [1, 2]\n outer = view(xs) }",
        )
        .unwrap_err();
        assert!(errors.iter().any(|error| {
            error
                .message
                .contains("cannot assign a view to variable `outer`")
        }));
    }

    #[test]
    fn assignment_may_still_rebind_a_view_holder() {
        check_source("fun f() { var xs = [1, 2]\n var ys = [3]\n view var v = xs\n v = ys }")
            .expect("rebinding a view holder is legal");
    }

    #[test]
    fn element_assignment_cannot_store_a_view() {
        let errors =
            check_source("fun f() { var outer = [[0]]\n var xs = [1, 2]\n outer[0] = view(xs) }")
                .unwrap_err();
        assert!(errors.iter().any(|error| {
            error
                .message
                .contains("cannot store a view in an array element")
        }));
    }

    #[test]
    fn field_assignment_cannot_store_a_view() {
        let errors = check_source(
            "class B { var array items }\nfun f() { var b = B()\n var xs = [1, 2]\n b.items = view(xs) }",
        )
        .unwrap_err();
        assert!(errors.iter().any(|error| {
            error
                .message
                .contains("cannot store a view in field `items`")
        }));
    }

    #[test]
    fn copying_a_view_may_be_stored_anywhere() {
        check_source(
            "class B { var array items }\nfun f() { var b = B()\n var outer = [[0]]\n var array[int] plain = [0]\n var xs = [1, 2]\n view var v = xs\n b.items = copy(v)\n outer[0] = copy(v)\n plain = copy(v) }",
        )
        .expect("an owned copy of a view is storable");
    }

    #[test]
    fn a_view_may_borrow_a_shared_pointee() {
        check_source("fun f() { shared array[int] s = [1, 2]\n view var v = s }")
            .expect("borrowing a shared pointee is legal");
    }

    #[test]
    fn a_mutable_view_may_borrow_a_shared_pointee() {
        check_source("fun f() { shared array[int] s = [1, 2]\n view mut var m = s }")
            .expect("exclusively borrowing a shared pointee is legal");
    }

    #[test]
    fn an_annotated_view_of_a_shared_pointee_still_checks_the_inner_type() {
        let errors =
            check_source("fun f() { shared array[int] s = [1, 2]\n view var string v = s }")
                .unwrap_err();
        assert!(errors.iter().any(|error| {
            error
                .message
                .contains("type mismatch: expected a view of `string`")
        }));
    }

    #[test]
    fn object_literal_cannot_store_a_view() {
        let errors = check_source("fun f() { var xs = [1, 2]\n var obj = { first: view(xs) } }")
            .unwrap_err();
        assert!(errors.iter().any(|error| {
            error
                .message
                .contains("cannot store a view in object property")
        }));
    }

    #[test]
    fn destructure_cannot_target_a_view() {
        let errors =
            check_source("fun f() { var xs = [1, 2]\n var [a, b] = view(xs) }").unwrap_err();
        assert!(
            errors
                .iter()
                .any(|error| { error.message.contains("cannot destructure a view") })
        );
    }

    #[test]
    fn result_variant_pattern_binds_the_payload_type() {
        let errors = check_source(
            "fun f(result[int, string] r) -> int {\n match (r) { case Ok(v) => { return v } case Err(e) => { var int bad = e } } return 0 }",
        )
        .unwrap_err();
        // `v` is an int (Ok payload); binding it to string errors on the
        // Err arm's misuse of `e`, proving both binders got their types.
        assert!(
            errors
                .iter()
                .any(|error| { error.message == "type mismatch: expected `int`, got `string`" })
        );
    }

    #[test]
    fn variant_pattern_on_a_non_result_is_rejected() {
        let errors = check_source(
            "fun f() -> int { match (7) { case Ok(v) => { return v } default => { return 0 } } }",
        )
        .unwrap_err();
        assert!(
            errors
                .iter()
                .any(|error| { error.message.contains("has no variant `Ok` to match here") })
        );
    }

    #[test]
    fn variant_pattern_accepts_a_result_scrutinee() {
        assert!(check_source(
            "fun f(result[int, string] r) -> int { match (r) { case Ok(v) => { return v } case Err(_) => { return -1 } } }"
        )
        .is_ok());
    }

    #[test]
    fn annotations_cannot_store_views_in_containers() {
        let errors = check_source("fun f(array[view int] xs) -> void { }").unwrap_err();
        assert!(errors.iter().any(|error| {
            error
                .message
                .contains("cannot store a view inside `array[view int]`")
        }));

        let errors = check_source("fun f() { var option[view int] o = nil }").unwrap_err();
        assert!(errors.iter().any(|error| {
            error
                .message
                .contains("cannot store a view inside `option[view int]`")
        }));

        let errors = check_source("fun f(shared view int s) -> void { }").unwrap_err();
        assert!(errors.iter().any(|error| {
            error
                .message
                .contains("cannot store a view inside `shared view int`")
        }));
    }

    #[test]
    fn view_of_a_view_is_rejected() {
        let errors =
            check_source("fun f() { var xs = [1, 2]\n view var r = view(xs) }").unwrap_err();
        assert!(errors.iter().any(|error| {
            error
                .message
                .contains("cannot take a view of type `view array<int>`")
        }));
    }

    #[test]
    fn field_read_has_the_declared_field_type() {
        let errors = check_source(
            "class Box { var int n = 0 }\nfun f() { var b = Box()\n var string s = b.n }",
        )
        .unwrap_err();
        assert!(
            errors.iter().any(|e| e.message.contains("type mismatch")),
            "{errors:?}"
        );

        assert!(
            check_source(
                "class Box { var int n = 0 }\nfun f() { var b = Box()\n var int k = b.n }"
            )
            .is_ok()
        );
    }

    #[test]
    fn field_read_type_is_checked_at_a_call_site() {
        let errors =
            check_source("class Box { var int n = 0 }\nfun f() { var b = Box()\n say(b.n) }")
                .unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("`say` expects a string, got `int`")),
            "{errors:?}"
        );
    }

    #[test]
    fn field_read_type_is_found_through_inheritance_and_views() {
        assert!(
            check_source(
                "class Base { var int n = 1 }\nclass Kid extends Base { }\n\
                 fun f() { var k = Kid()\n var int m = k.n }"
            )
            .is_ok()
        );

        let errors = check_source(
            "class Base { var int n = 1 }\nclass Kid extends Base { }\n\
             fun f() { var k = Kid()\n var string s = k.n }",
        )
        .unwrap_err();
        assert!(
            errors.iter().any(|e| e.message.contains("type mismatch")),
            "{errors:?}"
        );

        let errors = check_source(
            "class Box { var int n = 0 }\nfun g(view Box b) -> void { var string s = b.n }",
        )
        .unwrap_err();
        assert!(
            errors.iter().any(|e| e.message.contains("type mismatch")),
            "{errors:?}"
        );
    }

    #[test]
    fn field_write_must_match_the_declared_field_type() {
        let errors = check_source(
            "class Box { var int n = 0 }\nfun f() { var b = Box()\n b.n = \"not an int\" }",
        )
        .unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("field `n` has type `int`, got `string`")),
            "{errors:?}"
        );

        assert!(
            check_source("class Box { var int n = 0 }\nfun f() { var b = Box()\n b.n = 3 }")
                .is_ok()
        );
    }

    #[test]
    fn an_unannotated_or_method_member_stays_unchecked() {
        assert!(
            check_source(
                "class Box { var thing = nil\n fun run() -> void { } }\n\
                 fun f() { var b = Box()\n var string s = b.thing\n var x = b.run }"
            )
            .is_ok()
        );
    }

    #[test]
    fn a_cyclic_extends_chain_does_not_hang_field_lookup() {
        let errors = check_source(
            "class A extends B { }\nclass B extends A { }\nfun f(view A a) -> void { say(a.n) }",
        )
        .unwrap_err();
        assert!(!errors.is_empty());
    }

    #[test]
    fn channel_send_and_recv_are_allowed_as_top_level_async_statements() {
        assert!(
            check_source("async fun f(chan[int] c) {\n    5 |> c\n    x <| c\n}").is_ok(),
            "top-level channel send/recv in an async body should type-check"
        );
    }

    #[test]
    fn channel_send_inside_control_flow_is_allowed() {
        check_source("async fun f(chan[int] c) {\n    while (true) {\n        5 |> c\n    }\n}")
            .expect("channel send in an async control-flow statement is valid");
    }

    #[test]
    fn channel_send_in_sync_function_is_rejected() {
        let errors = check_source("fun f(chan[int] c) {\n    5 |> c\n}").unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("only allowed inside an async function")),
            "{errors:?}"
        );
    }

    #[test]
    fn channel_recv_in_sync_function_is_rejected() {
        let errors = check_source("fun f(chan[int] c) {\n    x <| c\n}").unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("only allowed inside an async function")),
            "{errors:?}"
        );
    }
}
