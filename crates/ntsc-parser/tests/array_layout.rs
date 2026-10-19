use ntsc_ast::expr::{Expr, LiteralValue};
use ntsc_ast::stmt::Stmt;
use ntsc_lexer::tokenize;
use ntsc_parser::parse;

fn array_elements(source: &str) -> Vec<Expr> {
    let program = parse(&tokenize(source)).expect("array should parse");
    assert_eq!(program.statements.len(), 1);
    match program.statements.into_iter().next().unwrap() {
        Stmt::Var {
            initializer: Some(Expr::ArrayLiteral { elements, .. }),
            ..
        } => elements,
        other => panic!("expected an array initializer, got {other:?}"),
    }
}

#[test]
fn multiline_arrays_preserve_element_values_and_order() {
    let elements = array_elements("var values = [\n// first\n1,\n\n2, // second\n3,\n]");
    let values: Vec<_> = elements
        .iter()
        .map(|element| match element {
            Expr::Literal {
                value: LiteralValue::Number(value),
                ..
            } => value.as_str(),
            other => panic!("expected a number, got {other:?}"),
        })
        .collect();
    assert_eq!(values, ["1", "2", "3"]);
}

#[test]
fn array_layout_accepts_empty_arrays_and_optional_trailing_commas() {
    for source in ["var values = []", "var values = [\n// empty\n]"] {
        assert!(array_elements(source).is_empty());
    }
    for source in [
        "var values = [1]",
        "var values = [1,]",
        "var values = [\n1\n]",
        "var values = [\n1\n,\n]",
    ] {
        assert_eq!(array_elements(source).len(), 1, "{source}");
    }
}

#[test]
fn nested_multiline_arrays_preserve_their_structure() {
    let elements = array_elements("var values = [\n[\n1,\n2,\n],\n[\n],\n]");
    assert_eq!(elements.len(), 2);
    assert!(matches!(&elements[0], Expr::ArrayLiteral { elements, .. } if elements.len() == 2));
    assert!(matches!(&elements[1], Expr::ArrayLiteral { elements, .. } if elements.is_empty()));
}

#[test]
fn newlines_do_not_replace_array_commas() {
    let errors = parse(&tokenize("var values = [1\n2]")).unwrap_err();
    assert_eq!(errors[0].message, "expected ']'");
}

#[test]
fn leading_and_repeated_array_commas_are_rejected() {
    for source in ["var values = [,]", "var values = [1,\n,2]"] {
        let errors = parse(&tokenize(source)).unwrap_err();
        assert_eq!(errors[0].message, "unexpected token ',' in expression");
    }
}

#[test]
fn unterminated_multiline_array_reports_the_missing_bracket() {
    let errors = parse(&tokenize("var values = [\n1\n")).unwrap_err();
    assert_eq!(errors[0].message, "expected ']'");
}
