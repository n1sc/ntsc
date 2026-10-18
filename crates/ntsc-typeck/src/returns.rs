//! Reachable exits used to validate non-void function bodies.

use ntsc_ast::expr::{Expr, LiteralValue};
use ntsc_ast::stmt::{MatchCase, Stmt};

#[derive(Default)]
struct Exits {
    fallthrough: bool,
    breaks: bool,
    continues: bool,
}

impl Exits {
    fn fallthrough() -> Self {
        Self {
            fallthrough: true,
            ..Self::default()
        }
    }

    fn merge(&mut self, other: Self) {
        self.fallthrough |= other.fallthrough;
        self.breaks |= other.breaks;
        self.continues |= other.continues;
    }
}

pub(crate) fn can_fall_through(body: &[Stmt]) -> bool {
    let exits = sequence_exits(body);
    exits.fallthrough || exits.breaks || exits.continues
}

fn sequence_exits(statements: &[Stmt]) -> Exits {
    let mut exits = Exits::fallthrough();
    for statement in statements {
        if !exits.fallthrough {
            break;
        }
        exits.fallthrough = false;
        exits.merge(statement_exits(statement));
    }
    exits
}

fn statement_exits(statement: &Stmt) -> Exits {
    match statement {
        Stmt::Return { .. } => Exits::default(),
        Stmt::Throw { .. } => Exits::default(),
        Stmt::Break { .. } => Exits {
            breaks: true,
            ..Exits::default()
        },
        Stmt::Continue { .. } => Exits {
            continues: true,
            ..Exits::default()
        },
        Stmt::Block { statements, .. } => sequence_exits(statements),
        Stmt::If {
            then_branch,
            elif_branches,
            else_branch,
            ..
        } => {
            let mut exits = statement_exits(then_branch);
            for branch in elif_branches {
                exits.merge(statement_exits(&branch.body));
            }
            exits.merge(
                else_branch
                    .as_deref()
                    .map(statement_exits)
                    .unwrap_or_else(Exits::fallthrough),
            );
            exits
        }
        Stmt::Match {
            cases,
            default_case,
            ..
        } => {
            let mut exits = Exits::default();
            for case in cases {
                exits.merge(statement_exits(&case.body));
            }
            if let Some(default) = default_case {
                exits.merge(statement_exits(default));
            } else if !match_is_exhaustive(cases) {
                exits.fallthrough = true;
            }
            exits
        }
        Stmt::While { condition, body } => loop_exits(body, boolean_literal(condition)),
        Stmt::For {
            condition, body, ..
        } => loop_exits(body, condition.as_ref().map_or(Some(true), boolean_literal)),
        Stmt::DoWhile { body, condition } => {
            let mut exits = statement_exits(body);
            exits.fallthrough = exits.breaks
                || ((exits.fallthrough || exits.continues)
                    && boolean_literal(condition) != Some(true));
            exits.breaks = false;
            exits.continues = false;
            exits
        }
        Stmt::ForIn { body, .. } | Stmt::ForAwait { body, .. } | Stmt::ChanRecvFor { body, .. } => {
            loop_exits(body, None)
        }
        Stmt::Try {
            try_block,
            catch_block,
            finally_block,
            ..
        } => {
            let mut exits = statement_exits(try_block);
            if let Some(catch) = catch_block {
                // Calls and expression evaluation may throw even without an
                // explicit `throw`, so a catch is always a possible path.
                exits.merge(statement_exits(catch));
            }
            if let Some(finally) = finally_block {
                let finally_exits = statement_exits(finally);
                exits.fallthrough &= finally_exits.fallthrough;
                exits.merge(Exits {
                    fallthrough: false,
                    ..finally_exits
                });
            }
            exits
        }
        Stmt::Retry {
            body, catch_block, ..
        } => {
            let mut exits = statement_exits(body);
            exits.fallthrough |= exits.breaks;
            exits.breaks = false;
            exits.continues = false;
            if let Some(catch) = catch_block {
                exits.merge(statement_exits(catch));
            }
            exits
        }
        Stmt::Unsafe { body } | Stmt::Quiet { body, .. } => statement_exits(body),
        _ => Exits::fallthrough(),
    }
}

fn loop_exits(body: &Stmt, condition: Option<bool>) -> Exits {
    if condition == Some(false) {
        return Exits::fallthrough();
    }
    let mut exits = statement_exits(body);
    exits.fallthrough = exits.breaks || condition != Some(true);
    exits.breaks = false;
    exits.continues = false;
    exits
}

fn boolean_literal(expression: &Expr) -> Option<bool> {
    match expression {
        Expr::Literal {
            value: LiteralValue::Bool(value),
            ..
        } => Some(*value),
        Expr::Grouping { expression, .. } => boolean_literal(expression),
        _ => None,
    }
}

fn match_is_exhaustive(cases: &[MatchCase]) -> bool {
    let mut has_ok = false;
    let mut has_err = false;
    let mut has_true = false;
    let mut has_false = false;
    for case in cases.iter().filter(|case| case.guard.is_none()) {
        if let Some(pattern) = &case.pattern {
            match pattern.variant.lexeme() {
                "Ok" => has_ok = true,
                "Err" => has_err = true,
                _ => {}
            }
        } else {
            match &case.value {
                Expr::Variable { name } if name.lexeme() == "_" => return true,
                value => match boolean_literal(value) {
                    Some(true) => has_true = true,
                    Some(false) => has_false = true,
                    None => {}
                },
            }
        }
    }
    (has_ok && has_err) || (has_true && has_false)
}
