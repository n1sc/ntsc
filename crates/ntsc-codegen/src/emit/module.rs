//! Emitter entry points: `emit_module`, test-block compilation, and top-level emission.

use super::*;

fn extern_declarations(program: &Program) -> Vec<&Stmt> {
    let mut declarations = Vec::new();
    for statement in &program.statements {
        match statement {
            Stmt::ExternFunction { .. } => declarations.push(statement),
            Stmt::ExternBlock { functions, .. } => declarations.extend(functions),
            _ => {}
        }
    }
    declarations
}

/// Emit LLVM IR for a parsed program and write it to an object file.
///
/// `optimize` runs the IR optimization pipeline (mem2reg, instcombine, ...)
/// before codegen; debug builds pass `false` so the emitted IR stays
/// predictable and the leak detector stays accurate.
pub fn emit_module(
    context: &Context,
    target_machine: &crate::context::TargetMachine,
    program: &Program,
    obj_path: &std::path::Path,
    test_mode: bool,
    report_leaks: bool,
    optimize: bool,
) -> Result<(), crate::CodegenError> {
    let _guard = crate::context::CodegenLockGuard::acquire();

    // Trait-object vtables are built from the tables the type checker
    // recorded while erasing trait declarations.
    super::dyn_obj::load_trait_tables(ntsc_typeck::take_trait_object_tables());

    // Compile-time constant values folded by the type checker.
    CONST_EVAL_VALUES.with(|map| {
        *map.borrow_mut() = ntsc_typeck::take_const_values();
    });

    // `use strings as s` binds `s` in the source; codegen translates the
    // alias back to the real stdlib module when dispatching `s.func()`.
    STDLIB_ALIASES.with(|map| {
        let mut m = map.borrow_mut();
        m.clear();
        for stmt in &program.statements {
            if let ntsc_ast::stmt::Stmt::Use {
                library,
                is_file_path: false,
                is_rust: false,
                alias: Some(alias),
                ..
            } = stmt
            {
                m.insert(alias.lexeme().to_string(), library.lexeme().to_string());
            }
        }
    });

    RUST_CRATE_NAMESPACES.with(|map| {
        let mut m = map.borrow_mut();
        m.clear();
        // Rust crates bind only through the explicit `use @crate` form —
        // the strict convention separating dependency namespaces from
        // stdlib module imports.
        for stmt in &program.statements {
            if let ntsc_ast::stmt::Stmt::Use {
                library,
                is_file_path: false,
                is_rust: true,
                alias,
                imported_symbols,
                ..
            } = stmt
                && imported_symbols.is_empty()
            {
                let crate_name = library.lexeme().to_string();
                let namespace = alias
                    .as_ref()
                    .map(|a| a.lexeme().to_string())
                    .unwrap_or_else(|| crate_name.clone());
                m.insert(namespace, crate_name);
            }
        }
    });

    RUST_SELECTIVE_IMPORTS.with(|map| {
        let mut m = map.borrow_mut();
        m.clear();
        for stmt in &program.statements {
            if let ntsc_ast::stmt::Stmt::Use {
                library,
                imported_symbols,
                imported_is_rust,
                ..
            } = stmt
                && !imported_symbols.is_empty()
            {
                for (symbol, is_rust) in imported_symbols.iter().zip(imported_is_rust.iter()) {
                    if *is_rust {
                        m.insert(symbol.lexeme().to_string(), library.lexeme().to_string());
                    }
                }
            }
        }
    });

    let externs = extern_declarations(program);
    FFI_FUNCTIONS.with(|functions| {
        let mut functions = functions.borrow_mut();
        functions.clear();
        functions.extend(externs.iter().filter_map(|statement| match statement {
            Stmt::ExternFunction { name, .. } => Some(name.lexeme().to_string()),
            _ => None,
        }));
    });

    let module = context.create_module("ntsc_main");

    // Anchor the module to the target machine's ABI. The optimization pass
    // pipeline resolves `getelementptr` byte offsets and struct sizes against
    // the module's data layout; without an explicit one it falls back to a
    // default where `i64` is 4-byte aligned, which corrupts every struct that
    // embeds `i64`/`double` fields (e.g. the async sleep future) on x86-64.
    module.set_data_layout(&target_machine.get_target_data().get_data_layout());

    CLASS_FIELDS.with(|map| {
        let mut m = map.borrow_mut();
        m.clear();
        for stmt in &program.statements {
            if let ntsc_ast::stmt::Stmt::Class { name, body, .. } = stmt {
                let mut fields = Vec::new();
                for item in body {
                    if let ntsc_ast::stmt::Stmt::Var {
                        name: field_name, ..
                    } = item
                    {
                        fields.push(field_name.lexeme().to_string());
                    }
                }
                m.insert(name.lexeme().to_string(), fields);
            }
        }
    });

    CLASS_FIELD_TYPES.with(|map| {
        let mut m = map.borrow_mut();
        m.clear();
        for stmt in &program.statements {
            if let ntsc_ast::stmt::Stmt::Class { name, body, .. } = stmt {
                let mut field_tys = Vec::new();
                for item in body {
                    if let ntsc_ast::stmt::Stmt::Var {
                        type_annotation,
                        initializer,
                        ..
                    } = item
                    {
                        let ty = if let Some(ann) = type_annotation {
                            type_annotation_to_ty(&Some(ann.clone()))
                        } else if let Some(init) = initializer {
                            expr_to_literal_ty(init).unwrap_or(Ty::Any)
                        } else {
                            Ty::Any
                        };
                        field_tys.push(ty);
                    }
                }
                m.insert(name.lexeme().to_string(), field_tys);
            }
        }
    });

    CLASS_FIELD_INITS.with(|map| {
        let mut m = map.borrow_mut();
        m.clear();
        for stmt in &program.statements {
            if let ntsc_ast::stmt::Stmt::Class { name, body, .. } = stmt {
                let mut inits = Vec::new();
                for item in body {
                    if let ntsc_ast::stmt::Stmt::Var { initializer, .. } = item {
                        inits.push(initializer.clone());
                    }
                }
                m.insert(name.lexeme().to_string(), inits);
            }
        }
    });

    CLASS_PARENTS.with(|map| {
        let mut m = map.borrow_mut();
        m.clear();
        for stmt in &program.statements {
            if let ntsc_ast::stmt::Stmt::Class { name, parent, .. } = stmt
                && let Some(parent) = parent
            {
                m.insert(name.lexeme().to_string(), parent.lexeme().to_string());
            }
        }
    });

    ENUM_VALUES.with(|map| {
        let mut enums = map.borrow_mut();
        enums.clear();
        ENUM_MEMBER_VALUES.with(|globals| {
            let mut global_members = globals.borrow_mut();
            global_members.clear();
            for stmt in &program.statements {
                if let ntsc_ast::stmt::Stmt::Enum { name, members, .. } = stmt {
                    let mut next = 0_i32;
                    let mut members_map = HashMap::new();
                    for member in members {
                        // Explicit `= value` must be a constant int literal;
                        // anything else falls back to the running counter.
                        let value = member
                            .value
                            .as_ref()
                            .and_then(const_int_expr_value)
                            .unwrap_or(next);
                        members_map.insert(member.name.lexeme().to_string(), value);
                        global_members.insert(member.name.lexeme().to_string(), value);
                        next = value.wrapping_add(1);
                    }
                    enums.insert(name.lexeme().to_string(), members_map);
                }
            }
        });
    });

    STATIC_CONST_TYPES.with(|map| map.borrow_mut().clear());
    STATIC_CONST_INITS.with(|map| map.borrow_mut().clear());

    CLASS_METHOD_TYPES.with(|map| {
        let mut m = map.borrow_mut();
        m.clear();
        for stmt in &program.statements {
            if let ntsc_ast::stmt::Stmt::Class { name, body, .. } = stmt {
                let mut methods = HashMap::new();
                for item in body {
                    if let ntsc_ast::stmt::Stmt::Function {
                        name: method_name,
                        return_type,
                        ..
                    } = item
                    {
                        methods.insert(
                            method_name.lexeme().to_string(),
                            function_return_ty(return_type),
                        );
                    }
                }
                m.insert(name.lexeme().to_string(), methods);
            }
        }
    });

    CLASS_METHOD_PARAM_TYPES.with(|map| {
        let mut m = map.borrow_mut();
        m.clear();
        for stmt in &program.statements {
            if let ntsc_ast::stmt::Stmt::Class { name, body, .. } = stmt {
                let mut methods = HashMap::new();
                for item in body {
                    if let ntsc_ast::stmt::Stmt::Function {
                        name: method_name,
                        params,
                        ..
                    } = item
                    {
                        let param_tys = params
                            .iter()
                            .map(|p| type_annotation_to_ty(&p.type_annotation))
                            .collect();
                        methods.insert(method_name.lexeme().to_string(), param_tys);
                    }
                }
                m.insert(name.lexeme().to_string(), methods);
            }
        }
    });

    FUNCTION_RETURN_TYPES.with(|map| {
        let mut m = map.borrow_mut();
        m.clear();
        for stmt in &program.statements {
            if let Stmt::Function {
                name, return_type, ..
            } = stmt
                && return_type.is_some()
            {
                m.insert(name.lexeme().to_string(), function_return_ty(return_type));
            }
        }
        for stmt in &externs {
            if let Stmt::ExternFunction {
                name, return_type, ..
            } = stmt
            {
                m.insert(name.lexeme().to_string(), function_return_ty(return_type));
            }
        }
    });

    FUNCTION_PARAM_TYPES.with(|map| {
        let mut m = map.borrow_mut();
        m.clear();
        for stmt in &program.statements {
            if let Stmt::Function { name, params, .. } = stmt {
                let param_tys = params
                    .iter()
                    .map(|p| type_annotation_to_ty(&p.type_annotation))
                    .collect();
                m.insert(name.lexeme().to_string(), param_tys);
            }
        }
        for stmt in &externs {
            if let Stmt::ExternFunction { name, params, .. } = stmt {
                let param_tys = params
                    .iter()
                    .map(|param| type_annotation_to_ty(&param.type_annotation))
                    .collect();
                m.insert(name.lexeme().to_string(), param_tys);
            }
        }
    });

    // Which free functions hand a fresh Rust object back to their caller, so
    // a call site tracks and releases the handle instead of leaking it (the
    // manifest and crate namespaces above are already loaded, which this
    // classification consults).
    super::set_user_object_returns(super::call::compute_user_object_returns(program));

    // Declare runtime functions.
    declare_runtime_functions(&module);

    // Declare every top-level function up front so call sites resolve
    // regardless of definition order (required once `use` imports merge
    // multiple modules into one program). The user `main` becomes
    // `__ntsc_user_main`; in test mode it is skipped entirely because the
    // harness main replaces the entry point.
    for stmt in &program.statements {
        if let Stmt::Function {
            name,
            params,
            return_type,
            ..
        } = stmt
        {
            let fn_name = if name.lexeme() == "main" {
                "__ntsc_user_main"
            } else {
                name.lexeme()
            };
            if test_mode && name.lexeme() == "main" {
                continue;
            }
            if module.get_function(fn_name).is_some() {
                continue;
            }
            let (fn_ty, _param_tys) = fn_type_from_params(context, params, return_type);
            module.add_function(fn_name, fn_ty, Some(inkwell::module::Linkage::External));
        }
    }

    for statement in externs {
        let Stmt::ExternFunction {
            name,
            params,
            return_type,
            c_style,
            ..
        } = statement
        else {
            continue;
        };
        if module.get_function(name.lexeme()).is_none() {
            let fn_ty = ffi_function_type(context, params, return_type, *c_style);
            module.add_function(
                name.lexeme(),
                fn_ty,
                Some(inkwell::module::Linkage::External),
            );
        }
    }

    let mut test_names: Vec<String> = Vec::new();
    let mut async_done: HashSet<String> = HashSet::new();
    let mut async_in_progress: HashSet<String> = HashSet::new();

    // Pre-pass: emit the futures of every `go` callee and `go { }` block
    // before any body is compiled, so a spawn site can reference their
    // struct and poll functions regardless of definition order.
    let mut go_statements: Vec<&Stmt> = Vec::new();
    for stmt in &program.statements {
        collect_go_statements(stmt, &mut go_statements);
    }
    for go_stmt in go_statements {
        let Stmt::Go {
            call,
            block,
            keyword_span,
        } = go_stmt
        else {
            continue;
        };
        if let Some(block) = block {
            let name = format!("__ntsc_go_{}", keyword_span.start);
            let captures = ntsc_typeck::go_captures(keyword_span.start);
            super::async_sm::emit_go_block_future(
                context,
                &module,
                program,
                &name,
                block,
                &captures,
                &mut async_done,
                &mut async_in_progress,
            )?;
            super::async_sm::emit_goroutine_trampoline(context, &module, &name)?;
        } else if let Expr::Call { callee, .. } = call
            && let Expr::Variable { name } = callee.as_ref()
            && let Some(callee_decl) = program.statements.iter().find(
                |s| matches!(s, Stmt::AsyncFunction { name: n, .. } if n.lexeme() == name.lexeme()),
            )
        {
            emit_async_function(
                context,
                &module,
                program,
                callee_decl,
                &mut async_done,
                &mut async_in_progress,
            )?;
            super::async_sm::emit_goroutine_trampoline(context, &module, name.lexeme())?;
        }
    }

    for stmt in &program.statements {
        match stmt {
            Stmt::Test { name, body } => {
                emit_test_function(context, &module, name, body)?;
                test_names.push(name.lexeme().to_string());
            }
            Stmt::Function { name, .. } if test_mode && name.lexeme() == "main" => {}
            Stmt::AsyncFunction { name, .. } if test_mode && name.lexeme() == "main" => {}
            Stmt::AsyncFunction { .. } => {
                emit_async_function(
                    context,
                    &module,
                    program,
                    stmt,
                    &mut async_done,
                    &mut async_in_progress,
                )?;
            }
            _ => emit_top_level(context, &module, program, stmt)?,
        }
    }

    if test_mode {
        emit_test_harness(context, &module, &test_names)?;
    }

    emit_c_main_wrapper(&module, report_leaks)?;

    // Vtable slots reference functions defined anywhere in the module, so
    // they can only be filled once every body exists.
    super::dyn_obj::finalize_trait_vtables(context, &module)?;

    // Validate before optimizing so mistakes surface with full IR context.
    if let Err(msg) = module.verify() {
        if std::env::var("NTSC_DUMP_IR").is_ok() {
            eprintln!("===== DUMP (verify failed) =====");
            eprintln!("{}", module.print_to_string().to_string());
        }
        return Err(crate::CodegenError::LLVMError(format!(
            "LLVM module verification failed: {msg}"
        )));
    }

    if std::env::var("NTSC_DUMP_IR").is_ok() {
        eprintln!("===== DUMP =====");
        eprintln!("{}", module.print_to_string().to_string());
    }

    if optimize {
        // Where the alloca-based variable slots become SSA registers;
        // skipping it leaves every loop induction variable on the stack
        // (~3% vs ~70% speedup).
        crate::context::run_optimization_passes(&module, target_machine)?;
    }

    crate::context::write_object_file(&module, target_machine, obj_path)
}

/// Recursively collect every `Stmt::Go` (in function bodies, control flow,
/// classes, and tests) for the goroutine-future pre-pass.
fn collect_go_statements<'a>(stmt: &'a Stmt, out: &mut Vec<&'a Stmt>) {
    if matches!(stmt, Stmt::Go { .. }) {
        out.push(stmt);
    }
    match stmt {
        Stmt::Block { statements, .. } => {
            for inner in statements {
                collect_go_statements(inner, out);
            }
        }
        Stmt::If {
            then_branch,
            elif_branches,
            else_branch,
            ..
        } => {
            collect_go_statements(then_branch, out);
            for branch in elif_branches {
                collect_go_statements(&branch.body, out);
            }
            if let Some(else_branch) = else_branch {
                collect_go_statements(else_branch, out);
            }
        }
        Stmt::While { body, .. }
        | Stmt::DoWhile { body, .. }
        | Stmt::Retry { body, .. }
        | Stmt::Unsafe { body, .. }
        | Stmt::Quiet { body, .. } => collect_go_statements(body, out),
        Stmt::For { init, body, .. } => {
            if let Some(init) = init {
                collect_go_statements(init, out);
            }
            collect_go_statements(body, out);
        }
        Stmt::ForIn { body, .. } | Stmt::ForAwait { body, .. } | Stmt::ChanRecvFor { body, .. } => {
            collect_go_statements(body, out)
        }
        Stmt::Match {
            cases,
            default_case,
            ..
        } => {
            for case in cases {
                collect_go_statements(&case.body, out);
            }
            if let Some(default_case) = default_case {
                collect_go_statements(default_case, out);
            }
        }
        Stmt::Try {
            try_block,
            catch_block,
            finally_block,
            ..
        } => {
            collect_go_statements(try_block, out);
            if let Some(catch_block) = catch_block {
                collect_go_statements(catch_block, out);
            }
            if let Some(finally_block) = finally_block {
                collect_go_statements(finally_block, out);
            }
        }
        Stmt::Function { body, .. }
        | Stmt::AsyncFunction { body, .. }
        | Stmt::Test { body, .. } => {
            for inner in body {
                collect_go_statements(inner, out);
            }
        }
        Stmt::Class { body, .. } => {
            for member in body {
                collect_go_statements(member, out);
            }
        }
        _ => {}
    }
}

/// Emit a top-level `go` statement inside a synthetic void function, so a
/// script body can spawn goroutines without a `main`.
fn emit_top_level_go<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    stmt: &Stmt,
) -> Result<(), crate::CodegenError> {
    let Stmt::Go { keyword_span, .. } = stmt else {
        return Ok(());
    };
    let fn_name = format!("__ntsc_init_go_{}", keyword_span.start);
    if module.get_function(&fn_name).is_some() {
        return Ok(());
    }
    let function = module.add_function(
        &fn_name,
        context.void_type().fn_type(&[], false),
        Some(inkwell::module::Linkage::Internal),
    );
    let entry = context.append_basic_block(function, "entry");
    let builder = context.create_builder();
    builder.position_at_end(entry);
    let entry_builder = context.create_builder();
    entry_builder.position_at_end(entry);
    let mut fn_ctx = FunctionContext::new(
        function,
        &builder,
        &entry_builder,
        entry,
        module,
        Ty::Void,
        context,
    );
    super::async_sm::emit_go_program_spawn(&mut fn_ctx, stmt)?;
    emit_exception_return(&mut fn_ctx, &Ty::Void, context)?;
    emit_drop_all_owned(&mut fn_ctx)?;
    builder.build_return(None)?;
    Ok(())
}

/// Emit a `test name { body }` block as a no-argument void function
/// named `test_<name>`.
pub(crate) fn emit_test_function<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    name: &ntsc_ast::token::Token,
    body: &[Stmt],
) -> Result<(), crate::CodegenError> {
    let fn_name = format!("test_{}", name.lexeme());
    let (fn_ty, _param_tys) = fn_type_from_params(context, &[], &None);
    let function = module.add_function(&fn_name, fn_ty, Some(inkwell::module::Linkage::External));

    if body.is_empty() {
        // An empty `test` block passes; it still needs a body so the
        // generated harness has something to call.
        let entry = context.append_basic_block(function, "entry");
        let builder = context.create_builder();
        builder.position_at_end(entry);
        builder.build_return(None)?;
        return Ok(());
    }

    let builder = context.create_builder();
    let entry_bb = context.append_basic_block(function, "entry");
    builder.position_at_end(entry_bb);

    let entry_builder = context.create_builder();
    entry_builder.position_at_end(entry_bb);

    let mut fn_ctx = FunctionContext::new(
        function,
        &builder,
        &entry_builder,
        entry_bb,
        module,
        Ty::Void,
        context,
    );
    fn_ctx.stack_allocated = analyze_stack_allocatable(body, module);
    fn_ctx.class_drops = analyze_class_drops(body, module, &[]);

    for stmt in body {
        emit_statement_in_function(&mut fn_ctx, stmt)?;
    }

    emit_exception_return(&mut fn_ctx, &Ty::Void, context)?;
    let current_block = fn_ctx.builder.get_insert_block().unwrap();
    if current_block.get_terminator().is_none() {
        emit_drop_all_owned(&mut fn_ctx)?;
        fn_ctx.builder.build_return(None)?;
    }
    Ok(())
}

/// Generate the test harness `main` from an NTSC source template, parse it,
/// and emit it: each `test_<name>` runs inside a `try`/`catch`, prints
/// `PASS <name>` / `FAIL <name>: <msg>`, prints a summary, and returns a
/// non-zero exit code when any test failed. Reusing the language's own
/// parser keeps the harness IR consistent with normal codegen.
pub(crate) fn emit_test_harness<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    test_names: &[String],
) -> Result<(), crate::CodegenError> {
    let mut source =
        String::from("fun main() -> int {\n    var __pass = 0;\n    var __fail = 0;\n");
    for name in test_names {
        source.push_str(&format!(
            "    try {{\n        test_{name}();\n        say(\"PASS {name}\");\n        __pass = __pass + 1;\n    }} catch (err) {{\n        say(\"FAIL {name}: \" + err);\n        __fail = __fail + 1;\n    }}\n"
        ));
    }
    source.push_str(&format!(
        "    say(\"{} \" + __pass + \" passed, \" + __fail + \" failed\");\n",
        crate::SUMMARY_MARKER
    ));
    source.push_str("    if (__fail > 0) {\n        return 1;\n    }\n    return 0;\n}\n");

    let tokens = ntsc_lexer::tokenize(&source);
    let harness = ntsc_parser::parse(&tokens).map_err(crate::CodegenError::Parse)?;

    for stmt in &harness.statements {
        emit_top_level(context, module, &harness, stmt)?;
    }
    Ok(())
}

// ── Top-level emission ──────────────────────────────────────────────────

pub(crate) fn emit_top_level<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    _program: &Program,
    stmt: &Stmt,
) -> Result<(), crate::CodegenError> {
    match stmt {
        Stmt::Function {
            name,
            params,
            return_type,
            body,
            ..
        } => {
            emit_function(context, module, name, params, return_type, body)?;
        }
        Stmt::AsyncFunction { .. } => {}
        Stmt::Class {
            name, parent, body, ..
        } => {
            emit_class(context, module, name, parent, body)?;
        }
        Stmt::Expression { expression } => {
            emit_implicit_init(context, module, expression)?;
        }
        Stmt::Go { .. } => {
            emit_top_level_go(context, module, stmt)?;
        }
        Stmt::Var {
            name,
            type_annotation,
            initializer,
            is_const,
            ..
        } => {
            if *is_const {
                emit_static_const(context, module, name, type_annotation, initializer)?;
            } else {
                emit_top_level_var(context, module, name, type_annotation, initializer)?;
            }
        }
        _ => {}
    }
    Ok(())
}
