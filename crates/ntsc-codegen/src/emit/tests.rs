//! Unit tests for IR emission.

use super::*;
use ntsc_ast::span::Span;
use ntsc_ast::token::{Token, TokenKind};

fn make_token(lexeme: &str) -> Token {
    Token::new(TokenKind::Identifier(lexeme.to_string()), Span::dummy())
}

fn build_context_and_tm() -> (Context, crate::context::TargetMachine) {
    let context = Context::create();
    crate::context::init_llvm();
    let tm = crate::context::create_target_machine(
        crate::host_triple(),
        inkwell::OptimizationLevel::None,
    )
    .expect("create target machine");
    (context, tm)
}

fn emit_program(
    program: &Program,
    test_name: &str,
) -> Result<std::path::PathBuf, crate::CodegenError> {
    let (context, tm) = build_context_and_tm();
    let dir = std::env::temp_dir().join(format!("ntsc_codegen_test_{test_name}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let obj_path = dir.join(format!("{test_name}.{}", crate::object_extension()));

    emit_module(&context, &tm, program, &obj_path, false, false, false)?;

    let metadata = std::fs::metadata(&obj_path).expect("object file exists");
    assert!(metadata.len() > 0, "object file should not be empty");
    Ok(obj_path)
}

#[test]
fn runtime_callbacks_use_pointer_parameters() {
    let context = Context::create();
    let module = context.create_module("runtime_callback_abi");
    declare_runtime_functions(&module, crate::host_triple());

    let spawn = module
        .get_function("ntsc_process_spawn_thread")
        .expect("spawn_thread declaration");
    assert!(spawn.get_type().get_param_types()[0].is_pointer_type());

    let sort_by = module
        .get_function("ntsc_sort_sort_by")
        .expect("sort_by declaration");
    assert!(sort_by.get_type().get_param_types()[1].is_pointer_type());
}

#[test]
fn pending_exception_fallback_uses_the_runtime_flag_type() {
    let context = Context::create();
    let module = context.create_module("exception_poll_fallback");
    declare_runtime_functions(&module, "unknown-unknown-unknown");
    assert!(module.get_global(EXCEPTION_SLOT).is_none());

    let function = module.add_function("run", context.i64_type().fn_type(&[], false), None);
    let entry = context.append_basic_block(function, "entry");
    let builder = context.create_builder();
    builder.position_at_end(entry);
    let mut function_context = FunctionContext::new(
        function,
        &builder,
        &builder,
        entry,
        &module,
        Ty::Int,
        &context,
    );
    function_context
        .emit_pending_exception_check()
        .expect("emit fallback poll");
    builder
        .build_return(Some(&context.i64_type().const_zero()))
        .expect("emit return");
    let handler = function_context.current_exception_handler();
    builder.position_at_end(handler);
    builder
        .build_return(Some(&context.i64_type().const_zero()))
        .expect("emit exception return");

    module.verify().expect("valid fallback poll IR");
    let ir = module.print_to_string().to_string();
    assert!(ir.contains("call i8 @ntsc_exception_pending()"), "{ir}");
    assert!(ir.contains("icmp ne i8"), "{ir}");
}

#[test]
fn emit_hello_world() {
    let program = Program {
        statements: vec![Stmt::Function {
            name: make_token("main"),
            generic_params: vec![],
            params: vec![],
            return_type: None,
            body: vec![Stmt::Say {
                expression: Expr::Literal {
                    value: LiteralValue::String("Hello, World!".into()),
                    span: Span::dummy(),
                },
                keyword_span: Span::dummy(),
            }],
        }],
    };
    let _ = emit_program(&program, "hello_world").expect("emit hello world");
}

#[test]
fn emit_int_expression() {
    let program = Program {
        statements: vec![Stmt::Function {
            name: make_token("main"),
            generic_params: vec![],
            params: vec![],
            return_type: None,
            body: vec![
                Stmt::Var {
                    name: make_token("x"),
                    type_annotation: Some(ntsc_ast::types::TypeAnnotation::Int),
                    initializer: Some(Expr::Literal {
                        value: LiteralValue::Number("42".into()),
                        span: Span::dummy(),
                    }),
                    is_static: false,
                    is_const: false,
                    view: None,
                },
                Stmt::Var {
                    name: make_token("y"),
                    type_annotation: None,
                    initializer: Some(Expr::Binary {
                        left: Box::new(Expr::Literal {
                            value: LiteralValue::Number("10".into()),
                            span: Span::dummy(),
                        }),
                        op: Token::new(TokenKind::Plus, Span::dummy()),
                        right: Box::new(Expr::Literal {
                            value: LiteralValue::Number("32".into()),
                            span: Span::dummy(),
                        }),
                    }),
                    is_static: false,
                    is_const: false,
                    view: None,
                },
            ],
        }],
    };
    let _ = emit_program(&program, "int_expr").expect("emit int expression");
}

#[test]
fn emit_if_else() {
    let program = Program {
        statements: vec![Stmt::Function {
            name: make_token("main"),
            generic_params: vec![],
            params: vec![],
            return_type: None,
            body: vec![Stmt::If {
                condition: Expr::Literal {
                    value: LiteralValue::Bool(true),
                    span: Span::dummy(),
                },
                then_branch: Box::new(Stmt::Block {
                    statements: vec![Stmt::Say {
                        expression: Expr::Literal {
                            value: LiteralValue::String("true".into()),
                            span: Span::dummy(),
                        },
                        keyword_span: Span::dummy(),
                    }],
                    open_span: Span::dummy(),
                    close_span: Span::dummy(),
                }),
                elif_branches: vec![],
                else_branch: Some(Box::new(Stmt::Block {
                    statements: vec![Stmt::Say {
                        expression: Expr::Literal {
                            value: LiteralValue::String("false".into()),
                            span: Span::dummy(),
                        },
                        keyword_span: Span::dummy(),
                    }],
                    open_span: Span::dummy(),
                    close_span: Span::dummy(),
                })),
            }],
        }],
    };
    let _ = emit_program(&program, "if_else").expect("emit if/else");
}

#[test]
fn emit_while_loop() {
    let program = Program {
        statements: vec![Stmt::Function {
            name: make_token("main"),
            generic_params: vec![],
            params: vec![],
            return_type: None,
            body: vec![Stmt::While {
                condition: Expr::Literal {
                    value: LiteralValue::Bool(false),
                    span: Span::dummy(),
                },
                body: Box::new(Stmt::Block {
                    statements: vec![Stmt::Say {
                        expression: Expr::Literal {
                            value: LiteralValue::String("loop".into()),
                            span: Span::dummy(),
                        },
                        keyword_span: Span::dummy(),
                    }],
                    open_span: Span::dummy(),
                    close_span: Span::dummy(),
                }),
            }],
        }],
    };
    let _ = emit_program(&program, "while_loop").expect("emit while loop");
}

#[test]
fn emit_function_with_params_and_return() {
    let program = Program {
        statements: vec![Stmt::Function {
            name: make_token("add"),
            generic_params: vec![],
            params: vec![
                ntsc_ast::expr::FunctionParam {
                    name: make_token("a"),
                    type_annotation: Some(ntsc_ast::types::TypeAnnotation::Int),
                },
                ntsc_ast::expr::FunctionParam {
                    name: make_token("b"),
                    type_annotation: Some(ntsc_ast::types::TypeAnnotation::Int),
                },
            ],
            return_type: Some(ntsc_ast::types::ReturnType {
                ty: ntsc_ast::types::TypeAnnotation::Int,
                arrow_span: Span::dummy(),
            }),
            body: vec![Stmt::Return {
                value: Some(Expr::Binary {
                    left: Box::new(Expr::Variable {
                        name: make_token("a"),
                    }),
                    op: Token::new(TokenKind::Plus, Span::dummy()),
                    right: Box::new(Expr::Variable {
                        name: make_token("b"),
                    }),
                }),
            }],
        }],
    };
    let _ = emit_program(&program, "func_params").expect("emit function with params");
}

// ── Escape analysis ──────────────────────────────────────────────────

fn parse_main_with_point(source: &str) -> (Vec<Stmt>, Module<'static>) {
    let tokens = ntsc_lexer::tokenize(source);
    let program = ntsc_parser::parse(&tokens).expect("parse source");
    let body = match program
        .statements
        .iter()
        .find(|s| matches!(s, Stmt::Function { name, .. } if name.lexeme() == "main"))
    {
        Some(Stmt::Function { body, .. }) => body.clone(),
        _ => panic!("no main function in test source"),
    };

    let context: &'static Context = Box::leak(Box::new(Context::create()));
    let module = context.create_module("escape_test");
    let struct_ty = context.opaque_struct_type("Point");
    struct_ty.set_body(
        &[
            context.i64_type().into(),
            context.i64_type().into(),
            context.ptr_type(inkwell::AddressSpace::default()).into(),
        ],
        false,
    );
    (body, module)
}

fn stack_allocated(body: &[Stmt], module: &Module<'_>) -> HashSet<String> {
    analyze_stack_allocatable(body, module)
}

#[test]
fn escape_analysis_stack_allocates_local_usage() {
    let (body, module) = parse_main_with_point(
        "fun main() {\n    var p = Point();\n    p.x = 3;\n    p.y = p.x + 4;\n    say(p.x + p.y);\n}",
    );
    assert!(stack_allocated(&body, &module).contains("p"));
}

#[test]
fn escape_analysis_supports_method_calls_and_copies() {
    let (body, module) = parse_main_with_point(
        "fun main() {\n    var p = Point();\n    p.reset();\n    var q = p;\n    q.x = 1;\n    say(q.x);\n}",
    );
    let safe = stack_allocated(&body, &module);
    assert!(safe.contains("p"));
    assert!(safe.contains("q"));
}

#[test]
fn escape_analysis_rejects_returned_object() {
    let (body, module) =
        parse_main_with_point("fun main() {\n    var p = Point();\n    return p;\n}");
    assert!(!stack_allocated(&body, &module).contains("p"));
}

#[test]
fn escape_analysis_rejects_object_passed_to_function() {
    let (body, module) =
        parse_main_with_point("fun main() {\n    var p = Point();\n    consume(p);\n}");
    assert!(!stack_allocated(&body, &module).contains("p"));
}

#[test]
fn escape_analysis_rejects_object_in_array_literal() {
    let (body, module) =
        parse_main_with_point("fun main() {\n    var p = Point();\n    var arr = [p];\n}");
    assert!(!stack_allocated(&body, &module).contains("p"));
}

#[test]
fn escape_analysis_rejects_object_copied_then_returned() {
    let (body, module) = parse_main_with_point(
        "fun main() {\n    var p = Point();\n    var q = p;\n    return q;\n}",
    );
    assert!(!stack_allocated(&body, &module).contains("p"));
}

#[test]
fn escape_analysis_rejects_reassignment() {
    let (body, module) =
        parse_main_with_point("fun main() {\n    var p = Point();\n    p = Point();\n}");
    assert!(!stack_allocated(&body, &module).contains("p"));
}

#[test]
fn escape_analysis_rejects_object_with_init_method() {
    let (body, module) =
        parse_main_with_point("fun main() {\n    var p = Point();\n    say(p.x);\n}");

    module.add_function(
        "Point.init",
        module.get_context().void_type().fn_type(&[], false),
        None,
    );
    assert!(!stack_allocated(&body, &module).contains("p"));
}

#[test]
fn escape_analysis_requires_zero_arguments() {
    let (body, module) =
        parse_main_with_point("fun main() {\n    var p = Point(1);\n    say(p.x);\n}");
    assert!(!stack_allocated(&body, &module).contains("p"));
}

#[test]
fn escape_analysis_rejects_object_in_say() {
    let (body, module) =
        parse_main_with_point("fun main() {\n    var p = Point();\n    say(p);\n}");
    assert!(!stack_allocated(&body, &module).contains("p"));
}

#[test]
fn rust_array_labels_lower_to_typed_arrays() {
    use ntsc_typeck::Ty;
    let array_string = Ty::Array(Box::new(Ty::String));
    assert_eq!(
        super::rust_array_elem_ty("array[string]"),
        Some(array_string.clone())
    );
    assert_eq!(
        super::rust_array_elem_ty("option[array[int]]"),
        Some(Ty::Array(Box::new(Ty::Int)))
    );
    assert_eq!(
        super::rust_array_elem_ty("result[array[bool]]"),
        Some(Ty::Array(Box::new(Ty::Bool)))
    );
    assert_eq!(
        super::rust_array_elem_ty("array[float]"),
        Some(Ty::Array(Box::new(Ty::Float)))
    );
    assert_eq!(super::rust_array_elem_ty("array[bytes]"), None);
    assert_eq!(super::rust_array_elem_ty("string"), None);
    assert_eq!(super::rust_array_elem_ty("object[Foo]"), None);
}

/// A package may be hyphenated (`mewsic-tui`) while the Neutron module that
/// imports it is `use @mewsic_tui`. The manifest tables must key both
/// spellings identically, or a lookup misses and the call silently falls
/// back to the handle assumption.
#[test]
fn crate_names_normalize_hyphens_to_one_key() {
    use crate::emit::{normalized_crate_name, table_key_for_test};

    assert_eq!(normalized_crate_name("mewsic-tui"), "mewsic_tui");
    assert_eq!(normalized_crate_name("mewsic_tui"), "mewsic_tui");
    assert_eq!(normalized_crate_name("jptomew"), "jptomew");
    assert_eq!(
        table_key_for_test("mewsic-tui", "Tui_new"),
        table_key_for_test("mewsic_tui", "Tui_new")
    );
    assert_eq!(
        table_key_for_test("keyring", "Entry_new"),
        ("keyring".to_string(), "Entry_new".to_string())
    );
}

/// A call must be followed by a poll of the runtime's pending-exception slot,
/// not by a call into `ntsc_exception_pending`: the slot is a thread-local the
/// generated code addresses directly, which is the difference between a
/// two-instruction check and an out-of-line call in every loop that calls out.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn pending_exception_poll_reads_the_thread_local_slot() {
    let source = r#"
fun helper(int value) -> int {
    return value;
}

fun run(int n) -> int {
    var int total = 0;
    var int i = 0;
    while (i < n) {
        total = total + helper(i);
        i = i + 1;
    }
    return total;
}
"#;
    let program = ntsc_parser::parse(&ntsc_lexer::tokenize(source)).expect("parse");
    let Stmt::Function {
        name,
        params,
        return_type,
        body,
        ..
    } = &program.statements[1]
    else {
        panic!("expected a function");
    };
    let context = Context::create();
    let module = context.create_module("exception_poll_ir");
    declare_runtime_functions(&module, crate::host_triple());
    let helper_ty = context
        .i64_type()
        .fn_type(&[context.i64_type().into()], false);
    module.add_function(
        "helper",
        helper_ty,
        Some(inkwell::module::Linkage::External),
    );
    let function =
        emit_function(&context, &module, name, params, return_type, body).expect("emit function");
    let ir = inkwell::values::AnyValue::print_to_string(&function).to_string();

    assert!(
        ir.contains("@ntsc_pending_exception_slot"),
        "expected the poll to read the thread-local slot:\n{ir}"
    );
    assert!(
        !ir.contains("@ntsc_exception_pending"),
        "expected no out-of-line poll call:\n{ir}"
    );
}

/// A call to a function that cannot throw must not poll the slot at all: the
/// poll is a load and a branch on every call, which dominates a leaf call in a
/// hot loop.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn exception_free_callees_skip_the_pending_poll() {
    let source = r#"
fun helper(int value) -> int {
    return value + 1;
}

fun run(int n) -> int {
    return helper(n);
}
"#;
    let program = ntsc_parser::parse(&ntsc_lexer::tokenize(source)).expect("parse");
    super::set_infallible_functions(super::call::compute_infallible_functions(&program, false));
    let Stmt::Function {
        name,
        params,
        return_type,
        body,
        ..
    } = &program.statements[1]
    else {
        panic!("expected a function");
    };
    let context = Context::create();
    let module = context.create_module("no_poll_ir");
    declare_runtime_functions(&module, crate::host_triple());
    let helper_ty = context
        .i64_type()
        .fn_type(&[context.i64_type().into()], false);
    module.add_function(
        "helper",
        helper_ty,
        Some(inkwell::module::Linkage::External),
    );
    let function =
        emit_function(&context, &module, name, params, return_type, body).expect("emit function");
    let ir = inkwell::values::AnyValue::print_to_string(&function).to_string();

    assert!(
        !ir.contains("ntsc_pending_exception_slot"),
        "expected no poll after an exception-free call:\n{ir}"
    );
}

/// Recursion must not stop the analysis: a function whose only call is to
/// itself, over scalars, has no throw path, so every node of a backtracking
/// search costs one direct call.
#[test]
fn recursive_scalar_functions_are_exception_free() {
    let source = r#"
fun search(int mask, int columns, int left, int right) -> int {
    if (columns == mask) {
        return 1;
    }
    var int available = mask & ~(columns | left | right);
    var int solutions = 0;
    while (available != 0) {
        var int bit = available & -available;
        available = available - bit;
        solutions = solutions + search(mask, columns | bit, ((left | bit) << 1) & mask, (right | bit) >> 1);
    }
    return solutions;
}
"#;
    let program = ntsc_parser::parse(&ntsc_lexer::tokenize(source)).expect("parse");

    let release = super::call::compute_infallible_functions(&program, false);
    assert!(
        release.contains("search"),
        "a wrapping build leaves search with no throw path"
    );

    let debug = super::call::compute_infallible_functions(&program, true);
    assert!(
        !debug.contains("search"),
        "arithmetic traps on overflow in a debug build"
    );
}
