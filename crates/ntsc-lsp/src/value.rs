use ntsc_ast::expr::Expr;
use ntsc_ast::stmt::Stmt;

#[derive(Debug, Clone)]
pub enum ValueSource {
    Name(String, usize),
    This(usize),
    Super(usize),
    Call(Box<ValueSource>),
    Await(Box<ValueSource>),
    Member(Box<ValueSource>, String),
    Index(Box<ValueSource>),
    Class(String),
}

impl ValueSource {
    pub fn parse(text: &str, base: usize) -> Option<Self> {
        let source = format!("{text};");
        let program = ntsc_parser::parse(&ntsc_lexer::tokenize(&source)).ok()?;
        let Stmt::Expression { expression } = program.statements.first()? else {
            return None;
        };
        let mut value = Self::from_expression(expression)?;
        value.shift_offsets(base);
        Some(value)
    }

    pub fn from_expression(expression: &Expr) -> Option<Self> {
        match expression {
            Expr::Variable { name } => Some(Self::Name(name.lexeme().to_string(), name.span.start)),
            Expr::This { keyword } => Some(Self::This(keyword.span.start)),
            Expr::Super { keyword } => Some(Self::Super(keyword.span.start)),
            Expr::Call { callee, .. } => Some(Self::Call(Box::new(Self::from_expression(callee)?))),
            Expr::Await { callee, .. } => {
                Some(Self::Await(Box::new(Self::from_expression(callee)?)))
            }
            Expr::Member { object, property } | Expr::OptionalMember { object, property } => {
                Some(Self::Member(
                    Box::new(Self::from_expression(object)?),
                    property.lexeme().to_string(),
                ))
            }
            Expr::IndexGet { object, .. } => {
                Some(Self::Index(Box::new(Self::from_expression(object)?)))
            }
            Expr::StructLiteral { class_name, .. } => {
                Some(Self::Class(class_name.lexeme().to_string()))
            }
            Expr::Grouping { expression, .. } | Expr::Copy { expression, .. } => {
                Self::from_expression(expression)
            }
            Expr::View { target, .. } | Expr::Borrow { target, .. } => {
                Self::from_expression(target)
            }
            _ => None,
        }
    }

    pub fn shift_offsets(&mut self, base: usize) {
        match self {
            Self::Name(_, offset) | Self::This(offset) | Self::Super(offset) => *offset += base,
            Self::Call(source)
            | Self::Await(source)
            | Self::Index(source)
            | Self::Member(source, _) => source.shift_offsets(base),
            Self::Class(_) => {}
        }
    }
}
