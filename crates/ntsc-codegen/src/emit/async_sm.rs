//! Lowering of `async fun` to poll-based state machines.
//!
//! Each async function becomes an opaque future struct (`ntsc_future_<name>`)
//! holding its `i32 state`, params, result, top-level locals, and one
//! sub-future slot per `await`, plus a poll function
//! (`i8 ntsc_future_<name>_poll(i8* future)`) that `switch`es on the state
//! field and returns 1 when done. All mutable state lives in the struct so it
//! survives suspension. Awaited callees are emitted first (reverse
//! topological order) so sub-future fields can reference their struct types;
//! cyclic `await` chains are compile errors. See docs/async-rfc.md §8.

use super::*;

// ── Async state machines ────────────────────────────────────────────────

/// The LLVM field type of an async future struct slot.
pub(crate) enum AsyncFieldTy {
    /// A regular NTSC-typed slot (state, parameter, result, or local).
    Native(Ty),

    /// A sub-future slot holding the child's future struct (one per await).
    Future(String),

    /// A sub-future slot holding an `i64` handle to a runtime-owned future
    /// (`async.sleep`, `http.*_async`), released when the awaiting future
    /// drops mid-flight.
    RuntimeFuture(RuntimeFutureKind),
}

/// A runtime-owned awaited future, keyed by its release function.
#[derive(Clone, Copy)]
pub(crate) enum RuntimeFutureKind {
    Sleep,
    Http,
    NetAccept,
    NetRecvLine,
    RustOp,
}

impl RuntimeFutureKind {
    /// `None` when the callee is a user async function, whose future is
    /// embedded inline instead.
    fn of_child(child_name: &str) -> Option<Self> {
        match child_name {
            "sleep" => Some(Self::Sleep),
            "net.accept_async" => Some(Self::NetAccept),
            "net.recv_line_async" => Some(Self::NetRecvLine),
            name if name.starts_with("http.") => Some(Self::Http),
            name if name.starts_with("rust.") => Some(Self::RustOp),
            _ => None,
        }
    }

    fn drop_fn_name(self) -> &'static str {
        match self {
            Self::Sleep => "ntsc_async_sleep_drop",
            Self::Http => "ntsc_async_http_drop",
            Self::NetAccept => "ntsc_async_net_accept_drop",
            Self::NetRecvLine => "ntsc_async_net_recv_line_drop",
            Self::RustOp => "ntsc_async_op_drop",
        }
    }
}

/// The runtime ABI of an awaitable `net` future: the four entry points and the
/// NTSC type its result carries.
struct NetFutureAbi {
    new_fn: &'static str,
    poll_fn: &'static str,
    result_fn: &'static str,
    drop_fn: &'static str,
    result_ty: Ty,
}

fn net_future_abi(child_name: &str) -> Option<NetFutureAbi> {
    match child_name {
        "net.accept_async" => Some(NetFutureAbi {
            new_fn: "ntsc_async_net_accept",
            poll_fn: "ntsc_async_net_accept_poll",
            result_fn: "ntsc_async_net_accept_result",
            drop_fn: "ntsc_async_net_accept_drop",
            result_ty: Ty::Int,
        }),
        "net.recv_line_async" => Some(NetFutureAbi {
            new_fn: "ntsc_async_net_recv_line",
            poll_fn: "ntsc_async_net_recv_line_poll",
            result_fn: "ntsc_async_net_recv_line_result",
            drop_fn: "ntsc_async_net_recv_line_drop",
            result_ty: Ty::String,
        }),
        _ => None,
    }
}

/// One `await` point in an async body.
///
/// The statement shape determines how the child result is stored on resume.
/// The child's result is loaded from `child_result_index` (always
/// `1 + <child param count>`: slot 0 is the child's state, slots 1..=n its
/// params, and its result sits right after) and coerced to `child_ret_ty`.
pub(crate) struct AwaitInfo {
    stmt: Stmt,
    child_name: String,
    child_ret_ty: Ty,
    child_result_index: u32,
}

/// The kind of a channel suspension point (a channel send or receive that
/// parks the goroutine like an `await`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChanOpKind {
    Send,
    Recv,
    /// `for v in ch { ... }`: suspend on a receive each iteration and run
    /// the body with `v` bound to the received value; leaves the loop when
    /// the channel is closed and drained.
    Loop,
}

/// A single suspension point in the unified state ordering, indexed by its
/// position (segment ordinal). The await/chan indices reference the
/// `await_infos`/`chan_infos` lists.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SuspKind {
    Await(usize),
    Chan(usize),
}

/// One channel send/receive suspension point in an async body. Channel ops
/// park the running goroutine: `ntask_chan_send`/`ntask_chan_recv` set the
/// goroutine's park state and the poll returns 0; on resume the operation has
/// been completed by the scheduler. A receive reads its result through
/// `ntask_chan_recv_result`.
pub(crate) struct ChanInfo {
    stmt: Stmt,
    op: ChanOpKind,
    /// Coercion target type for a receive (`Ty::Void` for a send).
    recv_ty: Ty,
}

/// Pre-analyzed layout of a single async function's future struct.
///
/// Field order is fixed and ABI-relevant: `state` (0) | params | `result`
/// | locals | `sub_<child>` (one per await). The `fields` map keys params
/// and locals by their source names; `state`, `result`, and the `sub_*`
/// slots are addressed by the indices in `result_index` and
/// `sub_field_base`.
pub(crate) struct AsyncLayout {
    name: String,
    field_tys: Vec<AsyncFieldTy>,
    fields: HashMap<String, u32>,
    result_index: u32,

    /// Index of the first sub-future slot (after params, result, and locals).
    sub_field_base: u32,
    await_infos: Vec<AwaitInfo>,
    /// Channel send/receive suspension points, in emission order.
    chan_infos: Vec<ChanInfo>,
    /// The suspend-state index for each `await_infos` entry. These and the
    /// channel points partition the state range `1..=suspension_count` in
    /// emission order.
    await_state_index: Vec<u32>,
    /// The suspend-state index for each `chan_infos` entry.
    chan_state_index: Vec<u32>,
    /// Total suspension points (awaits + channel ops); the done state is
    /// `suspension_count + 1`.
    suspension_count: u32,
    /// The suspension points in emission order; position i carries state
    /// index i.
    susp_order: Vec<SuspKind>,
    ret_ty: Ty,

    /// Anonymous async blocks compiled as part of this layout. Maps the
    /// generated function name to the block body.
    pub(crate) anon_async_blocks: Vec<(
        String,
        Vec<Stmt>,
        Option<ntsc_ast::expr::ReturnTypeAnnotation>,
    )>,

    /// Maps `Expr::AsyncBlock` spans to their generated anonymous function
    /// names, used during poll emission to resolve standalone blocks and
    /// `wait_any`/`wait_all` arguments.
    pub(crate) block_span_to_name: HashMap<usize, String>,
}

/// Infer the type of an async local that has no explicit annotation.
///
/// Type checking guarantees such a local is initialized with a literal or an
/// `await`, so the slot type is derived from those.
pub(crate) fn async_local_ty(
    program: &Program,
    type_annotation: &Option<ntsc_ast::types::TypeAnnotation>,
    initializer: &Option<Expr>,
) -> Ty {
    if let Some(annotation) = type_annotation {
        return type_annotation_to_ty(&Some(annotation.clone()));
    }
    if let Some(init) = initializer {
        if let Some(ty) = expr_to_literal_ty(init) {
            return ty;
        }
        if let Expr::Await { callee, .. } = init {
            let callee_name = match callee.as_ref() {
                Expr::Variable { name } => name.lexeme(),
                Expr::Member { object, property } if matches!(object.as_ref(), Expr::Variable { name } if name.lexeme() == "async") => {
                    property.lexeme()
                }
                Expr::Member { object, property }
                    if matches!(object.as_ref(), Expr::Variable { name } if name.lexeme() == "http")
                        && property.lexeme().ends_with("_async") =>
                {
                    return Ty::String;
                }
                Expr::AsyncBlock { return_type, .. } => {
                    return function_return_ty(return_type);
                }
                _ => return Ty::Void,
            };
            if callee_name == "sleep" {
                return Ty::Void;
            }
            for stmt in &program.statements {
                if let Stmt::AsyncFunction {
                    name: fn_name,
                    return_type,
                    ..
                } = stmt
                    && fn_name.lexeme() == callee_name
                {
                    return function_return_ty(return_type);
                }
            }
        }
    }
    Ty::Void
}

/// Collect every local variable of an async body into the future struct's
/// field map. Nested function/lambda bodies are independent functions (their
/// locals belong to their own futures) and are not descended into.
pub(crate) fn collect_async_locals(
    stmt: &Stmt,
    program: &Program,
    fields: &mut HashMap<String, u32>,
    field_names: &mut Vec<String>,
    field_tys: &mut Vec<AsyncFieldTy>,
) {
    let mut add = |name: &str, ty: Ty, fields: &mut HashMap<String, u32>| {
        if !fields.contains_key(name) {
            fields.insert(name.to_string(), field_names.len() as u32);
            field_names.push(name.to_string());
            field_tys.push(AsyncFieldTy::Native(ty));
        }
    };
    match stmt {
        Stmt::Var {
            name,
            type_annotation,
            initializer,
            ..
        } => {
            let ty = async_local_ty(program, type_annotation, initializer);
            add(name.lexeme(), ty, fields);
        }
        Stmt::Destructure { names, .. } => {
            for name in names {
                add(name.lexeme(), Ty::Any, fields);
            }
        }
        Stmt::ForIn { variable, body, .. } | Stmt::ForAwait { variable, body, .. } => {
            add(variable.lexeme(), Ty::Any, fields);
            collect_async_locals(body, program, fields, field_names, field_tys);
        }
        Stmt::ChanRecvFor {
            variable,
            channel,
            body,
        } => {
            // The loop variable holds what the channel carries. Typing it
            // `Any` would make every member access on it dynamic, which
            // miscompiles a class element (`m.field` read as the instance
            // pointer rather than the field).
            let element = ntsc_typeck::chan_recv_element_type(channel.span().start);
            add(variable.lexeme(), element, fields);
            collect_async_locals(body, program, fields, field_names, field_tys);
        }
        Stmt::Block { statements, .. } => {
            for inner in statements {
                collect_async_locals(inner, program, fields, field_names, field_tys);
            }
        }
        Stmt::If {
            then_branch,
            elif_branches,
            else_branch,
            ..
        } => {
            collect_async_locals(then_branch, program, fields, field_names, field_tys);
            for branch in elif_branches {
                collect_async_locals(&branch.body, program, fields, field_names, field_tys);
            }
            if let Some(else_branch) = else_branch {
                collect_async_locals(else_branch, program, fields, field_names, field_tys);
            }
        }
        Stmt::While { body, .. }
        | Stmt::DoWhile { body, .. }
        | Stmt::Retry { body, .. }
        | Stmt::Unsafe { body, .. }
        | Stmt::Quiet { body, .. } => {
            collect_async_locals(body, program, fields, field_names, field_tys)
        }
        Stmt::For { init, body, .. } => {
            if let Some(init) = init {
                collect_async_locals(init, program, fields, field_names, field_tys);
            }
            collect_async_locals(body, program, fields, field_names, field_tys);
        }
        Stmt::Match {
            cases,
            default_case,
            ..
        } => {
            for case in cases {
                collect_async_locals(&case.body, program, fields, field_names, field_tys);
            }
            if let Some(default_case) = default_case {
                collect_async_locals(default_case, program, fields, field_names, field_tys);
            }
        }
        Stmt::Try {
            try_block,
            catch_var,
            catch_block,
            finally_block,
        } => {
            if let Some(catch_var) = catch_var {
                add(catch_var.lexeme(), Ty::String, fields);
            }
            collect_async_locals(try_block, program, fields, field_names, field_tys);
            if let Some(catch_block) = catch_block {
                collect_async_locals(catch_block, program, fields, field_names, field_tys);
            }
            if let Some(finally_block) = finally_block {
                collect_async_locals(finally_block, program, fields, field_names, field_tys);
            }
        }
        Stmt::Function { .. } | Stmt::AsyncFunction { .. } => {}
        Stmt::Expression {
            expression: Expr::ChanRecv { receiver, .. },
            ..
        } => {
            // A `v |> ch` receive binds its receiver as a fresh local; type it
            // as `Any` for the field slot (receivers hold ints or owned
            // handles, both i64), with the precise element type supplied to
            // `define_var` on resume from the typeck registry.
            add(receiver.lexeme(), Ty::Any, fields);
        }
        _ => {}
    }
}

/// Flatten top-level `{ ... }` blocks of an async body into the statement
/// list. Type checking treats such blocks as transparent, and the poll body
/// emits the resulting statements once. Nested blocks inside control flow are
/// left untouched.
pub(crate) fn flatten_top_level_blocks(stmts: &[Stmt]) -> Vec<Stmt> {
    let mut out = Vec::new();
    for stmt in stmts {
        match stmt {
            Stmt::Block { statements, .. } => {
                out.extend(flatten_top_level_blocks(statements));
            }
            _ => out.push(stmt.clone()),
        }
    }
    out
}

struct SuspendAccum<'a> {
    program: &'a Program,
    fields: &'a HashMap<String, u32>,
    field_names: &'a mut Vec<String>,
    field_tys: &'a mut Vec<AsyncFieldTy>,
    await_infos: &'a mut Vec<AwaitInfo>,
    await_state_index: &'a mut Vec<u32>,
    chan_infos: &'a mut Vec<ChanInfo>,
    chan_state_index: &'a mut Vec<u32>,
    susp_order: &'a mut Vec<SuspKind>,
    anon_async_blocks: &'a mut Vec<(
        String,
        Vec<Stmt>,
        Option<ntsc_ast::expr::ReturnTypeAnnotation>,
    )>,
    anon_counter: &'a mut usize,
}

fn enumerate_suspends(
    stmt: &Stmt,
    accum: &mut SuspendAccum<'_>,
) -> Result<(), crate::CodegenError> {
    if await_stmt_parts(stmt).is_ok() {
        let (child_name, child_ret_ty, anon_body, anon_return_type) =
            await_callee_info(stmt, accum.program, accum.anon_counter)?;
        if let Some(body) = anon_body {
            accum
                .anon_async_blocks
                .push((child_name.clone(), body, anon_return_type));
        }
        let child_param_count =
            await_callee_param_count(accum.program, &child_name, accum.anon_async_blocks)?;
        accum.field_names.push(format!("sub_{child_name}"));
        match RuntimeFutureKind::of_child(&child_name) {
            Some(kind) => accum.field_tys.push(AsyncFieldTy::RuntimeFuture(kind)),
            None => accum
                .field_tys
                .push(AsyncFieldTy::Future(child_name.clone())),
        }
        let state = accum.susp_order.len() as u32;
        let await_idx = accum.await_infos.len();
        accum.await_infos.push(AwaitInfo {
            stmt: stmt.clone(),
            child_name,
            child_ret_ty,
            child_result_index: 1 + child_param_count as u32,
        });
        accum.await_state_index.push(state);
        accum.susp_order.push(SuspKind::Await(await_idx));
    } else if let Some((op, recv_ty)) = chan_target_ty(stmt) {
        let state = accum.susp_order.len() as u32;
        let chan_idx = accum.chan_infos.len();
        accum.chan_infos.push(ChanInfo {
            stmt: stmt.clone(),
            op,
            recv_ty: recv_ty.clone(),
        });
        accum.chan_state_index.push(state);
        accum.susp_order.push(SuspKind::Chan(chan_idx));
        if op != ChanOpKind::Send
            && let Ok(receiver) = chan_stmt_receiver(stmt)
            && let Some(field_index) = accum.fields.get(receiver.lexeme()).copied()
        {
            accum.field_tys[field_index as usize] = AsyncFieldTy::Native(recv_ty);
        }
        if let Stmt::ChanRecvFor { body, .. } = stmt {
            enumerate_suspends(body, accum)?;
        }
    } else {
        match stmt {
            Stmt::Block { statements, .. } => {
                for inner in statements {
                    enumerate_suspends(inner, accum)?;
                }
            }
            Stmt::If {
                then_branch,
                elif_branches,
                else_branch,
                ..
            } => {
                enumerate_suspends(then_branch, accum)?;
                for branch in elif_branches {
                    enumerate_suspends(&branch.body, accum)?;
                }
                if let Some(else_branch) = else_branch {
                    enumerate_suspends(else_branch, accum)?;
                }
            }
            Stmt::While { body, .. }
            | Stmt::DoWhile { body, .. }
            | Stmt::Retry { body, .. }
            | Stmt::Unsafe { body, .. }
            | Stmt::Quiet { body, .. } => enumerate_suspends(body, accum)?,
            Stmt::For { init, body, .. } => {
                if let Some(init) = init {
                    enumerate_suspends(init, accum)?;
                }
                enumerate_suspends(body, accum)?;
            }
            Stmt::ForIn { body, .. } | Stmt::ForAwait { body, .. } => {
                enumerate_suspends(body, accum)?;
            }
            Stmt::Match {
                cases,
                default_case,
                ..
            } => {
                for case in cases {
                    enumerate_suspends(&case.body, accum)?;
                }
                if let Some(default_case) = default_case {
                    enumerate_suspends(default_case, accum)?;
                }
            }
            Stmt::Try {
                try_block,
                catch_block,
                finally_block,
                ..
            } => {
                enumerate_suspends(try_block, accum)?;
                if let Some(catch_block) = catch_block {
                    enumerate_suspends(catch_block, accum)?;
                }
                if let Some(finally_block) = finally_block {
                    enumerate_suspends(finally_block, accum)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

pub(crate) fn build_async_layout(
    program: &Program,
    name: &ntsc_ast::token::Token,
    params: &[ntsc_ast::expr::FunctionParam],
    return_type: &Option<ntsc_ast::types::ReturnType>,
    body: &[Stmt],
) -> Result<AsyncLayout, crate::CodegenError> {
    let ret_ty = function_return_ty(return_type);

    let mut field_names = Vec::new();
    let mut field_tys = Vec::new();
    let mut fields = HashMap::new();

    fields.insert("state".to_string(), 0);
    field_names.push("state".to_string());
    field_tys.push(AsyncFieldTy::Native(Ty::Int));

    // Parameter slots (indices 1..=params.len()).
    for param in params {
        let key = param.name.lexeme().to_string();
        let param_ty = type_annotation_to_ty(&param.type_annotation);
        fields.insert(key.clone(), field_names.len() as u32);
        field_names.push(key.clone());
        field_tys.push(AsyncFieldTy::Native(param_ty));
    }

    let result_index = field_names.len() as u32;
    // Result slot, then local slots (declaration/discovery order).
    fields.insert("result".to_string(), result_index);
    field_names.push("result".to_string());
    field_tys.push(AsyncFieldTy::Native(ret_ty.clone()));

    for stmt in body {
        collect_async_locals(stmt, program, &mut fields, &mut field_names, &mut field_tys);
    }

    let sub_field_base = field_names.len() as u32;

    let mut await_infos = Vec::new();
    let mut await_state_index = Vec::new();
    let mut chan_infos = Vec::new();
    let mut chan_state_index = Vec::new();
    let mut susp_order = Vec::new();
    let mut anon_async_blocks = Vec::new();
    let mut anon_counter = 0usize;

    // Every suspension point (await or channel op) gets a distinct state
    // index equal to its position in emission order, shared by the poll
    // switch and the suspend/resume logic. The walk visits statements in the
    // same order the poll body emits them, so the ordinal the statement
    // emitter assigns to each suspend matches the index recorded here.
    let mut accum = SuspendAccum {
        program,
        fields: &fields,
        field_names: &mut field_names,
        field_tys: &mut field_tys,
        await_infos: &mut await_infos,
        await_state_index: &mut await_state_index,
        chan_infos: &mut chan_infos,
        chan_state_index: &mut chan_state_index,
        susp_order: &mut susp_order,
        anon_async_blocks: &mut anon_async_blocks,
        anon_counter: &mut anon_counter,
    };
    for stmt in body {
        enumerate_suspends(stmt, &mut accum)?;
    }
    let suspension_points = susp_order.len() as u32;

    // Discover standalone Expr::AsyncBlock (not inside await) from
    // wait_any/wait_all arguments and expression-position blocks.
    let mut block_span_to_name = HashMap::new();
    collect_standalone_async_blocks(
        body,
        &mut anon_async_blocks,
        &mut anon_counter,
        &mut block_span_to_name,
    );

    let layout = AsyncLayout {
        name: name.lexeme().to_string(),
        field_tys,
        fields,
        result_index,
        sub_field_base,
        await_infos,
        chan_infos,
        await_state_index,
        chan_state_index,
        suspension_count: suspension_points,
        susp_order,
        ret_ty,
        anon_async_blocks,
        block_span_to_name,
    };
    Ok(layout)
}

/// `(child_name, return_type, anon_body?, anon_return_type?)` returned by
/// `await_callee_info`.
pub(crate) type AwaitCalleeResult = (
    String,
    Ty,
    Option<Vec<Stmt>>,
    Option<ntsc_ast::expr::ReturnTypeAnnotation>,
);

/// Scan statements for `Expr::AsyncBlock` that appear outside of `await`
/// (standalone expression blocks and `wait_any`/`wait_all` arguments) and
/// register them as anonymous async blocks for compilation.
fn collect_standalone_async_blocks(
    stmts: &[Stmt],
    anon_async_blocks: &mut Vec<(
        String,
        Vec<Stmt>,
        Option<ntsc_ast::expr::ReturnTypeAnnotation>,
    )>,
    anon_counter: &mut usize,
    block_span_to_name: &mut HashMap<usize, String>,
) {
    for stmt in stmts {
        match stmt {
            Stmt::Expression { expression, .. } => {
                collect_expr_async_blocks(
                    expression,
                    anon_async_blocks,
                    anon_counter,
                    block_span_to_name,
                );
            }
            Stmt::Var {
                initializer: Some(init),
                ..
            } => {
                collect_expr_async_blocks(
                    init,
                    anon_async_blocks,
                    anon_counter,
                    block_span_to_name,
                );
            }
            Stmt::Return {
                value: Some(val), ..
            } => {
                collect_expr_async_blocks(val, anon_async_blocks, anon_counter, block_span_to_name);
            }
            Stmt::Block { statements, .. } => {
                collect_standalone_async_blocks(
                    statements,
                    anon_async_blocks,
                    anon_counter,
                    block_span_to_name,
                );
            }
            Stmt::If {
                condition,
                then_branch,
                elif_branches: _,
                else_branch,
            } => {
                collect_expr_async_blocks(
                    condition,
                    anon_async_blocks,
                    anon_counter,
                    block_span_to_name,
                );
                collect_standalone_async_blocks(
                    std::slice::from_ref(then_branch),
                    anon_async_blocks,
                    anon_counter,
                    block_span_to_name,
                );
                if let Some(eb) = else_branch {
                    collect_standalone_async_blocks(
                        std::slice::from_ref(eb),
                        anon_async_blocks,
                        anon_counter,
                        block_span_to_name,
                    );
                }
            }
            Stmt::While {
                condition, body, ..
            } => {
                collect_expr_async_blocks(
                    condition,
                    anon_async_blocks,
                    anon_counter,
                    block_span_to_name,
                );
                collect_standalone_async_blocks(
                    std::slice::from_ref(body),
                    anon_async_blocks,
                    anon_counter,
                    block_span_to_name,
                );
            }
            Stmt::For {
                init,
                condition,
                update,
                body,
                ..
            } => {
                if let Some(i) = init {
                    collect_standalone_async_blocks(
                        std::slice::from_ref(i),
                        anon_async_blocks,
                        anon_counter,
                        block_span_to_name,
                    );
                }
                if let Some(c) = condition {
                    collect_expr_async_blocks(
                        c,
                        anon_async_blocks,
                        anon_counter,
                        block_span_to_name,
                    );
                }
                if let Some(u) = update {
                    collect_expr_async_blocks(
                        u,
                        anon_async_blocks,
                        anon_counter,
                        block_span_to_name,
                    );
                }
                collect_standalone_async_blocks(
                    std::slice::from_ref(body),
                    anon_async_blocks,
                    anon_counter,
                    block_span_to_name,
                );
            }
            Stmt::ForIn { body, .. } | Stmt::ForAwait { body, .. } => {
                collect_standalone_async_blocks(
                    std::slice::from_ref(body),
                    anon_async_blocks,
                    anon_counter,
                    block_span_to_name,
                );
            }
            Stmt::Try {
                try_block,
                catch_block,
                finally_block,
                ..
            } => {
                collect_standalone_async_blocks(
                    std::slice::from_ref(try_block),
                    anon_async_blocks,
                    anon_counter,
                    block_span_to_name,
                );
                if let Some(cb) = catch_block {
                    collect_standalone_async_blocks(
                        std::slice::from_ref(cb),
                        anon_async_blocks,
                        anon_counter,
                        block_span_to_name,
                    );
                }
                if let Some(fb) = finally_block {
                    collect_standalone_async_blocks(
                        std::slice::from_ref(fb),
                        anon_async_blocks,
                        anon_counter,
                        block_span_to_name,
                    );
                }
            }
            _ => {}
        }
    }
}

fn collect_expr_async_blocks(
    expr: &Expr,
    anon_async_blocks: &mut Vec<(
        String,
        Vec<Stmt>,
        Option<ntsc_ast::expr::ReturnTypeAnnotation>,
    )>,
    anon_counter: &mut usize,
    block_span_to_name: &mut HashMap<usize, String>,
) {
    match expr {
        Expr::AsyncBlock {
            body,
            return_type,
            span,
            ..
        } => {
            *anon_counter += 1;
            let name = format!("__anon_async_{}", anon_counter);
            anon_async_blocks.push((name.clone(), body.clone(), return_type.clone()));
            block_span_to_name.insert(span.start, name);
        }
        Expr::Call {
            callee, arguments, ..
        } => {
            if is_wait_any_or_all(callee) {
                for arg in arguments {
                    collect_expr_async_blocks(
                        arg,
                        anon_async_blocks,
                        anon_counter,
                        block_span_to_name,
                    );
                }
            }
        }
        Expr::Await {
            callee, arguments, ..
        } => {
            collect_expr_async_blocks(callee, anon_async_blocks, anon_counter, block_span_to_name);
            for arg in arguments {
                collect_expr_async_blocks(arg, anon_async_blocks, anon_counter, block_span_to_name);
            }
        }
        _ => {}
    }
}

fn is_wait_any_or_all(expr: &Expr) -> bool {
    matches!(expr, Expr::Variable { name } if name.lexeme() == "wait_any" || name.lexeme() == "wait_all")
}

pub(crate) fn await_callee_info(
    stmt: &Stmt,
    program: &Program,
    anon_counter: &mut usize,
) -> Result<AwaitCalleeResult, crate::CodegenError> {
    let (callee_expr, _arguments) = await_stmt_parts(stmt)?;
    match callee_expr {
        Expr::Variable { name } => {
            let callee_name = name.lexeme();
            if callee_name == "sleep" {
                return Ok(("sleep".to_string(), Ty::Void, None, None));
            }
            let ret_ty = program
                .statements
                .iter()
                .find_map(|s| match s {
                    Stmt::AsyncFunction {
                        name: fn_name,
                        return_type,
                        ..
                    } if fn_name.lexeme() == callee_name => Some(function_return_ty(return_type)),
                    _ => None,
                })
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError(format!(
                        "internal: awaited callee `{callee_name}` is not a module-level async function"
                    ))
                })?;
            Ok((callee_name.to_string(), ret_ty, None, None))
        }
        Expr::Member { object, property }
            if matches!(
                object.as_ref(),
                Expr::Variable { name } if name.lexeme() == "async"
            ) =>
        {
            let callee_name = property.lexeme();
            if callee_name == "sleep" {
                return Ok(("sleep".to_string(), Ty::Void, None, None));
            }
            let ret_ty = program
                .statements
                .iter()
                .find_map(|s| match s {
                    Stmt::AsyncFunction {
                        name: fn_name,
                        return_type,
                        ..
                    } if fn_name.lexeme() == callee_name => Some(function_return_ty(return_type)),
                    _ => None,
                })
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError(format!(
                        "internal: awaited callee `{callee_name}` is not a module-level async function"
                    ))
                })?;
            Ok((callee_name.to_string(), ret_ty, None, None))
        }
        Expr::Member { object, property }
            if matches!(
                object.as_ref(),
                Expr::Variable { name } if name.lexeme() == "net"
            ) && property.lexeme() == "accept_async" =>
        {
            Ok(("net.accept_async".to_string(), Ty::Int, None, None))
        }
        Expr::Member { object, property }
            if matches!(
                object.as_ref(),
                Expr::Variable { name } if name.lexeme() == "net"
            ) && property.lexeme() == "recv_line_async" =>
        {
            Ok(("net.recv_line_async".to_string(), Ty::String, None, None))
        }
        Expr::Member { object, property }
            if let Expr::Variable { name } = object.as_ref()
                && let Some(crate_name) = super::RUST_CRATE_NAMESPACES.with(|m| {
                    m.borrow()
                        .get(name.lexeme().trim_start_matches('@'))
                        .cloned()
                })
                && super::rust_async(crate_name.as_str(), property.lexeme()) =>
        {
            let ret_ty = match super::rust_return_label(&crate_name, property.lexeme()).as_deref() {
                Some("bool") => Ty::Bool,
                Some("float") => Ty::Float,
                Some("void") | Some("result[void]") => Ty::Void,
                Some("int") | Some("result[int]") => Ty::Int,
                Some("string") | Some("result[string]") => Ty::String,
                Some(label) if label.starts_with("option[") => super::rust_option_scalar_ty(label)
                    .map(|inner| Ty::Option(Box::new(inner)))
                    .unwrap_or(Ty::Any),
                // Object and Result-of-object results arrive as opaque
                // registry handles.
                Some(label) => super::rust_array_elem_ty(label).unwrap_or(Ty::Any),
                None => Ty::Any,
            };
            Ok((
                format!("rust.{crate_name}.{}", property.lexeme()),
                ret_ty,
                None,
                None,
            ))
        }
        Expr::Member { object, property }
            if matches!(
                object.as_ref(),
                Expr::Variable { name } if name.lexeme() == "http"
            ) && property.lexeme().ends_with("_async") =>
        {
            Ok((
                format!("http.{}", property.lexeme()),
                Ty::String,
                None,
                None,
            ))
        }
        Expr::AsyncBlock {
            body, return_type, ..
        } => {
            *anon_counter += 1;
            let name = format!("__anon_async_{}", anon_counter);
            let ret_ty = function_return_ty(return_type);
            Ok((name, ret_ty, Some(body.clone()), return_type.clone()))
        }
        _ => Err(crate::CodegenError::LLVMError(
            "await requires a call to a module-level async function".into(),
        )),
    }
}

pub(crate) fn await_callee_param_count(
    program: &Program,
    child_name: &str,
    anon_async_blocks: &[(
        String,
        Vec<Stmt>,
        Option<ntsc_ast::expr::ReturnTypeAnnotation>,
    )],
) -> Result<usize, crate::CodegenError> {
    if child_name == "sleep"
        || child_name == "net.accept_async"
        || child_name == "net.recv_line_async"
    {
        return Ok(1);
    }
    // Offloaded http futures: 1 arg (get/delete/head) or 2 (post/put/patch).
    if let Some(method) = child_name.strip_prefix("http.") {
        return Ok(match method {
            "get_async" | "delete_async" | "head_async" => 1,
            _ => 2,
        });
    }
    // Offloaded Rust-bridge futures carry their arguments through the
    // async-args map; their handle slot has no child-future struct, so no
    // result index is derived from this count.
    if child_name.starts_with("rust.") {
        return Ok(0);
    }
    // Anonymous async blocks have no params.
    if anon_async_blocks
        .iter()
        .any(|(name, _, _)| name == child_name)
    {
        return Ok(0);
    }
    program
        .statements
        .iter()
        .find_map(|s| match s {
            Stmt::AsyncFunction { name, params, .. } if name.lexeme() == child_name => {
                Some(params.len())
            }
            _ => None,
        })
        .ok_or_else(|| {
            crate::CodegenError::LLVMError(format!(
                "internal: awaited callee `{child_name}` not found"
            ))
        })
}

/// The (callee, arguments) of an await statement, for all three legal
/// statement shapes (expression, variable initializer, return value).
pub(crate) fn await_stmt_parts(stmt: &Stmt) -> Result<(&Expr, &[Expr]), crate::CodegenError> {
    match stmt {
        Stmt::Expression {
            expression: Expr::Assign { value, .. },
            ..
        } => match value.as_ref() {
            Expr::Await {
                callee, arguments, ..
            } => Ok((callee.as_ref(), arguments)),
            _ => Err(crate::CodegenError::LLVMError(
                "internal: expected an await assignment".into(),
            )),
        },
        Stmt::Expression {
            expression: Expr::Await {
                callee, arguments, ..
            },
            ..
        }
        | Stmt::Var {
            initializer: Some(Expr::Await {
                callee, arguments, ..
            }),
            ..
        }
        | Stmt::Return {
            value: Some(Expr::Await {
                callee, arguments, ..
            }),
            ..
        } => Ok((callee.as_ref(), arguments)),
        _ => Err(crate::CodegenError::LLVMError(
            "internal: expected an await statement".into(),
        )),
    }
}

fn emit_await_assignment<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    name: &ntsc_ast::token::Token,
    value: BasicValueEnum<'ctx>,
    value_ty: &Ty,
) -> Result<(), crate::CodegenError> {
    let (slot, slot_ty) = fn_ctx
        .variables
        .get(name.lexeme())
        .map(|(slot, ty)| (*slot, ty.clone()))
        .ok_or_else(|| {
            crate::CodegenError::LLVMError(format!(
                "internal: awaited assignment target `{}` has no future field",
                name.lexeme()
            ))
        })?;
    let coerced = coerce_value(fn_ctx, TypedValue::new(value, value_ty.clone()), &slot_ty)?;
    emit_drop_replaced_value(fn_ctx, slot, &slot_ty, &coerced)?;
    fn_ctx.builder.build_store(slot, coerced.value)?;
    Ok(())
}

/// The channel-op kind of a top-level channel suspension statement. Sends
/// (`ch <| v`) and receives (`v |> ch`) both appear as top-level statement
/// expressions; the receive's freshly-bound receiver variable is typed from
/// the element type typeck recorded for the owning function.
fn chan_target_ty(stmt: &Stmt) -> Option<(ChanOpKind, Ty)> {
    match stmt {
        Stmt::Expression {
            expression: Expr::ChanSend { .. },
            ..
        } => Some((ChanOpKind::Send, Ty::Void)),
        Stmt::ChanRecvFor { channel, .. } => {
            let element = ntsc_typeck::chan_recv_element_type(channel.span().start);
            Some((ChanOpKind::Loop, element))
        }
        Stmt::Expression {
            expression: Expr::ChanRecv { op_span, .. },
            ..
        } => {
            let element = ntsc_typeck::chan_recv_element_type(op_span.start);
            Some((ChanOpKind::Recv, element))
        }
        _ => None,
    }
}

pub(crate) fn is_async_suspension_stmt(stmt: &Stmt) -> bool {
    await_stmt_parts(stmt).is_ok() || chan_target_ty(stmt).is_some()
}

/// The channel operand expression of a channel suspension statement.
pub(crate) fn chan_stmt_channel(stmt: &Stmt) -> Result<&Expr, crate::CodegenError> {
    match stmt {
        Stmt::Expression {
            expression: Expr::ChanSend { channel, .. },
            ..
        } => Ok(channel),
        Stmt::Expression {
            expression: Expr::ChanRecv { channel, .. },
            ..
        } => Ok(channel),
        Stmt::ChanRecvFor { channel, .. } => Ok(channel),
        _ => Err(crate::CodegenError::LLVMError(
            "internal: expected a channel suspension statement".into(),
        )),
    }
}

/// The value expression sent by a channel-send suspension statement.
pub(crate) fn chan_stmt_value(stmt: &Stmt) -> Result<&Expr, crate::CodegenError> {
    match stmt {
        Stmt::Expression {
            expression: Expr::ChanSend { value, .. },
            ..
        } => Ok(value),
        _ => Err(crate::CodegenError::LLVMError(
            "internal: expected a channel-send statement".into(),
        )),
    }
}

/// The receiver variable of a channel-receive suspension statement.
pub(crate) fn chan_stmt_receiver(
    stmt: &Stmt,
) -> Result<&ntsc_ast::token::Token, crate::CodegenError> {
    match stmt {
        Stmt::Expression {
            expression: Expr::ChanRecv { receiver, .. },
            ..
        } => Ok(receiver),
        Stmt::ChanRecvFor { variable, .. } => Ok(variable),
        _ => Err(crate::CodegenError::LLVMError(
            "internal: expected a channel-receive statement".into(),
        )),
    }
}

/// Declare the (opaque) future struct type for an async function. Called
/// for callees before callers so sub-future fields can reference resolved
/// types.
pub(crate) fn declare_async_future<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    layout: &AsyncLayout,
) -> Result<inkwell::types::StructType<'ctx>, crate::CodegenError> {
    let struct_name = format!("ntsc_future_{}", layout.name);
    let struct_ty = context.opaque_struct_type(&struct_name);
    let field_types: Vec<inkwell::types::BasicTypeEnum<'ctx>> = layout
        .field_tys
        .iter()
        .map(|field| match field {
            AsyncFieldTy::Native(ty) => Ok(ty_to_llvm(ty, context)),
            AsyncFieldTy::RuntimeFuture(_) => Ok(context.i64_type().as_basic_type_enum()),
            AsyncFieldTy::Future(child) => module
                .get_struct_type(&format!("ntsc_future_{child}"))
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError(format!(
                        "internal: child future `ntsc_future_{child}` not declared"
                    ))
                })
                .map(|t| t.as_basic_type_enum()),
        })
        .collect::<Result<Vec<_>, _>>()?;
    struct_ty.set_body(&field_types, false);
    Ok(struct_ty)
}

/// Emit the async state machine for one module-level async function.
/// Awaited callees are emitted first (reverse topological order); cycles are
/// rejected.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_async_function<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    program: &Program,
    decl: &Stmt,
    done: &mut HashSet<String>,
    in_progress: &mut HashSet<String>,
) -> Result<(), crate::CodegenError> {
    let (name_token, params, return_type, body) = match decl {
        Stmt::AsyncFunction {
            name,
            params,
            return_type,
            body,
            ..
        } => (name, params, return_type, body),
        _ => return Ok(()),
    };
    let fn_name = name_token.lexeme();

    if done.contains(fn_name) {
        return Ok(());
    }
    if !in_progress.insert(fn_name.to_string()) {
        return Err(crate::CodegenError::LLVMError(format!(
            "cyclic await chain involving async function `{fn_name}` is not supported"
        )));
    }

    let body = flatten_top_level_blocks(body);

    let layout = build_async_layout(program, name_token, params, return_type, &body)?;

    // Emit anonymous async blocks first.
    for (anon_name, anon_body, anon_ret) in &layout.anon_async_blocks {
        let anon_name_token = ntsc_ast::token::Token::new(
            ntsc_ast::token::TokenKind::Identifier(anon_name.clone()),
            ntsc_ast::span::Span::dummy(),
        );
        let anon_decl = Stmt::AsyncFunction {
            name: anon_name_token,
            params: vec![],
            return_type: anon_ret.clone(),
            body: anon_body.clone(),
        };
        emit_async_function(context, module, program, &anon_decl, done, in_progress)?;
    }

    // Emit awaited callees first so their future struct types exist when
    // this future's fields reference them (reverse topological order). The
    // built-in `async.sleep` has no emitted callee: its future struct and
    // poll function are declared as part of the runtime.
    for info in &layout.await_infos {
        if info.child_name == "sleep"
            || info.child_name == "net.accept_async"
            || info.child_name == "net.recv_line_async"
            || info.child_name.starts_with("http.")
            || info.child_name.starts_with("rust.")
        {
            continue;
        }
        // Anonymous async blocks were already emitted above.
        if layout
            .anon_async_blocks
            .iter()
            .any(|(n, _, _)| n == &info.child_name)
        {
            continue;
        }
        let callee = program
            .statements
            .iter()
            .find(|s| {
                matches!(s, Stmt::AsyncFunction { name, .. } if name.lexeme() == info.child_name)
            })
            .ok_or_else(|| {
                crate::CodegenError::LLVMError(format!(
                    "internal: awaited callee `{}` not found",
                    info.child_name
                ))
            })?;
        emit_async_function(context, module, program, callee, done, in_progress)?;
    }

    let struct_name = format!("ntsc_future_{fn_name}");
    declare_async_future(context, module, &layout)?;
    declare_async_drop(module, context, &struct_name)?;

    emit_async_poll(context, module, &struct_name, &layout, &body)?;
    emit_async_drop(context, module, &struct_name, &layout)?;

    if fn_name == "main" {
        emit_async_main_wrapper(context, module, &struct_name, &layout)?;
    }

    in_progress.remove(fn_name);
    done.insert(fn_name.to_string());
    Ok(())
}

/// Emit `ntsc_future_<name>_poll(i64 future) -> i8`, the state machine.
pub(crate) fn emit_async_poll<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    struct_name: &str,
    layout: &AsyncLayout,
    body: &[Stmt],
) -> Result<(), crate::CodegenError> {
    let future_ty = module.get_struct_type(struct_name).ok_or_else(|| {
        crate::CodegenError::LLVMError(format!("internal: missing future struct {struct_name}"))
    })?;
    let poll_name = format!("ntsc_future_{}_poll", layout.name);
    let poll_fn = module.add_function(
        &poll_name,
        context
            .i8_type()
            .fn_type(&[context.i64_type().into()], false),
        Some(inkwell::module::Linkage::External),
    );

    let builder = context.create_builder();
    let entry_builder = context.create_builder();
    let entry_bb = context.append_basic_block(poll_fn, "entry");
    builder.position_at_end(entry_bb);
    entry_builder.position_at_end(entry_bb);

    let future_handle = poll_fn
        .get_nth_param(0)
        .ok_or_else(|| crate::CodegenError::LLVMError("missing poll future param".into()))?
        .into_int_value();
    let future_ptr = builder.build_int_to_ptr(
        future_handle,
        context.ptr_type(AddressSpace::default()),
        "future_i8",
    )?;
    let future = builder.build_pointer_cast(
        future_ptr,
        context.ptr_type(AddressSpace::default()),
        "future",
    )?;
    let state_field = builder.build_struct_gep(future_ty, future, 0, "state_ptr")?;
    let state = builder.build_load(context.i32_type(), state_field, "state")?;

    let body_bb = context.append_basic_block(poll_fn, "body");
    let finish_bb = context.append_basic_block(poll_fn, "finish");
    let fallthrough_bb = context.append_basic_block(poll_fn, "fallthrough");

    let mut fn_ctx = FunctionContext::new(
        poll_fn,
        &builder,
        &entry_builder,
        entry_bb,
        module,
        Ty::Void,
        context,
    );
    fn_ctx.future_base = Some((future, future_ty));
    fn_ctx.async_fields = Some(layout.fields.clone());
    fn_ctx.async_layout = Some(layout);
    fn_ctx.block_span_to_name = Some(layout.block_span_to_name.clone());
    fn_ctx.cache_future_fields(layout.field_tys.len() as u32)?;

    builder.position_at_end(body_bb);
    for (name, index) in &layout.fields {
        if name == "state" || name == "result" {
            continue;
        }
        let ty = match &layout.field_tys[*index as usize] {
            AsyncFieldTy::Native(ty) => ty.clone(),
            AsyncFieldTy::Future(_) | AsyncFieldTy::RuntimeFuture(_) => continue,
        };
        let ptr = fn_ctx.future_field(*index)?;
        fn_ctx.define_var(name, ptr, ty);
    }

    for stmt in body {
        emit_statement_in_function(&mut fn_ctx, stmt)?;
    }

    if fn_ctx
        .builder
        .get_insert_block()
        .is_some_and(|block| block.get_terminator().is_none())
    {
        fn_ctx.builder.build_unconditional_branch(finish_bb)?;
    }
    if let Some(exception_bb) = fn_ctx.exception_return_bb {
        builder.position_at_end(exception_bb);
        if exception_bb.get_terminator().is_none() {
            builder.build_unconditional_branch(finish_bb)?;
        }
    }

    let mut switch_cases = Vec::with_capacity(layout.suspension_count as usize + 2);
    switch_cases.push((context.i32_type().const_zero(), body_bb));
    for (ordinal, resume_bb) in fn_ctx.async_resume_blocks.iter().enumerate() {
        switch_cases.push((
            context.i32_type().const_int((ordinal + 1) as u64, false),
            *resume_bb,
        ));
    }
    switch_cases.push((
        context
            .i32_type()
            .const_int((layout.suspension_count + 1) as u64, false),
        finish_bb,
    ));
    builder.position_at_end(entry_bb);
    builder.build_switch(state.into_int_value(), fallthrough_bb, &switch_cases)?;

    builder.position_at_end(fallthrough_bb);
    builder.build_return(Some(&context.i8_type().const_int(1, false)))?;

    builder.position_at_end(finish_bb);
    if layout.ret_ty != Ty::Void {
        let result_ptr =
            builder.build_struct_gep(future_ty, future, layout.result_index, "result_ptr")?;
        let default_val = default_llvm_value(&layout.ret_ty, context);
        builder.build_store(result_ptr, default_val)?;
    }
    builder.build_return(Some(&context.i8_type().const_int(1, false)))?;

    Ok(())
}

fn declare_async_drop<'ctx>(
    module: &Module<'ctx>,
    context: &'ctx Context,
    struct_name: &str,
) -> Result<(), crate::CodegenError> {
    let name = format!("{struct_name}_drop");
    if module.get_function(&name).is_none() {
        module.add_function(
            &name,
            context
                .void_type()
                .fn_type(&[context.ptr_type(AddressSpace::default()).into()], false),
            Some(inkwell::module::Linkage::Internal),
        );
    }
    Ok(())
}

fn emit_async_drop<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    struct_name: &str,
    layout: &AsyncLayout,
) -> Result<(), crate::CodegenError> {
    let function = module
        .get_function(&format!("{struct_name}_drop"))
        .ok_or_else(|| crate::CodegenError::LLVMError("missing async drop function".into()))?;
    let entry = context.append_basic_block(function, "entry");
    let builder = context.create_builder();
    let entry_builder = context.create_builder();
    builder.position_at_end(entry);
    entry_builder.position_at_end(entry);
    let future_ty = module.get_struct_type(struct_name).ok_or_else(|| {
        crate::CodegenError::LLVMError(format!("missing future struct {struct_name}"))
    })?;
    let future = function
        .get_nth_param(0)
        .ok_or_else(|| crate::CodegenError::LLVMError("missing async drop parameter".into()))?
        .into_pointer_value();
    let mut fn_ctx = FunctionContext::new(
        function,
        &builder,
        &entry_builder,
        entry,
        module,
        Ty::Void,
        context,
    );
    fn_ctx.future_base = Some((future, future_ty));

    for (index, field) in layout.field_tys.iter().enumerate() {
        let ptr = fn_ctx.future_field(index as u32)?;
        match field {
            AsyncFieldTy::Native(ty) if index != 0 && index as u32 != layout.result_index => {
                if ty_is_owned_handle(ty) {
                    let value = builder.build_load(ty_to_llvm(ty, context), ptr, "future_drop")?;
                    emit_drop_value(&mut fn_ctx, &TypedValue::new(value, ty.clone()))?;
                    builder.build_store(ptr, default_llvm_value(ty, context))?;
                }
            }
            AsyncFieldTy::RuntimeFuture(kind) => {
                // Release the handle so a future dropped while parked on it
                // does not leave the entry behind. The slot is zero until the
                // await arms it and the drops are null-safe.
                let drop_fn = module.get_function(kind.drop_fn_name()).ok_or_else(|| {
                    crate::CodegenError::LLVMError(format!("{} not declared", kind.drop_fn_name()))
                })?;
                let handle = builder
                    .build_load(context.i64_type(), ptr, "runtime_future_drop")?
                    .into_int_value();
                builder.build_call(drop_fn, &[handle.into()], "runtime_future_drop_call")?;
                builder.build_store(ptr, context.i64_type().const_zero())?;
            }
            AsyncFieldTy::Future(child) => {
                let child_fn = module
                    .get_function(&format!("ntsc_future_{child}_drop"))
                    .ok_or_else(|| {
                        crate::CodegenError::LLVMError(format!(
                            "missing child future drop function for `{child}`"
                        ))
                    })?;
                let child_ptr = builder.build_pointer_cast(
                    ptr,
                    context.ptr_type(AddressSpace::default()),
                    "child_future_drop",
                )?;
                builder.build_call(child_fn, &[child_ptr.into()], "child_drop")?;
            }
            _ => {}
        }
    }
    builder.build_return(None)?;
    Ok(())
}

pub(crate) fn emit_async_suspend_statement<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    stmt: &Stmt,
) -> Result<(), crate::CodegenError> {
    let layout = fn_ctx.async_layout.ok_or_else(|| {
        crate::CodegenError::LLVMError("async suspension outside a poll body".into())
    })?;
    let ordinal = fn_ctx.async_suspend_ordinal as usize;
    let kind = layout.susp_order.get(ordinal).copied().ok_or_else(|| {
        crate::CodegenError::LLVMError("async suspension ordinal exceeds layout".into())
    })?;
    match kind {
        SuspKind::Await(_) if await_stmt_parts(stmt).is_err() => {
            return Err(crate::CodegenError::LLVMError(
                "async await statement is missing from the suspension layout".into(),
            ));
        }
        SuspKind::Chan(_) if chan_target_ty(stmt).is_none() => {
            return Err(crate::CodegenError::LLVMError(
                "async channel statement is missing from the suspension layout".into(),
            ));
        }
        _ => {}
    }

    let resume_bb = fn_ctx
        .context
        .append_basic_block(fn_ctx.function, &format!("async_resume_{ordinal}"));
    fn_ctx.async_resume_blocks.push(resume_bb);
    match kind {
        SuspKind::Await(await_idx) => emit_await_suspend(fn_ctx, layout, await_idx)?,
        SuspKind::Chan(chan_idx) => emit_chan_suspend(fn_ctx, layout, chan_idx)?,
    }
    let pending_bb = fn_ctx
        .context
        .append_basic_block(fn_ctx.function, "async_pending");
    if fn_ctx
        .builder
        .get_insert_block()
        .is_some_and(|block| block.get_terminator().is_some())
    {
        return Ok(());
    }
    fn_ctx.builder.build_unconditional_branch(pending_bb)?;
    fn_ctx.builder.position_at_end(pending_bb);
    fn_ctx
        .builder
        .build_return(Some(&fn_ctx.context.i8_type().const_zero()))?;
    fn_ctx.builder.position_at_end(resume_bb);
    match kind {
        SuspKind::Await(await_idx) => emit_await_resume(fn_ctx, layout, await_idx)?,
        SuspKind::Chan(chan_idx) => emit_chan_resume(fn_ctx, layout, chan_idx)?,
    }
    fn_ctx.async_suspend_ordinal += 1;
    Ok(())
}

/// Suspend on await `await_idx`: zero the child's future slot, evaluate the
/// await arguments into the child's parameter fields, push the child poll
/// onto the executor, and store resume state `k+1`.
pub(crate) fn emit_await_suspend<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    layout: &AsyncLayout,
    await_idx: usize,
) -> Result<(), crate::CodegenError> {
    let info = &layout.await_infos[await_idx];
    let stmt = &info.stmt;
    let (_callee_expr, arguments) = await_stmt_parts(stmt)?;

    let child_slot = fn_ctx.future_field(layout.sub_field_base + await_idx as u32)?;

    // Offloaded accept: create the op, store its handle in the slot, and park
    // on its poll function (same shape as the offloaded http futures).
    if let Some(rest) = info.child_name.strip_prefix("rust.") {
        let mut parts = rest.splitn(2, '.');
        let crate_name = parts.next().unwrap_or_default().replace('-', "_");
        let fn_name = parts.next().unwrap_or_default();
        let bridge = format!("ntsc_adaptor_{crate_name}_{fn_name}");
        let args = emit_call_arguments(fn_ctx, arguments)?;
        // Declare the adaptor bridge on first use, as the synchronous
        // call path does: i64 handles in, an async-op handle out.
        let i64_ty = fn_ctx.context.i64_type();
        let param_tys: Vec<inkwell::types::BasicMetadataTypeEnum> =
            args.iter().map(|_| i64_ty.into()).collect();
        let fn_ty = i64_ty.fn_type(&param_tys, false);
        let function = fn_ctx.module.get_function(&bridge).unwrap_or_else(|| {
            fn_ctx
                .module
                .add_function(&bridge, fn_ty, Some(inkwell::module::Linkage::External))
        });
        let values: Vec<inkwell::values::BasicMetadataValueEnum<'ctx>> = args
            .iter()
            .map(|a| coerce_to_i64(fn_ctx, a.value).map(Into::into))
            .collect::<Result<_, _>>()?;
        let call = fn_ctx
            .builder
            .build_call(function, &values, "rust_async_new")?;
        let handle = call_result_to_value(fn_ctx, &call).into_int_value();
        fn_ctx.builder.build_store(child_slot, handle)?;
        let poll = fn_ctx
            .module
            .get_function("ntsc_async_op_poll")
            .ok_or_else(|| {
                crate::CodegenError::LLVMError("ntsc_async_op_poll not declared".into())
            })?;
        let poll_ptr = fn_ctx.builder.build_pointer_cast(
            poll.as_global_value().as_pointer_value(),
            fn_ctx.context.ptr_type(AddressSpace::default()),
            "rust_poll",
        )?;
        let push = fn_ctx
            .module
            .get_function("ntsc_async_push")
            .ok_or_else(|| crate::CodegenError::LLVMError("ntsc_async_push not declared".into()))?;
        fn_ctx
            .builder
            .build_call(push, &[poll_ptr.into(), handle.into()], "async_push")?;
        let state = fn_ctx.future_field(0)?;
        fn_ctx.builder.build_store(
            state,
            fn_ctx
                .context
                .i32_type()
                .const_int((layout.await_state_index[await_idx] + 1) as u64, false),
        )?;
        return Ok(());
    }

    if let Some(abi) = net_future_abi(&info.child_name) {
        let new_fn = fn_ctx.module.get_function(abi.new_fn).ok_or_else(|| {
            crate::CodegenError::LLVMError(format!("{} not declared", abi.new_fn))
        })?;
        let arg_values = emit_call_arguments(fn_ctx, arguments)?;
        let llvm_args: Vec<inkwell::values::BasicMetadataValueEnum<'ctx>> =
            arg_values.iter().map(|arg| arg.value.into()).collect();
        let op = fn_ctx
            .builder
            .build_call(new_fn, &llvm_args, "net_accept_new")?;
        let handle = call_result_to_value(fn_ctx, &op);
        fn_ctx.builder.build_store(child_slot, handle)?;

        let poll_fn = fn_ctx.module.get_function(abi.poll_fn).ok_or_else(|| {
            crate::CodegenError::LLVMError(format!("{} not declared", abi.poll_fn))
        })?;
        let poll_i8 = fn_ctx.builder.build_pointer_cast(
            poll_fn.as_global_value().as_pointer_value(),
            fn_ctx.context.ptr_type(AddressSpace::default()),
            "net_accept_poll_fn",
        )?;
        let push_fn = fn_ctx
            .module
            .get_function("ntsc_async_push")
            .ok_or_else(|| crate::CodegenError::LLVMError("ntsc_async_push not declared".into()))?;
        fn_ctx.builder.build_call(
            push_fn,
            &[poll_i8.into(), handle.into_int_value().into()],
            "async_push",
        )?;
        let state_ptr = fn_ctx.future_field(0)?;
        let next_state = fn_ctx
            .context
            .i32_type()
            .const_int((layout.await_state_index[await_idx] as u64) + 1, false);
        fn_ctx.builder.build_store(state_ptr, next_state)?;
        return Ok(());
    }

    // Offloaded runtime futures: create the op, store its handle in the
    // slot, and park on its poll function.
    if let Some(method) = info.child_name.strip_prefix("http.") {
        let new_fn_name = format!(
            "ntsc_async_http_{}",
            method.strip_suffix("_async").unwrap_or(method)
        );
        let new_fn = fn_ctx
            .module
            .get_function(&new_fn_name)
            .ok_or_else(|| crate::CodegenError::LLVMError(format!("{new_fn_name} not declared")))?;
        let arg_values = emit_call_arguments(fn_ctx, arguments)?;
        let llvm_args: Vec<inkwell::values::BasicMetadataValueEnum<'ctx>> =
            arg_values.iter().map(|arg| arg.value.into()).collect();
        let op = fn_ctx
            .builder
            .build_call(new_fn, &llvm_args, "http_async_new")?;
        let handle = call_result_to_value(fn_ctx, &op);
        fn_ctx.builder.build_store(child_slot, handle)?;

        let poll_fn = fn_ctx
            .module
            .get_function("ntsc_async_http_poll")
            .ok_or_else(|| {
                crate::CodegenError::LLVMError("ntsc_async_http_poll not declared".into())
            })?;
        let poll_ptr = poll_fn.as_global_value().as_pointer_value();
        let poll_i8 = fn_ctx.builder.build_pointer_cast(
            poll_ptr,
            fn_ctx.context.ptr_type(AddressSpace::default()),
            "http_poll_fn",
        )?;
        let push_fn = fn_ctx
            .module
            .get_function("ntsc_async_push")
            .ok_or_else(|| crate::CodegenError::LLVMError("ntsc_async_push not declared".into()))?;
        fn_ctx.builder.build_call(
            push_fn,
            &[poll_i8.into(), handle.into_int_value().into()],
            "async_push",
        )?;

        let state_ptr = fn_ctx.future_field(0)?;
        let next_state = fn_ctx
            .context
            .i32_type()
            .const_int((layout.await_state_index[await_idx] as u64) + 1, false);
        fn_ctx.builder.build_store(state_ptr, next_state)?;
        return Ok(());
    }

    if info.child_name == "sleep" {
        let sleep_new_fn = fn_ctx
            .module
            .get_function("ntsc_async_sleep_new")
            .ok_or_else(|| {
                crate::CodegenError::LLVMError("ntsc_async_sleep_new not declared".into())
            })?;
        let arg_values = emit_call_arguments(fn_ctx, arguments)?;
        let ms = arg_values
            .first()
            .ok_or_else(|| {
                crate::CodegenError::LLVMError("async.sleep requires a duration argument".into())
            })?
            .value
            .into_int_value();
        let sleep_result = fn_ctx
            .builder
            .build_call(sleep_new_fn, &[ms.into()], "sleep_new")?;
        let sleep_handle = call_result_to_value(fn_ctx, &sleep_result);
        fn_ctx.builder.build_store(child_slot, sleep_handle)?;

        let sleep_poll_fn = fn_ctx
            .module
            .get_function("ntsc_async_sleep_poll")
            .ok_or_else(|| {
                crate::CodegenError::LLVMError("ntsc_async_sleep_poll not declared".into())
            })?;
        let poll_ptr = sleep_poll_fn.as_global_value().as_pointer_value();
        let poll_i8 = fn_ctx.builder.build_pointer_cast(
            poll_ptr,
            fn_ctx.context.ptr_type(AddressSpace::default()),
            "sleep_poll_fn",
        )?;
        let push_fn = fn_ctx
            .module
            .get_function("ntsc_async_push")
            .ok_or_else(|| crate::CodegenError::LLVMError("ntsc_async_push not declared".into()))?;
        fn_ctx.builder.build_call(
            push_fn,
            &[poll_i8.into(), sleep_handle.into()],
            "async_push",
        )?;

        let state_ptr = fn_ctx.future_field(0)?;
        let next_state = fn_ctx
            .context
            .i32_type()
            .const_int((layout.await_state_index[await_idx] as u64) + 1, false);
        fn_ctx.builder.build_store(state_ptr, next_state)?;
        return Ok(());
    }

    let child_struct_name = format!("ntsc_future_{}", info.child_name);
    let child_struct_ty = fn_ctx
        .module
        .get_struct_type(&child_struct_name)
        .ok_or_else(|| {
            crate::CodegenError::LLVMError(format!(
                "internal: child future {child_struct_name} not declared"
            ))
        })?;

    let child_ptr = fn_ctx.builder.build_pointer_cast(
        child_slot,
        fn_ctx.context.ptr_type(AddressSpace::default()),
        "child_future",
    )?;

    let zero = fn_ctx.context.i8_type().const_zero();
    let child_size = child_struct_ty.size_of().ok_or_else(|| {
        crate::CodegenError::LLVMError(format!("internal: {child_struct_name} has no size"))
    })?;
    fn_ctx
        .builder
        .build_memset(child_ptr, 1, zero, child_size)?;

    let arg_values = emit_call_arguments(fn_ctx, arguments)?;
    for (i, arg_val) in arg_values.iter().enumerate() {
        // The child releases its parameter references in its drop.
        if let Some(arg) = arguments.get(i) {
            retain_chan_value(fn_ctx, arg, arg_val)?;
        }
        let slot = fn_ctx.builder.build_struct_gep(
            child_struct_ty,
            child_ptr,
            1 + i as u32,
            "child_arg",
        )?;
        fn_ctx.builder.build_store(slot, arg_val.value)?;
    }

    let child_poll_name = if info.child_name == "sleep" {
        "ntsc_async_sleep_poll".to_string()
    } else {
        format!("ntsc_future_{}_poll", info.child_name)
    };
    let child_poll_fn = fn_ctx
        .module
        .get_function(&child_poll_name)
        .ok_or_else(|| {
            crate::CodegenError::LLVMError(format!(
                "internal: child poll {child_poll_name} not declared"
            ))
        })?;
    let poll_ptr = child_poll_fn.as_global_value().as_pointer_value();
    let poll_i8 = fn_ctx.builder.build_pointer_cast(
        poll_ptr,
        fn_ctx.context.ptr_type(AddressSpace::default()),
        "child_poll_fn",
    )?;
    let child_handle =
        fn_ctx
            .builder
            .build_ptr_to_int(child_ptr, fn_ctx.context.i64_type(), "child_handle")?;
    let push_fn = fn_ctx
        .module
        .get_function("ntsc_async_push")
        .ok_or_else(|| crate::CodegenError::LLVMError("ntsc_async_push not declared".into()))?;
    fn_ctx.builder.build_call(
        push_fn,
        &[poll_i8.into(), child_handle.into()],
        "async_push",
    )?;

    let state_ptr = fn_ctx.future_field(0)?;
    let next_state = fn_ctx
        .context
        .i32_type()
        .const_int((layout.await_state_index[await_idx] as u64) + 1, false);
    fn_ctx.builder.build_store(state_ptr, next_state)?;
    Ok(())
}

/// On resume, load the awaited child's result from its future struct and
/// coerce it to the await's declared type; the sleeping path drops the
/// completed runtime sleep handle.
pub(crate) fn emit_await_resume<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    layout: &AsyncLayout,
    await_idx: usize,
) -> Result<(), crate::CodegenError> {
    let info = &layout.await_infos[await_idx];
    let stmt = &info.stmt;
    fn_ctx.emit_pending_exception_check()?;

    // Offloaded accept: reap the client socket handle, then drop the op. The
    // result is an `int` socket handle, not a string.
    if let Some(abi) = net_future_abi(&info.child_name) {
        let child_slot = fn_ctx.future_field(layout.sub_field_base + await_idx as u32)?;
        let handle = fn_ctx
            .builder
            .build_load(fn_ctx.context.i64_type(), child_slot, "net_handle")?
            .into_int_value();
        let result_fn = fn_ctx.module.get_function(abi.result_fn).ok_or_else(|| {
            crate::CodegenError::LLVMError(format!("{} not declared", abi.result_fn))
        })?;
        let result = fn_ctx
            .builder
            .build_call(result_fn, &[handle.into()], "net_accept_result")?;
        let socket = call_result_to_value(fn_ctx, &result);
        let drop_fn = fn_ctx.module.get_function(abi.drop_fn).ok_or_else(|| {
            crate::CodegenError::LLVMError(format!("{} not declared", abi.drop_fn))
        })?;
        fn_ctx
            .builder
            .build_call(drop_fn, &[handle.into()], "net_accept_drop")?;
        fn_ctx
            .builder
            .build_store(child_slot, fn_ctx.context.i64_type().const_zero())?;

        match stmt {
            Stmt::Expression {
                expression: Expr::Assign { name, .. },
            } => emit_await_assignment(fn_ctx, name, socket, &abi.result_ty)?,
            Stmt::Expression { .. } => {
                if matches!(abi.result_ty, Ty::String) {
                    emit_drop_value(fn_ctx, &TypedValue::new(socket, Ty::String))?;
                }
            }
            Stmt::Var {
                name,
                type_annotation,
                ..
            } => {
                let slot_ty = match type_annotation {
                    Some(_) => type_annotation_to_ty(type_annotation),
                    None => abi.result_ty.clone(),
                };
                let field_index = layout.fields.get(name.lexeme()).copied().ok_or_else(|| {
                    crate::CodegenError::LLVMError(format!(
                        "internal: awaited variable `{}` has no future field",
                        name.lexeme()
                    ))
                })?;
                let slot = fn_ctx.future_field(field_index)?;
                fn_ctx.define_var(name.lexeme(), slot, slot_ty.clone());
                let coerced = coerce_value(
                    fn_ctx,
                    TypedValue::new(socket, abi.result_ty.clone()),
                    &slot_ty,
                )?;
                fn_ctx.builder.build_store(slot, coerced.value)?;
            }
            Stmt::Return { .. } => {
                let coerced = coerce_value(
                    fn_ctx,
                    TypedValue::new(socket, abi.result_ty.clone()),
                    &layout.ret_ty,
                )?;
                emit_async_return(fn_ctx, layout, Some(coerced.value))?;
            }
            _ => {
                return Err(crate::CodegenError::LLVMError(
                    "internal: unexpected await statement shape".into(),
                ));
            }
        }
        return Ok(());
    }

    // Offloaded Rust-bridge futures (`await @crate.fn(...)`): reap the
    // bridge's marshaled result, then drop the completed op. The result
    // is a string handle, an int, or an object handle per the bridge's
    // return label.
    if info.child_name.starts_with("rust.") {
        let child_slot = fn_ctx.future_field(layout.sub_field_base + await_idx as u32)?;
        let handle = fn_ctx
            .builder
            .build_load(fn_ctx.context.i64_type(), child_slot, "rust_handle")?
            .into_int_value();
        let result_fn = fn_ctx
            .module
            .get_function("ntsc_async_op_result")
            .ok_or_else(|| {
                crate::CodegenError::LLVMError("ntsc_async_op_result not declared".into())
            })?;
        let result = fn_ctx
            .builder
            .build_call(result_fn, &[handle.into()], "rust_async_result")?;
        let value = call_result_to_value(fn_ctx, &result);
        // A worker-thread throw arrives re-raised by `ntsc_async_op_result`
        // on this thread; unwind the goroutine exactly like a synchronous
        // bridged call would.
        fn_ctx.emit_pending_exception_check()?;
        let drop_fn = fn_ctx
            .module
            .get_function("ntsc_async_op_drop")
            .ok_or_else(|| {
                crate::CodegenError::LLVMError("ntsc_async_op_drop not declared".into())
            })?;
        fn_ctx
            .builder
            .build_call(drop_fn, &[handle.into()], "rust_async_drop")?;
        fn_ctx
            .builder
            .build_store(child_slot, fn_ctx.context.i64_type().const_zero())?;

        match stmt {
            Stmt::Expression {
                expression: Expr::Assign { name, .. },
            } => emit_await_assignment(fn_ctx, name, value, &info.child_ret_ty)?,
            Stmt::Expression { .. } => {
                if matches!(info.child_ret_ty, Ty::String) {
                    emit_drop_value(fn_ctx, &TypedValue::new(value, Ty::String))?;
                }
            }
            Stmt::Var {
                name,
                type_annotation,
                ..
            } => {
                let slot_ty = match type_annotation {
                    Some(_) => type_annotation_to_ty(type_annotation),
                    None => info.child_ret_ty.clone(),
                };
                let field_index = layout.fields.get(name.lexeme()).copied().ok_or_else(|| {
                    crate::CodegenError::LLVMError(format!(
                        "internal: awaited variable `{}` has no future field",
                        name.lexeme()
                    ))
                })?;
                let slot = fn_ctx.future_field(field_index)?;
                fn_ctx.define_var(name.lexeme(), slot, slot_ty.clone());
                let coerced = coerce_value(
                    fn_ctx,
                    TypedValue::new(value, info.child_ret_ty.clone()),
                    &slot_ty,
                )?;
                fn_ctx.builder.build_store(slot, coerced.value)?;
            }
            Stmt::Return { .. } => {
                let coerced = coerce_value(
                    fn_ctx,
                    TypedValue::new(value, info.child_ret_ty.clone()),
                    &layout.ret_ty,
                )?;
                emit_async_return(fn_ctx, layout, Some(coerced.value))?;
            }
            _ => {
                return Err(crate::CodegenError::LLVMError(
                    "internal: unexpected await statement shape".into(),
                ));
            }
        }
        return Ok(());
    }

    // Offloaded http futures: reap the response handle from the op, then
    // drop the completed op.
    if info.child_name.strip_prefix("http.").is_some() {
        let child_slot = fn_ctx.future_field(layout.sub_field_base + await_idx as u32)?;
        let handle = fn_ctx
            .builder
            .build_load(fn_ctx.context.i64_type(), child_slot, "http_handle")?
            .into_int_value();
        let result_fn = fn_ctx
            .module
            .get_function("ntsc_async_http_result")
            .ok_or_else(|| {
                crate::CodegenError::LLVMError("ntsc_async_http_result not declared".into())
            })?;
        let result = fn_ctx
            .builder
            .build_call(result_fn, &[handle.into()], "http_async_result")?;
        let response = call_result_to_value(fn_ctx, &result);
        let drop_fn = fn_ctx
            .module
            .get_function("ntsc_async_http_drop")
            .ok_or_else(|| {
                crate::CodegenError::LLVMError("ntsc_async_http_drop not declared".into())
            })?;
        fn_ctx
            .builder
            .build_call(drop_fn, &[handle.into()], "http_async_drop")?;
        fn_ctx
            .builder
            .build_store(child_slot, fn_ctx.context.i64_type().const_zero())?;

        match stmt {
            Stmt::Expression {
                expression: Expr::Assign { name, .. },
            } => emit_await_assignment(fn_ctx, name, response, &Ty::String)?,
            Stmt::Expression { .. } => {
                emit_drop_value(fn_ctx, &TypedValue::new(response, Ty::String))?;
            }
            Stmt::Var {
                name,
                type_annotation,
                ..
            } => {
                // An unannotated `var x = await http.get_async(..)` binds to
                // the response string; without an annotation the variable
                // would default to `Any`, which loses ownership tracking (a
                // later move out of the slot would go unnoticed by the drop
                // path and the sent handle would be freed twice).
                let slot_ty = match type_annotation {
                    Some(_) => type_annotation_to_ty(type_annotation),
                    None => Ty::String,
                };
                let field_index = layout.fields.get(name.lexeme()).copied().ok_or_else(|| {
                    crate::CodegenError::LLVMError(format!(
                        "internal: awaited variable `{}` has no future field",
                        name.lexeme()
                    ))
                })?;
                let slot = fn_ctx.future_field(field_index)?;
                fn_ctx.define_var(name.lexeme(), slot, slot_ty.clone());
                let coerced =
                    coerce_value(fn_ctx, TypedValue::new(response, Ty::String), &slot_ty)?;
                fn_ctx.builder.build_store(slot, coerced.value)?;
            }
            Stmt::Return { .. } => {
                let coerced = coerce_value(
                    fn_ctx,
                    TypedValue::new(response, Ty::String),
                    &layout.ret_ty,
                )?;
                emit_async_return(fn_ctx, layout, Some(coerced.value))?;
            }
            _ => {
                return Err(crate::CodegenError::LLVMError(
                    "internal: unexpected await statement shape".into(),
                ));
            }
        }
        return Ok(());
    }

    if info.child_name == "sleep" {
        let child_slot = fn_ctx.future_field(layout.sub_field_base + await_idx as u32)?;
        let handle = fn_ctx
            .builder
            .build_load(fn_ctx.context.i64_type(), child_slot, "sleep_handle")?
            .into_int_value();
        let drop_fn = fn_ctx
            .module
            .get_function("ntsc_async_sleep_drop")
            .ok_or_else(|| {
                crate::CodegenError::LLVMError("ntsc_async_sleep_drop not declared".into())
            })?;
        fn_ctx
            .builder
            .build_call(drop_fn, &[handle.into()], "sleep_drop")?;

        fn_ctx
            .builder
            .build_store(child_slot, fn_ctx.context.i64_type().const_zero())?;
        if !matches!(stmt, Stmt::Expression { .. }) {
            return Err(crate::CodegenError::LLVMError(
                "internal: void await consumed by a value statement".into(),
            ));
        }
        return Ok(());
    }

    let child_struct_name = format!("ntsc_future_{}", info.child_name);
    let child_struct_ty = fn_ctx
        .module
        .get_struct_type(&child_struct_name)
        .ok_or_else(|| {
            crate::CodegenError::LLVMError(format!(
                "internal: child future {child_struct_name} not declared"
            ))
        })?;
    let child_slot = fn_ctx.future_field(layout.sub_field_base + await_idx as u32)?;
    let child_ptr = fn_ctx.builder.build_pointer_cast(
        child_slot,
        fn_ctx.context.ptr_type(AddressSpace::default()),
        "child_future",
    )?;

    let result_slot = fn_ctx.builder.build_struct_gep(
        child_struct_ty,
        child_ptr,
        info.child_result_index,
        "child_result",
    )?;
    let child_result_ty = child_struct_ty
        .get_field_type_at_index(info.child_result_index)
        .ok_or_else(|| {
            crate::CodegenError::LLVMError("internal: missing child result field".into())
        })?;
    let child_result =
        fn_ctx
            .builder
            .build_load(child_result_ty, result_slot, "child_result_val")?;

    if ty_is_owned_handle(&info.child_ret_ty) {
        fn_ctx.builder.build_store(
            result_slot,
            default_llvm_value(&info.child_ret_ty, fn_ctx.context),
        )?;
    }

    match stmt {
        Stmt::Expression {
            expression: Expr::Assign { name, .. },
        } => emit_await_assignment(fn_ctx, name, child_result, &info.child_ret_ty)?,
        Stmt::Expression { .. } => {}
        Stmt::Var {
            name,
            type_annotation,
            ..
        } => {
            // An unannotated `var x = await f(..)` binds the child's result
            // type (not `Any`) so ownership of an owned handle is tracked.
            let slot_ty = match type_annotation {
                Some(_) => type_annotation_to_ty(type_annotation),
                None => info.child_ret_ty.clone(),
            };
            let field_index = layout.fields.get(name.lexeme()).copied().ok_or_else(|| {
                crate::CodegenError::LLVMError(format!(
                    "internal: awaited variable `{}` has no future field",
                    name.lexeme()
                ))
            })?;
            let slot = fn_ctx.future_field(field_index)?;
            fn_ctx.define_var(name.lexeme(), slot, slot_ty.clone());
            let coerced = coerce_value(
                fn_ctx,
                TypedValue::new(child_result, info.child_ret_ty.clone()),
                &slot_ty,
            )?;
            fn_ctx.builder.build_store(slot, coerced.value)?;
        }
        Stmt::Return { .. } => {
            let coerced = coerce_value(
                fn_ctx,
                TypedValue::new(child_result, info.child_ret_ty.clone()),
                &layout.ret_ty,
            )?;
            emit_async_return(fn_ctx, layout, Some(coerced.value))?;
        }
        _ => {
            return Err(crate::CodegenError::LLVMError(
                "internal: unexpected await statement shape".into(),
            ));
        }
    }
    Ok(())
}

/// Suspend on a channel send/receive `chan_idx`: evaluate the channel (and
/// value), park the goroutine via `ntask_chan_send`/`ntask_chan_recv`, store
/// the resume state, and return 0. On the next poll the scheduler has
/// completed the op and `emit_chan_resume` reads any received value.
pub(crate) fn emit_chan_suspend<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    layout: &AsyncLayout,
    chan_idx: usize,
) -> Result<(), crate::CodegenError> {
    let info = &layout.chan_infos[chan_idx];
    let stmt = &info.stmt;
    let channel_expr = chan_stmt_channel(stmt)?;
    let channel = emit_expression(fn_ctx, channel_expr)?;
    let channel_arg = channel.value.into_int_value();

    let result = match info.op {
        ChanOpKind::Send => {
            let value_expr = chan_stmt_value(stmt)?;
            let value = emit_expression(fn_ctx, value_expr)?;
            // A send moves the value into the channel: null the source
            // variable's slot when the value was a named owned handle so the
            // goroutine's drop path does not double-free it.
            if let Expr::Variable { name } = value_expr
                && ty_is_owned_handle(&value.ntsc_type)
                && let Some((ptr, _)) = fn_ctx.variables.get(name.lexeme())
            {
                fn_ctx
                    .builder
                    .build_store(*ptr, default_llvm_value(&value.ntsc_type, fn_ctx.context))?;
            }
            let value_arg = value.value.into_int_value();
            let send_fn = fn_ctx
                .module
                .get_function("ntask_chan_send")
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError("ntask_chan_send not declared".into())
                })?;
            fn_ctx.builder.build_call(
                send_fn,
                &[channel_arg.into(), value_arg.into()],
                "chan_send",
            )?
        }
        ChanOpKind::Recv | ChanOpKind::Loop => {
            let recv_fn = fn_ctx
                .module
                .get_function("ntask_chan_recv")
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError("ntask_chan_recv not declared".into())
                })?;
            fn_ctx
                .builder
                .build_call(recv_fn, &[channel_arg.into()], "chan_recv")?
        }
    };
    let returned = call_result_to_value(fn_ctx, &result).into_int_value();
    // `ntask_chan_send`/`ntask_chan_recv` return 1 on an invalid channel
    // handle; assert so a corrupt handle surfaces as a panic. 0 is success.
    let handle_ok = fn_ctx.builder.build_int_compare(
        inkwell::IntPredicate::EQ,
        returned,
        fn_ctx.context.i8_type().const_zero(),
        "chan_handle_ok",
    )?;
    let handle_ok_i8 = fn_ctx.builder.build_int_z_extend(
        handle_ok,
        fn_ctx.context.i8_type(),
        "chan_handle_ok_i8",
    )?;
    fn_ctx.builder.build_call(
        fn_ctx
            .module
            .get_function("ntsc_assert")
            .ok_or_else(|| crate::CodegenError::LLVMError("ntsc_assert not declared".into()))?,
        &[
            handle_ok_i8.into(),
            fn_ctx.context.i64_type().const_zero().into(),
        ],
        "chan_handle_assert",
    )?;

    let state_ptr = fn_ctx.future_field(0)?;
    let next_state = fn_ctx
        .context
        .i32_type()
        .const_int((layout.chan_state_index[chan_idx] as u64) + 1, false);
    fn_ctx.builder.build_store(state_ptr, next_state)?;
    Ok(())
}

/// On resume after a channel receive, read the completed value through
/// `ntask_chan_recv_result`, coerce it to the statement's target type, and
/// store it (or return it). A send has nothing to resume.
pub(crate) fn emit_chan_resume<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    layout: &AsyncLayout,
    chan_idx: usize,
) -> Result<(), crate::CodegenError> {
    let info = &layout.chan_infos[chan_idx];
    if info.op == ChanOpKind::Send {
        return Ok(());
    }
    let stmt = &info.stmt;

    let result_fn = fn_ctx
        .module
        .get_function("ntask_chan_recv_result")
        .ok_or_else(|| {
            crate::CodegenError::LLVMError("ntask_chan_recv_result not declared".into())
        })?;
    let result = fn_ctx
        .builder
        .build_call(result_fn, &[], "chan_recv_result")?;
    let received = call_result_to_value(fn_ctx, &result);

    let receiver = chan_stmt_receiver(stmt)?;
    let receiver_name = receiver.lexeme();
    let field_index = layout.fields.get(receiver_name).copied().ok_or_else(|| {
        crate::CodegenError::LLVMError(format!(
            "internal: received variable `{receiver_name}` has no future field"
        ))
    })?;
    let slot = fn_ctx.future_field(field_index)?;
    fn_ctx.define_var(receiver_name, slot, info.recv_ty.clone());
    let coerced = coerce_value(
        fn_ctx,
        TypedValue::new(received, info.recv_ty.clone()),
        &info.recv_ty,
    )?;
    emit_drop_replaced_value(fn_ctx, slot, &info.recv_ty, &coerced)?;
    fn_ctx.builder.build_store(slot, coerced.value)?;

    if info.op == ChanOpKind::Loop {
        // Loop receive: run the body only when a real value arrived; a
        // close-drain (`recv_ok == 0`) skips straight past the loop.
        let ok_fn = fn_ctx
            .module
            .get_function("ntask_chan_recv_ok")
            .ok_or_else(|| {
                crate::CodegenError::LLVMError("ntask_chan_recv_ok not declared".into())
            })?;
        let ok = fn_ctx.builder.build_call(ok_fn, &[], "chan_recv_ok")?;
        let ok_val = call_result_to_value(fn_ctx, &ok).into_int_value();
        let run_body = fn_ctx.builder.build_int_compare(
            inkwell::IntPredicate::EQ,
            ok_val,
            fn_ctx.context.i8_type().const_int(1, false),
            "recv_got_value",
        )?;
        let body_bb = fn_ctx
            .context
            .append_basic_block(fn_ctx.function, "chan_loop_body");
        let end_bb = fn_ctx
            .context
            .append_basic_block(fn_ctx.function, "chan_loop_end");
        fn_ctx
            .builder
            .build_conditional_branch(run_body, body_bb, end_bb)?;
        fn_ctx.builder.position_at_end(body_bb);
        if let Stmt::ChanRecvFor { body, .. } = stmt {
            emit_statement_in_function(fn_ctx, body.as_ref())?;
        }
        if fn_ctx
            .builder
            .get_insert_block()
            .is_some_and(|block| block.get_terminator().is_none())
        {
            // Back-edge: suspend on the next receive, exactly like the
            // initial suspension (recv, assert handle, store state, yield).
            let channel = emit_expression(fn_ctx, &stmt_channel_clone(stmt)?)?;
            let channel_arg = channel.value.into_int_value();
            let recv_fn = fn_ctx
                .module
                .get_function("ntask_chan_recv")
                .ok_or_else(|| {
                    crate::CodegenError::LLVMError("ntask_chan_recv not declared".into())
                })?;
            let result =
                fn_ctx
                    .builder
                    .build_call(recv_fn, &[channel_arg.into()], "chan_recv_next")?;
            let returned = call_result_to_value(fn_ctx, &result).into_int_value();
            let handle_ok = fn_ctx.builder.build_int_compare(
                inkwell::IntPredicate::EQ,
                returned,
                fn_ctx.context.i8_type().const_zero(),
                "chan_handle_ok_next",
            )?;
            let handle_ok_i8 = fn_ctx.builder.build_int_z_extend(
                handle_ok,
                fn_ctx.context.i8_type(),
                "chan_handle_ok_next_i8",
            )?;
            fn_ctx.builder.build_call(
                fn_ctx.module.get_function("ntsc_assert").ok_or_else(|| {
                    crate::CodegenError::LLVMError("ntsc_assert not declared".into())
                })?,
                &[
                    handle_ok_i8.into(),
                    fn_ctx.context.i64_type().const_zero().into(),
                ],
                "chan_handle_assert_next",
            )?;
            let state_ptr = fn_ctx.future_field(0)?;
            let next_state = fn_ctx
                .context
                .i32_type()
                .const_int((layout.chan_state_index[chan_idx] as u64) + 1, false);
            fn_ctx.builder.build_store(state_ptr, next_state)?;
            fn_ctx
                .builder
                .build_return(Some(&fn_ctx.context.i8_type().const_int(0, false)))?;
        }
        fn_ctx.builder.position_at_end(end_bb);
    }
    Ok(())
}

/// The channel operand expression of a channel suspension statement,
/// cloned so the loop back-edge can re-evaluate it.
fn stmt_channel_clone(stmt: &Stmt) -> Result<Expr, crate::CodegenError> {
    match stmt {
        Stmt::ChanRecvFor { channel, .. } => Ok(channel.clone()),
        _ => chan_stmt_channel(stmt).cloned(),
    }
}

pub(crate) fn emit_async_return_statement<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    layout: &AsyncLayout,
    stmt: &Stmt,
) -> Result<(), crate::CodegenError> {
    let Stmt::Return { value } = stmt else {
        return Err(crate::CodegenError::LLVMError(
            "internal: expected an async return statement".into(),
        ));
    };
    let value = if let Some(expr) = value {
        let value = emit_expression(fn_ctx, expr)?;
        let value = coerce_value(fn_ctx, value, &layout.ret_ty)?;
        if let Expr::Variable { name } = expr
            && ty_is_owned_handle(&value.ntsc_type)
            && let Some((slot, _)) = fn_ctx.variables.get(name.lexeme())
        {
            fn_ctx
                .builder
                .build_store(*slot, default_llvm_value(&value.ntsc_type, fn_ctx.context))?;
        }
        if let Expr::Variable { name } = expr {
            super::set_rust_object_var(name.lexeme(), None);
        }
        Some(value.value)
    } else {
        None
    };
    emit_async_return(fn_ctx, layout, value)
}

/// Complete the future: store the result, set the done state, return 1.
pub(crate) fn emit_async_return<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    layout: &AsyncLayout,
    value: Option<BasicValueEnum<'ctx>>,
) -> Result<(), crate::CodegenError> {
    if let Some(value) = value {
        let result_ptr = fn_ctx.future_field(layout.result_index)?;
        fn_ctx.builder.build_store(result_ptr, value)?;
    }
    let state_ptr = fn_ctx.future_field(0)?;
    let done_state = fn_ctx
        .context
        .i32_type()
        .const_int((layout.suspension_count as u64) + 1, false);
    fn_ctx.builder.build_store(state_ptr, done_state)?;
    fn_ctx
        .builder
        .build_return(Some(&fn_ctx.context.i8_type().const_int(1, false)))?;
    Ok(())
}

/// Spawn a goroutine for `Stmt::Go`: allocate and zero the callee's future,
/// fill its parameter/capture slots, then `ntask_go` it onto the scheduler
/// and drop the handle. The callee's future struct and poll must already
/// exist (module-level async functions are emitted by the pipeline; `go { }`
/// blocks by the pre-pass in `module.rs`).
pub(crate) fn emit_go_program_spawn<'ctx>(
    fn_ctx: &mut FunctionContext<'ctx, '_>,
    stmt: &Stmt,
) -> Result<(), crate::CodegenError> {
    let Stmt::Go {
        call,
        block,
        keyword_span,
    } = stmt
    else {
        return Err(crate::CodegenError::LLVMError(
            "internal: expected a go statement".into(),
        ));
    };

    let (child_name, arguments): (String, Vec<Expr>) = if let Some(_block) = block {
        let name = format!("__ntsc_go_{}", keyword_span.start);
        let captures = ntsc_typeck::go_captures(keyword_span.start);
        let args = captures
            .iter()
            .map(|(capture, _)| Expr::Variable {
                name: ntsc_ast::token::Token::new(
                    ntsc_ast::token::TokenKind::Identifier(capture.clone()),
                    ntsc_ast::span::Span::dummy(),
                ),
            })
            .collect();
        (name, args)
    } else {
        match call {
            Expr::Call {
                callee, arguments, ..
            } => {
                let name = match callee.as_ref() {
                    Expr::Variable { name } => name.lexeme().to_string(),
                    _ => {
                        return Err(crate::CodegenError::LLVMError(
                            "go requires a call to a named async function".into(),
                        ));
                    }
                };
                (name, arguments.clone())
            }
            _ => {
                return Err(crate::CodegenError::LLVMError(
                    "go requires a call or a block".into(),
                ));
            }
        }
    };

    let child_struct_name = format!("ntsc_future_{child_name}");
    let child_struct_ty = fn_ctx
        .module
        .get_struct_type(&child_struct_name)
        .ok_or_else(|| {
            crate::CodegenError::LLVMError(format!(
                "internal: go callee future {child_struct_name} not declared"
            ))
        })?;
    let child_size = child_struct_ty.size_of().ok_or_else(|| {
        crate::CodegenError::LLVMError(format!("internal: {child_struct_name} has no size"))
    })?;

    let malloc_fn = fn_ctx
        .module
        .get_function("malloc")
        .ok_or_else(|| crate::CodegenError::LLVMError("malloc not declared".into()))?;
    let child_ptr = call_result_to_value(
        fn_ctx,
        &fn_ctx
            .builder
            .build_call(malloc_fn, &[child_size.into()], "go_future_alloc")?,
    )
    .into_pointer_value();
    let child_ptr = fn_ctx.builder.build_pointer_cast(
        child_ptr,
        fn_ctx.context.ptr_type(AddressSpace::default()),
        "go_future",
    )?;
    let zero = fn_ctx.context.i8_type().const_zero();
    fn_ctx
        .builder
        .build_memset(child_ptr, 1, zero, child_size)?;

    let arg_values = emit_call_arguments(fn_ctx, &arguments)?;
    let mut object_fields = Vec::new();
    for (i, arg_val) in arg_values.iter().enumerate() {
        // The goroutine's future releases its own `chan` reference on drop.
        retain_chan_value(fn_ctx, &arguments[i], arg_val)?;
        let mut stored = arg_val.value;
        if let Expr::Variable { name } = &arguments[i]
            && let Some((crate_name, _)) = super::rust_object_var(name.lexeme())
        {
            let handle = super::call::coerce_to_i64(fn_ctx, stored)?;
            stored = super::emit_object_retain(fn_ctx, &crate_name, handle)?.into();
            object_fields.push(1 + i as u32);
        } else if super::call::fresh_object_call(&arguments[i]).is_some() {
            object_fields.push(1 + i as u32);
        }
        // A borrow of another owner's handle (an element read like `a[i]`,
        // not a bare variable and not a fresh construction) must not enter
        // the future as-is: the future's drop would reclaim the source
        // container's registry entry from under it. Bare variables steal
        // their slot below, and fresh values have no other owner, so both
        // keep moving as before; everything else gets an independent copy.
        else if ty_is_owned_handle(&arg_val.ntsc_type)
            && !matches!(arg_val.ntsc_type, Ty::Chan(_))
            && !matches!(&arguments[i], Expr::Variable { .. })
            && !expr_is_fresh(fn_ctx, &arguments[i], arg_val)
        {
            if let Ty::Shared(_) = &arg_val.ntsc_type {
                let retain_fn = fn_ctx
                    .module
                    .get_function("ntsc_shared_retain")
                    .ok_or_else(|| {
                        crate::CodegenError::LLVMError("ntsc_shared_retain not declared".into())
                    })?;
                fn_ctx.builder.build_call(
                    retain_fn,
                    &[inkwell::values::BasicMetadataValueEnum::IntValue(
                        stored.into_int_value(),
                    )],
                    "go_arg_retain",
                )?;
            } else {
                stored =
                    copy_owned_value(fn_ctx, &TypedValue::new(stored, arg_val.ntsc_type.clone()))?
                        .value;
            }
        }
        let slot =
            fn_ctx
                .builder
                .build_struct_gep(child_struct_ty, child_ptr, 1 + i as u32, "go_arg")?;
        fn_ctx.builder.build_store(slot, stored)?;
        if ty_is_owned_handle(&arg_val.ntsc_type)
            && !matches!(arg_val.ntsc_type, Ty::Chan(_))
            && let Expr::Variable { name } = &arguments[i]
            && let Some((ptr, _)) = fn_ctx.variables.get(name.lexeme())
        {
            fn_ctx
                .builder
                .build_store(*ptr, default_llvm_value(&arg_val.ntsc_type, fn_ctx.context))?;
        }
    }

    let trampoline_name = if object_fields.is_empty() {
        format!("ntsc_future_{child_name}_go_poll")
    } else {
        format!("ntsc_future_{child_name}_go_{}_poll", keyword_span.start)
    };
    if fn_ctx.module.get_function(&trampoline_name).is_none() {
        if object_fields.is_empty() {
            emit_goroutine_trampoline(fn_ctx.context, fn_ctx.module, &child_name)?;
        } else {
            emit_go_object_trampoline(
                fn_ctx.context,
                fn_ctx.module,
                &child_name,
                keyword_span.start,
                &object_fields,
            )?;
        }
    }
    let poll_fn = fn_ctx
        .module
        .get_function(&trampoline_name)
        .ok_or_else(|| {
            crate::CodegenError::LLVMError(format!(
                "internal: go callee trampoline {trampoline_name} not declared"
            ))
        })?;
    let poll_ptr = fn_ctx.builder.build_pointer_cast(
        poll_fn.as_global_value().as_pointer_value(),
        fn_ctx.context.ptr_type(AddressSpace::default()),
        "go_poll_fn",
    )?;
    let child_handle =
        fn_ctx
            .builder
            .build_ptr_to_int(child_ptr, fn_ctx.context.i64_type(), "go_handle")?;
    // `go f(x)` discards the handle, so spawn detached: no registry wrapper is
    // allocated for something nothing can join. The cleanup thunk still lets
    // the scheduler reclaim a future that never completes. It is created on
    // demand here, so a spawn that runs before the callee's trampoline was
    // emitted still hands the scheduler a cleanup — a missing thunk used to
    // fall back to a spawn without cleanup, leaking the future and every
    // handle its slots hold at shutdown.
    let cleanup_fn = if object_fields.is_empty() {
        get_or_create_goroutine_cleanup(fn_ctx.context, fn_ctx.module, &child_name)?
    } else {
        get_or_create_go_object_cleanup(
            fn_ctx.context,
            fn_ctx.module,
            &child_name,
            keyword_span.start,
            &object_fields,
        )?
    };
    let cleanup_ptr = fn_ctx.builder.build_pointer_cast(
        cleanup_fn.as_global_value().as_pointer_value(),
        fn_ctx.context.ptr_type(AddressSpace::default()),
        "go_cleanup_fn",
    )?;
    let go_detached_fn = fn_ctx
        .module
        .get_function("ntask_go_detached")
        .ok_or_else(|| crate::CodegenError::LLVMError("ntask_go_detached not declared".into()))?;
    fn_ctx.builder.build_call(
        go_detached_fn,
        &[poll_ptr.into(), child_handle.into(), cleanup_ptr.into()],
        "go_spawn",
    )?;
    Ok(())
}

/// Emit `ntsc_future_<name>_go_poll(i64 future) -> i8`: the goroutine entry
/// wrapper. Drives the child's poll; on completion it reclaims the
/// heap-allocated future through the cleanup thunk, since the scheduler does
/// not know how the future was allocated.
pub(crate) fn emit_goroutine_trampoline<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    child_name: &str,
) -> Result<(), crate::CodegenError> {
    let trampoline_name = format!("ntsc_future_{child_name}_go_poll");
    if module.get_function(&trampoline_name).is_some() {
        return Ok(());
    }
    let child_poll = module
        .get_function(&format!("ntsc_future_{child_name}_poll"))
        .ok_or_else(|| {
            crate::CodegenError::LLVMError(format!(
                "internal: go callee poll ntsc_future_{child_name}_poll not declared"
            ))
        })?;
    let cleanup_fn = get_or_create_goroutine_cleanup(context, module, child_name)?;

    let trampoline = module.add_function(
        &trampoline_name,
        context
            .i8_type()
            .fn_type(&[context.i64_type().into()], false),
        Some(inkwell::module::Linkage::External),
    );
    let entry = context.append_basic_block(trampoline, "entry");
    let builder = context.create_builder();
    builder.position_at_end(entry);

    let handle = trampoline
        .get_nth_param(0)
        .ok_or_else(|| crate::CodegenError::LLVMError("missing trampoline param".into()))?
        .into_int_value();
    let done = builder.build_call(child_poll, &[handle.into()], "go_poll")?;
    let done_val = done
        .try_as_basic_value()
        .basic()
        .ok_or_else(|| crate::CodegenError::LLVMError("poll returned void".into()))?
        .into_int_value();
    let is_done = builder.build_int_compare(
        inkwell::IntPredicate::EQ,
        done_val,
        context.i8_type().const_int(1, false),
        "go_done",
    )?;

    let cleanup = context.append_basic_block(trampoline, "cleanup");
    let exit = context.append_basic_block(trampoline, "exit");
    builder.build_conditional_branch(is_done, cleanup, exit)?;

    builder.position_at_end(cleanup);
    builder.build_call(cleanup_fn, &[handle.into()], "go_future_cleanup")?;
    builder.build_unconditional_branch(exit)?;

    builder.position_at_end(exit);
    builder.build_return(Some(&done_val))?;
    Ok(())
}

/// Get (or emit) `ntsc_future_<name>_go_cleanup(i64 future)`: reclaims one
/// heap-allocated goroutine future. The trampoline calls it on completion; the
/// scheduler calls it at shutdown for a goroutine that never completed.
fn get_or_create_goroutine_cleanup<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    child_name: &str,
) -> Result<FunctionValue<'ctx>, crate::CodegenError> {
    let name = format!("ntsc_future_{child_name}_go_cleanup");
    if let Some(existing) = module.get_function(&name) {
        return Ok(existing);
    }
    let child_drop = module
        .get_function(&format!("ntsc_future_{child_name}_drop"))
        .ok_or_else(|| {
            crate::CodegenError::LLVMError(format!(
                "internal: go callee drop ntsc_future_{child_name}_drop not declared"
            ))
        })?;
    let free_fn = module
        .get_function("free")
        .ok_or_else(|| crate::CodegenError::LLVMError("free not declared".into()))?;

    let function = module.add_function(
        &name,
        context
            .void_type()
            .fn_type(&[context.i64_type().into()], false),
        Some(inkwell::module::Linkage::External),
    );
    let entry = context.append_basic_block(function, "entry");
    let builder = context.create_builder();
    builder.position_at_end(entry);

    let handle = function
        .get_nth_param(0)
        .ok_or_else(|| crate::CodegenError::LLVMError("missing cleanup param".into()))?
        .into_int_value();
    let is_null = builder.build_int_compare(
        inkwell::IntPredicate::EQ,
        handle,
        context.i64_type().const_zero(),
        "go_cleanup_null",
    )?;
    let body = context.append_basic_block(function, "body");
    let done = context.append_basic_block(function, "done");
    builder.build_conditional_branch(is_null, done, body)?;

    builder.position_at_end(body);
    let ptr = builder.build_int_to_ptr(
        handle,
        context.ptr_type(AddressSpace::default()),
        "go_future_ptr",
    )?;
    builder.build_call(child_drop, &[ptr.into()], "go_future_drop")?;
    builder.build_call(free_fn, &[ptr.into()], "go_future_free")?;
    builder.build_unconditional_branch(done)?;

    builder.position_at_end(done);
    builder.build_return(None)?;
    Ok(function)
}

fn get_or_create_go_object_drop<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    child_name: &str,
    span: usize,
    object_fields: &[u32],
) -> Result<FunctionValue<'ctx>, crate::CodegenError> {
    let name = format!("ntsc_future_{child_name}_go_{span}_drop");
    if let Some(existing) = module.get_function(&name) {
        return Ok(existing);
    }
    let child_drop = module
        .get_function(&format!("ntsc_future_{child_name}_drop"))
        .ok_or_else(|| {
            crate::CodegenError::LLVMError(format!(
                "internal: go callee drop ntsc_future_{child_name}_drop not declared"
            ))
        })?;
    let child_struct = module
        .get_struct_type(&format!("ntsc_future_{child_name}"))
        .ok_or_else(|| {
            crate::CodegenError::LLVMError(format!(
                "internal: go callee future ntsc_future_{child_name} not declared"
            ))
        })?;
    let function = module.add_function(
        &name,
        context
            .void_type()
            .fn_type(&[context.ptr_type(AddressSpace::default()).into()], false),
        Some(inkwell::module::Linkage::Internal),
    );
    let entry = context.append_basic_block(function, "entry");
    let builder = context.create_builder();
    builder.position_at_end(entry);
    let future = function
        .get_nth_param(0)
        .ok_or_else(|| crate::CodegenError::LLVMError("missing go object drop parameter".into()))?
        .into_pointer_value();
    builder.build_call(child_drop, &[future.into()], "go_child_drop")?;
    let release = module
        .get_function("ntsc_shared_object_release")
        .unwrap_or_else(|| {
            module.add_function(
                "ntsc_shared_object_release",
                context
                    .void_type()
                    .fn_type(&[context.i64_type().into()], false),
                Some(inkwell::module::Linkage::External),
            )
        });
    for field in object_fields {
        let slot = builder.build_struct_gep(child_struct, future, *field, "go_object")?;
        let handle = builder
            .build_load(context.i64_type(), slot, "go_object_handle")?
            .into_int_value();
        builder.build_call(release, &[handle.into()], "go_object_release")?;
        builder.build_store(slot, context.i64_type().const_zero())?;
    }
    builder.build_return(None)?;
    Ok(function)
}

fn get_or_create_go_object_cleanup<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    child_name: &str,
    span: usize,
    object_fields: &[u32],
) -> Result<FunctionValue<'ctx>, crate::CodegenError> {
    let name = format!("ntsc_future_{child_name}_go_{span}_cleanup");
    if let Some(existing) = module.get_function(&name) {
        return Ok(existing);
    }
    let drop_fn = get_or_create_go_object_drop(context, module, child_name, span, object_fields)?;
    let free_fn = module
        .get_function("free")
        .ok_or_else(|| crate::CodegenError::LLVMError("free not declared".into()))?;
    let function = module.add_function(
        &name,
        context
            .void_type()
            .fn_type(&[context.i64_type().into()], false),
        Some(inkwell::module::Linkage::External),
    );
    let entry = context.append_basic_block(function, "entry");
    let builder = context.create_builder();
    builder.position_at_end(entry);
    let handle = function
        .get_nth_param(0)
        .ok_or_else(|| crate::CodegenError::LLVMError("missing go object cleanup param".into()))?
        .into_int_value();
    let is_null = builder.build_int_compare(
        inkwell::IntPredicate::EQ,
        handle,
        context.i64_type().const_zero(),
        "go_cleanup_null",
    )?;
    let body = context.append_basic_block(function, "body");
    let done = context.append_basic_block(function, "done");
    builder.build_conditional_branch(is_null, done, body)?;
    builder.position_at_end(body);
    let ptr = builder.build_int_to_ptr(
        handle,
        context.ptr_type(AddressSpace::default()),
        "go_future_ptr",
    )?;
    builder.build_call(drop_fn, &[ptr.into()], "go_future_drop")?;
    builder.build_call(free_fn, &[ptr.into()], "go_future_free")?;
    builder.build_unconditional_branch(done)?;
    builder.position_at_end(done);
    builder.build_return(None)?;
    Ok(function)
}

fn emit_go_object_trampoline<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    child_name: &str,
    span: usize,
    object_fields: &[u32],
) -> Result<(), crate::CodegenError> {
    let trampoline_name = format!("ntsc_future_{child_name}_go_{span}_poll");
    if module.get_function(&trampoline_name).is_some() {
        return Ok(());
    }
    let child_poll = module
        .get_function(&format!("ntsc_future_{child_name}_poll"))
        .ok_or_else(|| {
            crate::CodegenError::LLVMError(format!(
                "internal: go callee poll ntsc_future_{child_name}_poll not declared"
            ))
        })?;
    let cleanup_fn =
        get_or_create_go_object_cleanup(context, module, child_name, span, object_fields)?;
    let trampoline = module.add_function(
        &trampoline_name,
        context
            .i8_type()
            .fn_type(&[context.i64_type().into()], false),
        Some(inkwell::module::Linkage::External),
    );
    let entry = context.append_basic_block(trampoline, "entry");
    let builder = context.create_builder();
    builder.position_at_end(entry);
    let handle = trampoline
        .get_nth_param(0)
        .ok_or_else(|| crate::CodegenError::LLVMError("missing trampoline param".into()))?
        .into_int_value();
    let done = builder.build_call(child_poll, &[handle.into()], "go_poll")?;
    let done_val = done
        .try_as_basic_value()
        .basic()
        .ok_or_else(|| crate::CodegenError::LLVMError("poll returned void".into()))?
        .into_int_value();
    let is_done = builder.build_int_compare(
        inkwell::IntPredicate::EQ,
        done_val,
        context.i8_type().const_int(1, false),
        "go_done",
    )?;
    let cleanup = context.append_basic_block(trampoline, "cleanup");
    let exit = context.append_basic_block(trampoline, "exit");
    builder.build_conditional_branch(is_done, cleanup, exit)?;
    builder.position_at_end(cleanup);
    builder.build_call(cleanup_fn, &[handle.into()], "go_future_cleanup")?;
    builder.build_unconditional_branch(exit)?;
    builder.position_at_end(exit);
    builder.build_return(Some(&done_val))?;
    Ok(())
}

/// Convert a checker type back to a param annotation. Capture types are
/// scalar or handle types, so the general cases reduce to a small table.
fn ty_to_annotation(ty: &Ty) -> ntsc_ast::types::TypeAnnotation {
    use ntsc_ast::types::TypeAnnotation as Ann;
    match ty {
        Ty::Int => Ann::Int,
        Ty::Float => Ann::Float,
        Ty::Bool => Ann::Bool,
        Ty::String => Ann::String,
        Ty::Array(element) => Ann::Array(Some(Box::new(ty_to_annotation(element)))),
        Ty::Chan(element) => Ann::Chan(Box::new(ty_to_annotation(element))),
        _ => Ann::Any,
    }
}

/// Compile a `go { ... }` block as a synthetic async function whose params
/// are the block's captured outer locals (in the sorted order typeck
/// recorded), then emit its future through the normal async machinery.
/// Called from the module pre-pass, which runs before any body is emitted,
/// so the future struct exists when the spawn site references it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_go_block_future<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    program: &Program,
    name: &str,
    block: &[Stmt],
    captures: &[(String, Ty)],
    done: &mut HashSet<String>,
    in_progress: &mut HashSet<String>,
) -> Result<(), crate::CodegenError> {
    if module
        .get_function(&format!("ntsc_future_{name}_poll"))
        .is_some()
    {
        return Ok(());
    }
    let name_token = ntsc_ast::token::Token::new(
        ntsc_ast::token::TokenKind::Identifier(name.to_string()),
        ntsc_ast::span::Span::dummy(),
    );
    let params = captures
        .iter()
        .map(|(capture, ty)| ntsc_ast::expr::FunctionParam {
            name: ntsc_ast::token::Token::new(
                ntsc_ast::token::TokenKind::Identifier(capture.clone()),
                ntsc_ast::span::Span::dummy(),
            ),
            type_annotation: Some(ty_to_annotation(ty)),
        })
        .collect();
    let decl = Stmt::AsyncFunction {
        name: name_token,
        params,
        return_type: None,
        body: block.to_vec(),
    };
    emit_async_function(context, module, program, &decl, done, in_progress)
}

/// Synchronous `__ntsc_user_main` for an async `main`: stack-allocate and
/// zero the root future, drive it through `ntsc_async_run`, and return the
/// stored result (the C `main` wrapper truncates it to the exit code).
pub(crate) fn emit_async_main_wrapper<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    struct_name: &str,
    layout: &AsyncLayout,
) -> Result<(), crate::CodegenError> {
    let future_ty = module.get_struct_type(struct_name).ok_or_else(|| {
        crate::CodegenError::LLVMError(format!("internal: missing future struct {struct_name}"))
    })?;
    let future_size = future_ty.size_of().ok_or_else(|| {
        crate::CodegenError::LLVMError(format!("internal: {struct_name} has no size"))
    })?;

    let ret_llvm = ty_to_llvm(&layout.ret_ty, context);
    let wrapper_ty = if layout.ret_ty == Ty::Void {
        context.void_type().fn_type(&[], false)
    } else {
        ret_llvm.fn_type(&[], false)
    };
    let wrapper = module.add_function(
        "__ntsc_user_main",
        wrapper_ty,
        Some(inkwell::module::Linkage::External),
    );
    let entry = context.append_basic_block(wrapper, "entry");
    let builder = context.create_builder();
    builder.position_at_end(entry);
    let entry_builder = context.create_builder();
    entry_builder.position_at_end(entry);

    let fn_ctx = FunctionContext::new(
        wrapper,
        &builder,
        &entry_builder,
        entry,
        module,
        Ty::Void,
        context,
    );

    let future = fn_ctx.builder.build_alloca(future_ty, "future")?;

    let zero = fn_ctx.context.i8_type().const_zero();
    fn_ctx.builder.build_memset(future, 1, zero, future_size)?;

    let run_fn = fn_ctx
        .module
        .get_function("ntsc_async_run")
        .ok_or_else(|| crate::CodegenError::LLVMError("ntsc_async_run not declared".into()))?;
    let poll_fn = fn_ctx
        .module
        .get_function(&format!("ntsc_future_{}_poll", layout.name))
        .ok_or_else(|| crate::CodegenError::LLVMError("missing main poll function".into()))?;
    let poll_i8 = fn_ctx.builder.build_pointer_cast(
        poll_fn.as_global_value().as_pointer_value(),
        fn_ctx.context.ptr_type(AddressSpace::default()),
        "poll_fn",
    )?;
    let future_handle =
        fn_ctx
            .builder
            .build_ptr_to_int(future, fn_ctx.context.i64_type(), "future_handle")?;
    fn_ctx
        .builder
        .build_call(run_fn, &[poll_i8.into(), future_handle.into()], "async_run")?;

    let result = if layout.ret_ty == Ty::Void {
        None
    } else {
        let result_ptr = fn_ctx.builder.build_struct_gep(
            future_ty,
            future,
            layout.result_index,
            "result_ptr",
        )?;
        let result = fn_ctx
            .builder
            .build_load(ret_llvm, result_ptr, "main_result")?;
        Some(result)
    };

    if let Some(result) = result {
        if ty_is_owned_handle(&layout.ret_ty) {
            fn_ctx.builder.build_store(
                fn_ctx.builder.build_struct_gep(
                    future_ty,
                    future,
                    layout.result_index,
                    "result_drop_slot",
                )?,
                default_llvm_value(&layout.ret_ty, context),
            )?;
        }
        let drop_fn = fn_ctx
            .module
            .get_function(&format!("{struct_name}_drop"))
            .ok_or_else(|| {
                crate::CodegenError::LLVMError("missing main future drop function".into())
            })?;
        fn_ctx
            .builder
            .build_call(drop_fn, &[future.into()], "async_future_drop")?;
        fn_ctx.builder.build_return(Some(&result))?;
    } else {
        let drop_fn = fn_ctx
            .module
            .get_function(&format!("{struct_name}_drop"))
            .ok_or_else(|| {
                crate::CodegenError::LLVMError("missing main future drop function".into())
            })?;
        fn_ctx
            .builder
            .build_call(drop_fn, &[future.into()], "async_future_drop")?;
        fn_ctx.builder.build_return(None)?;
    }
    Ok(())
}
