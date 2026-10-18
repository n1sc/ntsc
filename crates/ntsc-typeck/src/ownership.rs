//! Own · Move · View ownership checker.
//!
//! Runs after type checking and verifies the memory-model invariants:
//!
//! - every value has exactly one owner;
//! - assignment, passing a value to an owned parameter, returning a value,
//!   storing a value in an array/field, and destructuring **move** the value,
//!   leaving the source dead (use-after-move is an error);
//! - a `view` / `view mut` is a non-owning borrow. Expression-level views live
//!   for the current statement; `view var` declarations borrow their source
//!   for as long as the declaring variable is live;
//! - **non-lexical lifetimes**: a declared borrow ends at its holder's final
//!   use rather than at the end of the declaring scope, so a later move or
//!   exclusive borrow of the source is permitted once the last use of the
//!   holder has been checked. Statement granularity: a holder used anywhere
//!   in the current (or an enclosing) statement or in any statement after the
//!   current one keeps its borrow live;
//! - a live view conflicts with a move, reassignment, or exclusive borrow of
//!   its source; conflicts identify the owner, the borrowing holder, and the
//!   borrow's origin;
//! - `copy(expr)` deep-copies without moving the source;
//! - method receivers and `this` are implicitly `view mut` for the call/body;
//! - `for in` iteration borrows its container for the whole loop.
//!
//! The checker is deliberately permissive where the static type is unknown
//! (builtin/module calls, unannotated call results): those are treated as
//! reads/views rather than moves so existing programs keep compiling, at the
//! cost of some missed move errors.

use std::collections::{HashMap, HashSet};

use ntsc_ast::expr::{Expr, LiteralValue};
use ntsc_ast::span::Span;
use ntsc_ast::stmt::{Program, Stmt};
use ntsc_ast::types::TypeAnnotation;

use crate::resolve::TypeError;
use crate::ty::Ty;

/// How a value of a given type behaves under assignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ValueKind {
    /// Plain values (`int`, `float`, `bool`) — copying is invisible.
    Scalar,

    /// Owned heap values (`string`, `array`, `object`, classes) — moved.
    Heap,

    /// Explicitly shared (refcounted) heap values — copied, never moved.
    /// Aliasing is the point of `shared`, so copies are always permitted.
    Shared,

    /// A channel handle — reference counted like `Shared`, so copying it is
    /// what `var b = a` and `go f(ch)` do, but unlike `shared` its count is
    /// synchronized, so a copy may cross into a goroutine.
    Chan,

    /// Function/closure references — immutable, copyable.
    Function,

    /// A view — cannot be moved, cannot be stored.
    View,

    /// Type not statically known — treated permissively (never moved).
    Unknown,
}

/// The kind of borrow a view holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewKind {
    /// A shared borrow (`view`).
    Shared,
    /// An exclusive borrow (`view mut`).
    Mut,
}

/// Classification of a value used to decide move-versus-view at call sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParamKind {
    /// Owned parameter: the argument is moved.
    Owned,

    /// Shared view parameter (`view T`).
    ViewShared,

    /// Exclusive view parameter (`view mut T`).
    ViewMut,

    /// Unknown — permissive, treated as a read.
    Unknown,
}

/// What a thread boundary does with an owned heap payload (`string`,
/// `array`, `object`, class instance). The two boundaries differ, so the
/// policy lives on the boundary rather than on the value kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeapPolicy {
    /// The runtime copies the payload across, so each side owns independent
    /// data and the sender keeps its own value.
    Copies,

    /// The payload cannot cross. Carries the reason for the diagnostic.
    Rejects(&'static str),
}

/// A stdlib call that hands a value to code running on another thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ThreadBoundary {
    /// Qualified call name, for diagnostics.
    call: &'static str,

    /// Argument indices whose values cross the boundary.
    payloads: &'static [usize],

    /// How an owned heap payload is treated.
    heap: HeapPolicy,
}

/// A borrow held by a `view var` declaration: the holder borrows the source
/// for as long as the holder is live (ending at its final use under
/// non-lexical lifetimes).
#[derive(Debug, Clone, PartialEq, Eq)]
struct BorrowRecord {
    /// The name and defining scope of the borrowed source. The depth
    /// prevents a shadow with the same name from being mistaken for this
    /// owner.
    source: String,
    source_depth: usize,
    kind: ViewKind,

    /// Where the borrow was taken, for diagnostics.
    origin: Span,
}

/// A view currently held on a source, with the holder (for declared
/// borrows) and the borrow's origin, used for diagnostics.
struct ViewRef<'a> {
    kind: ViewKind,
    holder: Option<&'a str>,
    origin: Span,
}

/// The statement-level borrows of one name, captured so an assignment can
/// put them back after checking the expressions it evaluates before the
/// write.
struct SavedViews {
    name: String,
    shared: Option<(usize, Span)>,
    exclusive: Option<(usize, Span)>,
}

/// What the values inside a container are. Reading a place out of a
/// container is a copy when the place holds a scalar and a borrow when it
/// holds a heap value the container still owns, so the two have to be told
/// apart.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Contents {
    /// An array: every element has this kind.
    Elements(ValueKind),

    /// An instance of the named class: its fields have the class's kinds.
    Instance(String),
}

pub struct OwnershipChecker {
    pub errors: Vec<TypeError>,

    /// Every (message, span) already reported, so a defect reached twice —
    /// by two checks, or by the two passes over a loop body — is reported
    /// once.
    reported: HashSet<(String, Span)>,

    /// Scope stack of name → kind. Lookups search innermost-out.
    scopes: Vec<HashMap<String, ValueKind>>,

    /// What each in-scope container holds, one layer per scope, mirroring
    /// `scopes`. Only names whose contents are statically known are present
    /// (`array[string]`, an array-of-arrays literal, a class instance, …).
    contents: Vec<HashMap<String, Contents>>,

    /// Field kinds of every declared class: class → field → kind. A field
    /// holding a heap value is owned by the instance, so reading it borrows.
    class_fields: HashMap<String, HashMap<String, ValueKind>>,

    /// Names currently dead (moved) in the enclosing control-flow flow:
    /// name → the scope depths of the dead variables. Moves are keyed by
    /// the moved variable's defining scope so shadowing definitions and
    /// scope exits do not revive or poison the wrong variable.
    moved: HashMap<String, Vec<usize>>,

    /// Nesting depth of `go` statements whose call is being checked: a
    /// `go` call's arguments are shared with the goroutine (handles stay
    // usable in the caller) instead of moved into the callee.
    go_depth: usize,

    /// Shared views live in the current statement: name → (count, origin).
    statement_views: HashMap<String, (usize, Span)>,

    /// Exclusive views live in the current statement: name → (count,
    /// origin).
    statement_mut_views: HashMap<String, (usize, Span)>,

    /// Views held for the whole body of an enclosing `for in` loop or
    /// method receiver (`this`): name → (kind, origin). Each entry is one
    /// layer.
    held_views: Vec<HashMap<String, (ViewKind, Span)>>,

    /// Borrows held by `view var` declarations: holder → possible records,
    /// one layer per scope (mirroring `scopes`). Multiple records arise
    /// when control-flow paths assign the holder from different owners.
    borrows: Vec<HashMap<String, Vec<BorrowRecord>>>,

    /// Signatures of top-level functions: name → (param kinds, return
    /// kind).
    local_fns: HashMap<String, (Vec<ParamKind>, ValueKind)>,

    /// Whether we are inside a method body (`this` is a receiver view).
    in_method: bool,

    /// Names used anywhere in the enclosing statement, one layer per nested
    /// statement. A holder named here is live (its borrow conflicts).
    stmt_uses: Vec<HashSet<String>>,

    /// Names used in the statements after the current one in the enclosing
    /// blocks, one layer per enclosing block.
    live_tails: Vec<HashSet<String>>,
}

impl OwnershipChecker {
    pub fn new() -> Self {
        Self {
            errors: Vec::new(),
            reported: HashSet::new(),
            scopes: vec![HashMap::new()],
            contents: vec![HashMap::new()],
            class_fields: HashMap::new(),
            moved: HashMap::new(),
            go_depth: 0,
            statement_views: HashMap::new(),
            statement_mut_views: HashMap::new(),
            held_views: Vec::new(),
            borrows: vec![HashMap::new()],
            local_fns: HashMap::new(),
            in_method: false,
            stmt_uses: Vec::new(),
            live_tails: Vec::new(),
        }
    }

    pub fn check_program(&mut self, program: &Program) {
        self.collect_local_fns(program);
        self.collect_class_fields(program);
        self.push_scope();
        self.check_sequence(&program.statements);
        self.pop_scope();
    }

    // ── Setup ────────────────────────────────────────────────────────────

    /// Record the kind of every declared class field, so reading a field can
    /// tell a scalar copy from a borrow of a value the instance owns. Fields
    /// inherited through `extends` are folded into the subclass.
    fn collect_class_fields(&mut self, program: &Program) {
        let mut parents: Vec<(String, String)> = Vec::new();
        for stmt in &program.statements {
            let Stmt::Class {
                name, parent, body, ..
            } = stmt
            else {
                continue;
            };
            let mut fields = HashMap::new();
            for member in body {
                if let Stmt::Var {
                    name: field,
                    type_annotation,
                    ..
                } = member
                    && let Some(annotation) = type_annotation
                {
                    fields.insert(field.lexeme().to_string(), kind_of_annotation(annotation));
                }
            }
            if let Some(parent) = parent {
                parents.push((name.lexeme().to_string(), parent.lexeme().to_string()));
            }
            self.class_fields.insert(name.lexeme().to_string(), fields);
        }

        // One pass per link is enough for the chains the resolver allows: it
        // rejects inheritance cycles before ownership runs.
        for _ in 0..parents.len() {
            for (child, parent) in &parents {
                let inherited = self.class_fields.get(parent).cloned().unwrap_or_default();
                if let Some(fields) = self.class_fields.get_mut(child) {
                    for (field, kind) in inherited {
                        fields.entry(field).or_insert(kind);
                    }
                }
            }
        }
    }

    /// Record parameter/return kinds of every top-level function so calls to
    /// them can distinguish moves from views.
    fn collect_local_fns(&mut self, program: &Program) {
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
                _ => continue,
            };
            let param_kinds = params.iter().map(|param| self.param_kind(param)).collect();
            let return_kind = return_type
                .as_ref()
                .map(|rt| kind_of_annotation(&rt.ty))
                .unwrap_or(ValueKind::Unknown);
            self.local_fns
                .insert(name.lexeme().to_string(), (param_kinds, return_kind));
        }
    }

    /// Parameter ownership class. `view` / `view mut` parameters are views;
    /// everything else is owned.
    fn param_kind(&self, param: &ntsc_ast::expr::FunctionParam) -> ParamKind {
        match param.type_annotation.as_ref() {
            Some(TypeAnnotation::View(_, true)) => ParamKind::ViewMut,
            Some(TypeAnnotation::View(_, false)) => ParamKind::ViewShared,
            _ => ParamKind::Owned,
        }
    }

    // ── Scope helpers ────────────────────────────────────────────────────

    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
        self.contents.push(HashMap::new());
        self.borrows.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        let popped_depth = self.scopes.len() - 1;
        self.scopes.pop();
        self.contents.pop();
        self.borrows.pop();

        // Variables defined in the popped scope no longer exist: forget
        // their dead marks so a later shadow of the same name in a fresh
        // scope of the same depth is not born dead. Moves of outer
        // variables survive: they are keyed by the outer variable's own
        // depth.
        for depths in self.moved.values_mut() {
            depths.retain(|depth| *depth < popped_depth);
        }
        self.moved.retain(|_, depths| !depths.is_empty());
    }

    fn define_kind(&mut self, name: &str, kind: ValueKind) {
        self.scopes
            .last_mut()
            .expect("no scope on stack")
            .insert(name.to_string(), kind);
    }

    /// Record what the container `name` holds, so a later element or field
    /// read knows whether it copies a scalar or borrows a heap value the
    /// container still owns.
    fn define_contents(&mut self, name: &str, contents: Contents) {
        self.contents
            .last_mut()
            .expect("no scope on stack")
            .insert(name.to_string(), contents);
    }

    /// What the container `name` holds, innermost scope first. `None` when the
    /// container is unknown or its contents are not statically known — the
    /// permissive case.
    fn lookup_contents(&self, name: &str) -> Option<&Contents> {
        self.contents.iter().rev().find_map(|layer| layer.get(name))
    }

    /// The kind of `container[i]` when the container's element kind is
    /// known.
    fn element_kind(&self, object: &Expr) -> Option<ValueKind> {
        let Expr::Variable { name } = object else {
            return None;
        };
        match self.lookup_contents(name.lexeme())? {
            Contents::Elements(kind) => Some(*kind),
            Contents::Instance(_) => None,
        }
    }

    /// The kind of `object.field` when `object` is a variable holding a known
    /// class instance, or `None` when either is unknown.
    fn field_kind(&self, object: &Expr, field: &str) -> Option<ValueKind> {
        let Expr::Variable { name } = object else {
            return None;
        };
        let class = match self.lookup_contents(name.lexeme())? {
            Contents::Instance(class) => class,
            Contents::Elements(_) => return None,
        };
        self.class_fields.get(class)?.get(field).copied()
    }

    /// What a `var`'s initializer says its container holds: the element kind
    /// of an array literal, or the class of a constructor call. `None` when
    /// it cannot be determined or an array literal's elements disagree.
    ///
    /// A constructor call and a plain call share one AST node, so an
    /// unknown callee simply yields `None` and reads stay unclassified.
    fn contents_of_initializer(&self, expr: &Expr) -> Option<Contents> {
        match expr {
            Expr::ArrayLiteral { elements, .. } => {
                let mut kind = None;
                for element in elements {
                    let element_kind = match element {
                        Expr::Literal {
                            value: LiteralValue::String(_),
                            ..
                        }
                        | Expr::ArrayLiteral { .. } => ValueKind::Heap,
                        Expr::Literal {
                            value: LiteralValue::Number(_) | LiteralValue::Bool(_),
                            ..
                        } => ValueKind::Scalar,
                        Expr::Variable { name } => self.lookup_kind(name.lexeme())?,
                        _ => return None,
                    };
                    match kind {
                        None => kind = Some(element_kind),
                        Some(seen) if seen == element_kind => {}
                        Some(_) => return None,
                    }
                }
                kind.map(Contents::Elements)
            }

            Expr::Call { callee, .. } => match callee.as_ref() {
                Expr::Variable { name } if self.class_fields.contains_key(name.lexeme()) => {
                    Some(Contents::Instance(name.lexeme().to_string()))
                }
                _ => None,
            },
            Expr::Grouping { expression, .. } => self.contents_of_initializer(expression),
            _ => None,
        }
    }

    /// What a variable's annotation says it holds: the element kind of an
    /// `array[T]`, or the class of a `Named` annotation that names a
    /// declared class. `None` when the annotation says nothing useful.
    fn contents_of_annotation(&self, annotation: &TypeAnnotation) -> Option<Contents> {
        match annotation {
            TypeAnnotation::Array(element) => {
                Some(Contents::Elements(kind_of_annotation(element.as_ref()?)))
            }
            TypeAnnotation::Named(name) if self.class_fields.contains_key(name.lexeme()) => {
                Some(Contents::Instance(name.lexeme().to_string()))
            }
            TypeAnnotation::Option(inner)
            | TypeAnnotation::Shared(inner)
            | TypeAnnotation::View(inner, _) => self.contents_of_annotation(inner),
            _ => None,
        }
    }

    fn lookup_kind(&self, name: &str) -> Option<ValueKind> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).copied())
    }

    /// The scope depth a name resolves to (innermost bound), with its kind.
    fn lookup_depth(&self, name: &str) -> Option<(usize, ValueKind)> {
        self.scopes
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, scope)| scope.get(name).copied().map(|kind| (index, kind)))
    }

    /// Whether the currently visible variable of `name` is dead (moved).
    fn is_moved(&self, name: &str) -> bool {
        self.lookup_depth(name).is_some_and(|(depth, _)| {
            self.moved
                .get(name)
                .is_some_and(|depths| depths.contains(&depth))
        })
    }

    /// Reinitialization by assignment: mark the visible variable of `name`
    /// alive again.
    fn reinitialize(&mut self, name: &str) {
        if let Some((depth, _)) = self.lookup_depth(name)
            && let Some(depths) = self.moved.get_mut(name)
        {
            depths.retain(|d| *d != depth);
            if depths.is_empty() {
                self.moved.remove(name);
            }
        }
    }

    // ── View lifecycle ───────────────────────────────────────────────────

    fn clear_statement_views(&mut self) {
        self.statement_views.clear();
        self.statement_mut_views.clear();
    }

    /// Whether a holder of a declared borrow may still be used: it is live if
    /// it is used in the current or an enclosing statement, or in any
    /// statement after the current one in an enclosing block.
    fn is_borrow_live(&self, holder: &str) -> bool {
        self.stmt_uses.iter().any(|uses| uses.contains(holder))
            || self.live_tails.iter().any(|tails| tails.contains(holder))
    }

    /// The strongest live view of `name`, or `None` when it is not
    /// borrowed.
    ///
    /// Statement views and held views are always live; declared borrows are
    /// live only while their holder may still be used.
    fn is_viewed(&self, name: &str) -> Option<ViewRef<'_>> {
        if let Some((_, origin)) = self.statement_mut_views.get(name) {
            return Some(ViewRef {
                kind: ViewKind::Mut,
                holder: None,
                origin: *origin,
            });
        }
        if let Some((_, origin)) = self.statement_views.get(name) {
            return Some(ViewRef {
                kind: ViewKind::Shared,
                holder: None,
                origin: *origin,
            });
        }
        for layer in &self.held_views {
            if let Some((kind, origin)) = layer.get(name) {
                return Some(ViewRef {
                    kind: *kind,
                    holder: None,
                    origin: *origin,
                });
            }
        }

        let mut best: Option<ViewRef<'_>> = None;
        let visible_depth = self.lookup_depth(name).map(|(depth, _)| depth);
        for layer in &self.borrows {
            for (holder, records) in layer {
                for record in records {
                    if record.source == name
                        && Some(record.source_depth) == visible_depth
                        && self.is_borrow_live(holder)
                    {
                        // Declared borrows: an exclusive borrow outranks a
                        // shared one so the most restrictive conflict is
                        // reported.
                        let candidate = ViewRef {
                            kind: record.kind,
                            holder: Some(holder.as_str()),
                            origin: record.origin,
                        };
                        if best.is_none() || candidate.kind == ViewKind::Mut {
                            best = Some(candidate);
                        }
                    }
                }
            }
        }
        best
    }

    /// The possible borrow records held by `holder`, if any.
    fn find_borrows(&self, holder: &str) -> Option<&[BorrowRecord]> {
        self.borrows
            .iter()
            .rev()
            .find_map(|layer| layer.get(holder).map(Vec::as_slice))
    }

    /// End the declared borrow held by `holder` (e.g. on reassignment).
    fn release_borrow(&mut self, holder: &str) {
        for layer in &mut self.borrows {
            layer.remove(holder);
        }
    }

    /// A human description of a live view for diagnostics.
    fn view_description(&self, view: &ViewRef<'_>) -> String {
        match view.holder {
            Some(holder) => {
                format!("it is borrowed by `{holder}` (borrowed at {})", view.origin)
            }
            None => format!("it is viewed (viewed at {})", view.origin),
        }
    }

    /// Hold a borrow on `source` for the holder for the rest of its life.
    ///
    /// Used by `view var` declarations: the owner must be defined at or
    /// above the holder's scope, and the borrow lasts until the holder's
    /// final use.
    fn borrow_source(&mut self, holder: &str, source: &str, kind: ViewKind, origin: Span) {
        if self.is_moved(source) {
            self.error(format!("cannot view `{source}`: it was moved"), origin);
            return;
        }
        let Some((source_depth, _)) = self.lookup_depth(source) else {
            return;
        };
        let holder_depth = self
            .lookup_depth(holder)
            .map_or(self.scopes.len() - 1, |(depth, _)| depth);
        if source_depth > holder_depth {
            self.error(
                format!(
                    "view `{holder}` cannot borrow `{source}`: owner `{source}` is declared in a shorter-lived inner scope"
                ),
                origin,
            );
            return;
        }
        if let Some(existing) = self.is_viewed(source)
            && (existing.kind == ViewKind::Mut || kind == ViewKind::Mut)
        {
            self.error(
                format!(
                    "cannot take a {} view of `{source}`: it is already viewed; {}",
                    if kind == ViewKind::Mut {
                        "mutable"
                    } else {
                        "shared"
                    },
                    self.view_description(&existing),
                ),
                origin,
            );
        }
        self.borrows[holder_depth].insert(
            holder.to_string(),
            vec![BorrowRecord {
                source: source.to_string(),
                source_depth,
                kind,
                origin,
            }],
        );
    }

    /// Register a statement-scoped view on `name`, reporting a conflict if the
    /// name is already exclusively viewed or is currently moved.
    fn register_view(&mut self, name: &str, kind: ViewKind, span: Span) {
        if name == "this" {
            return;
        }
        if self.is_moved(name) {
            self.error(format!("cannot view `{name}`: it was moved"), span);
            return;
        }
        if let Some(existing) = self.is_viewed(name)
            && (existing.kind == ViewKind::Mut || kind == ViewKind::Mut)
        {
            self.error(
                format!(
                    "cannot take a {} view of `{name}`: it is already viewed; {}",
                    if kind == ViewKind::Mut {
                        "mutable"
                    } else {
                        "shared"
                    },
                    self.view_description(&existing),
                ),
                span,
            );
        }
        match kind {
            ViewKind::Shared => {
                let entry = self
                    .statement_views
                    .entry(name.to_string())
                    .or_insert((0, span));
                entry.0 += 1;
            }
            ViewKind::Mut => {
                let entry = self
                    .statement_mut_views
                    .entry(name.to_string())
                    .or_insert((0, span));
                entry.0 += 1;
            }
        }
    }

    /// Report an error and mark `name` dead. `context` describes the moving
    /// operation for the diagnostic.
    fn move_value(&mut self, name: &str, span: Span, context: &str) {
        if name == "this" {
            self.error("cannot move `this`; the receiver is a view", span);
            return;
        }
        if let Some(view) = self.is_viewed(name) {
            self.error(
                format!(
                    "cannot move `{name}` while it is viewed: {} ({context})",
                    self.view_description(&view),
                ),
                span,
            );
            return;
        }
        if self.is_moved(name) {
            self.error(format!("use of moved value: `{name}`"), span);
            return;
        }
        if let Some((depth, _)) = self.lookup_depth(name) {
            self.moved.entry(name.to_string()).or_default().push(depth);
        }
    }

    /// Move a value only if it is statically known to be an owned heap value.
    fn move_heap_value(&mut self, name: &str, span: Span, context: &str) {
        if self.lookup_kind(name) == Some(ValueKind::Heap) {
            self.move_value(name, span, context);
        }
    }

    fn error(&mut self, message: impl Into<String>, span: Span) {
        let message = message.into();

        if !self.reported.insert((message.clone(), span)) {
            return;
        }
        self.errors.push(TypeError {
            code: Some(ntsc_diag::codes::OWNERSHIP),
            message,
            span,
            help: None,
        });
    }

    /// Same as [`Self::error`], with a one-sentence fix-it rendered as a
    /// `help:` line under the error.
    fn error_with_help(&mut self, message: impl Into<String>, span: Span, help: impl Into<String>) {
        let message = message.into();

        if !self.reported.insert((message.clone(), span)) {
            return;
        }
        self.errors.push(TypeError {
            code: Some(ntsc_diag::codes::OWNERSHIP),
            message,
            span,
            help: Some(help.into()),
        });
    }

    // ── Statements ───────────────────────────────────────────────────────

    fn check_stmt(&mut self, stmt: &Stmt) {
        self.clear_statement_views();
        match stmt {
            Stmt::Var {
                name,
                type_annotation,
                initializer,
                view,
                ..
            } => {
                // A `&T` / `&mut T` declaration borrows its referent for as
                // long as the reference is live, exactly like `view var`. An
                // unannotated `var r = &x` declares the same borrow.
                let ref_borrow = match (type_annotation.as_ref(), initializer.as_ref()) {
                    (Some(TypeAnnotation::Ref(_, mutable)), _) => Some(*mutable),
                    (None, Some(Expr::Borrow { mutable, .. })) => Some(*mutable),
                    _ => None,
                };
                let declares_borrow = view.is_some() || ref_borrow.is_some();
                let mut_borrow = matches!(view, Some(ntsc_ast::types::ViewMutability::Mutable))
                    || ref_borrow == Some(true);

                let declared = if declares_borrow {
                    ValueKind::View
                } else {
                    type_annotation
                        .as_ref()
                        .map(kind_of_annotation)
                        .unwrap_or(ValueKind::Unknown)
                };
                let init_kind = initializer.as_ref().and_then(|init| self.check_expr(init));
                if !declares_borrow && let Some(Expr::Variable { name: source }) = initializer {
                    self.move_heap_value(source.lexeme(), source.span, "assignment");
                }

                if init_kind == Some(ValueKind::View) && !matches!(declared, ValueKind::View) {
                    let message = match initializer {
                        Some(
                            Expr::IndexGet { .. }
                            | Expr::Member { .. }
                            | Expr::OptionalMember { .. },
                        ) => format!(
                            "cannot store a borrowed element in `{}`; the container owns it",
                            name.lexeme()
                        ),
                        _ => format!(
                            "cannot store a view in `{}`; a view may not be kept beyond the current block",
                            name.lexeme()
                        ),
                    };
                    let help = match initializer {
                        Some(
                            Expr::IndexGet { .. }
                            | Expr::Member { .. }
                            | Expr::OptionalMember { .. },
                        ) => {
                            "store an independent value: `var T name = copy(source)`, or borrow with `view var name`"
                        }
                        _ => "declare the variable as `view var name = source` to keep the borrow",
                    };
                    self.error_with_help(message, name.span, help);
                }

                if declares_borrow {
                    // `&place` wraps its referent, so unwrap it before
                    // looking for the variable the borrow reaches.
                    let borrow_target = match initializer {
                        Some(Expr::Borrow { target, .. }) => Some(&**target),
                        other => other.as_ref(),
                    };

                    // Evaluating `&place` already registered a transient
                    // borrow for this statement. The declaration replaces it
                    // with a borrow that lives as long as the holder, so the
                    // transient one must go first or it conflicts with itself.
                    if ref_borrow.is_some() {
                        self.clear_statement_views();
                    }

                    // A declared view borrows its source until the view
                    // dies, so it can only point at a variable, element, or
                    // field; borrowing a temporary would leave it dangling.
                    if let Some(source) = root_source(borrow_target) {
                        // A variable whose kind is unknown can still hold
                        // an instance (e.g. `var b = Bag()`); the borrow
                        // reaches a field the instance owns, so a later
                        // move of the instance must conflict too.
                        if self.is_borrowable_kind(self.lookup_kind(source.lexeme()))
                            || self.holds_instance(source.lexeme())
                            || ref_borrow.is_some()
                        {
                            self.borrow_source(
                                name.lexeme(),
                                source.lexeme(),
                                if mut_borrow {
                                    ViewKind::Mut
                                } else {
                                    ViewKind::Shared
                                },
                                source.span,
                            );
                        }
                    } else {
                        self.error(
                            format!(
                                "cannot store a view of a temporary value in `{}`; borrow a variable, element, or field instead",
                                name.lexeme()
                            ),
                            name.span,
                        );
                    }
                }
                let final_kind = match declared {
                    ValueKind::Unknown => match init_kind {
                        Some(ValueKind::View) | None => ValueKind::Unknown,
                        Some(kind) => kind,
                    },
                    kind => kind,
                };
                self.define_kind(name.lexeme(), final_kind);

                if let Some(contents) = type_annotation
                    .as_ref()
                    .and_then(|annotation| self.contents_of_annotation(annotation))
                    .or_else(|| {
                        initializer
                            .as_ref()
                            .and_then(|init| self.contents_of_initializer(init))
                    })
                {
                    // Remember what the variable holds — an array's
                    // element kind or an instance's class — so element and
                    // field reads can tell a scalar copy from a borrow of
                    // a value the container still owns. The annotation is
                    // authoritative; the initializer is only a fallback.
                    self.define_contents(name.lexeme(), contents);
                }
            }
            Stmt::Expression { expression } => {
                let _ = self.check_expr(expression);
            }
            Stmt::Say { expression, .. } => {
                let _ = self.check_expr(expression);
            }
            Stmt::Block { statements, .. } => {
                self.push_scope();
                self.check_sequence(statements);
                self.pop_scope();
            }
            Stmt::If {
                condition,
                then_branch,
                elif_branches,
                else_branch,
            } => self.check_if(condition, then_branch, elif_branches, else_branch),
            Stmt::While { condition, body } => {
                self.check_loop(|checker| {
                    let _ = checker.check_expr(condition);
                    checker.check_stmt(body);
                });
            }
            Stmt::DoWhile { body, condition } => {
                self.check_loop(|checker| {
                    checker.check_stmt(body);
                    let _ = checker.check_expr(condition);
                });
            }
            Stmt::For {
                init,
                condition,
                update,
                body,
            } => {
                self.push_scope();
                if let Some(init) = init {
                    self.check_stmt(init);
                }
                self.check_loop(|checker| {
                    if let Some(condition) = condition {
                        let _ = checker.check_expr(condition);
                    }
                    if let Some(update) = update {
                        let _ = checker.check_expr(update);
                    }
                    checker.check_stmt(body);
                });
                self.pop_scope();
            }
            Stmt::ForIn {
                variable,
                iterable,
                body,
            } => {
                self.check_for_in(variable, iterable, body);
            }
            Stmt::ForAwait {
                variable: _,
                producer,
                body,
            } => {
                let _ = self.check_expr(producer);
                self.push_scope();
                self.check_stmt(body);
                self.pop_scope();
            }
            Stmt::ChanRecvFor {
                variable,
                channel,
                body,
            } => {
                let _ = self.check_expr(channel);
                self.push_scope();
                self.define_kind(variable.lexeme(), ValueKind::Heap);
                self.check_stmt(body);
                self.pop_scope();
            }
            Stmt::Go {
                call,
                block,
                keyword_span,
            } => {
                self.go_depth += 1;
                let _ = self.check_expr(call);
                self.go_depth -= 1;
                if let Some(block) = block {
                    self.push_scope();
                    self.check_sequence(block);
                    self.pop_scope();
                    // Owned (non-channel) captures move into the goroutine's
                    // future: the caller cannot use them afterwards. Capture
                    // types come from the typed pass via the GO_CAPTURES
                    // bridge; channels stay shared (the spawner keeps
                    // ownership of the registry handle), scalars are copied.
                    for (name, ty) in crate::resolve::go_captures(keyword_span.start) {
                        if matches!(
                            ty,
                            Ty::String | Ty::Array(_) | Ty::Object | Ty::Class(_) | Ty::Own(_)
                        ) {
                            self.move_heap_value(&name, *keyword_span, "go capture");
                        }
                    }
                }
            }
            Stmt::Return { value } => {
                if let Some(expr) = value {
                    let _ = self.check_expr(expr);
                    if let Expr::Variable { name } = expr {
                        self.move_heap_value(name.lexeme(), name.span, "return");
                    }
                }
            }
            Stmt::Throw { value } => {
                let _ = self.check_expr(value);
            }
            Stmt::Match {
                expression,
                cases,
                default_case,
            } => self.check_match(expression, cases, default_case),
            Stmt::Try {
                try_block,
                catch_var,
                catch_block,
                finally_block,
            } => self.check_try(try_block, catch_var, catch_block, finally_block),
            Stmt::Retry {
                count,
                body,
                catch_block,
                ..
            } => {
                let _ = self.check_expr(count);
                // A retried body repeats its move effects like a loop
                // body; a caught failure abandons the body's state.
                let before = self.moved.clone();
                let before_borrows = self.borrows.clone();

                self.check_loop(|checker| checker.check_stmt(body));
                if let Some(catch) = catch_block {
                    let body_borrows = self.borrows.clone();
                    self.borrows = before_borrows.clone();
                    self.check_stmt(catch);
                    self.merge_moved(&before);
                    self.borrows = union_borrows(&body_borrows, &self.borrows);
                }
            }
            Stmt::Function { params, body, .. } | Stmt::AsyncFunction { params, body, .. } => {
                self.check_function(params, body);
            }
            Stmt::Test { body, .. } => self.check_function(&[], body),
            Stmt::Class { body, .. } => {
                self.push_scope();
                for member in body {
                    match member {
                        Stmt::Function { params, body, .. }
                        | Stmt::AsyncFunction { params, body, .. } => {
                            self.check_method(params, body);
                        }
                        _ => {}
                    }
                }
                self.pop_scope();
            }
            Stmt::Enum { .. }
            | Stmt::TypeAlias { .. }
            | Stmt::Trait { .. }
            | Stmt::Impl { .. }
            | Stmt::Break { .. }
            | Stmt::Continue { .. }
            | Stmt::Use { .. }
            | Stmt::ExternFunction { .. }
            | Stmt::ExternBlock { .. } => {}
            Stmt::Unsafe { body } => self.check_stmt(body),
            Stmt::Quiet { body, .. } => self.check_stmt(body),
            Stmt::Destructure {
                initializer, names, ..
            } => {
                let init_kind = self.check_expr(initializer);
                // Destructuring binds the elements of the source; an owned
                // source is moved (partially), so it is dead until
                // reassigned.
                if let Expr::Variable { name: source } = initializer
                    && init_kind == Some(ValueKind::Heap)
                {
                    self.move_value(source.lexeme(), source.span, "destructuring");
                }
                for name in names {
                    self.define_kind(name.lexeme(), ValueKind::Unknown);
                }
            }
        }
    }

    fn check_sequence(&mut self, statements: &[Stmt]) {
        let mut suffix = vec![HashSet::new(); statements.len() + 1];
        for i in (0..statements.len()).rev() {
            let mut uses = suffix[i + 1].clone();
            uses.extend(deep_stmt_uses(&statements[i]));
            suffix[i] = uses;
        }
        self.live_tails.push(HashSet::new());
        for (i, stmt) in statements.iter().enumerate() {
            *self.live_tails.last_mut().expect("no tail layer") =
                std::mem::take(&mut suffix[i + 1]);
            self.stmt_uses.push(deep_stmt_uses(stmt));
            self.check_stmt(stmt);
            self.stmt_uses.pop();
        }
        self.live_tails.pop();
    }

    /// `if` branches are alternatives: the state after the statement is the
    /// union of the final states of every branch that falls through.
    /// Branches that diverge contribute nothing; when no branch reaches the
    /// join point the code after the `if` is unreachable.
    fn check_if(
        &mut self,
        condition: &Expr,
        then_branch: &Stmt,
        elif_branches: &[ntsc_ast::stmt::ElifBranch],
        else_branch: &Option<Box<Stmt>>,
    ) {
        let _ = self.check_expr(condition);
        let before = self.moved.clone();
        let before_borrows = self.borrows.clone();

        let mut merged: Option<HashMap<String, Vec<usize>>> = None;
        let mut merged_borrows: Option<Vec<HashMap<String, Vec<BorrowRecord>>>> = None;
        let mut reached_join = false;

        self.check_stmt(then_branch);
        if !diverges(then_branch) {
            merged = Some(self.moved.clone());
            merged_borrows = Some(self.borrows.clone());
            reached_join = true;
        }

        for elif in elif_branches {
            self.moved = before.clone();
            self.borrows = before_borrows.clone();
            let _ = self.check_expr(&elif.condition);
            self.check_stmt(&elif.body);
            if diverges(&elif.body) {
                continue;
            }
            merged = Some(match merged {
                Some(prior) => union(&prior, &self.moved),
                None => self.moved.clone(),
            });
            merged_borrows = Some(match merged_borrows {
                Some(prior) => union_borrows(&prior, &self.borrows),
                None => self.borrows.clone(),
            });
            reached_join = true;
        }

        if let Some(else_branch) = else_branch {
            self.moved = before.clone();
            self.borrows = before_borrows.clone();
            self.check_stmt(else_branch);
            if !diverges(else_branch) {
                merged = Some(match merged {
                    Some(prior) => union(&prior, &self.moved),
                    None => self.moved.clone(),
                });
                merged_borrows = Some(match merged_borrows {
                    Some(prior) => union_borrows(&prior, &self.borrows),
                    None => self.borrows.clone(),
                });
                reached_join = true;
            }
        } else {
            merged = Some(match merged {
                Some(prior) => union(&prior, &before),
                None => before.clone(),
            });
            merged_borrows = Some(match merged_borrows {
                Some(prior) => union_borrows(&prior, &before_borrows),
                None => before_borrows.clone(),
            });
            reached_join = true;
        }

        if !reached_join {
            self.moved = before;
            self.borrows = before_borrows;
            return;
        }
        self.moved = merged.unwrap_or(before);
        self.borrows = merged_borrows.unwrap_or(before_borrows);
    }

    /// Like `if`, match arms are alternatives: surviving state is the union of
    /// the arms that fall through; a missing default makes the join include
    /// the pre-match state.
    fn check_match(
        &mut self,
        expression: &Expr,
        cases: &[ntsc_ast::stmt::MatchCase],
        default_case: &Option<Box<Stmt>>,
    ) {
        let _ = self.check_expr(expression);
        let before = self.moved.clone();
        let before_borrows = self.borrows.clone();

        let mut merged: Option<HashMap<String, Vec<usize>>> = None;
        let mut merged_borrows: Option<Vec<HashMap<String, Vec<BorrowRecord>>>> = None;
        let mut reached_join = false;

        for case in cases {
            self.moved = before.clone();
            self.borrows = before_borrows.clone();
            let _ = self.check_expr(&case.value);
            if let Some(guard) = &case.guard {
                let _ = self.check_expr(guard);
            }
            self.check_stmt(&case.body);

            if diverges(&case.body) {
                continue;
            }
            merged = Some(match merged {
                Some(prior) => union(&prior, &self.moved),
                None => self.moved.clone(),
            });
            merged_borrows = Some(match merged_borrows {
                Some(prior) => union_borrows(&prior, &self.borrows),
                None => self.borrows.clone(),
            });
            reached_join = true;
        }

        if let Some(default) = default_case {
            self.moved = before.clone();
            self.borrows = before_borrows.clone();
            self.check_stmt(default);
            if !diverges(default) {
                merged = Some(match merged {
                    Some(prior) => union(&prior, &self.moved),
                    None => self.moved.clone(),
                });
                merged_borrows = Some(match merged_borrows {
                    Some(prior) => union_borrows(&prior, &self.borrows),
                    None => self.borrows.clone(),
                });
                reached_join = true;
            }
        } else {
            merged = Some(match merged {
                Some(prior) => union(&prior, &before),
                None => before.clone(),
            });
            merged_borrows = Some(match merged_borrows {
                Some(prior) => union_borrows(&prior, &before_borrows),
                None => before_borrows.clone(),
            });
            reached_join = true;
        }

        if !reached_join {
            self.moved = before;
            self.borrows = before_borrows;
            return;
        }
        self.moved = merged.unwrap_or(before);
        self.borrows = merged_borrows.unwrap_or(before_borrows);
    }

    /// An exception can interrupt the try block at any point, so the catch
    /// block runs on the pre-try state; the state after try/catch is the
    /// union of the try's and the catch's results. `finally` observes both
    /// outcomes and is merged back into whichever one resumes.
    fn check_try(
        &mut self,
        try_block: &Stmt,
        catch_var: &Option<ntsc_ast::token::Token>,
        catch_block: &Option<Box<Stmt>>,
        finally_block: &Option<Box<Stmt>>,
    ) {
        let before = self.moved.clone();
        let before_borrows = self.borrows.clone();
        self.check_stmt(try_block);
        let mut merged = union(&self.moved, &before);
        let mut merged_borrows = union_borrows(&self.borrows, &before_borrows);

        if let (Some(var), Some(catch)) = (catch_var, catch_block) {
            self.moved = before.clone();
            self.borrows = before_borrows.clone();
            self.push_scope();

            self.define_kind(var.lexeme(), ValueKind::Heap);
            self.check_stmt(catch);
            self.pop_scope();
            merged = union(&merged, &self.moved);
            merged_borrows = union_borrows(&merged_borrows, &self.borrows);
        } else if let Some(catch) = catch_block {
            self.moved = before.clone();
            self.borrows = before_borrows.clone();
            self.check_stmt(catch);
            merged = union(&merged, &self.moved);
            merged_borrows = union_borrows(&merged_borrows, &self.borrows);
        }

        self.moved = merged;
        self.borrows = merged_borrows;
        if let Some(finally) = finally_block {
            let before_finally = std::mem::take(&mut self.moved);
            let before_finally_borrows = self.borrows.clone();
            self.check_stmt(finally);
            self.moved = union(&before_finally, &self.moved);
            self.borrows = union_borrows(&before_finally_borrows, &self.borrows);
        }
    }

    /// Iterating over an array keeps a shared view on the container for the
    /// whole loop, so the body cannot move or assign it.
    fn check_for_in(&mut self, variable: &ntsc_ast::token::Token, iterable: &Expr, body: &Stmt) {
        let mut borrowed_container = false;
        if let Expr::Variable { name } = iterable {
            let kind = self
                .lookup_kind(name.lexeme())
                .unwrap_or(ValueKind::Unknown);
            if kind == ValueKind::Heap && !self.is_moved(name.lexeme()) {
                let mut layer = HashMap::new();
                layer.insert(
                    name.lexeme().to_string(),
                    (ViewKind::Shared, iterable.span()),
                );
                self.held_views.push(layer);
                borrowed_container = true;
            }
        } else {
            let _ = self.check_expr(iterable);
        }
        self.push_scope();
        self.define_kind(variable.lexeme(), ValueKind::Unknown);
        self.check_loop(|checker| checker.check_stmt(body));
        self.pop_scope();

        if borrowed_container {
            self.held_views.pop();
        }
    }

    fn check_function(&mut self, params: &[ntsc_ast::expr::FunctionParam], body: &[Stmt]) {
        self.push_scope();
        let saved_moved = std::mem::take(&mut self.moved);
        for param in params {
            let kind = param
                .type_annotation
                .as_ref()
                .map(kind_of_annotation)
                .unwrap_or(ValueKind::Unknown);
            self.define_kind(param.name.lexeme(), kind);
        }
        self.check_sequence(body);
        self.moved = saved_moved;
        self.pop_scope();
    }

    /// A method holds an exclusive borrow on `this` for its whole body, so
    /// the receiver can be read and mutated but never moved or reassigned.
    fn check_method(&mut self, params: &[ntsc_ast::expr::FunctionParam], body: &[Stmt]) {
        self.push_scope();
        self.define_kind("this", ValueKind::View);
        let mut receiver = HashMap::new();
        receiver.insert("this".to_string(), (ViewKind::Mut, Span::dummy()));
        self.held_views.push(receiver);
        let saved_moved = std::mem::take(&mut self.moved);
        let saved_in_method = self.in_method;
        self.in_method = true;
        for param in params {
            let kind = param
                .type_annotation
                .as_ref()
                .map(kind_of_annotation)
                .unwrap_or(ValueKind::Unknown);
            self.define_kind(param.name.lexeme(), kind);
        }
        self.check_sequence(body);
        self.in_method = saved_in_method;
        self.moved = saved_moved;
        self.held_views.pop();
        self.pop_scope();
    }

    fn merge_moved(&mut self, before: &HashMap<String, Vec<usize>>) {
        self.moved = union(before, &self.moved);
    }

    /// A loop body may run zero, once, or many times, and later passes repeat
    /// its effects; two passes over the body already cover every reachable
    /// state, so the merged state is the pre-loop state unioned with the
    /// first pass's.
    fn check_loop(&mut self, mut parts: impl FnMut(&mut Self)) {
        let before = self.moved.clone();
        let before_borrows = self.borrows.clone();
        parts(self);
        self.merge_moved(&before);
        self.borrows = union_borrows(&before_borrows, &self.borrows);
        if self.moved == before {
            return;
        }
        let first_pass = self.moved.clone();
        let first_borrows = self.borrows.clone();

        parts(self);
        self.moved = union(&first_pass, &self.moved);
        self.borrows = union_borrows(&first_borrows, &self.borrows);
    }

    // ── Expressions ──────────────────────────────────────────────────────

    fn check_expr(&mut self, expr: &Expr) -> Option<ValueKind> {
        match expr {
            Expr::Literal { value, .. } => Some(match value {
                ntsc_ast::expr::LiteralValue::Nil => ValueKind::Scalar,
                ntsc_ast::expr::LiteralValue::Bool(_) => ValueKind::Scalar,
                ntsc_ast::expr::LiteralValue::Number(_) => ValueKind::Scalar,
                ntsc_ast::expr::LiteralValue::String(_) => ValueKind::Heap,
            }),
            Expr::Variable { name } => {
                let kind = self
                    .lookup_kind(name.lexeme())
                    .unwrap_or(ValueKind::Unknown);
                if self.is_moved(name.lexeme()) {
                    self.error(
                        format!("use of moved value: `{}`", name.lexeme()),
                        name.span,
                    );
                }
                Some(kind)
            }
            Expr::Assign { name, value } => {
                let value_kind = self.check_expr(value);
                if let Expr::Variable { name: source } = &**value {
                    self.move_heap_value(source.lexeme(), source.span, "assignment");
                }
                let target_kind = self.lookup_kind(name.lexeme());

                if matches!(
                    target_kind,
                    Some(ValueKind::Heap | ValueKind::Shared | ValueKind::Chan)
                ) && self.is_viewed(name.lexeme()).is_some()
                {
                    self.error(
                        format!("cannot assign to `{}` while it is viewed", name.lexeme()),
                        name.span,
                    );
                }

                // Assigning to a declared view rebinds it: the old borrow
                // dies and a fresh one is recorded, either from `view
                // var`-style initializers or by inheriting the borrows of
                // an existing view.
                if target_kind == Some(ValueKind::View) {
                    self.release_borrow(name.lexeme());
                    if let Expr::View {
                        target, mutable, ..
                    } = &**value
                    {
                        if let Some(source) = root_source(Some(target))
                            && self.is_borrowable_kind(self.lookup_kind(source.lexeme()))
                        {
                            self.borrow_source(
                                name.lexeme(),
                                source.lexeme(),
                                if *mutable {
                                    ViewKind::Mut
                                } else {
                                    ViewKind::Shared
                                },
                                target.span(),
                            );
                        }
                    } else if let Expr::Variable { name: source } = &**value
                        && let Some(records) = self.find_borrows(source.lexeme())
                    {
                        let records = records.to_vec();
                        if let Some((holder_depth, _)) = self.lookup_depth(name.lexeme()) {
                            self.borrows[holder_depth].insert(name.lexeme().to_string(), records);
                        }
                    }
                }

                self.reinitialize(name.lexeme());
                value_kind
            }
            Expr::Binary { left, right, op } => {
                let left_kind = self.check_expr(left);
                let right_kind = self.check_expr(right);

                // `+` on any string operand yields a heap-owned string.
                if op.kind == ntsc_ast::token::TokenKind::Plus
                    && (left_kind == Some(ValueKind::Heap) || right_kind == Some(ValueKind::Heap))
                {
                    Some(ValueKind::Heap)
                } else {
                    Some(ValueKind::Scalar)
                }
            }
            Expr::Unary { right, .. } => {
                let _ = self.check_expr(right);
                Some(ValueKind::Scalar)
            }
            Expr::PostfixUnary { left, .. } => {
                let _ = self.check_expr(left);
                Some(ValueKind::Scalar)
            }
            Expr::Grouping { expression, .. } => self.check_expr(expression),
            Expr::Ternary {
                condition,
                then_branch,
                else_branch,
            } => {
                let _ = self.check_expr(condition);
                // The result kind is the common kind of both branches;
                // differing kinds lose the information.
                let a = self.check_expr(then_branch);
                let b = self.check_expr(else_branch);
                match (a, b) {
                    (Some(a), Some(b)) if a == b => Some(a),
                    _ => Some(ValueKind::Unknown),
                }
            }
            Expr::Call {
                callee, arguments, ..
            } => self.check_call(callee, arguments, false),
            Expr::Await {
                callee, arguments, ..
            } => self.check_call(callee, arguments, true),
            Expr::AsyncBlock { body, .. } => {
                self.push_scope();
                for stmt in body {
                    self.check_stmt(stmt);
                }
                self.pop_scope();
                None
            }
            Expr::ChanSend { channel, value, .. } => {
                let _ = self.check_expr(channel);
                let _ = self.check_expr(value);
                // Sending moves the value into the channel; the sender can no
                // longer use it.
                if let Expr::Variable { name } = &**value {
                    self.move_heap_value(name.lexeme(), value.span(), "channel send");
                }
                None
            }
            Expr::ChanRecv {
                receiver, channel, ..
            } => {
                let _ = self.check_expr(channel);
                // The received element is owned by the receiver and freed once
                // at scope exit.
                self.define_kind(receiver.lexeme(), ValueKind::Heap);
                Some(ValueKind::Heap)
            }
            Expr::Close { channel, .. } => {
                let _ = self.check_expr(channel);
                None
            }
            Expr::View {
                target, mutable, ..
            } => {
                // Only a variable (or an element/field of one, handled by
                // the container-borrow logic in the element/member arms)
                // can be borrowable here; borrowing a temporary would dangle.
                if let Expr::Variable { name } = &**target {
                    let kind = self
                        .lookup_kind(name.lexeme())
                        .unwrap_or(ValueKind::Unknown);
                    if self.is_borrowable_kind(Some(kind)) {
                        self.register_view(
                            name.lexeme(),
                            if *mutable {
                                ViewKind::Mut
                            } else {
                                ViewKind::Shared
                            },
                            name.span,
                        );
                    }
                } else {
                    let _ = self.check_expr(target);
                }
                Some(ValueKind::View)
            }
            Expr::Copy { expression, .. } => {
                let kind = self.check_expr(expression);

                // A copy of a view produces a value the caller now owns.
                match kind {
                    Some(ValueKind::View) => Some(ValueKind::Heap),
                    kind => kind,
                }
            }
            Expr::Borrow {
                target, mutable, ..
            } => {
                let _ = self.check_expr(target);
                if let Expr::Variable { name } = &**target
                    && self.is_borrowable_kind(self.lookup_kind(name.lexeme()))
                {
                    self.register_view(
                        name.lexeme(),
                        if *mutable {
                            ViewKind::Mut
                        } else {
                            ViewKind::Shared
                        },
                        name.span,
                    );
                }
                Some(ValueKind::View)
            }
            Expr::RawDeref { target, .. } => {
                let _ = self.check_expr(target);
                Some(ValueKind::Unknown)
            }
            Expr::RawDerefSet { target, value, .. } => {
                let _ = self.check_expr(target);
                let _ = self.check_expr(value);
                Some(ValueKind::Unknown)
            }
            Expr::Member { object, property } | Expr::OptionalMember { object, property } => {
                self.check_member_read(object);

                // A heap-typed field is read through the container's handle
                // and aliases the container, i.e. is a borrow.
                match self.field_kind(object, property.lexeme()) {
                    Some(ValueKind::Heap) => Some(ValueKind::View),
                    _ => Some(ValueKind::Unknown),
                }
            }
            // Setting a field borrows the container exclusively for this
            // statement. The value expression is checked before the borrow
            // is registered so that a read of the container inside the
            // value does not collide with the view recorded for the write.
            Expr::MemberSet { object, value, .. } => {
                let saved = self.snapshot_container_views(object);
                let value_kind = self.check_expr(value);
                self.restore_container_views(saved);
                self.check_member_write(object);
                if let Expr::Variable { name: source } = &**value {
                    self.move_heap_value(source.lexeme(), source.span, "assignment");
                }
                value_kind
            }
            Expr::IndexGet { object, index } => {
                let _ = self.check_expr(index);
                self.check_member_read(object);

                match self.element_kind(object) {
                    Some(ValueKind::Heap) => Some(ValueKind::View),
                    _ => Some(ValueKind::Unknown),
                }
            }
            Expr::IndexSet {
                object,
                index,
                value,
                ..
            } => {
                let saved = self.snapshot_container_views(object);
                let _ = self.check_expr(index);
                let value_kind = self.check_expr(value);
                self.restore_container_views(saved);
                self.check_member_write(object);
                if let Expr::Variable { name: source } = &**value {
                    self.move_heap_value(source.lexeme(), source.span, "assignment");
                }
                value_kind
            }
            Expr::This { .. } => Some(self.lookup_kind("this").unwrap_or(ValueKind::Heap)),
            Expr::Spread { value, .. } => {
                let _ = self.check_expr(value);
                Some(ValueKind::Unknown)
            }
            Expr::ObjectLiteral { properties, .. } => {
                for property in properties {
                    let _ = self.check_expr(&property.value);
                    if let Expr::Variable { name: source } = &property.value {
                        self.move_heap_value(source.lexeme(), source.span, "assignment");
                    }
                }
                Some(ValueKind::Heap)
            }
            Expr::ArrayLiteral { elements, .. } => {
                for element in elements {
                    let _ = self.check_expr(element);
                    if let Expr::Variable { name: source } = element {
                        self.move_heap_value(source.lexeme(), source.span, "assignment");
                    }
                }
                Some(ValueKind::Heap)
            }
            Expr::Lambda { params, body, .. } => {
                self.check_function(params, body);
                Some(ValueKind::Function)
            }
            Expr::Propagate { value, .. } => {
                let _ = self.check_expr(value);
                // A result is a heap cell, like an option box.
                Some(ValueKind::Heap)
            }
            Expr::StructLiteral {
                class_name: _,
                fields,
                update,
                ..
            } => {
                for field in fields {
                    let _ = self.check_expr(&field.value);
                    if let Expr::Variable { name: source } = &field.value {
                        self.move_heap_value(source.lexeme(), source.span, "struct field");
                    }
                }
                // A `..base` update only reads the base's fields; the base
                // itself stays owned by its binding, like a member read.
                if let Some(update) = update {
                    if let Expr::Variable { name } = update.as_ref() {
                        if self.is_borrowable_kind(self.lookup_kind(name.lexeme())) {
                            self.register_view(name.lexeme(), ViewKind::Shared, name.span);
                        }
                    } else {
                        let _ = self.check_expr(update);
                    }
                }
                Some(ValueKind::Heap)
            }
            Expr::TupleLiteral { elements, .. } => {
                for element in elements {
                    let _ = self.check_expr(element);
                }
                Some(ValueKind::Heap)
            }
            Expr::TupleIndex { object, .. } => {
                let _ = self.check_expr(object);
                Some(ValueKind::Heap)
            }
        }
    }

    fn check_member_read(&mut self, object: &Expr) {
        // Reading an element or field of a container borrows it for this
        // statement, because the value is reached through its handle.
        if let Expr::Variable { name } = object {
            if self.is_borrowable_kind(self.lookup_kind(name.lexeme())) {
                self.register_view(name.lexeme(), ViewKind::Shared, name.span);
            }
        } else {
            let _ = self.check_expr(object);
        }
    }

    fn snapshot_container_views(&self, object: &Expr) -> Option<SavedViews> {
        // Save the container's statement views so the value expression can
        // be checked before the write registers its own borrow.
        let name = root_source(Some(object))?.lexeme().to_string();
        Some(SavedViews {
            shared: self.statement_views.get(&name).copied(),
            exclusive: self.statement_mut_views.get(&name).copied(),
            name,
        })
    }

    fn restore_container_views(&mut self, saved: Option<SavedViews>) {
        let Some(saved) = saved else { return };
        restore_statement_view(&mut self.statement_views, &saved.name, saved.shared);
        restore_statement_view(&mut self.statement_mut_views, &saved.name, saved.exclusive);
    }

    fn check_member_write(&mut self, object: &Expr) {
        if let Expr::Variable { name } = object {
            if self.is_borrowable_kind(self.lookup_kind(name.lexeme())) {
                self.register_view(name.lexeme(), ViewKind::Mut, name.span);
            } else if let Some(described) = self
                .is_viewed(name.lexeme())
                .map(|existing| self.view_description(&existing))
            {
                self.error(
                    format!(
                        "cannot assign to a field of `{}` while it is viewed: {described}",
                        name.lexeme()
                    ),
                    name.span,
                );
            }
        } else {
            let _ = self.check_expr(object);
        }
    }

    fn holds_instance(&self, name: &str) -> bool {
        matches!(self.lookup_contents(name), Some(Contents::Instance(_)))
    }

    fn is_borrowable_kind(&self, kind: Option<ValueKind>) -> bool {
        matches!(
            kind,
            Some(ValueKind::Heap | ValueKind::Shared | ValueKind::Chan)
        )
    }

    fn check_call(
        &mut self,
        callee: &Expr,
        arguments: &[Expr],
        is_await: bool,
    ) -> Option<ValueKind> {
        // Receiver and argument borrows are released when the call ends:
        // an argument is borrowed for the duration of the call only, and
        // the move-out of an owned argument must not conflict with the
        // callee's receiver view of the same name.
        let saved_views = std::mem::take(&mut self.statement_views);
        let saved_mut_views = std::mem::take(&mut self.statement_mut_views);
        let result = self.check_call_inner(callee, arguments, is_await);
        self.statement_views = saved_views;
        self.statement_mut_views = saved_mut_views;
        result
    }

    fn check_thread_boundary(&mut self, boundary: ThreadBoundary, arguments: &[Expr]) {
        // `process.spawn_thread` hands its payload argument to another
        // thread, so the caller may not keep (or later free) a reference
        // to it; `collections.channel_send` copies instead, so its payload
        // may be an owned heap value.
        for &index in boundary.payloads {
            let Some(argument) = arguments.get(index) else {
                continue;
            };

            let Some(name) = root_source(Some(argument)) else {
                continue;
            };
            let kind = self
                .lookup_kind(name.lexeme())
                .unwrap_or(ValueKind::Unknown);
            if let Some(reason) = thread_unsafe_reason(kind, boundary.heap) {
                self.error(
                    format!(
                        "cannot pass `{}` to {}: {reason}",
                        name.lexeme(),
                        boundary.call
                    ),
                    name.span,
                );
            }
        }
    }

    fn check_call_inner(
        &mut self,
        callee: &Expr,
        arguments: &[Expr],
        is_await: bool,
    ) -> Option<ValueKind> {
        let mut param_kinds: Option<Vec<ParamKind>> = None;
        if let Expr::Variable { name } = callee
            && let Some((params, _)) = self.local_fns.get(name.lexeme())
        {
            param_kinds = Some(params.clone());
        }

        let module_arg_view = if let Expr::Member { object, property } = callee {
            if let Expr::Variable { name } = &**object {
                module_array_arg_view_kind(name.lexeme(), property.lexeme())
            } else {
                None
            }
        } else {
            None
        };

        // A method call borrows its receiver exclusively for the duration
        // of the call: the callee may mutate or move through it while the
        // caller must not.
        if let Expr::Member { object, .. } = callee {
            if let Expr::Variable { name } = &**object {
                let kind = self
                    .lookup_kind(name.lexeme())
                    .unwrap_or(ValueKind::Unknown);
                if self.is_borrowable_kind(Some(kind)) {
                    self.register_view(name.lexeme(), ViewKind::Mut, name.span);
                } else if kind == ValueKind::View {
                } else {
                    let _ = self.check_expr(object);
                }
            } else {
                let _ = self.check_expr(object);
            }
        } else {
            let _ = self.check_expr(callee);
        }

        for (index, argument) in arguments.iter().enumerate() {
            // Match the argument against the declared parameter kind when
            // known: an owned parameter takes the value (move), a view
            // parameter borrows it. Modules without visible signatures
            // default to borrowing, except `arrays.push`/`pop`, which take
            // their first argument mutably.
            let param_kind = param_kinds
                .as_ref()
                .and_then(|params| params.get(index))
                .copied()
                .unwrap_or(ParamKind::Unknown);
            let arg_kind = self.check_expr(argument);
            if let Expr::Variable { name } = argument {
                let arg_value_kind = self
                    .lookup_kind(name.lexeme())
                    .unwrap_or(ValueKind::Unknown);
                match (arg_value_kind, param_kind) {
                    (ValueKind::Heap, ParamKind::Owned) if self.go_depth > 0 => {
                        self.register_view(name.lexeme(), ViewKind::Shared, name.span);
                    }
                    (ValueKind::Heap, ParamKind::Owned) => {
                        self.move_value(name.lexeme(), name.span, "argument");
                    }
                    (ValueKind::Heap, ParamKind::ViewShared)
                    | (ValueKind::Heap, ParamKind::Unknown) => {
                        let kind = if index == 0 {
                            module_arg_view.unwrap_or(ViewKind::Shared)
                        } else {
                            ViewKind::Shared
                        };
                        self.register_view(name.lexeme(), kind, name.span);
                    }
                    (ValueKind::Heap, ParamKind::ViewMut) => {
                        self.register_view(name.lexeme(), ViewKind::Mut, name.span);
                    }

                    (ValueKind::Shared, ParamKind::ViewShared)
                    | (ValueKind::Shared, ParamKind::ViewMut)
                    | (ValueKind::Shared, ParamKind::Unknown) => {
                        let kind = if index == 0 {
                            module_arg_view.unwrap_or(ViewKind::Shared)
                        } else {
                            ViewKind::Shared
                        };
                        self.register_view(name.lexeme(), kind, name.span);
                    }
                    _ => {
                        let _ = arg_kind;
                    }
                }
            }
        }

        if let Expr::Member { object, property } = callee
            && let Expr::Variable { name } = &**object
            && let Some(boundary) = thread_boundary(name.lexeme(), property.lexeme())
        {
            self.check_thread_boundary(boundary, arguments);
        }

        if is_await {
            if let Expr::Variable { name } = callee
                && let Some((_, return_kind)) = self.local_fns.get(name.lexeme())
            {
                return Some(*return_kind);
            }
            return Some(ValueKind::Unknown);
        }
        if let Expr::Variable { name } = callee
            && let Some((_, return_kind)) = self.local_fns.get(name.lexeme())
        {
            Some(*return_kind)
        } else {
            Some(ValueKind::Unknown)
        }
    }
}

impl Default for OwnershipChecker {
    fn default() -> Self {
        Self::new()
    }
}

/// Whether a statement can never fall through: an explicit control-flow
/// exit, a block ending in one, or an `if`/`match`/`try` whose every
/// branch does. Branches that diverge contribute nothing to a join.
fn diverges(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Return { .. } | Stmt::Throw { .. } | Stmt::Break { .. } | Stmt::Continue { .. } => {
            true
        }

        Stmt::Block { statements, .. } => statements.iter().any(diverges),

        Stmt::If {
            then_branch,
            elif_branches,
            else_branch,
            ..
        } => else_branch.as_ref().is_some_and(|else_branch| {
            diverges(then_branch)
                && elif_branches.iter().all(|elif| diverges(&elif.body))
                && diverges(else_branch)
        }),

        Stmt::Match {
            cases,
            default_case,
            ..
        } => default_case.as_ref().is_some_and(|default| {
            cases.iter().all(|case| diverges(&case.body)) && diverges(default)
        }),

        Stmt::Try {
            try_block,
            catch_block,
            finally_block,
            ..
        } => {
            finally_block.as_ref().is_some_and(|f| diverges(f))
                || (diverges(try_block) && catch_block.as_ref().is_some_and(|c| diverges(c)))
        }
        _ => false,
    }
}

fn union(
    a: &HashMap<String, Vec<usize>>,
    b: &HashMap<String, Vec<usize>>,
) -> HashMap<String, Vec<usize>> {
    let mut result = a.clone();
    for (name, depths) in b {
        let entry = result.entry(name.clone()).or_default();
        for depth in depths {
            if !entry.contains(depth) {
                entry.push(*depth);
            }
        }
    }
    result
}

fn union_borrows(
    a: &[HashMap<String, Vec<BorrowRecord>>],
    b: &[HashMap<String, Vec<BorrowRecord>>],
) -> Vec<HashMap<String, Vec<BorrowRecord>>> {
    let mut result = a.to_vec();
    if result.len() < b.len() {
        result.resize_with(b.len(), HashMap::new);
    }
    for (depth, layer) in b.iter().enumerate() {
        for (holder, records) in layer {
            let entry = result[depth].entry(holder.clone()).or_default();
            for record in records {
                if !entry.contains(record) {
                    entry.push(record.clone());
                }
            }
        }
    }
    result
}

fn kind_of_annotation(annotation: &TypeAnnotation) -> ValueKind {
    match annotation {
        TypeAnnotation::Int | TypeAnnotation::Float | TypeAnnotation::Bool => ValueKind::Scalar,
        TypeAnnotation::String | TypeAnnotation::Pointer | TypeAnnotation::Own(_) => {
            ValueKind::Heap
        }
        TypeAnnotation::Array(_) => ValueKind::Heap,

        // A slice owns its registry entry (so it is reclaimed) while
        // borrowing the array behind it.
        TypeAnnotation::Slice(_) => ValueKind::Heap,
        TypeAnnotation::Object => ValueKind::Heap,
        TypeAnnotation::Named(_) => ValueKind::Heap,
        TypeAnnotation::Option(inner) => kind_of_annotation(inner),
        // A result is always a heap cell, whatever it carries.
        TypeAnnotation::Result { .. } => ValueKind::Heap,
        TypeAnnotation::View(..) => ValueKind::View,
        TypeAnnotation::Shared(_) => ValueKind::Shared,
        TypeAnnotation::Any => ValueKind::Unknown,
        TypeAnnotation::Ref(..) | TypeAnnotation::RawPointer(..) => ValueKind::View,
        // A trait object owns its fat-pointer header and (through it) the
        // wrapped instance.
        TypeAnnotation::Dyn(_) | TypeAnnotation::ImplTrait(_) => ValueKind::Heap,
        // Tuples are stack-allocated value types.
        TypeAnnotation::Tuple(_) => ValueKind::Scalar,
        // A channel handle is reference counted: a copy is a new reference,
        // never a move, so both the spawner and the goroutine can use it.
        TypeAnnotation::Chan(_) => ValueKind::Chan,
    }
}

fn restore_statement_view(
    map: &mut HashMap<String, (usize, Span)>,
    name: &str,
    saved: Option<(usize, Span)>,
) {
    match saved {
        Some(entry) => {
            map.insert(name.to_string(), entry);
        }
        None => {
            map.remove(name);
        }
    }
}

fn root_source(expr: Option<&Expr>) -> Option<&ntsc_ast::token::Token> {
    let expr = expr?;
    match expr {
        Expr::Variable { name } => Some(name),
        Expr::Member { object, .. }
        | Expr::OptionalMember { object, .. }
        | Expr::IndexGet { object, .. } => root_source(Some(object)),
        Expr::Grouping { expression, .. } => root_source(Some(expression)),
        _ => None,
    }
}

fn thread_unsafe_reason(kind: ValueKind, heap: HeapPolicy) -> Option<&'static str> {
    match kind {
        ValueKind::Scalar | ValueKind::Function | ValueKind::Unknown => None,
        // A channel handle is exactly what is meant to cross a thread
        // boundary: its reference count is synchronized.
        ValueKind::Chan => None,
        ValueKind::Heap => match heap {
            HeapPolicy::Copies => None,
            HeapPolicy::Rejects(reason) => Some(reason),
        },
        ValueKind::Shared => Some(
            "`shared` values are reference-counted without synchronization, so two threads \
             holding copies would race on both the value and its count; send the data \
             through a channel instead",
        ),
        ValueKind::View => Some(
            "views cannot cross threads; a borrow lives only as long as the borrowing scope, \
             which does not have to outlive the thread that receives it",
        ),
    }
}

/// The runtime calls that hand an argument to another thread.
///
/// `spawn_thread` returns before the new thread runs, so the payload
/// must be copy-safe; `channel_send` copies synchronously and can take
/// an owned value.
fn thread_boundary(module: &str, prop: &str) -> Option<ThreadBoundary> {
    match (module, prop) {
        ("process", "spawn_thread") => Some(ThreadBoundary {
            call: "process.spawn_thread",

            payloads: &[1],

            heap: HeapPolicy::Rejects(
                "an owned heap value would cross as a raw handle that both threads then alias \
                 without synchronization, and the caller's scope exit would free it while the \
                 thread is still using it; pass a channel handle and send the data with \
                 collections.channel_send",
            ),
        }),
        ("collections", "channel_send") => Some(ThreadBoundary {
            call: "collections.channel_send",
            payloads: &[1],

            heap: HeapPolicy::Copies,
        }),
        _ => None,
    }
}

/// How an `arrays` module call treats its array argument: the in-place
/// mutators take it as an exclusive borrow, the constructors take no
/// array, and every other call only reads it.
///
/// `clear` belongs with the mutators: it empties the array in place.
/// Classifying it as a shared (read-only) borrow left the slot borrowed,
/// so a later mutation through the same variable silently operated on a
/// copy and the caller's array never changed.
fn module_array_arg_view_kind(module: &str, prop: &str) -> Option<ViewKind> {
    if module != "arrays" {
        return None;
    }
    match prop {
        "push" | "pop" | "clear" => Some(ViewKind::Mut),
        "new" | "range" | "fill" => None,
        _ => Some(ViewKind::Shared),
    }
}

/// All names the statement reads or writes, transitively. Used to decide
/// whether a declared borrow outlives its holder: NLL-style, the
/// borrow is live while the holder appears in any statement from here
/// to the end of the enclosing block.
fn deep_stmt_uses(stmt: &Stmt) -> HashSet<String> {
    let mut uses = HashSet::new();
    collect_stmt_uses(stmt, &mut uses);
    uses
}

pub(crate) fn collect_stmt_uses(stmt: &Stmt, uses: &mut HashSet<String>) {
    match stmt {
        Stmt::Var { initializer, .. } => {
            if let Some(init) = initializer {
                collect_expr_uses(init, uses);
            }
        }
        Stmt::Expression { expression } => collect_expr_uses(expression, uses),
        Stmt::Say { expression, .. } => collect_expr_uses(expression, uses),
        Stmt::Block { statements, .. } => {
            for inner in statements {
                collect_stmt_uses(inner, uses);
            }
        }
        Stmt::If {
            condition,
            then_branch,
            elif_branches,
            else_branch,
        } => {
            collect_expr_uses(condition, uses);
            collect_stmt_uses(then_branch, uses);
            for elif in elif_branches {
                collect_expr_uses(&elif.condition, uses);
                collect_stmt_uses(&elif.body, uses);
            }
            if let Some(else_branch) = else_branch {
                collect_stmt_uses(else_branch, uses);
            }
        }
        Stmt::While { condition, body } | Stmt::DoWhile { condition, body } => {
            collect_expr_uses(condition, uses);
            collect_stmt_uses(body, uses);
        }
        Stmt::For {
            init,
            condition,
            update,
            body,
        } => {
            if let Some(init) = init {
                collect_stmt_uses(init, uses);
            }
            if let Some(condition) = condition {
                collect_expr_uses(condition, uses);
            }
            if let Some(update) = update {
                collect_expr_uses(update, uses);
            }
            collect_stmt_uses(body, uses);
        }
        Stmt::ForIn { iterable, body, .. } => {
            collect_expr_uses(iterable, uses);
            collect_stmt_uses(body, uses);
        }
        Stmt::ForAwait { producer, body, .. } => {
            collect_expr_uses(producer, uses);
            collect_stmt_uses(body, uses);
        }
        Stmt::ChanRecvFor { channel, body, .. } => {
            collect_expr_uses(channel, uses);
            collect_stmt_uses(body, uses);
        }
        Stmt::Go { call, block, .. } => {
            collect_expr_uses(call, uses);
            if let Some(block) = block {
                for stmt in block {
                    collect_stmt_uses(stmt, uses);
                }
            }
        }
        Stmt::Return { value } => {
            if let Some(value) = value {
                collect_expr_uses(value, uses);
            }
        }
        Stmt::Throw { value } => collect_expr_uses(value, uses),
        Stmt::Match {
            expression,
            cases,
            default_case,
        } => {
            collect_expr_uses(expression, uses);
            for case in cases {
                collect_expr_uses(&case.value, uses);
                if let Some(guard) = &case.guard {
                    collect_expr_uses(guard, uses);
                }
                collect_stmt_uses(&case.body, uses);
            }
            if let Some(default) = default_case {
                collect_stmt_uses(default, uses);
            }
        }
        Stmt::Try {
            try_block,
            catch_block,
            finally_block,
            ..
        } => {
            collect_stmt_uses(try_block, uses);
            if let Some(catch) = catch_block {
                collect_stmt_uses(catch, uses);
            }
            if let Some(finally) = finally_block {
                collect_stmt_uses(finally, uses);
            }
        }
        Stmt::Retry {
            count,
            body,
            catch_block,
            ..
        } => {
            collect_expr_uses(count, uses);
            collect_stmt_uses(body, uses);
            if let Some(catch) = catch_block {
                collect_stmt_uses(catch, uses);
            }
        }
        Stmt::Destructure { initializer, .. } => collect_expr_uses(initializer, uses),
        Stmt::Unsafe { body } | Stmt::Quiet { body, .. } => collect_stmt_uses(body, uses),
        Stmt::Function { .. }
        | Stmt::AsyncFunction { .. }
        | Stmt::ExternFunction { .. }
        | Stmt::ExternBlock { .. }
        | Stmt::Test { .. }
        | Stmt::Class { .. }
        | Stmt::Enum { .. }
        | Stmt::TypeAlias { .. }
        | Stmt::Trait { .. }
        | Stmt::Impl { .. }
        | Stmt::Break { .. }
        | Stmt::Continue { .. }
        | Stmt::Use { .. } => {}
    }
}

pub(crate) fn collect_expr_uses(expr: &Expr, uses: &mut HashSet<String>) {
    match expr {
        Expr::Variable { name } => {
            uses.insert(name.lexeme().to_string());
        }
        Expr::Literal { .. } | Expr::This { .. } => {}
        Expr::Binary { left, right, .. } => {
            collect_expr_uses(left, uses);
            collect_expr_uses(right, uses);
        }
        Expr::Unary { right, .. } => collect_expr_uses(right, uses),
        Expr::PostfixUnary { left, .. } => collect_expr_uses(left, uses),
        Expr::Grouping { expression, .. } => collect_expr_uses(expression, uses),
        Expr::Member { object, .. } | Expr::OptionalMember { object, .. } => {
            collect_expr_uses(object, uses);
        }
        Expr::Call {
            callee, arguments, ..
        }
        | Expr::Await {
            callee, arguments, ..
        } => {
            collect_expr_uses(callee, uses);
            for argument in arguments {
                collect_expr_uses(argument, uses);
            }
        }
        Expr::AsyncBlock { body, .. } => {
            for stmt in body {
                collect_stmt_uses(stmt, uses);
            }
        }
        Expr::ChanSend { channel, value, .. } => {
            collect_expr_uses(channel, uses);
            collect_expr_uses(value, uses);
        }
        Expr::ChanRecv {
            receiver, channel, ..
        } => {
            collect_expr_uses(channel, uses);
            uses.insert(receiver.lexeme().to_string());
        }
        Expr::Close { channel, .. } => collect_expr_uses(channel, uses),
        Expr::Assign { name, value } => {
            uses.insert(name.lexeme().to_string());
            collect_expr_uses(value, uses);
        }
        Expr::IndexGet { object, index } => {
            collect_expr_uses(object, uses);
            collect_expr_uses(index, uses);
        }
        Expr::IndexSet {
            object,
            index,
            value,
        } => {
            collect_expr_uses(object, uses);
            collect_expr_uses(index, uses);
            collect_expr_uses(value, uses);
        }
        Expr::MemberSet { object, value, .. } => {
            collect_expr_uses(object, uses);
            collect_expr_uses(value, uses);
        }
        Expr::Lambda { .. } => {}
        Expr::Ternary {
            condition,
            then_branch,
            else_branch,
        } => {
            collect_expr_uses(condition, uses);
            collect_expr_uses(then_branch, uses);
            collect_expr_uses(else_branch, uses);
        }
        Expr::Spread { value, .. } => collect_expr_uses(value, uses),
        Expr::ObjectLiteral { properties, .. } => {
            for property in properties {
                collect_expr_uses(&property.value, uses);
            }
        }
        Expr::ArrayLiteral { elements, .. } => {
            for element in elements {
                collect_expr_uses(element, uses);
            }
        }
        Expr::View { target, .. } => collect_expr_uses(target, uses),
        Expr::Copy { expression, .. } => collect_expr_uses(expression, uses),
        Expr::Borrow { target, .. } | Expr::RawDeref { target, .. } => {
            collect_expr_uses(target, uses)
        }
        Expr::RawDerefSet { target, value, .. } => {
            collect_expr_uses(target, uses);
            collect_expr_uses(value, uses);
        }
        Expr::StructLiteral {
            class_name,
            fields,
            update,
            ..
        } => {
            uses.insert(class_name.lexeme().to_string());
            for field in fields {
                collect_expr_uses(&field.value, uses);
            }
            if let Some(update) = update {
                collect_expr_uses(update, uses);
            }
        }
        Expr::Propagate { value, .. } => collect_expr_uses(value, uses),
        Expr::TupleLiteral { elements, .. } => {
            for element in elements {
                collect_expr_uses(element, uses);
            }
        }
        Expr::TupleIndex { object, .. } => collect_expr_uses(object, uses),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ntsc_lexer::tokenize;
    use ntsc_parser::parse;

    fn ownership_errors(source: &str) -> Vec<String> {
        let tokens = tokenize(source);
        let program = match parse(&tokens) {
            Ok(program) => program,
            Err(_) => return vec!["parse error".into()],
        };
        let mut checker = OwnershipChecker::new();
        checker.check_program(&program);
        checker
            .errors
            .into_iter()
            .map(|error| error.message)
            .collect()
    }

    fn assert_clean(source: &str) {
        let errors = ownership_errors(source);
        assert!(
            errors.is_empty(),
            "expected no ownership errors, got: {errors:?}"
        );
    }

    fn assert_error(source: &str, needle: &str) {
        let errors = ownership_errors(source);
        assert!(
            errors.iter().any(|error| error.contains(needle)),
            "expected an error containing `{needle}`, got: {errors:?}"
        );
    }

    // ── Reference borrows (`&T` / `&mut T`) ──────────────────────────────

    #[test]
    fn exclusive_reference_conflicts_with_a_live_shared_reference() {
        assert_error(
            "fun main() { var xs = [1, 2]\n var &array[int] r = &xs\n var &mut array[int] w = &mut xs\n say(r[0]) }",
            "xs",
        );
    }

    #[test]
    fn two_exclusive_references_conflict() {
        assert_error(
            "fun main() { var xs = [1, 2]\n var &mut array[int] a = &mut xs\n var &mut array[int] b = &mut xs\n say(a[0] + b[0]) }",
            "xs",
        );
    }

    #[test]
    fn moving_a_referenced_owner_is_rejected() {
        assert_error(
            "fun main() { var xs = [1, 2]\n var &array[int] r = &xs\n var moved = xs\n say(r[0]) }",
            "xs",
        );
    }

    #[test]
    fn assigning_a_mutably_referenced_owner_is_rejected() {
        assert_error(
            "fun main() { var xs = [1, 2]\n var &mut array[int] w = &mut xs\n xs = [3]\n say(w[0]) }",
            "xs",
        );
    }

    #[test]
    fn a_reference_may_not_borrow_a_temporary() {
        assert_error(
            "fun main() { var &array[int] r = &[1, 2]\n say(r[0]) }",
            "temporary",
        );
    }

    #[test]
    fn sequential_references_are_accepted_under_non_lexical_lifetimes() {
        // The shared borrow is dead after its final use, so the exclusive
        // borrow that follows does not conflict.
        assert_clean(
            "fun main() { var xs = [1, 2]\n var &array[int] r = &xs\n say(r[0])\n var &mut array[int] w = &mut xs\n say(w[0]) }",
        );
    }

    #[test]
    fn a_reference_cannot_cross_a_thread_boundary() {
        assert_error(
            "fun main() { var xs = [1, 2]\n var &array[int] r = &xs\n var t = process.spawn_thread(fun(int x) { say(x) }, r) }",
            "cannot cross",
        );
    }

    #[test]
    fn an_owning_allocation_cannot_cross_a_thread_boundary() {
        assert_error(
            "fun main() { var own int boxed = alloc(1)\n var t = process.spawn_thread(fun(int x) { say(x) }, boxed) }",
            "spawn_thread",
        );
    }

    #[test]
    fn assignment_moves_owned_heap_value() {
        assert_error(
            "fun main() { var a = [1, 2]; var b = a; say(a[0]) }",
            "moved",
        );
    }

    #[test]
    fn view_var_declaration_borrows_source_for_scope() {
        assert_clean(
            "fun main() {\n    var xs = [1, 2, 3];\n    view var m = xs;\n    say(\"\" + m[0])\n}\n",
        );
        assert_error(
            "fun main() {\n    var a = [1, 2];\n    view var r = a;\n    var b = a;\n    say(\"\" + r[0])\n}\n",
            "cannot move `a` while it is viewed",
        );
        assert_error(
            "fun main() {\n    var xs = [1, 2];\n    view mut var m = xs;\n    say(\"\" + xs[0])\n    say(\"\" + m[0])\n}\n",
            "already viewed",
        );
        assert_error(
            "fun main() {\n    var xs = [1, 2];\n    view var r = xs;\n    view mut var m = xs;\n    say(\"\" + r[0])\n}\n",
            "already viewed",
        );
    }

    #[test]
    fn view_var_rejects_temporary_source() {
        assert_error(
            "fun main() {\n    view var r = [1, 2];\n    say(\"x\")\n}\n",
            "temporary value",
        );
    }

    #[test]
    fn view_var_scoped_to_block() {
        assert_clean(
            "fun main() {\n    var xs = [1, 2];\n    {\n        view mut var m = xs;\n        m[0] = 9;\n    }\n    say(\"\" + xs[0])\n}\n",
        );
    }

    // ── Non-lexical lifetimes ────────────────────────────────────────────

    #[test]
    fn borrow_ends_after_final_use() {
        assert_clean(
            "fun main() {\n    var a = [1, 2];\n    view var r = a;\n    var b = a;\n    say(\"\" + b[0])\n}\n",
        );
    }

    #[test]
    fn borrow_survives_until_last_use() {
        assert_error(
            "fun main() {\n    var a = [1, 2];\n    view var r = a;\n    var b = a;\n    say(\"\" + r[0])\n}\n",
            "cannot move `a` while it is viewed",
        );
    }

    #[test]
    fn last_use_inside_a_loop_ends_the_borrow_after_the_loop() {
        assert_clean(
            "fun main() {\n    var a = [1, 2];\n    view var r = a;\n    while (true) { say(\"\" + r[0]); break }\n    var b = a;\n    say(\"\" + b[0])\n}\n",
        );
    }

    #[test]
    fn move_before_later_use_in_the_same_loop_conflicts() {
        assert_error(
            "fun main() {\n    var a = [1, 2];\n    view var r = a;\n    while (true) { var b = a; say(\"\" + r[0]); break }\n    say(\"x\")\n}\n",
            "cannot move `a` while it is viewed",
        );
    }

    #[test]
    fn use_in_a_branch_after_the_move_keeps_the_borrow_live() {
        assert_error(
            "fun main() {\n    var a = [1, 2];\n    view var r = a;\n    var b = a;\n    if (b[0] > 0) { say(\"\" + r[0]) }\n}\n",
            "cannot move `a` while it is viewed",
        );
    }

    #[test]
    fn reassigning_a_view_holder_ends_its_borrow() {
        assert_clean(
            "fun main() {\n    var a = [1, 2];\n    var c = [3, 4];\n    view var r = a;\n    r = view c;\n    var b = a;\n    say(\"\" + b[0] + r[0])\n}\n",
        );
    }

    #[test]
    fn move_error_names_the_borrowing_view() {
        let errors = ownership_errors(
            "fun main() {\n    var a = [1, 2];\n    view var r = a;\n    var b = a;\n    say(\"\" + r[0])\n}\n",
        );
        assert!(
            errors.iter().any(|error| error.contains("borrowed by `r`")),
            "expected the diagnostic to name the borrowing view, got: {errors:?}"
        );
    }

    #[test]
    fn move_while_viewed_by_statement_view_reports_origin() {
        let errors = ownership_errors(
            "fun main() {\n    var a = [1, 2];\n    var b = arrays.length(a);\n    say(arrays.length(a))\n}\n",
        );

        assert!(errors.is_empty(), "got: {errors:?}");
    }

    // ── Moves ────────────────────────────────────────────────────────────

    #[test]
    fn passing_to_owned_param_moves_source() {
        assert_error(
            "fun consume(array[int] xs) -> int { return arrays.length(xs) }
             fun main() { var a = [1, 2]; say(consume(a)); say(a[0]) }",
            "moved",
        );
    }

    #[test]
    fn view_param_does_not_move_source() {
        assert_clean(
            "fun peek(view array[int] xs) -> int { return arrays.length(xs) }
             fun main() { var a = [1, 2]; say(peek(a)); say(arrays.length(a)) }",
        );
    }

    #[test]
    fn copy_leaves_source_usable() {
        assert_clean(
            "fun main() { var a = [1, 2]; var b = copy(a); say(arrays.length(a) + arrays.length(b)) }",
        );
    }

    #[test]
    fn scalar_assignment_is_not_a_move() {
        assert_clean("fun main() { var n = 42; var m = n; say(n + m) }");
    }

    #[test]
    fn view_cannot_wrap_scalar() {
        assert_clean("fun f(view int n) -> int { return n }");
    }

    #[test]
    fn call_argument_view_dies_when_call_returns() {
        assert_clean(
            "fun main() { var nums = [3, 1, 2]; nums = arrays.sort(nums, 0); say(nums[0]) }",
        );
    }

    #[test]
    fn functional_arrays_ops_take_a_shared_view() {
        assert_clean(
            "fun main() { var nums = [3, 1, 2]; var s = arrays.sort(nums); say(arrays.length(nums) + arrays.length(s)) }",
        );
        assert_clean(
            "fun main() { var nums = [3, 1, 2]; var s = arrays.slice(nums, 0, 1); say(arrays.length(nums)) }",
        );
    }

    #[test]
    fn in_place_arrays_ops_take_an_exclusive_view() {
        assert_clean("fun main() { var a = [1, 2]; arrays.push(a, 3); say(arrays.length(a)) }");
        assert_error(
            "fun main() { var a = [1, 2]; view var r = a; arrays.push(a, 3); say(\"\" + r[0]) }",
            "already viewed",
        );
        assert_error(
            "fun main() { var a = [1, 2]; view mut var m = a; arrays.push(a, 3); say(\"\" + m[0]) }",
            "already viewed",
        );
    }

    #[test]
    fn views_cannot_cross_threads() {
        assert_clean("fun worker(int ch) { } fun main() { process.spawn_thread(worker, 0) }");
        assert_error(
            "fun worker(int ch) { } fun main() { var xs = [1, 2]; view var r = xs; process.spawn_thread(worker, r) }",
            "views cannot cross threads",
        );
    }

    #[test]
    fn owned_heap_values_cannot_be_handed_to_a_thread() {
        assert_error(
            "fun worker(int ch) { }
             fun main() {
                 var xs = [1, 2]
                 process.spawn_thread(worker, xs)
             }",
            "cannot pass `xs` to process.spawn_thread",
        );
        assert_error(
            "fun worker(int ch) { }
             fun main() {
                 var s = \"payload\"
                 process.spawn_thread(worker, s)
             }",
            "collections.channel_send",
        );

        assert_error(
            "fun worker(int ch) { }
             fun main() {
                 var xss = [[1], [2]]
                 process.spawn_thread(worker, xss[0])
             }",
            "cannot pass `xss` to process.spawn_thread",
        );
    }

    #[test]
    fn shared_values_cannot_cross_threads() {
        assert_error(
            "fun worker(int ch) { }
             fun main() {
                 shared array[int] s = [1, 2];
                 process.spawn_thread(worker, s)
             }",
            "reference-counted without synchronization",
        );
        assert_error(
            "fun main() {
                 var rx = collections.channel(4)
                 var tx = collections.channel_sender(rx)
                 shared array[int] s = [1, 2];
                 collections.channel_send(tx, s)
             }",
            "reference-counted without synchronization",
        );
    }

    #[test]
    fn channel_send_copies_its_message() {
        assert_clean(
            "fun main() {
                 var rx = collections.channel(4)
                 var tx = collections.channel_sender(rx)
                 var msg = \"ping\"
                 collections.channel_send(tx, msg)
                 say(msg)
             }",
        );

        assert_error(
            "fun main() {
                 var rx = collections.channel(4)
                 var tx = collections.channel_sender(rx)
                 var xs = [1, 2]
                 view var r = xs
                 collections.channel_send(tx, r)
             }",
            "views cannot cross threads",
        );
    }

    #[test]
    fn scalars_and_handles_cross_threads_freely() {
        assert_clean(
            "fun worker(int ch) { }
             fun main() {
                 var rx = collections.channel(4)
                 var tx = collections.channel_sender(rx)
                 var t = process.spawn_thread(worker, tx)
                 process.thread_join(t)
                 collections.channel_close(rx)
             }",
        );
    }

    #[test]
    fn awaiting_is_not_a_thread_boundary() {
        assert_clean(
            "async fun step(int n) -> int { await async.sleep(1) return n }
             async fun main() -> int {
                 var s = await step(1)
                 say(\"\" + s)
                 return 0
             }",
        );
    }

    #[test]
    fn cannot_move_while_viewed() {
        assert_error(
            "fun consume(array[int] xs) -> int { return arrays.length(xs) }
             fun main() { var a = [1, 2]; say(arrays.length(a)); var b = consume(a); say(arrays.length(a)) }",
            "moved",
        );
    }

    #[test]
    fn this_view_cannot_be_stored() {
        assert_error(
            "class Box {
                 fun grab() { var x = this }
             }",
            "cannot store a view",
        );
    }

    #[test]
    fn shared_values_are_never_moved() {
        assert_clean(
            "fun main() {
                 shared array[int] a = [1, 2];
                 shared array[int] b = a;
                 say(arrays.length(a) + arrays.length(b))
             }",
        );
        assert_clean(
            "fun bump(shared array[int] xs) -> int { return arrays.length(xs) }
             fun main() {
                 shared array[int] a = [1, 2];
                 say(bump(a));
                 say(arrays.length(a))
             }",
        );
        assert_clean("fun main() { shared array[int] a = [1, 2]; a = a; say(\"x\") }");
    }

    #[test]
    fn views_can_borrow_shared_pointees() {
        assert_clean(
            "fun peek(view array[int] xs) -> int { return arrays.length(xs) }
             fun main() {
                 shared array[int] s = [1, 2];
                 view var r = s;
                 say(peek(s));
                 say(arrays.length(s))
             }",
        );
    }

    #[test]
    fn owned_source_moves_into_shared_slot() {
        assert_error(
            "fun main() {
                 var a = [1, 2];
                 shared array[int] s = a;
                 say(a[0])
             }",
            "moved",
        );
    }

    #[test]
    fn reassigning_a_shared_source_while_borrowed_is_rejected() {
        assert_error(
            "fun main() {
                 shared array[int] s = [1, 2];
                 view var r = s;
                 s = copy(s);
                 say(\"\" + r[0])
             }",
            "cannot assign to `s` while it is viewed",
        );
    }

    #[test]
    fn view_cannot_escape_an_inner_owner_scope() {
        assert_error(
            "fun main() {
                 var outer = [0];
                 view var r = outer;
                 { var inner = [1, 2]; r = view inner }
                 say(\"\" + r[0])
             }",
            "view `r` cannot borrow `inner`",
        );
    }

    #[test]
    fn inner_view_may_borrow_an_outer_owner() {
        assert_clean(
            "fun main() {
                 var outer = [1, 2];
                 { view var r = outer; say(\"\" + r[0]) }
                 var moved = outer;
                 say(\"\" + moved[0])
             }",
        );
    }

    #[test]
    fn reassigned_view_keeps_its_new_owner_borrowed() {
        assert_error(
            "fun main() {
                 var first = [1]; var second = [2];
                 view var r = first;
                 r = view second;
                 var moved = second;
                 say(\"\" + r[0])
             }",
            "cannot move `second` while it is viewed",
        );
    }

    #[test]
    fn branch_join_keeps_every_possible_view_owner_borrowed() {
        assert_error(
            "fun main() {
                 var first = [1]; var second = [2];
                 view var r = first;
                 if (true) { r = view first } else { r = view second }
                 var moved = second;
                 say(\"\" + r[0])
             }",
            "cannot move `second` while it is viewed",
        );
    }

    #[test]
    fn double_move_is_use_after_move() {
        assert_error(
            "fun main() { var a = [1, 2]; var b = a; var c = a; say(\"x\") }",
            "use of moved value",
        );
    }

    // ── Destructuring ────────────────────────────────────────────────────

    #[test]
    fn destructuring_moves_an_owned_source() {
        assert_error(
            "fun main() { var xs = [1, 2]; var [a, b] = xs; say(\"\" + xs[0] + a + b) }",
            "moved",
        );

        assert_clean(
            "fun main() { var xs = [1, 2]; var [a, b] = xs; xs = [3, 4]; say(\"\" + xs[0] + a + b) }",
        );
    }

    // ── Path-sensitive moves and reinitialization ────────────────────────

    #[test]
    fn shadowed_name_is_fresh_after_outer_move() {
        assert_clean(
            "fun main() { var a = [1, 2]; var b = a; { var a = [3]; say(\"\" + a[0]) } say(\"x\") }",
        );
    }

    #[test]
    fn assigning_to_a_shadow_does_not_revive_outer_moved() {
        assert_error(
            "fun main() { var a = [1, 2]; var b = a; { var a = [3]; a = [4]; say(\"\" + a[0]) } say(\"\" + a[0]) }",
            "moved",
        );
    }

    #[test]
    fn reinit_by_assignment_then_use_is_allowed() {
        assert_clean("fun main() { var a = [1, 2]; var b = a; a = [3]; say(\"\" + a[0]) }");
    }

    #[test]
    fn move_then_reinit_then_move_again_is_a_use_after_move() {
        assert_error(
            "fun main() { var a = [1, 2]; var b = a; a = [3]; var c = a; say(\"\" + a[0]) }",
            "moved",
        );
    }

    #[test]
    fn use_after_conditional_move_on_some_path_is_rejected() {
        assert_error(
            "fun main() { var a = [1, 2]; if (true) { var b = a } say(\"\" + a[0]) }",
            "moved",
        );
    }

    #[test]
    fn reinit_on_every_branch_allows_the_use_afterwards() {
        assert_clean(
            "fun main() { var a = [1, 2]; var b = a; if (true) { a = [3] } else { a = [4] } say(\"\" + a[0]) }",
        );
    }

    #[test]
    fn reinit_on_only_one_branch_stays_dead() {
        assert_error(
            "fun main() { var a = [1, 2]; var b = a; if (true) { a = [3] } say(\"\" + a[0]) }",
            "moved",
        );
    }

    #[test]
    fn loop_body_move_makes_the_source_dead_after_the_loop() {
        assert_error(
            "fun main() { var a = [1, 2]; while (true) { var b = a; break } say(\"\" + a[0]) }",
            "moved",
        );
    }

    #[test]
    fn loop_body_reinit_keeps_the_source_alive_after_the_loop() {
        assert_clean(
            "fun main() { var a = [1, 2]; while (true) { var b = a; a = [3]; break } say(\"\" + a[0]) }",
        );
    }

    #[test]
    fn loop_carried_move_is_reported_in_every_loop_form() {
        for source in [
            "fun main() { var a = [1, 2]; for (var i = 0; i < 3; i = i + 1) { var b = a; say(\"\" + b[0]) } }",
            "fun main() { var a = [1, 2]; var n = 0; while (n < 3) { var b = a; n = n + 1; say(\"\" + b[0]) } }",
            "fun main() { var a = [1, 2]; var n = 0; do { var b = a; n = n + 1; say(\"\" + b[0]) } while (n < 3) }",
            "fun main() { var a = [1, 2]; var ys = [7, 8]; for (var y in ys) { var b = a; say(\"\" + b[0]) } }",
            "fun main() { var a = [1, 2]; retry 3 { var b = a; say(\"\" + b[0]) } }",
        ] {
            assert_error(source, "use of moved value: `a`");
        }
    }

    #[test]
    fn loop_carried_move_through_the_condition_is_reported() {
        assert_error(
            "fun main() { var a = [1, 2]; var n = 0; while (n < arrays.length(a)) { var b = a; n = n + 1; say(\"\" + b[0]) } }",
            "use of moved value: `a`",
        );
    }

    #[test]
    fn loop_body_move_of_a_body_local_is_clean() {
        assert_clean(
            "fun main() { for (var i = 0; i < 3; i = i + 1) { var a = [1, 2]; var b = a; say(\"\" + b[0]) } }",
        );
    }

    #[test]
    fn loop_carried_move_reinitialized_before_the_next_use_is_clean() {
        assert_clean(
            "fun main() { var a = [1, 2]; var n = 0; while (n < 3) { a = [3, 4]; var b = a; n = n + 1; say(\"\" + b[0]) } }",
        );
    }

    #[test]
    fn a_loop_body_reports_each_error_once() {
        let errors = ownership_errors(
            "fun main() { var a = [1, 2]; for (var i = 0; i < 3; i = i + 1) { var b = a; say(\"\" + b[0]) } }",
        );
        let moves = errors
            .iter()
            .filter(|error| error.contains("use of moved value: `a`"))
            .count();
        assert_eq!(moves, 1, "expected one diagnostic, got: {errors:?}");
    }

    #[test]
    fn nested_for_in_keeps_borrowing_the_outer_container() {
        assert_error(
            "fun main() { var xs = [1, 2]; for (var x in xs) { for (var y in [3, 4]) { say(\"\" + y) } var b = xs; say(\"\" + b[0]) } }",
            "cannot move `xs` while it is viewed",
        );
    }

    #[test]
    fn storing_a_heap_element_of_an_annotated_array_is_rejected() {
        assert_error(
            "fun main() { var array[string] names = [\"ada\"]; var s = names[0]; say(s) }",
            "cannot store a borrowed element in `s`",
        );
    }

    #[test]
    fn storing_a_heap_element_of_an_inferred_array_is_rejected() {
        assert_error(
            "fun main() { var outer = [[1], [2]]; var inner = outer[0]; say(\"\" + inner[0]) }",
            "cannot store a borrowed element in `inner`",
        );
    }

    #[test]
    fn storing_a_scalar_element_is_a_copy() {
        assert_clean("fun main() { var xs = [1, 2]; var n = xs[0]; say(\"\" + n) }");
    }

    #[test]
    fn copying_a_heap_element_is_clean() {
        assert_clean(
            "fun main() { var array[string] names = [\"ada\"]; var s = copy(names[0]); say(s) }",
        );
    }

    #[test]
    fn viewing_a_heap_element_is_clean() {
        assert_clean(
            "fun main() { var array[string] names = [\"ada\"]; view var s = names[0]; say(s) }",
        );
    }

    #[test]
    fn reading_a_heap_element_without_storing_it_is_clean() {
        assert_clean("fun main() { var array[string] names = [\"ada\"]; say(names[0]) }");
    }

    #[test]
    fn storing_a_heap_field_is_rejected() {
        assert_error(
            "class Bag { var array items } fun main() { var b = Bag(); var taken = b.items; say(\"\" + taken[0]) }",
            "cannot store a borrowed element in `taken`",
        );
    }

    #[test]
    fn storing_a_scalar_field_is_a_copy() {
        assert_clean(
            "class P { var int n } fun main() { var p = P(); var n = p.n; say(\"\" + n) }",
        );
    }

    #[test]
    fn storing_an_inherited_heap_field_is_rejected() {
        assert_error(
            "class Bag { var array items } class Sack extends Bag { var int n } fun main() { var s = Sack(); var taken = s.items; say(\"\" + taken[0]) }",
            "cannot store a borrowed element in `taken`",
        );
    }

    #[test]
    fn copying_a_heap_field_is_clean() {
        assert_clean(
            "class Bag { var array items } fun main() { var b = Bag(); var taken = copy(b.items); say(\"\" + taken[0]) }",
        );
    }

    #[test]
    fn an_annotated_class_variable_classifies_its_field_reads() {
        assert_error(
            "class Bag { var array items } fun make() -> Bag { return Bag() } fun main() { var Bag b = make(); var taken = b.items; say(\"\" + taken[0]) }",
            "cannot store a borrowed element in `taken`",
        );
    }

    #[test]
    fn a_field_of_an_unknown_object_stays_permissive() {
        assert_clean(
            "fun main() { var o = { \"items\": [1] }; var taken = o.items; say(\"\" + taken[0]) }",
        );
    }

    #[test]
    fn a_move_on_a_returning_branch_does_not_reach_the_join() {
        assert_clean(
            "fun f(int n) -> int { var xs = [1, 2]; if (n > 0) { var a = xs; return 1 } return arrays.length(xs) }",
        );
    }

    #[test]
    fn a_move_on_a_throwing_branch_does_not_reach_the_join() {
        assert_clean(
            "fun f(int n) -> int { var xs = [1, 2]; if (n > 0) { var a = xs; throw \"no\" } return arrays.length(xs) }",
        );
    }

    #[test]
    fn a_move_on_a_breaking_branch_does_not_reach_the_join() {
        assert_clean(
            "fun main() { var xs = [1, 2]; while (true) { if (true) { var a = xs; break } say(\"\" + arrays.length(xs)) } }",
        );
    }

    #[test]
    fn a_move_on_a_continuing_branch_does_not_reach_the_join() {
        assert_clean(
            "fun f() -> int { var xs = [1, 2]; var n = 0; while (n < 1) { n = n + 1; if (n > 5) { var a = xs; continue } } return arrays.length(xs) }",
        );
    }

    #[test]
    fn a_move_on_a_falling_through_branch_still_reaches_the_join() {
        assert_error(
            "fun f(int n) -> int { var xs = [1, 2]; if (n > 0) { var a = xs } return arrays.length(xs) }",
            "use of moved value: `xs`",
        );
    }

    #[test]
    fn a_move_before_the_return_in_the_same_branch_is_still_checked() {
        assert_error(
            "fun f() -> int { var xs = [1, 2]; if (true) { var a = xs; return arrays.length(xs) } return 0 }",
            "use of moved value: `xs`",
        );
    }

    #[test]
    fn a_move_in_a_returning_match_case_does_not_reach_the_join() {
        assert_clean(
            "fun f(int n) -> int { var xs = [1, 2]; match (n) { case 1 => { var a = xs; return 1 } default => { } } return arrays.length(xs) }",
        );
    }

    #[test]
    fn a_move_in_a_falling_through_match_case_still_reaches_the_join() {
        assert_error(
            "fun f(int n) -> int { var xs = [1, 2]; match (n) { case 1 => { var a = xs } default => { } } return arrays.length(xs) }",
            "use of moved value: `xs`",
        );
    }

    #[test]
    fn a_move_in_a_nested_returning_block_does_not_reach_the_join() {
        assert_clean(
            "fun f(int n) -> int { var xs = [1, 2]; if (n > 0) { { var a = xs; return 1 } } return arrays.length(xs) }",
        );
    }

    #[test]
    fn an_if_whose_every_arm_returns_leaves_the_join_unreachable() {
        assert_clean(
            "fun f(int n) -> int { var xs = [1, 2]; if (n > 0) { var a = xs; return 1 } else { var b = xs; return 2 } }",
        );
    }

    #[test]
    fn match_merge_keeps_dead_names_dead() {
        assert_error(
            "fun main() { var a = [1, 2]; match (1) { case 1 => { var b = a } default => { var c = a } } say(\"\" + a[0]) }",
            "moved",
        );
    }

    #[test]
    fn shadowed_name_scoped_moves_prune_on_block_exit() {
        assert_clean(
            "fun main() { { var a = [1, 2]; var b = a } { var a = [3]; say(\"\" + a[0]) } }",
        );
    }

    #[test]
    fn field_write_may_read_the_same_instance() {
        assert_clean(
            "class Counter { var int n  fun init() { this.n = 0 } } fun main() { var Counter c = Counter(); c.n = c.n + 1; say(\"\" + c.n) }",
        );
        assert_clean(
            "class N { var string name  fun init() { this.name = \"a\" } } fun main() { var N x = N(); x.name = x.name; say(x.name) }",
        );
    }

    #[test]
    fn element_write_may_read_the_same_container() {
        assert_clean("fun main() { var xs = [1, 2, 3]; xs[0] = xs[1] + 1; say(\"\" + xs[0]) }");

        assert_clean("fun main() { var xs = [2, 0, 7]; xs[xs[1]] = 9; say(\"\" + xs[0]) }");
    }

    #[test]
    fn field_write_conflicts_with_a_declared_borrow_of_the_field() {
        assert_error(
            "class Bag { var array[int] items  fun init() { this.items = [1, 2] } } fun main() { var b = Bag(); view var v = b.items; b.items = [3]; say(\"\" + arrays.length(v)) }",
            "cannot assign to a field of `b` while it is viewed",
        );
        assert_error(
            "class Bag { var array[int] items  fun init() { this.items = [1, 2] } } fun main() { var Bag b = Bag(); view var v = b.items; b.items = [3]; say(\"\" + arrays.length(v)) }",
            "cannot take a mutable view of `b`: it is already viewed",
        );
    }
}
