use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};

use ntsc_ast::expr::{Expr, FunctionParam, LiteralValue};
use ntsc_ast::span::Span;
use ntsc_ast::stmt::{GenericParam, Program, Stmt};
use ntsc_ast::token::{Token, TokenKind};
use ntsc_ast::types::{ReturnType, TypeAnnotation};

use crate::TypeError;

/// One vtable slot of a trait: the method's declared signature with its
/// annotations resolved to codegen-level types.
#[derive(Clone)]
pub struct TraitMethodInfo {
    pub name: String,
    pub param_tys: Vec<crate::Ty>,
    pub return_ty: Option<crate::Ty>,
}

/// Everything the code generator needs to build trait-object vtables for
/// one trait: its methods in declaration order (slot 0 is reserved for
/// the drop wrapper).
#[derive(Clone, Default)]
pub struct TraitObjectInfo {
    pub methods: Vec<TraitMethodInfo>,
}

thread_local! {
    static TRAIT_OBJECT_TABLES: RefCell<HashMap<String, TraitObjectInfo>> =
        RefCell::new(HashMap::new());
    static IMPLEMENTATION_REGISTRY: RefCell<HashSet<(String, String)>> =
        RefCell::new(HashSet::new());
}

/// Hands the recorded trait method tables to the code generator. The
/// compiler is single-threaded per compilation, so a thread-local handoff
/// mirrors how the emitter already receives class metadata.
pub fn take_trait_object_tables() -> HashMap<String, TraitObjectInfo> {
    TRAIT_OBJECT_TABLES.with(|tables| tables.take())
}

/// Whether `class_name` implements `trait_name`, directly or through a
/// supertrait chain. Populated by `prepare_program`; trait declarations
/// always accompany trait-object use, so the registry is filled before
/// assignability is ever queried.
pub(crate) fn implementation_exists(trait_name: &str, class_name: &str) -> bool {
    IMPLEMENTATION_REGISTRY.with(|registry| {
        registry
            .borrow()
            .contains(&(trait_name.to_string(), class_name.to_string()))
    })
}

#[derive(Clone)]
struct GenericTemplate {
    function: Stmt,
    params: Vec<GenericParam>,
}

#[derive(Clone)]
struct GenericClassTemplate {
    class: Stmt,
    params: Vec<GenericParam>,
}

#[derive(Clone)]
struct GenericEnumTemplate {
    enumeration: Stmt,
    params: Vec<GenericParam>,
}

#[derive(Clone)]
struct TypeAliasTemplate {
    params: Vec<GenericParam>,
    target: TypeAnnotation,
}

struct Context {
    traits: HashMap<String, Stmt>,
    trait_parents: HashMap<String, Vec<Token>>,
    trait_associated_types: HashMap<String, HashSet<String>>,
    implementations: HashSet<(String, String)>,
    associated_type_bindings: HashMap<(String, String, String), TypeAnnotation>,
    classes: HashSet<String>,
    class_templates: HashMap<String, GenericClassTemplate>,
    class_specializations: HashMap<String, String>,
    enum_templates: HashMap<String, GenericEnumTemplate>,
    enum_specializations: HashSet<String>,
    aliases: HashMap<String, TypeAliasTemplate>,
    resolving_aliases: HashSet<String>,
    templates: HashMap<String, GenericTemplate>,
    specializations: HashMap<(String, String), String>,
    specialization_returns: HashMap<String, TypeAnnotation>,
    generated: Vec<Stmt>,
    generated_classes: Vec<Stmt>,
    errors: Vec<TypeError>,
}

/// Lowers trait declarations and generic functions into the existing, concrete
/// AST understood by the resolver, type checker, and code generator.
pub fn prepare_program(program: &Program) -> Result<Program, Vec<TypeError>> {
    let mut context = Context {
        traits: HashMap::new(),
        trait_parents: HashMap::new(),
        trait_associated_types: HashMap::new(),
        implementations: HashSet::new(),
        associated_type_bindings: HashMap::new(),
        classes: HashSet::new(),
        class_templates: HashMap::new(),
        class_specializations: HashMap::new(),
        enum_templates: HashMap::new(),
        enum_specializations: HashSet::new(),
        aliases: HashMap::new(),
        resolving_aliases: HashSet::new(),
        templates: HashMap::new(),
        specializations: HashMap::new(),
        specialization_returns: HashMap::new(),
        generated: Vec::new(),
        generated_classes: Vec::new(),
        errors: Vec::new(),
    };

    for statement in &program.statements {
        match statement {
            Stmt::Trait {
                name,
                parents,
                associated_types,
                methods,
            } => {
                context
                    .trait_parents
                    .insert(name.lexeme().to_string(), parents.clone());
                for method in methods {
                    if declaration_mentions_impl_trait(method) {
                        context.error(
                            format!(
                                "trait method `{}` cannot declare an `impl Trait` return; \
                                 use the concrete type in implementations",
                                function_name(method).unwrap_or("?")
                            ),
                            stmt_span(method),
                        );
                    }
                }
                let mut names = HashSet::new();
                for associated_type in associated_types {
                    if !names.insert(associated_type.lexeme().to_string()) {
                        context.error(
                            format!(
                                "duplicate associated type `{}` in trait `{}`",
                                associated_type.lexeme(),
                                name.lexeme()
                            ),
                            associated_type.span,
                        );
                    }
                }
                context
                    .trait_associated_types
                    .insert(name.lexeme().to_string(), names);
                if context
                    .traits
                    .insert(name.lexeme().to_string(), statement.clone())
                    .is_some()
                {
                    context.error(format!("duplicate trait `{}`", name.lexeme()), name.span);
                }
            }
            Stmt::Class {
                name,
                generic_params,
                ..
            } if !generic_params.is_empty() => {
                if context
                    .class_templates
                    .insert(
                        name.lexeme().to_string(),
                        GenericClassTemplate {
                            class: statement.clone(),
                            params: generic_params.clone(),
                        },
                    )
                    .is_some()
                {
                    context.error(format!("duplicate class `{}`", name.lexeme()), name.span);
                }
            }
            Stmt::Class { name, .. } => {
                context.classes.insert(name.lexeme().to_string());
            }
            Stmt::Enum {
                name,
                generic_params,
                ..
            } if !generic_params.is_empty() => {
                context.enum_templates.insert(
                    name.lexeme().to_string(),
                    GenericEnumTemplate {
                        enumeration: statement.clone(),
                        params: generic_params.clone(),
                    },
                );
            }
            Stmt::TypeAlias {
                name,
                generic_params,
                target,
            } => {
                if context
                    .aliases
                    .insert(
                        name.lexeme().to_string(),
                        TypeAliasTemplate {
                            params: generic_params.clone(),
                            target: target.clone(),
                        },
                    )
                    .is_some()
                {
                    context.error(
                        format!("duplicate type alias `{}`", name.lexeme()),
                        name.span,
                    );
                }
            }
            Stmt::Function {
                name,
                generic_params,
                ..
            } if !generic_params.is_empty()
                && context
                    .templates
                    .insert(
                        name.lexeme().to_string(),
                        GenericTemplate {
                            function: statement.clone(),
                            params: generic_params.clone(),
                        },
                    )
                    .is_some() =>
            {
                context.error(format!("duplicate function `{}`", name.lexeme()), name.span);
            }
            _ => {}
        }
    }

    context.validate_supertraits();
    if !context.errors.is_empty() {
        return Err(context.errors);
    }

    for template in context.templates.values() {
        for generic in &template.params {
            for bound in &generic.bounds {
                if !context.traits.contains_key(bound.lexeme()) {
                    context.errors.push(TypeError {
                        message: format!("unknown trait `{}` in generic bound", bound.lexeme()),
                        span: bound.span,
                        code: None,
                        help: None,
                    });
                }
            }
        }
    }
    for template in context.class_templates.values() {
        for generic in &template.params {
            for bound in &generic.bounds {
                if !context.traits.contains_key(bound.lexeme()) {
                    context.errors.push(TypeError {
                        message: format!("unknown trait `{}` in generic bound", bound.lexeme()),
                        span: bound.span,
                        code: None,
                        help: None,
                    });
                }
            }
        }
    }
    for template in context.enum_templates.values() {
        for generic in &template.params {
            for bound in &generic.bounds {
                if !context.traits.contains_key(bound.lexeme()) {
                    context.errors.push(TypeError {
                        message: format!("unknown trait `{}` in generic bound", bound.lexeme()),
                        span: bound.span,
                        code: None,
                        help: None,
                    });
                }
            }
        }
    }
    for template in context.aliases.values() {
        for generic in &template.params {
            for bound in &generic.bounds {
                if !context.traits.contains_key(bound.lexeme()) {
                    context.errors.push(TypeError {
                        message: format!("unknown trait `{}` in generic bound", bound.lexeme()),
                        span: bound.span,
                        code: None,
                        help: None,
                    });
                }
            }
        }
    }

    let mut impl_methods: HashMap<String, Vec<Stmt>> = HashMap::new();
    for statement in &program.statements {
        if let Stmt::Impl {
            trait_name,
            type_name,
            body,
        } = statement
        {
            let trait_key = trait_name.lexeme().to_string();
            let type_key = type_name.lexeme().to_string();
            let key = (trait_key.clone(), type_key.clone());
            if !context.traits.contains_key(&trait_key) {
                context.error(format!("unknown trait `{trait_key}`"), trait_name.span);
                continue;
            }
            if !context.classes.contains(&type_key) {
                context.error(
                    format!("unknown type `{type_key}` in trait implementation"),
                    type_name.span,
                );
                continue;
            }
            if !context.implementations.insert(key.clone()) {
                context.error(
                    format!("duplicate implementation of `{trait_key}` for `{type_key}`"),
                    trait_name.span,
                );
                continue;
            }
            let mut associated_substitutions = HashMap::new();
            let mut bound_names = HashSet::new();

            // Implementing a trait implies implementing its supertraits:
            // the closure below drives implicit registration, associated
            // type binding under every declaring ancestor, and inherited
            // method requirements.
            let mut chain = vec![trait_key.clone()];
            chain.extend(context.trait_ancestors(&trait_key));

            let declared_associated_names: HashSet<String> = chain
                .iter()
                .filter_map(|ancestor| context.trait_associated_types.get(ancestor))
                .flat_map(|names| names.iter().cloned())
                .collect();

            for member in body {
                match member {
                    Stmt::TypeAlias {
                        name,
                        generic_params,
                        target,
                    } => {
                        if !declared_associated_names.contains(name.lexeme()) {
                            context.error(
                                format!(
                                    "trait `{trait_key}` has no associated type `{}`",
                                    name.lexeme()
                                ),
                                name.span,
                            );
                        } else if !generic_params.is_empty() {
                            context.error(
                                "associated type bindings cannot be generic".into(),
                                name.span,
                            );
                        } else if !bound_names.insert(name.lexeme().to_string()) {
                            context.error(
                                format!("duplicate associated type `{}`", name.lexeme()),
                                name.span,
                            );
                        } else {
                            let target = context.normalize_type(target.clone());
                            for ancestor in &chain {
                                let declares = context
                                    .trait_associated_types
                                    .get(ancestor)
                                    .is_some_and(|names| names.contains(name.lexeme()));
                                if declares {
                                    context.associated_type_bindings.insert(
                                        (
                                            ancestor.clone(),
                                            type_key.clone(),
                                            name.lexeme().to_string(),
                                        ),
                                        target.clone(),
                                    );
                                }
                            }
                            associated_substitutions.insert(
                                name.lexeme().to_string(),
                                target.clone(),
                            );
                            associated_substitutions.insert(
                                format!("Self::{}", name.lexeme()),
                                target,
                            );
                        }
                    }
                    Stmt::Function { .. } => {}
                    _ => context.error(
                        "trait implementations may only contain methods and associated type bindings".into(),
                        stmt_span(member),
                    ),
                }
            }
            for associated_name in &declared_associated_names {
                if !bound_names.contains(associated_name) {
                    context.error(
                        format!("trait `{trait_key}` requires associated type `{associated_name}`"),
                        trait_name.span,
                    );
                }
            }

            // Every ancestor's required methods must be satisfied by an
            // explicit method, or inherited as the trait's default body.
            let mut inherited_defaults: Vec<Stmt> = Vec::new();
            for ancestor in &chain {
                let required = match context.traits.get(ancestor) {
                    Some(Stmt::Trait { methods, .. }) => methods.clone(),
                    _ => continue,
                };
                for method in required {
                    let Some(required_name) = function_name(&method) else {
                        continue;
                    };
                    let found = body
                        .iter()
                        .find(|candidate| function_name(candidate) == Some(required_name));
                    match found {
                        Some(candidate)
                            if same_signature(
                                &substitute_stmt_types(method.clone(), &associated_substitutions),
                                candidate,
                            ) => {}
                        Some(candidate) => context.error(
                            format!("method `{required_name}` does not match trait `{ancestor}`"),
                            stmt_span(candidate),
                        ),
                        None => {
                            let has_default =
                                matches!(&method, Stmt::Function { body, .. } if !body.is_empty());
                            if has_default {
                                inherited_defaults
                                    .push(substitute_stmt_types(method, &associated_substitutions));
                            } else {
                                context.error(
                                    format!("trait `{ancestor}` requires method `{required_name}`"),
                                    stmt_span(&method),
                                );
                            }
                        }
                    }
                }
            }

            // Ancestor implementations are implicit: registering them lets
            // `dyn Parent` and bounds accept this class without a separate
            // `impl`.
            for ancestor in &chain {
                context
                    .implementations
                    .insert((ancestor.clone(), type_key.clone()));
            }

            let mut methods = body
                .iter()
                .filter(|member| matches!(member, Stmt::Function { .. }))
                .cloned()
                .collect::<Vec<_>>();
            methods.extend(inherited_defaults);
            impl_methods.entry(type_key).or_default().extend(methods);
        }
    }

    if !context.errors.is_empty() {
        return Err(context.errors);
    }

    let mut output = Vec::new();
    for statement in &program.statements {
        match statement {
            Stmt::Trait { .. } | Stmt::Impl { .. } => continue,
            Stmt::Function { generic_params, .. } if !generic_params.is_empty() => continue,
            Stmt::Class { generic_params, .. } if !generic_params.is_empty() => continue,
            Stmt::Enum { generic_params, .. } if !generic_params.is_empty() => continue,
            Stmt::TypeAlias { .. } => continue,
            Stmt::Class {
                name,
                parent,
                body,
                generic_params,
            } => {
                let mut merged = body.clone();
                if let Some(methods) = impl_methods.get(name.lexeme()) {
                    merged.extend(methods.clone());
                }
                let mut env = HashMap::new();
                let merged = merged
                    .into_iter()
                    .map(|member| context.transform_stmt(member, &mut env))
                    .collect();
                output.push(Stmt::Class {
                    name: name.clone(),
                    generic_params: generic_params.clone(),
                    parent: parent.clone(),
                    body: merged,
                });
            }
            other => output.push(context.transform_stmt(other.clone(), &mut HashMap::new())),
        }
    }
    // Specialized classes must be emitted before functions that construct
    // them. Keeping them in declaration order also makes nested applications
    // available to the code generator's struct lookup.
    let generated_classes = std::mem::take(&mut context.generated_classes);
    let mut ordered = generated_classes;
    ordered.extend(output);
    output = ordered;
    record_trait_object_tables(&context);
    IMPLEMENTATION_REGISTRY.with(|registry| {
        *registry.borrow_mut() = context.implementations.clone();
    });
    output.extend(context.generated);
    if context.errors.is_empty() {
        Ok(Program { statements: output })
    } else {
        Err(context.errors)
    }
}

/// Publishes per-trait method tables (declaration order) for the emitter's
/// vtable construction. Traits with associated types are not object-safe
/// and never become trait objects.
fn record_trait_object_tables(context: &Context) {
    let mut tables: HashMap<String, TraitObjectInfo> = HashMap::new();
    for (name, statement) in &context.traits {
        let Stmt::Trait { methods, .. } = statement else {
            continue;
        };
        let has_associated_types = context
            .trait_associated_types
            .get(name)
            .is_some_and(|names| !names.is_empty());
        if has_associated_types {
            continue;
        }
        // The vtable must also carry every inherited method: dispatch on a
        // `dyn Parent` value reachable through this trait resolves the same
        // slots. Ancestor methods come first; diamond duplicates keep the
        // nearest declaration.
        let mut seen_methods: HashSet<String> = HashSet::new();
        let mut chain_methods: Vec<&Stmt> = Vec::new();
        for ancestor in context.trait_ancestors(name) {
            if let Some(Stmt::Trait { methods, .. }) = context.traits.get(&ancestor) {
                chain_methods.extend(methods.iter());
            }
        }
        chain_methods.extend(methods.iter());
        let info = TraitObjectInfo {
            methods: chain_methods
                .into_iter()
                .filter_map(|method| {
                    let Stmt::Function {
                        name,
                        params,
                        return_type,
                        ..
                    } = method
                    else {
                        return None;
                    };
                    if !seen_methods.insert(name.lexeme().to_string()) {
                        return None;
                    }
                    Some(TraitMethodInfo {
                        name: name.lexeme().to_string(),
                        param_tys: params
                            .iter()
                            .map(|param| {
                                param
                                    .type_annotation
                                    .as_ref()
                                    .map(annotation_to_ty)
                                    .unwrap_or(crate::Ty::Any)
                            })
                            .collect(),
                        return_ty: return_type.as_ref().map(|ret| annotation_to_ty(&ret.ty)),
                    })
                })
                .collect(),
        };
        tables.insert(name.clone(), info);
    }
    TRAIT_OBJECT_TABLES.with(|slot| *slot.borrow_mut() = tables);
}

fn annotation_to_ty(annotation: &TypeAnnotation) -> crate::Ty {
    use crate::Ty;
    match annotation {
        TypeAnnotation::Int => Ty::Int,
        TypeAnnotation::Float => Ty::Float,
        TypeAnnotation::String => Ty::String,
        TypeAnnotation::Bool => Ty::Bool,
        TypeAnnotation::Array(element) => Ty::Array(Box::new(
            element.as_deref().map(annotation_to_ty).unwrap_or(Ty::Any),
        )),
        TypeAnnotation::Object => Ty::Object,
        TypeAnnotation::Option(inner) => Ty::Option(Box::new(annotation_to_ty(inner))),
        TypeAnnotation::Result { ok, err } => Ty::Result {
            ok: Box::new(annotation_to_ty(ok)),
            err: Box::new(annotation_to_ty(err)),
        },
        TypeAnnotation::View(inner, mutable) => {
            Ty::View(Box::new(annotation_to_ty(inner)), *mutable)
        }
        TypeAnnotation::Any => Ty::Any,
        TypeAnnotation::Pointer => Ty::Pointer,
        TypeAnnotation::Slice(element) => Ty::Slice(Box::new(
            element.as_deref().map(annotation_to_ty).unwrap_or(Ty::Any),
        )),
        TypeAnnotation::Own(inner) => Ty::Own(Box::new(annotation_to_ty(inner))),
        TypeAnnotation::Ref(inner, mutable) => Ty::Ref(Box::new(annotation_to_ty(inner)), *mutable),
        TypeAnnotation::RawPointer(inner, mutable) => {
            Ty::RawPointer(Box::new(annotation_to_ty(inner)), *mutable)
        }
        TypeAnnotation::Named(token) if token.lexeme() == "void" => Ty::Void,
        TypeAnnotation::Named(token) => Ty::Class(token.lexeme().to_string()),
        TypeAnnotation::Shared(inner) => Ty::Shared(Box::new(annotation_to_ty(inner))),
        TypeAnnotation::ImplTrait(_) => Ty::Any,
        TypeAnnotation::Dyn(token) => Ty::Dyn(token.lexeme().to_string()),
        TypeAnnotation::Tuple(types) => Ty::Tuple(types.iter().map(annotation_to_ty).collect()),
        TypeAnnotation::Chan(inner) => Ty::Chan(Box::new(annotation_to_ty(inner))),
    }
}

impl Context {
    fn error(&mut self, message: String, span: Span) {
        self.errors.push(TypeError {
            message,
            span,
            code: None,
            help: None,
        });
    }

    /// Every trait `name` transitively inherits from, breadth-first and
    /// deduplicated, excluding `name` itself. Cycle-safe.
    fn trait_ancestors(&self, name: &str) -> Vec<String> {
        let mut ancestors = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<String> = VecDeque::new();
        if let Some(parents) = self.trait_parents.get(name) {
            for parent in parents {
                queue.push_back(parent.lexeme().to_string());
            }
        }
        while let Some(current) = queue.pop_front() {
            if current == name || !visited.insert(current.clone()) {
                continue;
            }
            if let Some(parents) = self.trait_parents.get(&current) {
                for parent in parents {
                    queue.push_back(parent.lexeme().to_string());
                }
            }
            ancestors.push(current);
        }
        ancestors
    }

    fn validate_supertraits(&mut self) {
        let declarations = self.trait_parents.clone();
        for (name, parents) in &declarations {
            for parent in parents {
                if !self.traits.contains_key(parent.lexeme()) {
                    self.error(
                        format!(
                            "trait `{}` extends unknown trait `{}`",
                            name,
                            parent.lexeme()
                        ),
                        parent.span,
                    );
                }
            }
            // Walking the ancestor graph with a visited set terminates on
            // cycles and turns one through the starting trait into "the
            // starting trait reappears in its own ancestry".
            let mut queue: VecDeque<&str> = VecDeque::new();
            let mut visited: HashSet<String> = HashSet::new();
            for parent in parents {
                queue.push_back(parent.lexeme());
            }
            while let Some(current) = queue.pop_front() {
                if current == name.as_str() {
                    self.error(
                        format!("trait `{name}` inherits from itself"),
                        Span::dummy(),
                    );
                    break;
                }
                if !visited.insert(current.to_string()) {
                    continue;
                }
                if let Some(grandparents) = self.trait_parents.get(current) {
                    for grandparent in grandparents {
                        queue.push_back(grandparent.lexeme());
                    }
                }
            }
        }
    }

    fn normalize_type(&mut self, annotation: TypeAnnotation) -> TypeAnnotation {
        match annotation {
            TypeAnnotation::Array(inner) => {
                TypeAnnotation::Array(inner.map(|inner| Box::new(self.normalize_type(*inner))))
            }
            TypeAnnotation::Option(inner) => {
                TypeAnnotation::Option(Box::new(self.normalize_type(*inner)))
            }
            TypeAnnotation::View(inner, mutable) => {
                TypeAnnotation::View(Box::new(self.normalize_type(*inner)), mutable)
            }
            TypeAnnotation::Shared(inner) => {
                TypeAnnotation::Shared(Box::new(self.normalize_type(*inner)))
            }
            TypeAnnotation::Slice(inner) => {
                TypeAnnotation::Slice(inner.map(|inner| Box::new(self.normalize_type(*inner))))
            }
            TypeAnnotation::Own(inner) => {
                TypeAnnotation::Own(Box::new(self.normalize_type(*inner)))
            }
            TypeAnnotation::Ref(inner, mutable) => {
                TypeAnnotation::Ref(Box::new(self.normalize_type(*inner)), mutable)
            }
            TypeAnnotation::RawPointer(inner, mutable) => {
                TypeAnnotation::RawPointer(Box::new(self.normalize_type(*inner)), mutable)
            }
            TypeAnnotation::Named(token) => {
                if let Some(expanded) = self.expand_alias(&token) {
                    return expanded;
                }
                self.ensure_class_specialization(token.lexeme(), token.span);
                self.ensure_enum_specialization(token.lexeme(), token.span);
                TypeAnnotation::Named(token)
            }
            other => other,
        }
    }

    fn resolve_associated_type(
        &mut self,
        annotation: &TypeAnnotation,
        substitutions: &HashMap<String, TypeAnnotation>,
        params: &[GenericParam],
        span: Span,
    ) -> TypeAnnotation {
        let substituted = substitute_type(annotation, substitutions);
        match substituted {
            TypeAnnotation::Named(token) => {
                let Some((parameter, associated_name)) = token.lexeme().split_once("::") else {
                    return TypeAnnotation::Named(token);
                };
                let Some(actual) = substitutions.get(parameter) else {
                    self.error(
                        format!(
                            "associated type projection requires generic parameter `{parameter}`"
                        ),
                        span,
                    );
                    return TypeAnnotation::Any;
                };
                let Some(generic) = params.iter().find(|param| param.name.lexeme() == parameter)
                else {
                    self.error(format!("`{parameter}` is not a generic parameter"), span);
                    return TypeAnnotation::Any;
                };
                let matches = generic.bounds.iter().filter_map(|bound| {
                    self.associated_type_bindings
                        .get(&(
                            bound.lexeme().to_string(),
                            type_key(actual),
                            associated_name.to_string(),
                        ))
                        .cloned()
                });
                let bindings: Vec<_> = matches.collect();
                match bindings.as_slice() {
                    [binding] => substitute_type(binding, substitutions),
                    [] => {
                        self.error(
                            format!(
                                "trait bounds for `{parameter}` do not define associated type `{associated_name}`"
                            ),
                            span,
                        );
                        TypeAnnotation::Any
                    }
                    _ => {
                        self.error(
                            format!(
                                "associated type `{parameter}::{associated_name}` is ambiguous; use one trait bound"
                            ),
                            span,
                        );
                        TypeAnnotation::Any
                    }
                }
            }
            TypeAnnotation::Array(inner) => TypeAnnotation::Array(inner.map(|inner| {
                Box::new(self.resolve_associated_type(&inner, substitutions, params, span))
            })),
            TypeAnnotation::Option(inner) => TypeAnnotation::Option(Box::new(
                self.resolve_associated_type(&inner, substitutions, params, span),
            )),
            TypeAnnotation::View(inner, mutable) => TypeAnnotation::View(
                Box::new(self.resolve_associated_type(&inner, substitutions, params, span)),
                mutable,
            ),
            TypeAnnotation::Shared(inner) => TypeAnnotation::Shared(Box::new(
                self.resolve_associated_type(&inner, substitutions, params, span),
            )),
            TypeAnnotation::Slice(inner) => TypeAnnotation::Slice(inner.map(|inner| {
                Box::new(self.resolve_associated_type(&inner, substitutions, params, span))
            })),
            TypeAnnotation::Own(inner) => TypeAnnotation::Own(Box::new(
                self.resolve_associated_type(&inner, substitutions, params, span),
            )),
            TypeAnnotation::Ref(inner, mutable) => TypeAnnotation::Ref(
                Box::new(self.resolve_associated_type(&inner, substitutions, params, span)),
                mutable,
            ),
            TypeAnnotation::RawPointer(inner, mutable) => TypeAnnotation::RawPointer(
                Box::new(self.resolve_associated_type(&inner, substitutions, params, span)),
                mutable,
            ),
            other => other,
        }
    }

    fn specialization_substitutions(
        &mut self,
        params: &[GenericParam],
        substitutions: &HashMap<String, TypeAnnotation>,
        span: Span,
    ) -> HashMap<String, TypeAnnotation> {
        let mut resolved = substitutions.clone();
        for param in params {
            if !substitutions.contains_key(param.name.lexeme()) {
                continue;
            }
            let associated_names: HashSet<String> = param
                .bounds
                .iter()
                .flat_map(|bound| {
                    self.trait_associated_types
                        .get(bound.lexeme())
                        .into_iter()
                        .flat_map(|names| names.iter().cloned())
                })
                .collect();
            for associated_name in associated_names {
                let projection = format!("{}::{associated_name}", param.name.lexeme());
                let projection_type = self.resolve_associated_type(
                    &TypeAnnotation::Named(Token::new(
                        TokenKind::Identifier(projection.clone()),
                        param.name.span,
                    )),
                    substitutions,
                    params,
                    span,
                );
                resolved.insert(projection, projection_type);
            }
        }
        resolved
    }

    fn expand_alias(&mut self, token: &Token) -> Option<TypeAnnotation> {
        let (base, argument_sources) = split_applied_name(token.lexeme())
            .map_or((token.lexeme(), Vec::new()), |(base, arguments)| {
                (base, arguments)
            });
        let template = self.aliases.get(base)?.clone();
        if template.params.len() != argument_sources.len() {
            self.error(
                format!(
                    "type alias `{base}` expects {} type arguments, got {}",
                    template.params.len(),
                    argument_sources.len()
                ),
                token.span,
            );
            return Some(TypeAnnotation::Any);
        }
        if !self.resolving_aliases.insert(token.lexeme().to_string()) {
            self.error(
                format!("cyclic type alias involving `{}`", token.lexeme()),
                token.span,
            );
            return Some(TypeAnnotation::Any);
        }
        let arguments = argument_sources
            .iter()
            .map(|source| parse_type_source(source, token.span));
        let substitutions: HashMap<_, _> = template
            .params
            .iter()
            .zip(arguments)
            .map(|(param, argument)| (param.name.lexeme().to_string(), argument))
            .collect();
        let expanded = if self.validate_bounds(&template.params, &substitutions, token.span) {
            let substitutions =
                self.specialization_substitutions(&template.params, &substitutions, token.span);
            self.normalize_type(substitute_type(&template.target, &substitutions))
        } else {
            TypeAnnotation::Any
        };
        self.resolving_aliases.remove(token.lexeme());
        Some(expanded)
    }

    fn ensure_class_specialization(&mut self, name: &str, span: Span) {
        let Some((base, argument_sources)) = split_applied_name(name) else {
            return;
        };
        if self.class_specializations.contains_key(name) {
            return;
        }
        let Some(template) = self.class_templates.get(base).cloned() else {
            if !self.enum_templates.contains_key(base) {
                self.error(format!("unknown generic type `{base}`"), span);
            }
            return;
        };
        if template.params.len() != argument_sources.len() {
            self.error(
                format!(
                    "generic class `{base}` expects {} type arguments, got {}",
                    template.params.len(),
                    argument_sources.len()
                ),
                span,
            );
            return;
        }
        let arguments: Vec<_> = argument_sources
            .iter()
            .map(|source| parse_type_source(source, span))
            .collect();
        let substitutions: HashMap<_, _> = template
            .params
            .iter()
            .zip(arguments)
            .map(|(param, argument)| (param.name.lexeme().to_string(), argument))
            .collect();
        if !self.validate_bounds(&template.params, &substitutions, span) {
            return;
        }
        let substitutions =
            self.specialization_substitutions(&template.params, &substitutions, span);
        self.class_specializations
            .insert(name.to_string(), name.to_string());
        let Stmt::Class {
            name: template_name,
            parent,
            body,
            ..
        } = template.class
        else {
            return;
        };
        let mut env = HashMap::new();
        let body = body
            .into_iter()
            .map(|stmt| substitute_stmt_types(stmt, &substitutions))
            .map(|stmt| self.transform_stmt(stmt, &mut env))
            .collect();
        let parent = parent.map(|parent| {
            let parent_name = substitutions
                .get(parent.lexeme())
                .map(type_annotation_name)
                .unwrap_or_else(|| parent.lexeme().to_string());
            Token::new(TokenKind::Identifier(parent_name), parent.span)
        });
        self.classes.insert(name.to_string());
        self.generated_classes.push(Stmt::Class {
            name: Token::new(TokenKind::Identifier(name.to_string()), template_name.span),
            generic_params: Vec::new(),
            parent,
            body,
        });
    }

    fn ensure_enum_specialization(&mut self, name: &str, span: Span) {
        let Some((base, argument_sources)) = split_applied_name(name) else {
            return;
        };
        if self.enum_specializations.contains(name) {
            return;
        }
        let Some(template) = self.enum_templates.get(base).cloned() else {
            return;
        };
        if template.params.len() != argument_sources.len() {
            self.error(
                format!(
                    "generic enum `{base}` expects {} type arguments, got {}",
                    template.params.len(),
                    argument_sources.len()
                ),
                span,
            );
            return;
        }
        let arguments = argument_sources
            .iter()
            .map(|source| parse_type_source(source, span));
        let substitutions: HashMap<_, _> = template
            .params
            .iter()
            .zip(arguments)
            .map(|(param, argument)| (param.name.lexeme().to_string(), argument))
            .collect();
        if !self.validate_bounds(&template.params, &substitutions, span) {
            return;
        }
        let substitutions =
            self.specialization_substitutions(&template.params, &substitutions, span);
        self.enum_specializations.insert(name.to_string());
        let Stmt::Enum {
            name: template_name,
            members,
            ..
        } = template.enumeration
        else {
            return;
        };
        let members = members
            .into_iter()
            .map(|mut member| {
                member.data_types = member
                    .data_types
                    .into_iter()
                    .map(|ty| substitute_type(&ty, &substitutions))
                    .collect();
                member
            })
            .collect();
        self.generated.push(Stmt::Enum {
            name: Token::new(TokenKind::Identifier(name.to_string()), template_name.span),
            generic_params: Vec::new(),
            members,
        });
    }

    fn validate_bounds(
        &mut self,
        params: &[GenericParam],
        substitutions: &HashMap<String, TypeAnnotation>,
        span: Span,
    ) -> bool {
        for param in params {
            let Some(actual) = substitutions.get(param.name.lexeme()) else {
                continue;
            };
            for bound in &param.bounds {
                if !self
                    .implementations
                    .contains(&(bound.lexeme().to_string(), type_key(actual)))
                {
                    self.error(
                        format!(
                            "type `{}` does not implement trait `{}`",
                            type_annotation_name(actual),
                            bound.lexeme()
                        ),
                        span,
                    );
                    return false;
                }
            }
        }
        true
    }

    /// Replaces an `impl Trait` return annotation with the single concrete
    /// class every `return` in the body produces. Requires that class to
    /// implement the trait and the trait to be object-safe.
    fn resolve_impl_trait_return(
        &mut self,
        function_name: &Token,
        trait_name: &Token,
        body: &[Stmt],
        env: &HashMap<String, TypeAnnotation>,
    ) -> TypeAnnotation {
        let key = trait_name.lexeme().to_string();
        if !self.traits.contains_key(&key) {
            self.error(format!("unknown trait `{key}`"), trait_name.span);
            return TypeAnnotation::Any;
        }
        let has_associated_types = self
            .trait_associated_types
            .get(&key)
            .is_some_and(|names| !names.is_empty());
        if has_associated_types {
            self.error(
                format!(
                    "trait `{key}` has associated types and cannot be used as an `impl` return"
                ),
                trait_name.span,
            );
            return TypeAnnotation::Any;
        }
        if function_name.lexeme() == "main" {
            self.error(
                format!("`main` cannot declare an `impl {key}` return"),
                trait_name.span,
            );
            return TypeAnnotation::Any;
        }
        let returns = collect_return_expressions(body);
        if returns.is_empty() {
            self.error(
                format!(
                    "function `{}` with `impl {key}` return must return a value",
                    function_name.lexeme()
                ),
                function_name.span,
            );
            return TypeAnnotation::Any;
        }
        let mut concrete: Option<Token> = None;
        for expression in returns {
            let Some(inferred) = self.infer_expr_type(expression, env) else {
                self.error(
                    format!("cannot infer the concrete type returned as `impl {key}`"),
                    expression.span(),
                );
                continue;
            };
            let TypeAnnotation::Named(token) = inferred else {
                self.error(
                    format!("value returned as `impl {key}` must be a class instance"),
                    expression.span(),
                );
                continue;
            };
            if !self
                .implementations
                .contains(&(key.clone(), token.lexeme().to_string()))
            {
                self.error(
                    format!("class `{}` does not implement `{key}`", token.lexeme()),
                    expression.span(),
                );
                continue;
            }
            match &concrete {
                Some(existing) if existing.lexeme() != token.lexeme() => {
                    self.error(
                        format!(
                            "`impl {key}` return is ambiguous between `{}` and `{}`",
                            existing.lexeme(),
                            token.lexeme()
                        ),
                        expression.span(),
                    );
                }
                _ => concrete = Some(token),
            }
        }
        match concrete {
            Some(token) => TypeAnnotation::Named(token),
            None => TypeAnnotation::Any,
        }
    }

    fn transform_stmt(
        &mut self,
        statement: Stmt,
        env: &mut HashMap<String, TypeAnnotation>,
    ) -> Stmt {
        match statement {
            Stmt::Function {
                name,
                generic_params,
                params,
                return_type,
                body,
            } => {
                let params = params
                    .into_iter()
                    .map(|param| {
                        let annotation = param
                            .type_annotation
                            .map(|ty| self.normalize_type(substitute_type(&ty, &HashMap::new())));
                        if let Some(annotation) = &annotation {
                            env.insert(param.name.lexeme().to_string(), annotation.clone());
                        }
                        FunctionParam {
                            name: param.name,
                            type_annotation: annotation,
                        }
                    })
                    .collect();
                let body: Vec<Stmt> = body
                    .into_iter()
                    .map(|stmt| self.transform_stmt(stmt, env))
                    .collect();
                let return_type = return_type.map(|ret| {
                    let resolved = match &ret.ty {
                        TypeAnnotation::ImplTrait(trait_name) => {
                            self.resolve_impl_trait_return(&name, trait_name, &body, env)
                        }
                        _ => ret.ty.clone(),
                    };
                    ReturnType {
                        ty: self.normalize_type(resolved),
                        arrow_span: ret.arrow_span,
                    }
                });
                Stmt::Function {
                    name,
                    generic_params,
                    params,
                    return_type,
                    body,
                }
            }
            Stmt::Var {
                name,
                type_annotation,
                initializer,
                is_static,
                is_const,
                view,
            } => {
                let initializer = initializer.map(|expr| self.transform_expr(expr, env));
                let annotation = type_annotation
                    .map(|ty| self.normalize_type(substitute_type(&ty, &HashMap::new())));
                if let Some(annotation) = &annotation {
                    env.insert(name.lexeme().to_string(), annotation.clone());
                } else if let Some(initializer) = &initializer
                    && let Some(ty) = self.infer_expr_type(initializer, env)
                {
                    env.insert(name.lexeme().to_string(), ty);
                }
                Stmt::Var {
                    name,
                    type_annotation: annotation,
                    initializer,
                    is_static,
                    is_const,
                    view,
                }
            }
            Stmt::Expression { expression } => Stmt::Expression {
                expression: self.transform_expr(expression, env),
            },
            Stmt::Say {
                expression,
                keyword_span,
            } => Stmt::Say {
                expression: self.transform_expr(expression, env),
                keyword_span,
            },
            Stmt::Return { value } => Stmt::Return {
                value: value.map(|expr| self.transform_expr(expr, env)),
            },
            Stmt::Block {
                statements,
                open_span,
                close_span,
            } => Stmt::Block {
                statements: statements
                    .into_iter()
                    .map(|stmt| self.transform_stmt(stmt, env))
                    .collect(),
                open_span,
                close_span,
            },
            Stmt::If {
                condition,
                then_branch,
                elif_branches,
                else_branch,
            } => Stmt::If {
                condition: self.transform_expr(condition, env),
                then_branch: Box::new(self.transform_stmt(*then_branch, env)),
                elif_branches: elif_branches
                    .into_iter()
                    .map(|branch| ntsc_ast::stmt::ElifBranch {
                        condition: self.transform_expr(branch.condition, env),
                        body: Box::new(self.transform_stmt(*branch.body, env)),
                        elif_span: branch.elif_span,
                    })
                    .collect(),
                else_branch: else_branch.map(|branch| Box::new(self.transform_stmt(*branch, env))),
            },
            Stmt::While { condition, body } => Stmt::While {
                condition: self.transform_expr(condition, env),
                body: Box::new(self.transform_stmt(*body, env)),
            },
            Stmt::DoWhile { body, condition } => Stmt::DoWhile {
                body: Box::new(self.transform_stmt(*body, env)),
                condition: self.transform_expr(condition, env),
            },
            Stmt::For {
                init,
                condition,
                update,
                body,
            } => Stmt::For {
                init: init.map(|stmt| Box::new(self.transform_stmt(*stmt, env))),
                condition: condition.map(|expr| self.transform_expr(expr, env)),
                update: update.map(|expr| self.transform_expr(expr, env)),
                body: Box::new(self.transform_stmt(*body, env)),
            },
            Stmt::ForIn {
                variable,
                iterable,
                body,
            } => Stmt::ForIn {
                variable,
                iterable: self.transform_expr(iterable, env),
                body: Box::new(self.transform_stmt(*body, env)),
            },
            Stmt::ForAwait {
                variable,
                producer,
                body,
            } => Stmt::ForAwait {
                variable,
                producer: self.transform_expr(producer, env),
                body: Box::new(self.transform_stmt(*body, env)),
            },
            Stmt::Class {
                name,
                generic_params,
                parent,
                body,
            } => Stmt::Class {
                name,
                generic_params,
                parent,
                body: body
                    .into_iter()
                    .map(|stmt| self.transform_stmt(stmt, env))
                    .collect(),
            },
            Stmt::AsyncFunction { .. } | Stmt::Test { .. } => statement,
            Stmt::Match {
                expression,
                cases,
                default_case,
            } => Stmt::Match {
                expression: self.transform_expr(expression, env),
                cases: cases
                    .into_iter()
                    .map(|case| ntsc_ast::stmt::MatchCase {
                        value: self.transform_expr(case.value, env),
                        pattern: case.pattern,
                        guard: case.guard.map(|guard| self.transform_expr(guard, env)),
                        body: self.transform_stmt(case.body, env),
                        case_span: case.case_span,
                    })
                    .collect(),
                default_case: default_case.map(|stmt| Box::new(self.transform_stmt(*stmt, env))),
            },
            Stmt::Try {
                try_block,
                catch_var,
                catch_block,
                finally_block,
            } => Stmt::Try {
                try_block: Box::new(self.transform_stmt(*try_block, env)),
                catch_var,
                catch_block: catch_block.map(|stmt| Box::new(self.transform_stmt(*stmt, env))),
                finally_block: finally_block.map(|stmt| Box::new(self.transform_stmt(*stmt, env))),
            },
            Stmt::Throw { value } => Stmt::Throw {
                value: self.transform_expr(value, env),
            },
            Stmt::Retry {
                count,
                body,
                catch_var,
                catch_block,
            } => Stmt::Retry {
                count: self.transform_expr(count, env),
                body: Box::new(self.transform_stmt(*body, env)),
                catch_var,
                catch_block: catch_block.map(|stmt| Box::new(self.transform_stmt(*stmt, env))),
            },
            Stmt::Unsafe { body } => Stmt::Unsafe {
                body: Box::new(self.transform_stmt(*body, env)),
            },
            Stmt::Quiet { suppressed, body } => Stmt::Quiet {
                suppressed,
                body: Box::new(self.transform_stmt(*body, env)),
            },
            Stmt::Destructure {
                is_array,
                is_tuple,
                names,
                keys,
                initializer,
            } => Stmt::Destructure {
                is_array,
                is_tuple,
                names,
                keys,
                initializer: self.transform_expr(initializer, env),
            },
            other => other,
        }
    }

    fn transform_expr(&mut self, expression: Expr, env: &HashMap<String, TypeAnnotation>) -> Expr {
        match expression {
            Expr::Call {
                callee,
                paren,
                arguments,
            } => {
                let arguments: Vec<_> = arguments
                    .into_iter()
                    .map(|arg| self.transform_expr(arg, env))
                    .collect();
                let callee = match *callee {
                    Expr::Variable { name } if self.templates.contains_key(name.lexeme()) => {
                        let template = self.templates.get(name.lexeme()).cloned();
                        match template.and_then(|template| {
                            self.infer_substitutions(&template, &arguments, env)
                                .map(|subs| (template, subs))
                        }) {
                            Some((template, substitutions)) => {
                                let specialized = self.specialize(&name, &template, &substitutions);
                                Expr::Variable { name: specialized }
                            }
                            None => Expr::Variable { name },
                        }
                    }
                    Expr::Variable { name } => {
                        self.ensure_class_specialization(name.lexeme(), name.span);
                        Expr::Variable { name }
                    }
                    other => self.transform_expr(other, env),
                };
                Expr::Call {
                    callee: Box::new(callee),
                    paren,
                    arguments,
                }
            }
            Expr::Binary { left, op, right } => Expr::Binary {
                left: Box::new(self.transform_expr(*left, env)),
                op,
                right: Box::new(self.transform_expr(*right, env)),
            },
            Expr::Unary { op, right } => Expr::Unary {
                op,
                right: Box::new(self.transform_expr(*right, env)),
            },
            Expr::PostfixUnary { op, left } => Expr::PostfixUnary {
                op,
                left: Box::new(self.transform_expr(*left, env)),
            },
            Expr::Grouping {
                expression,
                open_span,
                close_span,
            } => Expr::Grouping {
                expression: Box::new(self.transform_expr(*expression, env)),
                open_span,
                close_span,
            },
            Expr::Member { object, property } => Expr::Member {
                object: Box::new(self.transform_expr(*object, env)),
                property,
            },
            Expr::OptionalMember { object, property } => Expr::OptionalMember {
                object: Box::new(self.transform_expr(*object, env)),
                property,
            },
            Expr::Assign { name, value } => Expr::Assign {
                name,
                value: Box::new(self.transform_expr(*value, env)),
            },
            Expr::IndexGet { object, index } => Expr::IndexGet {
                object: Box::new(self.transform_expr(*object, env)),
                index: Box::new(self.transform_expr(*index, env)),
            },
            Expr::IndexSet {
                object,
                index,
                value,
            } => Expr::IndexSet {
                object: Box::new(self.transform_expr(*object, env)),
                index: Box::new(self.transform_expr(*index, env)),
                value: Box::new(self.transform_expr(*value, env)),
            },
            Expr::MemberSet {
                object,
                property,
                value,
            } => Expr::MemberSet {
                object: Box::new(self.transform_expr(*object, env)),
                property,
                value: Box::new(self.transform_expr(*value, env)),
            },
            Expr::Lambda {
                params,
                return_type,
                body,
                span,
            } => Expr::Lambda {
                params,
                return_type,
                body,
                span,
            },
            Expr::Ternary {
                condition,
                then_branch,
                else_branch,
            } => Expr::Ternary {
                condition: Box::new(self.transform_expr(*condition, env)),
                then_branch: Box::new(self.transform_expr(*then_branch, env)),
                else_branch: Box::new(self.transform_expr(*else_branch, env)),
            },
            Expr::Spread { value, op_span } => Expr::Spread {
                value: Box::new(self.transform_expr(*value, env)),
                op_span,
            },
            Expr::ObjectLiteral { properties, span } => Expr::ObjectLiteral {
                properties: properties
                    .into_iter()
                    .map(|p| ntsc_ast::expr::ObjectProperty {
                        key: p.key,
                        value: self.transform_expr(p.value, env),
                        key_span: p.key_span,
                    })
                    .collect(),
                span,
            },
            Expr::ArrayLiteral { elements, span } => Expr::ArrayLiteral {
                elements: elements
                    .into_iter()
                    .map(|e| self.transform_expr(e, env))
                    .collect(),
                span,
            },
            Expr::Await {
                callee,
                arguments,
                span,
            } => Expr::Await {
                callee: Box::new(self.transform_expr(*callee, env)),
                arguments: arguments
                    .into_iter()
                    .map(|e| self.transform_expr(e, env))
                    .collect(),
                span,
            },
            Expr::AsyncBlock {
                body,
                return_type,
                span,
            } => Expr::AsyncBlock {
                body,
                return_type,
                span,
            },
            Expr::View {
                target,
                mutable,
                keyword,
            } => Expr::View {
                target: Box::new(self.transform_expr(*target, env)),
                mutable,
                keyword,
            },
            Expr::Copy {
                expression,
                keyword,
            } => Expr::Copy {
                expression: Box::new(self.transform_expr(*expression, env)),
                keyword,
            },
            Expr::Borrow {
                target,
                mutable,
                keyword,
            } => Expr::Borrow {
                target: Box::new(self.transform_expr(*target, env)),
                mutable,
                keyword,
            },
            Expr::RawDeref { target, star } => Expr::RawDeref {
                target: Box::new(self.transform_expr(*target, env)),
                star,
            },
            Expr::RawDerefSet {
                target,
                value,
                star,
            } => Expr::RawDerefSet {
                target: Box::new(self.transform_expr(*target, env)),
                value: Box::new(self.transform_expr(*value, env)),
                star,
            },
            Expr::StructLiteral {
                class_name,
                fields,
                update,
                span,
            } => Expr::StructLiteral {
                class_name,
                fields: fields
                    .into_iter()
                    .map(|p| ntsc_ast::expr::ObjectProperty {
                        key: p.key,
                        value: self.transform_expr(p.value, env),
                        key_span: p.key_span,
                    })
                    .collect(),
                update: update.map(|e| Box::new(self.transform_expr(*e, env))),
                span,
            },
            other => other,
        }
    }

    fn infer_substitutions(
        &mut self,
        template: &GenericTemplate,
        arguments: &[Expr],
        env: &HashMap<String, TypeAnnotation>,
    ) -> Option<HashMap<String, TypeAnnotation>> {
        let Stmt::Function { params, .. } = &template.function else {
            return None;
        };
        let mut substitutions = HashMap::new();
        let generic_names: HashSet<_> = template
            .params
            .iter()
            .map(|param| param.name.lexeme())
            .collect();
        for (param, argument) in params.iter().zip(arguments) {
            let Some(annotation) = &param.type_annotation else {
                continue;
            };
            let Some(actual) = self.infer_expr_type(argument, env) else {
                self.error(
                    "cannot infer a generic type argument; add a type annotation".into(),
                    argument.span(),
                );
                return None;
            };
            if !unify(annotation, &actual, &generic_names, &mut substitutions) {
                self.error(
                    "generic arguments infer conflicting types".into(),
                    argument.span(),
                );
                return None;
            }
        }
        for generic in &template.params {
            let Some(actual) = substitutions.get(generic.name.lexeme()) else {
                self.error(
                    format!(
                        "cannot infer generic type parameter `{}`",
                        generic.name.lexeme()
                    ),
                    generic.name.span,
                );
                return None;
            };
            for bound in &generic.bounds {
                if !self
                    .implementations
                    .contains(&(bound.lexeme().to_string(), type_key(actual)))
                {
                    self.error(
                        format!(
                            "type `{actual:?}` does not implement trait `{}`",
                            bound.lexeme()
                        ),
                        actual_span(actual, generic.name.span),
                    );
                    return None;
                }
            }
        }
        Some(substitutions)
    }

    fn specialize(
        &mut self,
        name: &Token,
        template: &GenericTemplate,
        substitutions: &HashMap<String, TypeAnnotation>,
    ) -> Token {
        let key = format_key(substitutions);
        if let Some(existing) = self
            .specializations
            .get(&(name.lexeme().to_string(), key.clone()))
        {
            return Token::new(TokenKind::Identifier(existing.clone()), name.span);
        }
        let mangled = format!("__generic_{}_{}", name.lexeme(), key);
        self.specializations
            .insert((name.lexeme().to_string(), key), mangled.clone());
        let Stmt::Function {
            name: original_name,
            params,
            return_type,
            body,
            ..
        } = &template.function
        else {
            return Token::new(TokenKind::Identifier(mangled), name.span);
        };
        let substitutions =
            self.specialization_substitutions(&template.params, substitutions, name.span);
        let params: Vec<FunctionParam> = params
            .iter()
            .map(|param| FunctionParam {
                name: param.name.clone(),
                type_annotation: param
                    .type_annotation
                    .as_ref()
                    .map(|ty| substitute_type(ty, &substitutions)),
            })
            .collect();
        let return_type = return_type.as_ref().map(|ret| ReturnType {
            ty: substitute_type(&ret.ty, &substitutions),
            arrow_span: ret.arrow_span,
        });
        if let Some(return_type) = &return_type {
            self.specialization_returns
                .insert(mangled.clone(), return_type.ty.clone());
        }
        let mut env = HashMap::new();
        for param in &params {
            if let Some(annotation) = &param.type_annotation {
                env.insert(param.name.lexeme().to_string(), annotation.clone());
            }
        }
        let body: Vec<Stmt> = body
            .iter()
            .cloned()
            .map(|stmt| substitute_stmt_types(stmt, &substitutions))
            .map(|stmt| self.transform_stmt(stmt, &mut env))
            .collect();
        self.generated.push(Stmt::Function {
            name: Token::new(TokenKind::Identifier(mangled.clone()), original_name.span),
            generic_params: Vec::new(),
            params,
            return_type,
            body,
        });
        Token::new(TokenKind::Identifier(mangled), name.span)
    }

    fn infer_expr_type(
        &self,
        expression: &Expr,
        env: &HashMap<String, TypeAnnotation>,
    ) -> Option<TypeAnnotation> {
        match expression {
            Expr::Literal {
                value: LiteralValue::Number(value),
                ..
            } => Some(if value.contains('.') {
                TypeAnnotation::Float
            } else {
                TypeAnnotation::Int
            }),
            Expr::Literal {
                value: LiteralValue::String(_),
                ..
            } => Some(TypeAnnotation::String),
            Expr::Literal {
                value: LiteralValue::Bool(_),
                ..
            } => Some(TypeAnnotation::Bool),
            Expr::Literal { .. } => Some(TypeAnnotation::Any),
            Expr::Variable { name } => env.get(name.lexeme()).cloned().or_else(|| {
                (self.classes.contains(name.lexeme())
                    || self.class_templates.contains_key(name.lexeme())
                    || split_applied_name(name.lexeme()).is_some())
                .then(|| TypeAnnotation::Named(name.clone()))
            }),
            Expr::Call { callee, .. } => match callee.as_ref() {
                Expr::Variable { name }
                    if self.classes.contains(name.lexeme())
                        || self.class_templates.contains_key(name.lexeme())
                        || split_applied_name(name.lexeme()).is_some() =>
                {
                    Some(TypeAnnotation::Named(name.clone()))
                }
                Expr::Variable { name } => self.specialization_returns.get(name.lexeme()).cloned(),
                _ => None,
            },
            Expr::Grouping { expression, .. } => self.infer_expr_type(expression, env),
            _ => None,
        }
    }
}

/// Every value expression returned anywhere inside a function body,
/// including from nested control-flow blocks.
fn collect_return_expressions(body: &[Stmt]) -> Vec<&Expr> {
    let mut returns = Vec::new();
    for statement in body {
        collect_returns_from_stmt(statement, &mut returns);
    }
    returns
}

fn collect_returns_from_stmt<'a>(statement: &'a Stmt, returns: &mut Vec<&'a Expr>) {
    match statement {
        Stmt::Return { value: Some(value) } => returns.push(value),
        Stmt::Return { value: None } => {}
        Stmt::Block { statements, .. } => {
            for nested in statements {
                collect_returns_from_stmt(nested, returns);
            }
        }
        Stmt::If {
            then_branch,
            elif_branches,
            else_branch,
            ..
        } => {
            collect_returns_from_stmt(then_branch, returns);
            for branch in elif_branches {
                collect_returns_from_stmt(&branch.body, returns);
            }
            if let Some(branch) = else_branch {
                collect_returns_from_stmt(branch, returns);
            }
        }
        Stmt::While { body, .. }
        | Stmt::DoWhile { body, .. }
        | Stmt::For { body, .. }
        | Stmt::ForIn { body, .. }
        | Stmt::ForAwait { body, .. }
        | Stmt::Unsafe { body }
        | Stmt::Quiet { body, .. } => collect_returns_from_stmt(body, returns),
        Stmt::Match {
            cases,
            default_case,
            ..
        } => {
            for case in cases {
                collect_returns_from_stmt(&case.body, returns);
            }
            if let Some(default_case) = default_case {
                collect_returns_from_stmt(default_case, returns);
            }
        }
        Stmt::Try {
            try_block,
            catch_block,
            finally_block,
            ..
        } => {
            collect_returns_from_stmt(try_block, returns);
            if let Some(catch_block) = catch_block {
                collect_returns_from_stmt(catch_block, returns);
            }
            if let Some(finally_block) = finally_block {
                collect_returns_from_stmt(finally_block, returns);
            }
        }
        Stmt::Retry {
            body, catch_block, ..
        } => {
            collect_returns_from_stmt(body, returns);
            if let Some(catch_block) = catch_block {
                collect_returns_from_stmt(catch_block, returns);
            }
        }
        _ => {}
    }
}

/// Whether a method declaration mentions `impl Trait` anywhere in its
/// parameter or return annotations.
fn declaration_mentions_impl_trait(statement: &Stmt) -> bool {
    let Stmt::Function {
        params,
        return_type,
        ..
    } = statement
    else {
        return false;
    };
    let mentions = |annotation: &TypeAnnotation| {
        fn probe(annotation: &TypeAnnotation) -> bool {
            match annotation {
                TypeAnnotation::ImplTrait(_) => true,
                TypeAnnotation::Array(inner) | TypeAnnotation::Slice(inner) => {
                    inner.as_deref().is_some_and(probe)
                }
                TypeAnnotation::Option(inner)
                | TypeAnnotation::View(inner, _)
                | TypeAnnotation::Shared(inner)
                | TypeAnnotation::Own(inner)
                | TypeAnnotation::Ref(inner, _)
                | TypeAnnotation::RawPointer(inner, _) => probe(inner),
                _ => false,
            }
        }
        probe(annotation)
    };
    params
        .iter()
        .any(|param| param.type_annotation.as_ref().is_some_and(&mentions))
        || return_type.as_ref().is_some_and(|ret| mentions(&ret.ty))
}

fn function_name(statement: &Stmt) -> Option<&str> {
    match statement {
        Stmt::Function { name, .. } => Some(name.lexeme()),
        _ => None,
    }
}

fn same_signature(left: &Stmt, right: &Stmt) -> bool {
    let (
        Stmt::Function {
            params: left_params,
            return_type: left_return,
            ..
        },
        Stmt::Function {
            params: right_params,
            return_type: right_return,
            ..
        },
    ) = (left, right)
    else {
        return false;
    };
    left_params.len() == right_params.len()
        && left_params.iter().zip(right_params).all(|(a, b)| {
            match (&a.type_annotation, &b.type_annotation) {
                (Some(left), Some(right)) => same_type(left, right),
                (None, None) => true,
                _ => false,
            }
        })
        && match (left_return, right_return) {
            (Some(left), Some(right)) => same_type(&left.ty, &right.ty),
            (None, None) => true,
            _ => false,
        }
}

fn same_type(left: &TypeAnnotation, right: &TypeAnnotation) -> bool {
    match (left, right) {
        (TypeAnnotation::Named(left), TypeAnnotation::Named(right)) => {
            left.lexeme() == right.lexeme()
        }
        (TypeAnnotation::Array(left), TypeAnnotation::Array(right))
        | (TypeAnnotation::Slice(left), TypeAnnotation::Slice(right)) => match (left, right) {
            (Some(left), Some(right)) => same_type(left, right),
            (None, None) => true,
            _ => false,
        },
        (TypeAnnotation::Option(left), TypeAnnotation::Option(right))
        | (TypeAnnotation::Shared(left), TypeAnnotation::Shared(right))
        | (TypeAnnotation::Own(left), TypeAnnotation::Own(right)) => same_type(left, right),
        (TypeAnnotation::View(left, left_mut), TypeAnnotation::View(right, right_mut))
        | (TypeAnnotation::Ref(left, left_mut), TypeAnnotation::Ref(right, right_mut))
        | (
            TypeAnnotation::RawPointer(left, left_mut),
            TypeAnnotation::RawPointer(right, right_mut),
        ) => left_mut == right_mut && same_type(left, right),
        (TypeAnnotation::Dyn(left), TypeAnnotation::Dyn(right))
        | (TypeAnnotation::ImplTrait(left), TypeAnnotation::ImplTrait(right)) => {
            left.lexeme() == right.lexeme()
        }
        _ => std::mem::discriminant(left) == std::mem::discriminant(right),
    }
}

fn stmt_span(statement: &Stmt) -> Span {
    match statement {
        Stmt::Function { name, .. } => name.span,
        _ => Span::dummy(),
    }
}

fn unify(
    pattern: &TypeAnnotation,
    actual: &TypeAnnotation,
    generic_names: &HashSet<&str>,
    substitutions: &mut HashMap<String, TypeAnnotation>,
) -> bool {
    match (pattern, actual) {
        (TypeAnnotation::Named(token), actual) if generic_names.contains(token.lexeme()) => {
            match substitutions.get(token.lexeme()) {
                Some(previous) => previous == actual,
                None => {
                    substitutions.insert(token.lexeme().to_string(), actual.clone());
                    true
                }
            }
        }
        (TypeAnnotation::Array(Some(pattern)), TypeAnnotation::Array(Some(actual)))
        | (TypeAnnotation::Option(pattern), TypeAnnotation::Option(actual))
        | (TypeAnnotation::Shared(pattern), TypeAnnotation::Shared(actual))
        | (TypeAnnotation::Own(pattern), TypeAnnotation::Own(actual)) => {
            unify(pattern, actual, generic_names, substitutions)
        }
        (TypeAnnotation::Slice(Some(pattern)), TypeAnnotation::Slice(Some(actual))) => {
            unify(pattern, actual, generic_names, substitutions)
        }
        (TypeAnnotation::View(pattern, pattern_mut), TypeAnnotation::View(actual, actual_mut))
        | (TypeAnnotation::Ref(pattern, pattern_mut), TypeAnnotation::Ref(actual, actual_mut))
        | (
            TypeAnnotation::RawPointer(pattern, pattern_mut),
            TypeAnnotation::RawPointer(actual, actual_mut),
        ) => pattern_mut == actual_mut && unify(pattern, actual, generic_names, substitutions),
        (TypeAnnotation::View(pattern, _), actual) => {
            unify(pattern, actual, generic_names, substitutions)
        }
        _ => true,
    }
}

fn substitute_type(
    ty: &TypeAnnotation,
    substitutions: &HashMap<String, TypeAnnotation>,
) -> TypeAnnotation {
    match ty {
        TypeAnnotation::Named(token) => substitutions
            .get(token.lexeme())
            .cloned()
            .unwrap_or_else(|| ty.clone()),
        TypeAnnotation::Array(inner) => TypeAnnotation::Array(
            inner
                .as_ref()
                .map(|inner| Box::new(substitute_type(inner, substitutions))),
        ),
        TypeAnnotation::Option(inner) => {
            TypeAnnotation::Option(Box::new(substitute_type(inner, substitutions)))
        }
        TypeAnnotation::View(inner, mutable) => {
            TypeAnnotation::View(Box::new(substitute_type(inner, substitutions)), *mutable)
        }
        TypeAnnotation::Shared(inner) => {
            TypeAnnotation::Shared(Box::new(substitute_type(inner, substitutions)))
        }
        TypeAnnotation::Slice(inner) => TypeAnnotation::Slice(
            inner
                .as_ref()
                .map(|inner| Box::new(substitute_type(inner, substitutions))),
        ),
        TypeAnnotation::Own(inner) => {
            TypeAnnotation::Own(Box::new(substitute_type(inner, substitutions)))
        }
        TypeAnnotation::Ref(inner, mutable) => {
            TypeAnnotation::Ref(Box::new(substitute_type(inner, substitutions)), *mutable)
        }
        TypeAnnotation::RawPointer(inner, mutable) => {
            TypeAnnotation::RawPointer(Box::new(substitute_type(inner, substitutions)), *mutable)
        }
        _ => ty.clone(),
    }
}

fn substitute_stmt_types(statement: Stmt, substitutions: &HashMap<String, TypeAnnotation>) -> Stmt {
    match statement {
        Stmt::Function {
            name,
            generic_params,
            params,
            return_type,
            body,
        } => Stmt::Function {
            name,
            generic_params,
            params: params
                .into_iter()
                .map(|param| FunctionParam {
                    name: param.name,
                    type_annotation: param
                        .type_annotation
                        .map(|ty| substitute_type(&ty, substitutions)),
                })
                .collect(),
            return_type: return_type.map(|ret| ReturnType {
                ty: substitute_type(&ret.ty, substitutions),
                arrow_span: ret.arrow_span,
            }),
            body: body
                .into_iter()
                .map(|stmt| substitute_stmt_types(stmt, substitutions))
                .collect(),
        },
        Stmt::Var {
            name,
            type_annotation,
            initializer,
            is_static,
            is_const,
            view,
        } => Stmt::Var {
            name,
            type_annotation: type_annotation.map(|ty| substitute_type(&ty, substitutions)),
            initializer,
            is_static,
            is_const,
            view,
        },
        Stmt::Block {
            statements,
            open_span,
            close_span,
        } => Stmt::Block {
            statements: statements
                .into_iter()
                .map(|stmt| substitute_stmt_types(stmt, substitutions))
                .collect(),
            open_span,
            close_span,
        },
        other => other,
    }
}

fn format_key(substitutions: &HashMap<String, TypeAnnotation>) -> String {
    let mut entries: Vec<_> = substitutions.iter().collect();
    entries.sort_by_key(|(name, _)| *name);
    entries
        .into_iter()
        .map(|(_, ty)| type_key(ty))
        .collect::<Vec<_>>()
        .join("_")
}

fn type_key(ty: &TypeAnnotation) -> String {
    match ty {
        TypeAnnotation::Named(token) => token.lexeme().to_string(),
        TypeAnnotation::Int => "int".into(),
        TypeAnnotation::Float => "float".into(),
        TypeAnnotation::String => "string".into(),
        TypeAnnotation::Bool => "bool".into(),
        _ => ty.label().replace([' ', '[', ']'], "_"),
    }
}

fn actual_span(_: &TypeAnnotation, fallback: Span) -> Span {
    fallback
}

fn split_applied_name(name: &str) -> Option<(&str, Vec<&str>)> {
    let open = name.find('<')?;
    let close = name.rfind('>')?;
    if close <= open || close != name.len() - 1 {
        return None;
    }
    let base = &name[..open];
    let source = &name[open + 1..close];
    let mut arguments = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (index, character) in source.char_indices() {
        match character {
            '<' | '[' => depth += 1,
            '>' | ']' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                arguments.push(source[start..index].trim());
                start = index + 1;
            }
            _ => {}
        }
    }
    arguments.push(source[start..].trim());
    Some((base, arguments))
}

fn parse_type_source(source: &str, span: Span) -> TypeAnnotation {
    match source.trim() {
        "int" => TypeAnnotation::Int,
        "float" => TypeAnnotation::Float,
        "string" => TypeAnnotation::String,
        "bool" => TypeAnnotation::Bool,
        value => TypeAnnotation::Named(Token::new(TokenKind::Identifier(value.to_string()), span)),
    }
}

fn type_annotation_name(annotation: &TypeAnnotation) -> String {
    match annotation {
        TypeAnnotation::Named(token) => token.lexeme().to_string(),
        TypeAnnotation::Int => "int".into(),
        TypeAnnotation::Float => "float".into(),
        TypeAnnotation::String => "string".into(),
        TypeAnnotation::Bool => "bool".into(),
        _ => annotation.label().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &str) -> Program {
        let tokens = ntsc_lexer::tokenize(source);
        ntsc_parser::parse(&tokens).expect("test source should parse")
    }

    #[test]
    fn prepares_generic_functions_and_trait_impls() {
        let program = parse(
            r#"
            trait Printable {
                fun format() -> string
            }
            class User { var int id fun init() {} }
            impl Printable for User {
                fun format() -> string { return "user" }
            }
            fun identity<T>(T value) -> T { return value }
            fun show<T: Printable>(view T value) { say(value.format()) }
            fun main() {
                var int answer = identity(41)
                var User user = User()
                show(user)
                answer = identity(answer)
            }
            "#,
        );
        let prepared = prepare_program(&program).expect("program should prepare");
        assert!(
            prepared
                .statements
                .iter()
                .all(|statement| !matches!(statement, Stmt::Trait { .. } | Stmt::Impl { .. }))
        );
        assert!(prepared.statements.iter().any(|statement| matches!(statement, Stmt::Function { name, .. } if name.lexeme() == "__generic_identity_int")));
        assert!(prepared.statements.iter().any(|statement| matches!(statement, Stmt::Function { name, .. } if name.lexeme() == "__generic_show_User")));
        crate::resolve::check_prepared_program(&prepared)
            .expect("prepared program should type-check");
    }

    #[test]
    fn rejects_unsatisfied_trait_bound() {
        let program = parse(
            r#"
            trait Printable { fun format() -> string }
            class User { var int id fun init() {} }
            fun show<T: Printable>(view T value) { say(value.format()) }
            fun main() {
                var User user = User()
                show(user)
            }
            "#,
        );
        let errors = prepare_program(&program).expect_err("missing implementation should fail");
        assert!(errors.iter().any(|error| {
            error
                .message
                .contains("does not implement trait `Printable`")
        }));
    }

    #[test]
    fn prepares_generic_classes_and_nested_applications() {
        let program = parse(
            r#"
            class Box<T> {
                var T value
                fun init(T value) { this.value = value }
            }
            class Pair<T, U> {
                var T first
                var U second
                fun init(T first, U second) {
                    this.first = first
                    this.second = second
                }
            }
            fun main() {
                var Box<Pair<int, string> > boxed = Box<Pair<int, string> >()
            }
            "#,
        );
        let prepared = prepare_program(&program).expect("generic classes should prepare");
        assert!(prepared.statements.iter().any(|statement| {
            matches!(statement, Stmt::Class { name, generic_params, .. } if name.lexeme() == "Box<Pair<int,string>>" && generic_params.is_empty())
        }));
        assert!(prepared.statements.iter().any(|statement| {
            matches!(statement, Stmt::Class { name, generic_params, .. } if name.lexeme() == "Pair<int,string>" && generic_params.is_empty())
        }));
        crate::resolve::check_prepared_program(&prepared)
            .expect("specialized classes should type-check");
    }

    #[test]
    fn prepares_generic_enums_and_substitutes_payload_types() {
        let program = parse(
            r#"
            enum Maybe<T> { Some(T), None }
            class Holder { var Maybe<int> value }
            fun main() { }
            "#,
        );
        let prepared = prepare_program(&program).expect("generic enum should prepare");
        assert!(prepared.statements.iter().any(|statement| {
            matches!(statement, Stmt::Enum { name, generic_params, members } if name.lexeme() == "Maybe<int>" && generic_params.is_empty() && matches!(members[0].data_types.first(), Some(TypeAnnotation::Int)))
        }));
    }

    #[test]
    fn expands_type_aliases_and_where_clauses() {
        let program = parse(
            r#"
            trait Named { fun name() -> string }
            class User { var int id fun init(int id) { this.id = id } }
            impl Named for User { fun name() -> string { return "user" } }
            type Identifier = int
            type Wrapped<T> = option[T]
            fun label<T>(view T value) where T: Named { say(value.name()) }
            fun main() {
                var Identifier id = 7
                var Wrapped<int> maybe = id
                var User user = User(id)
                label(user)
            }
            "#,
        );
        let prepared = prepare_program(&program).expect("aliases should prepare");
        assert!(
            prepared
                .statements
                .iter()
                .all(|statement| !matches!(statement, Stmt::TypeAlias { .. }))
        );
        crate::resolve::check_prepared_program(&prepared).expect("aliases should type-check");
    }

    #[test]
    fn resolves_associated_types_for_generic_specializations() {
        let program = parse(
            r#"
            trait Producer {
                type Item
                fun item() -> Item
            }
            class User { var int id fun init() {} }
            impl Producer for User {
                type Item = int
                fun item() -> int { return 7 }
            }
            fun read<T: Producer>(view T value) -> T::Item {
                return value.item()
            }
            fun main() {
                var User user = User()
                var int total = read(user)
            }
            "#,
        );
        let prepared = prepare_program(&program).expect("associated types should prepare");
        assert!(prepared.statements.iter().any(|statement| {
            matches!(statement, Stmt::Function { name, return_type: Some(ret), .. } if name.lexeme() == "__generic_read_User" && ret.ty == TypeAnnotation::Int)
        }));
        crate::resolve::check_prepared_program(&prepared)
            .expect("associated type specialization should type-check");
    }

    #[test]
    fn rejects_missing_associated_type_bindings() {
        let program = parse(
            r#"
            trait Producer { type Item fun item() -> Item }
            class User { fun init() {} }
            impl Producer for User { fun item() -> int { return 7 } }
            "#,
        );
        let errors = prepare_program(&program).expect_err("missing associated type should fail");
        assert!(
            errors
                .iter()
                .any(|error| error.message.contains("requires associated type `Item`"))
        );
    }

    #[test]
    fn resolves_associated_types_in_generic_aliases_and_classes() {
        let program = parse(
            r#"
            trait Producer { type Item fun item() -> Item }
            class User { var int id fun init() {} }
            impl Producer for User {
                type Item = int
                fun item() -> int { return 7 }
            }
            type Produced<T: Producer> = T::Item
            class Holder<T: Producer> {
                var T::Item value
                fun init(T::Item value) { this.value = value }
            }
            fun main() {
                var Produced<User> value = 7
                var Holder<User> holder = Holder<User>(value)
            }
            "#,
        );
        let prepared = prepare_program(&program).expect("associated types should specialize");
        assert!(prepared.statements.iter().any(|statement| {
            matches!(statement, Stmt::Class { name, body, .. } if name.lexeme() == "Holder<User>" && matches!(&body[0], Stmt::Var { type_annotation: Some(TypeAnnotation::Int), .. }))
        }));
        crate::resolve::check_prepared_program(&prepared)
            .expect("associated aliases and classes should type-check");
    }
}
