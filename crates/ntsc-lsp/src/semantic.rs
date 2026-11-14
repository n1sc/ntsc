use std::collections::{HashMap, HashSet};

use ntsc_ast::stmt::Stmt;
use ntsc_ast::types::TypeAnnotation;

use crate::analysis::{FileAnalysis, Symbol, SymbolKind};

pub use crate::value::ValueSource;

#[derive(Debug, Clone, Default)]
struct ValueFact {
    annotation: Option<TypeAnnotation>,
    source: Option<ValueSource>,
    is_static: bool,
}

#[derive(Debug, Clone, Default)]
pub struct SemanticIndex {
    values: HashMap<usize, ValueFact>,
    returns: HashMap<usize, TypeAnnotation>,
    asynchronous: HashSet<usize>,
    pub private_symbols: HashSet<usize>,
    bases: HashMap<String, String>,
    enum_owners: HashMap<usize, String>,
}

impl SemanticIndex {
    pub fn new(analysis: &FileAnalysis) -> Self {
        let mut index = Self::default();
        if let Some(program) = &analysis.program {
            for statement in &program.statements {
                index.statement(statement);
            }
        }
        index
    }

    fn statement(&mut self, statement: &Stmt) {
        match statement {
            Stmt::Private { statement } => {
                match statement.as_ref() {
                    Stmt::Var { name, .. }
                    | Stmt::Function { name, .. }
                    | Stmt::AsyncFunction { name, .. }
                    | Stmt::Class { name, .. }
                    | Stmt::Enum { name, .. }
                    | Stmt::Trait { name, .. }
                    | Stmt::TypeAlias { name, .. } => {
                        self.private_symbols.insert(name.span.start);
                    }
                    _ => {}
                }
                self.statement(statement);
            }
            Stmt::Var {
                name,
                type_annotation,
                initializer,
                is_static,
                ..
            } => {
                self.values.insert(
                    name.span.start,
                    ValueFact {
                        annotation: type_annotation.clone(),
                        source: initializer.as_ref().and_then(ValueSource::from_expression),
                        is_static: *is_static,
                    },
                );
            }
            Stmt::Function {
                name,
                params,
                return_type,
                body,
                ..
            }
            | Stmt::AsyncFunction {
                name,
                params,
                return_type,
                body,
            } => {
                if matches!(statement, Stmt::AsyncFunction { .. }) {
                    self.asynchronous.insert(name.span.start);
                }
                if let Some(return_type) = return_type {
                    self.returns.insert(name.span.start, return_type.ty.clone());
                }
                for parameter in params {
                    self.values.insert(
                        parameter.name.span.start,
                        ValueFact {
                            annotation: parameter.type_annotation.clone(),
                            source: None,
                            is_static: false,
                        },
                    );
                }
                for statement in body {
                    self.statement(statement);
                }
            }
            Stmt::Class {
                name, parent, body, ..
            } => {
                if let Some(parent) = parent {
                    self.bases
                        .insert(name.lexeme().to_string(), parent.lexeme().to_string());
                }
                for member in body {
                    self.statement(member);
                }
            }
            Stmt::Enum { name, members, .. } => {
                for member in members {
                    self.enum_owners
                        .insert(member.name.span.start, name.lexeme().to_string());
                }
            }
            Stmt::TypeAlias { name, target, .. } => {
                self.values.insert(
                    name.span.start,
                    ValueFact {
                        annotation: Some(target.clone()),
                        source: None,
                        is_static: false,
                    },
                );
            }
            Stmt::Block { statements, .. } => {
                for statement in statements {
                    self.statement(statement);
                }
            }
            Stmt::If {
                then_branch,
                elif_branches,
                else_branch,
                ..
            } => {
                self.statement(then_branch);
                for branch in elif_branches {
                    self.statement(&branch.body);
                }
                if let Some(branch) = else_branch {
                    self.statement(branch);
                }
            }
            Stmt::While { body, .. }
            | Stmt::DoWhile { body, .. }
            | Stmt::ChanRecvFor { body, .. }
            | Stmt::Unsafe { body }
            | Stmt::Quiet { body, .. } => self.statement(body),
            Stmt::ForIn {
                variable,
                iterable,
                body,
            }
            | Stmt::ForAwait {
                variable,
                producer: iterable,
                body,
            } => {
                self.values.insert(
                    variable.span.start,
                    ValueFact {
                        annotation: None,
                        source: ValueSource::from_expression(iterable)
                            .map(|source| ValueSource::Index(Box::new(source))),
                        is_static: false,
                    },
                );
                self.statement(body);
            }
            Stmt::For { init, body, .. } => {
                if let Some(init) = init {
                    self.statement(init);
                }
                self.statement(body);
            }
            Stmt::Try {
                try_block,
                catch_block,
                finally_block,
                ..
            } => {
                self.statement(try_block);
                for block in [catch_block, finally_block].into_iter().flatten() {
                    self.statement(block);
                }
            }
            Stmt::Retry {
                body, catch_block, ..
            } => {
                self.statement(body);
                if let Some(block) = catch_block {
                    self.statement(block);
                }
            }
            Stmt::Match {
                cases,
                default_case,
                ..
            } => {
                for case in cases {
                    self.statement(&case.body);
                }
                if let Some(case) = default_case {
                    self.statement(case);
                }
            }
            _ => {}
        }
    }
}

#[derive(Debug, Clone)]
enum ValueType {
    Named(String, usize, ReceiverKind),
    Array(Box<ValueType>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReceiverKind {
    Instance,
    Namespace,
}

pub struct SemanticView<'a> {
    pub analysis: &'a FileAnalysis,
    pub index: &'a SemanticIndex,
}

pub struct SemanticContext<'a> {
    pub views: Vec<SemanticView<'a>>,
    pub current: usize,
    pub namespaces: HashMap<(usize, String), usize>,
    pub namespaced_files: HashSet<usize>,
}

impl<'a> SemanticContext<'a> {
    fn annotation_type(
        &self,
        view: usize,
        annotation: &TypeAnnotation,
        depth: usize,
    ) -> Option<ValueType> {
        if depth > 32 {
            return None;
        }
        match annotation {
            TypeAnnotation::Named(name)
            | TypeAnnotation::Dyn(name)
            | TypeAnnotation::ImplTrait(name) => {
                let (owner, symbol) = self.type_symbol(view, name.lexeme())?;
                if symbol.kind == SymbolKind::TypeAlias {
                    let fact = self.views[owner]
                        .index
                        .values
                        .get(&symbol.name_span.start)?;
                    return self.annotation_type(owner, fact.annotation.as_ref()?, depth + 1);
                }
                Some(ValueType::Named(
                    symbol.name.clone(),
                    owner,
                    ReceiverKind::Instance,
                ))
            }
            TypeAnnotation::Array(Some(inner)) | TypeAnnotation::Slice(Some(inner)) => Some(
                ValueType::Array(Box::new(self.annotation_type(view, inner, depth + 1)?)),
            ),
            TypeAnnotation::Own(inner)
            | TypeAnnotation::Shared(inner)
            | TypeAnnotation::View(inner, _)
            | TypeAnnotation::Ref(inner, _) => self.annotation_type(view, inner, depth + 1),
            _ => None,
        }
    }

    fn type_symbol(&self, view: usize, name: &str) -> Option<(usize, &'a Symbol)> {
        let is_type = |symbol: &&Symbol| {
            symbol.name == name
                && matches!(
                    symbol.kind,
                    SymbolKind::Class
                        | SymbolKind::Enum
                        | SymbolKind::Trait
                        | SymbolKind::TypeAlias
                )
        };
        if let Some(symbol) = self.views.get(view)?.analysis.symbols.iter().find(is_type) {
            return Some((view, symbol));
        }
        let mut found = None;
        for (owner, candidate) in self.views.iter().enumerate() {
            if self.namespaced_files.contains(&owner) && owner != view {
                continue;
            }
            for symbol in candidate.analysis.symbols.iter().filter(is_type) {
                if candidate
                    .index
                    .private_symbols
                    .contains(&symbol.name_span.start)
                {
                    continue;
                }
                if found.is_some() {
                    return None;
                }
                found = Some((owner, symbol));
            }
        }
        found
    }

    fn named_symbol(&self, view: usize, name: &str, offset: usize) -> Option<(usize, &'a Symbol)> {
        let local = self
            .views
            .get(view)?
            .analysis
            .visible_symbols_at(offset)
            .into_iter()
            .filter(|symbol| symbol.name == name)
            .min_by_key(|symbol| {
                let (start, end) = symbol.scope_range;
                (
                    end.saturating_sub(start),
                    usize::MAX - symbol.name_span.start,
                )
            });
        if let Some(symbol) = local {
            return Some((view, symbol));
        }
        let mut found = None;
        for (index, candidate) in self.views.iter().enumerate() {
            if index == view {
                continue;
            }
            if self.namespaced_files.contains(&index) {
                continue;
            }
            for symbol in &candidate.analysis.symbols {
                if symbol.name != name
                    || symbol.parent.is_some()
                    || symbol.scope_range != (0, usize::MAX)
                    || candidate
                        .index
                        .private_symbols
                        .contains(&symbol.name_span.start)
                {
                    continue;
                }
                if let Some((previous_view, previous_symbol)) = found
                    && (previous_view != index || previous_symbol != symbol.name_span.start)
                {
                    return None;
                }
                found = Some((index, symbol.name_span.start));
            }
        }
        let (index, start) = found?;
        self.views[index]
            .analysis
            .symbols
            .iter()
            .find(|symbol| symbol.name_span.start == start)
            .map(|symbol| (index, symbol))
    }

    fn source_symbol(
        &self,
        view: usize,
        source: &ValueSource,
        depth: usize,
    ) -> Option<(usize, &'a Symbol)> {
        if depth > 32 {
            return None;
        }
        match source {
            ValueSource::Name(name, offset) => self.named_symbol(view, name, *offset),
            ValueSource::Member(receiver, name) => {
                if let ValueSource::Name(namespace, offset) = receiver.as_ref()
                    && self
                        .named_symbol(view, namespace, *offset)
                        .is_some_and(|(_, symbol)| symbol.kind == SymbolKind::Module)
                    && let Some(&owner) = self.namespaces.get(&(view, namespace.clone()))
                {
                    return self.views[owner]
                        .analysis
                        .symbols
                        .iter()
                        .find(|symbol| {
                            symbol.name == *name
                                && symbol.parent.is_none()
                                && symbol.container.is_none()
                                && symbol.scope_range == (0, usize::MAX)
                                && !self.views[owner]
                                    .index
                                    .private_symbols
                                    .contains(&symbol.name_span.start)
                        })
                        .map(|symbol| (owner, symbol));
                }
                let ValueType::Named(owner, owner_view, kind) =
                    self.source_type(view, receiver, depth + 1)?
                else {
                    return None;
                };
                self.members_of(&owner, owner_view, kind)
                    .into_iter()
                    .find(|(_, symbol)| symbol.name == *name)
            }
            _ => None,
        }
    }

    fn source_type(&self, view: usize, source: &ValueSource, depth: usize) -> Option<ValueType> {
        if depth > 32 {
            return None;
        }
        match source {
            ValueSource::Class(name) => {
                let (owner, symbol) = self.type_symbol(view, name)?;
                Some(ValueType::Named(
                    symbol.name.clone(),
                    owner,
                    ReceiverKind::Instance,
                ))
            }
            ValueSource::This(offset) => {
                let function = self.views.get(view)?.analysis.enclosing_function(*offset)?;
                let (owner, symbol) = self.type_symbol(view, function.container.as_ref()?)?;
                Some(ValueType::Named(
                    symbol.name.clone(),
                    owner,
                    ReceiverKind::Instance,
                ))
            }
            ValueSource::Super(offset) => {
                let function = self.views.get(view)?.analysis.enclosing_function(*offset)?;
                let parent = self.views[view]
                    .index
                    .bases
                    .get(function.container.as_ref()?)?;
                let (owner, symbol) = self.type_symbol(view, parent)?;
                Some(ValueType::Named(
                    symbol.name.clone(),
                    owner,
                    ReceiverKind::Instance,
                ))
            }
            ValueSource::Index(receiver) => match self.source_type(view, receiver, depth + 1)? {
                ValueType::Array(element) => Some(*element),
                _ => None,
            },
            ValueSource::Call(callee) | ValueSource::Await(callee) => {
                let (owner, symbol) = self.source_symbol(view, callee, depth + 1)?;
                if matches!(source, ValueSource::Call(_))
                    && self.views[owner]
                        .index
                        .asynchronous
                        .contains(&symbol.name_span.start)
                {
                    return None;
                }
                if matches!(symbol.kind, SymbolKind::Class | SymbolKind::Enum) {
                    return Some(ValueType::Named(
                        symbol.name.clone(),
                        owner,
                        ReceiverKind::Instance,
                    ));
                }
                self.annotation_type(
                    owner,
                    self.views[owner]
                        .index
                        .returns
                        .get(&symbol.name_span.start)?,
                    depth + 1,
                )
            }
            ValueSource::Name(_, _) | ValueSource::Member(_, _) => {
                if let ValueSource::Name(name, offset) = source {
                    if name == "this" {
                        return self.source_type(view, &ValueSource::This(*offset), depth + 1);
                    }
                    if name == "super" {
                        let function =
                            self.views.get(view)?.analysis.enclosing_function(*offset)?;
                        let parent = self.views[view]
                            .index
                            .bases
                            .get(function.container.as_ref()?)?;
                        let (owner, symbol) = self.type_symbol(view, parent)?;
                        return Some(ValueType::Named(
                            symbol.name.clone(),
                            owner,
                            ReceiverKind::Instance,
                        ));
                    }
                }
                let (owner, symbol) = self.source_symbol(view, source, depth + 1)?;
                if matches!(symbol.kind, SymbolKind::Class | SymbolKind::Enum) {
                    return Some(ValueType::Named(
                        symbol.name.clone(),
                        owner,
                        ReceiverKind::Namespace,
                    ));
                }
                let fact = self.views[owner]
                    .index
                    .values
                    .get(&symbol.name_span.start)?;
                match &fact.annotation {
                    Some(annotation) => self.annotation_type(owner, annotation, depth + 1),
                    None => self.source_type(owner, fact.source.as_ref()?, depth + 1),
                }
            }
        }
    }

    fn members_of(&self, owner: &str, view: usize, kind: ReceiverKind) -> Vec<(usize, &'a Symbol)> {
        let mut members = Vec::new();
        let mut owner = owner.to_string();
        let mut owner_view = view;
        let mut seen = HashSet::new();
        while seen.insert(owner.clone()) {
            let mut base = None;
            if let Some(candidate) = self.views.get(owner_view) {
                for symbol in &candidate.analysis.symbols {
                    if symbol.container.as_deref() == Some(&owner)
                        || candidate.index.enum_owners.get(&symbol.name_span.start) == Some(&owner)
                    {
                        if symbol.kind == SymbolKind::EnumMember && kind == ReceiverKind::Instance {
                            continue;
                        }
                        if owner_view != self.current
                            && candidate
                                .index
                                .private_symbols
                                .contains(&symbol.name_span.start)
                        {
                            continue;
                        }
                        if kind == ReceiverKind::Namespace
                            && symbol.kind != SymbolKind::EnumMember
                            && !candidate
                                .index
                                .values
                                .get(&symbol.name_span.start)
                                .is_some_and(|fact| fact.is_static)
                        {
                            continue;
                        }
                        if !members
                            .iter()
                            .any(|(_, previous): &(usize, &Symbol)| previous.name == symbol.name)
                        {
                            members.push((owner_view, symbol));
                        }
                    }
                }
                if base.is_none() {
                    base = candidate.index.bases.get(&owner).cloned();
                }
            }
            let Some(parent) = base else { break };
            let Some((parent_view, _)) = self.type_symbol(owner_view, &parent) else {
                break;
            };
            owner_view = parent_view;
            owner = parent;
        }
        members
    }

    pub fn members(&self, source: &ValueSource) -> Vec<(usize, &'a Symbol)> {
        match self.source_type(self.current, source, 0) {
            Some(ValueType::Named(owner, view, kind)) => self.members_of(&owner, view, kind),
            _ => Vec::new(),
        }
    }

    pub fn member(&self, source: &ValueSource, name: &str) -> Option<(usize, &'a Symbol)> {
        self.source_symbol(
            self.current,
            &ValueSource::Member(Box::new(source.clone()), name.to_string()),
            0,
        )
    }

    pub fn named(&self, name: &str, offset: usize) -> Option<(usize, &'a Symbol)> {
        self.named_symbol(self.current, name, offset)
    }
}
