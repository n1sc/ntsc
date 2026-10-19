# toml-utils

Read and edit `neutron.toml` manifests from NTSC: dependency lookup plus
line-preserving edits that keep comments and unrelated tables intact.

## Install

```console
$ ntsc pkg install toml-utils
```

```toml
[dependencies]
toml-utils = "^1.0.0"
```

## Use

```ntsc
use "toml-utils"

use arrays

fun main() {
    var result[string, string] parsed = read_toml("neutron.toml")
    match (parsed) {
        case Err(e) => say("error: " + e)
        case Ok(document) => {
            var deps = get_dependencies(document)
            var names = dependency_names(deps)
            for (var i = 0; i < arrays.length(names); i = i + 1) {
                var name = arrays.get(names, i)
                say(name + " " + get_dependency(deps, name))
            }
        }
    }
}
```

## API

| Function | Returns |
| --- | --- |
| `read_toml(path)` | The parsed manifest as compact JSON, or an error when it is missing or malformed. |
| `read_text(path)` | The raw file contents. |
| `get_package_name(toml_json)` | `[package].name`, or an error when absent. |
| `get_entry(toml_json)` | `[package].entry`, or an error when absent. |
| `get_dependencies(toml_json)` | The `[dependencies]` table as JSON, or `{}`. |
| `dependency_names(deps)` | Every dependency name. |
| `get_dependency(deps, name)` | The requirement for `name`, or `""`. |
| `set_dependency(path, name, spec)` | Rewrites or inserts one line in `[dependencies]`. |
| `remove_dependency(path, name)` | Deletes one line from `[dependencies]`. |
| `split_lines(content)`, `write_lines(path, lines)` | Line-level helpers used by the editors. |

`set_dependency` and `remove_dependency` edit the file as text rather than
re-serialising the parsed document, so formatting, comments, and tables they
do not understand survive a round trip.

## License

Neutron Permissive License (NPL) 1.1. See [`LICENSE`](LICENSE).
