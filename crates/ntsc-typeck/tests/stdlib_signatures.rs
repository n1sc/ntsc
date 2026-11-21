use ntsc_lexer::tokenize;
use ntsc_parser::parse;
use ntsc_typeck::{TypeError, check_program};

fn check(source: &str) -> Result<(), Vec<TypeError>> {
    check_program(&parse(&tokenize(source)).expect("test source should parse"))
}

#[test]
fn split_returns_string_not_array_even_through_alias() {
    for import in ["use strings", "use strings as s"] {
        let module = if import.ends_with(" as s") {
            "s"
        } else {
            "strings"
        };
        let source = format!(
            "{import}\nfun main() {{ var array[string] parts = {module}.split(\"a-b\", \"-\"); }}"
        );
        let errors = check(&source).expect_err("split must not type as an array");
        assert!(
            errors
                .iter()
                .any(|error| error.message.contains("got `string`")),
            "{errors:?}"
        );
        let source = source.replace(".split(", ".split_array(");
        assert!(check(&source).is_ok(), "{source}");
    }
}

#[test]
fn native_return_types_match_the_runtime_contract() {
    let source = r#"
use strings
use sys
use json
use fmt
use math
fun main() {
    var array[string] args = sys.args();
    var string listing = sys.listdir(".");
    var string tree = sys.walk(".");
    var bool written = sys.write("file", "text");
    var bool removed = sys.rm("file");
    var string parsed = json.parse("{}");
    var string value = json.get(parsed, "key");
    var string keys = json.keys(parsed);
    var string hex = fmt.to_hex(255);
    var string oct = fmt.to_oct(8);
    var int code = strings.char_code("a", 0);
    var int last = strings.last_index_of("aba", "a");
    var float square = math.pow(2, 3);
}
"#;
    assert!(check(source).is_ok(), "{:?}", check(source));
}

#[test]
fn array_operations_preserve_element_types() {
    let source = r#"
use arrays
fun main() {
    var array[int] nums = arrays.range(0, 3);
    nums = arrays.sort(nums);
    nums = arrays.sort(nums, 0);
    nums = arrays.remove(nums, 9);
    nums = arrays.remove_at(nums, 0);
    nums = arrays.reverse(nums);
    nums = arrays.shuffle(nums);
    nums = arrays.clear(nums);
    var array[string] texts = arrays.fill("x", 2);
    var string text = arrays.get(texts, 0);
    var string last = arrays.pop(texts);
    var bool all = arrays.every(nums, fun(int value) -> bool { return value > 0; });
}
"#;
    assert!(check(source).is_ok(), "{:?}", check(source));
    for expression in ["arrays.clone([1, 2])", "arrays.fill(7, 2)"] {
        let source = format!("use arrays\nfun main() {{ var array[string] a = {expression}; }}");
        assert!(check(&source).is_err(), "{source}");
    }
    assert!(check("use arrays\nfun main() { var string a = arrays.get([1, 2], 0); }").is_err());
}

#[test]
fn wrong_arguments_and_unknown_methods_are_rejected() {
    for expression in [
        "strings.split(123, \"-\")",
        "strings.char_code(\"a\", \"0\")",
        "strings.char_code(\"a\", 0, 1)",
        "strings.last_index_of(\"a\", \"a\", 0)",
        "strings.no_such_method(\"a\")",
        "math.sin(\"a\")",
        "arrays.length(\"not an array\")",
        "arrays.push([1, 2], \"a\")",
        "arrays.sort([1, 2], \"mode\")",
        "arrays.set([1, 2], 0, 3)",
        "arrays.every([1, 2], fun(string value) -> bool { return true; })",
    ] {
        let source = format!("use strings\nuse math\nuse arrays\nfun main() {{ {expression}; }}");
        assert!(check(&source).is_err(), "{source}");
    }
}

#[test]
fn module_aliases_do_not_override_shadowing_variables() {
    let source = r#"
use strings as s
class Local {
    fun split() -> int { return 7; }
}
fun main() {
    {
        var Local s = Local();
        var int answer = s.split();
    }
    var string answer = s.split("a-b", "-");
}
"#;
    assert!(check(source).is_ok(), "{:?}", check(source));
}

#[test]
fn arguments_are_checked_even_when_arity_is_wrong() {
    let errors = check("use strings\nfun main() { strings.trim(\"a\", 1 + true); }")
        .expect_err("argument error and arity error should be retained");
    assert!(
        errors
            .iter()
            .any(|error| error.message.contains("expects 1 argument"))
    );
    assert!(
        errors
            .iter()
            .any(|error| !error.message.contains("expects 1 argument")),
        "{errors:?}"
    );
}
