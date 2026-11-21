//! Native stdlib contracts shared by type checking and call emission.

use crate::Ty;

#[derive(Clone, Copy)]
pub enum StdlibType {
    Int,
    Float,
    String,
    Bool,
    Void,
    StringArray,
}

impl StdlibType {
    pub fn ty(self) -> Ty {
        match self {
            Self::Int => Ty::Int,
            Self::Float => Ty::Float,
            Self::String => Ty::String,
            Self::Bool => Ty::Bool,
            Self::Void => Ty::Void,
            Self::StringArray => Ty::Array(Box::new(Ty::String)),
        }
    }
}

pub struct StdlibSignature {
    pub parameters: &'static [StdlibType],
    pub return_type: StdlibType,
}

pub fn signature(module: &str, method: &str) -> Option<StdlibSignature> {
    use StdlibType::*;
    let (parameters, return_type): (&'static [StdlibType], StdlibType) = match module {
        "strings" => match method {
            "length" => (&[String], Int),
            "count" | "last_index_of" => (&[String, String], Int),
            "index_of" => (&[String, String, Int], Int),
            "char_code" => (&[String, Int], Int),
            "contains" | "starts_with" | "ends_with" => (&[String, String], Bool),
            "is_empty" | "is_alpha" | "is_digit" | "is_alnum" => (&[String], Bool),
            "split" => (&[String, String], String),
            "split_array" => (&[String, String], StringArray),
            "substring" => (&[String, Int, Int], String),
            "replace" | "replace_first" | "join" => (&[String, String, String], String),
            "trim" | "trim_left" | "trim_right" | "upper" | "lower" | "reverse" => {
                (&[String], String)
            }
            "from_char_code" => (&[Int], String),
            "repeat" | "char_at" => (&[String, Int], String),
            "join_all" => (&[StringArray, String], String),
            _ => return None,
        },
        "sys" => match method {
            "args" => (&[], StringArray),
            "cwd" => (&[], String),
            "read" | "env" | "listdir" | "walk" | "readlink" => (&[String], String),
            "write" | "append" | "cp" | "cp_all" | "symlink" => (&[String, String], Bool),
            "exists" | "mkdir" | "rm" | "rm_all" | "is_symlink" => (&[String], Bool),
            "exit" => (&[Int], Void),
            "sleep" => (&[Float], Void),
            "exec" => (&[String], Int),
            _ => return None,
        },
        "json" => match method {
            "parse" | "stringify" | "escape_string" | "keys" | "stringify_pretty" => {
                (&[String], String)
            }
            "get" => (&[String, String], String),
            "at" => (&[String, Int], String),
            "length" => (&[String], Int),
            "has" => (&[String, String], Bool),
            "is_valid" => (&[String], Bool),
            _ => return None,
        },
        "fmt" => match method {
            "to_int" => (&[String], Int),
            "to_float" => (&[String], Float),
            "i64_to_str" | "type_name" | "to_hex" | "to_oct" => (&[Int], String),
            "f64_to_str" => (&[Float], String),
            "is_int" | "is_float" => (&[String], Bool),
            "pad_left" | "pad_right" => (&[String, Int, Int], String),
            _ => return None,
        },
        "math" => match method {
            "abs" | "sqrt" | "floor" | "ceil" | "round" | "sin" | "cos" | "tan" => {
                (&[Float], Float)
            }
            "pow" => (&[Float, Float], Float),
            _ => return None,
        },
        _ => return None,
    };
    Some(StdlibSignature {
        parameters,
        return_type,
    })
}

pub fn has_signatures(module: &str) -> bool {
    matches!(module, "strings" | "sys" | "json" | "fmt" | "math")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn abi_type(ty: StdlibType) -> &'static str {
        match ty {
            StdlibType::Float => "f64",
            StdlibType::Bool => "i8",
            StdlibType::Void => "",
            StdlibType::Int | StdlibType::String | StdlibType::StringArray => "i64",
        }
    }

    #[test]
    fn signatures_cover_runtime_exports_and_match_their_abi() {
        let modules = [
            (
                "strings",
                include_str!("../../ntsc-runtime/src/modules/strings.rs"),
            ),
            ("sys", include_str!("../../ntsc-runtime/src/modules/sys.rs")),
            (
                "json",
                include_str!("../../ntsc-runtime/src/modules/json.rs"),
            ),
            ("fmt", include_str!("../../ntsc-runtime/src/modules/fmt.rs")),
            (
                "math",
                include_str!("../../ntsc-runtime/src/modules/math.rs"),
            ),
        ];
        for (module, source) in modules {
            let prefix = format!("pub extern \"C\" fn ntsc_{module}_");
            let mut exports = 0;
            for line in source.lines().filter_map(|line| line.strip_prefix(&prefix)) {
                let (method, declaration) = line.split_once('(').unwrap();
                let (parameters, result) = declaration.split_once(')').unwrap();
                let signature = signature(module, method).expect("runtime export needs a contract");
                let parameters: Vec<_> = parameters
                    .split(',')
                    .filter(|s| !s.trim().is_empty())
                    .collect();
                assert_eq!(
                    signature.parameters.len(),
                    parameters.len(),
                    "{module}.{method}"
                );
                for (expected, parameter) in signature.parameters.iter().zip(parameters) {
                    let actual = parameter.split_once(':').unwrap().1.trim();
                    assert!(
                        abi_type(*expected) == actual
                            || (matches!(expected, StdlibType::Int) && actual == "u8"),
                        "{module}.{method}: unexpected parameter ABI {actual}"
                    );
                }
                let actual_result = result
                    .trim()
                    .trim_end_matches('{')
                    .trim()
                    .strip_prefix("->")
                    .unwrap_or("")
                    .trim();
                assert_eq!(
                    abi_type(signature.return_type),
                    actual_result,
                    "{module}.{method}"
                );
                exports += 1;
            }
            assert!(exports > 0, "{module} exports were not inspected");
        }
    }
}
