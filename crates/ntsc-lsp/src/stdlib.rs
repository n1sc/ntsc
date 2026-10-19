//! Static knowledge of the Neutron standard library used for completion
//! and hover when the compiler cannot provide type information.
//!
//! Module names mirror [`ntsc_typeck::names::BUILTIN_MODULES`]; the function
//! lists cover the most commonly used entry points per module (see
//! `docs/reference/stdlib.md`).

/// A stdlib function entry: name (without the module prefix) and a short
/// description shown in completion and hover.
pub struct StdFunction {
    pub name: &'static str,
    pub detail: &'static str,
}

pub struct StdModule {
    pub name: &'static str,
    pub description: &'static str,
    pub functions: &'static [StdFunction],
}

macro_rules! fns {
    ($(($name:literal, $detail:literal)),* $(,)?) => {
        &[$(StdFunction { name: $name, detail: $detail }),*]
    };
}

pub static MODULES: &[StdModule] = &[
    StdModule {
        name: "arrays",
        description: "Heap array operations.",
        functions: fns!(
            ("new", "arrays.new() -> array[any]"),
            ("length", "arrays.length(a) -> int"),
            ("isEmpty", "arrays.isEmpty(a) -> bool"),
            ("get", "arrays.get(a, i) -> T"),
            ("at", "arrays.at(a, i) -> T"),
            ("push", "arrays.push(a, v)"),
            ("pop", "arrays.pop(a) -> T"),
            ("remove_at", "arrays.remove_at(a, i)"),
            ("remove", "arrays.remove(a, v)"),
            ("clear", "arrays.clear(a)"),
            ("reverse", "arrays.reverse(a)"),
            ("sort", "arrays.sort(a)"),
            ("shuffle", "arrays.shuffle(a)"),
            ("contains", "arrays.contains(a, v) -> bool"),
            ("index_of", "arrays.index_of(a, v) -> int"),
            ("slice", "arrays.slice(a, start, end) -> array[T]"),
            ("clone", "arrays.clone(a) -> array[T]"),
            ("flat", "arrays.flat(a) -> array[T]"),
            ("range", "arrays.range(start, end) -> array[int]"),
            ("fill", "arrays.fill(v, count) -> array[T]"),
        ),
    },
    StdModule {
        name: "strings",
        description: "Owned UTF-8 string operations (byte-based indexing).",
        functions: fns!(
            ("length", "strings.length(s) -> int"),
            ("is_empty", "strings.is_empty(s) -> bool"),
            ("substring", "strings.substring(s, start, end) -> string"),
            ("index_of", "strings.index_of(s, sub, start) -> int"),
            ("last_index_of", "strings.last_index_of(s, sub) -> int"),
            ("contains", "strings.contains(s, sub) -> bool"),
            ("starts_with", "strings.starts_with(s, prefix) -> bool"),
            ("ends_with", "strings.ends_with(s, suffix) -> bool"),
            ("count", "strings.count(s, sub) -> int"),
            ("replace", "strings.replace(s, from, to) -> string"),
            (
                "replace_first",
                "strings.replace_first(s, from, to) -> string"
            ),
            ("trim", "strings.trim(s) -> string"),
            ("trim_left", "strings.trim_left(s) -> string"),
            ("trim_right", "strings.trim_right(s) -> string"),
            ("upper", "strings.upper(s) -> string"),
            ("lower", "strings.lower(s) -> string"),
            ("reverse", "strings.reverse(s) -> string"),
            ("repeat", "strings.repeat(s, n) -> string"),
            ("split", "strings.split(s, delim) -> string"),
            (
                "split_array",
                "strings.split_array(s, delim) -> array[string]"
            ),
            ("join", "strings.join(a, b, delim) -> string"),
            ("join_all", "strings.join_all(arr, sep) -> string"),
            ("char_at", "strings.char_at(s, i) -> string"),
            ("char_code", "strings.char_code(s, i) -> int"),
            ("from_char_code", "strings.from_char_code(code) -> string"),
            ("is_alpha", "strings.is_alpha(s) -> bool"),
            ("is_digit", "strings.is_digit(s) -> bool"),
            ("is_alnum", "strings.is_alnum(s) -> bool"),
        ),
    },
    StdModule {
        name: "fmt",
        description: "Formatting and parsing helpers.",
        functions: fns!(
            ("to_int", "fmt.to_int(s) -> int"),
            ("to_float", "fmt.to_float(s) -> float"),
            ("i64_to_str", "fmt.i64_to_str(n) -> string"),
            ("f64_to_str", "fmt.f64_to_str(f) -> string"),
            ("to_hex", "fmt.to_hex(n) -> string"),
            ("to_oct", "fmt.to_oct(n) -> string"),
            ("is_int", "fmt.is_int(s) -> bool"),
            ("is_float", "fmt.is_float(s) -> bool"),
            ("type_name", "fmt.type_name(tag) -> string"),
            ("pad_left", "fmt.pad_left(s, width, pad) -> string"),
            ("pad_right", "fmt.pad_right(s, width, pad) -> string"),
        ),
    },
    StdModule {
        name: "math",
        description: "Float math functions.",
        functions: fns!(
            ("sqrt", "math.sqrt(x) -> float"),
            ("pow", "math.pow(base, exp) -> float"),
            ("abs", "math.abs(x) -> float"),
            ("ceil", "math.ceil(x) -> float"),
            ("floor", "math.floor(x) -> float"),
            ("round", "math.round(x) -> float"),
            ("sin", "math.sin(x) -> float"),
            ("cos", "math.cos(x) -> float"),
            ("tan", "math.tan(x) -> float"),
        ),
    },
    StdModule {
        name: "io",
        description: "Input/output helpers.",
        functions: fns!(
            ("read_file", "io.read_file(path) -> string"),
            ("write_file", "io.write_file(path, contents)"),
            ("append_file", "io.append_file(path, contents)"),
            ("exists", "io.exists(path) -> bool"),
            ("read_line", "io.read_line() -> string"),
            ("print", "io.print(value)"),
            ("println", "io.println(value)"),
        ),
    },
    StdModule {
        name: "os",
        description: "Operating-system interaction.",
        functions: fns!(
            ("args", "os.args() -> array[string]"),
            ("env", "os.env(name) -> string"),
            ("set_env", "os.set_env(name, value)"),
            ("exit", "os.exit(code)"),
            ("cwd", "os.cwd() -> string"),
        ),
    },
    StdModule {
        name: "paths",
        description: "Filesystem path manipulation.",
        functions: fns!(
            ("join", "paths.join(a, b) -> string"),
            ("basename", "paths.basename(p) -> string"),
            ("dirname", "paths.dirname(p) -> string"),
            ("extension", "paths.extension(p) -> string"),
            ("exists", "paths.exists(p) -> bool"),
            ("is_dir", "paths.is_dir(p) -> bool"),
            ("is_file", "paths.is_file(p) -> bool"),
        ),
    },
    StdModule {
        name: "json",
        description: "JSON encoding and decoding.",
        functions: fns!(
            ("parse", "json.parse(s) -> any"),
            ("stringify", "json.stringify(v) -> string"),
        ),
    },
    StdModule {
        name: "http",
        description: "HTTP client.",
        functions: fns!(
            ("get", "http.get(url) -> string"),
            ("post", "http.post(url, body) -> string"),
            ("request", "http.request(method, url, body) -> string"),
            (
                "request_full",
                "http.request_full(method, url, body, options) -> string"
            ),
        ),
    },
    StdModule {
        name: "random",
        description: "Random number generation.",
        functions: fns!(
            ("int", "random.int(lo, hi) -> int"),
            ("float", "random.float() -> float"),
            ("shuffle", "random.shuffle(a)"),
            ("choice", "random.choice(a) -> T"),
        ),
    },
    StdModule {
        name: "time",
        description: "Clocks and sleeping.",
        functions: fns!(
            ("now", "time.now() -> int"),
            ("sleep", "time.sleep(ms)"),
            ("format", "time.format(ts) -> string"),
        ),
    },
    StdModule {
        name: "collections",
        description: "Map/set collections.",
        functions: fns!(
            ("new_map", "collections.new_map() -> object"),
            ("keys", "collections.keys(m) -> array[string]"),
            ("values", "collections.values(m) -> array[any]"),
            ("has", "collections.has(m, k) -> bool"),
        ),
    },
    StdModule {
        name: "sys",
        description: "System information.",
        functions: fns!(
            ("arch", "sys.arch() -> string"),
            ("os", "sys.os() -> string"),
            ("args", "sys.args() -> array[string]"),
        ),
    },
    StdModule {
        name: "process",
        description: "Child processes.",
        functions: fns!(
            ("run", "process.run(cmd) -> int"),
            ("output", "process.output(cmd) -> string"),
        ),
    },
    StdModule {
        name: "term",
        description: "Interactive terminal mode and key events.",
        functions: fns!(
            ("is_tty", "term.is_tty() -> bool"),
            ("open", "term.open() -> int"),
            ("restore", "term.restore(handle) -> bool"),
            ("poll_key", "term.poll_key(handle, timeout_ms) -> bool"),
            ("key_code", "term.key_code() -> string"),
            ("key_character", "term.key_character() -> string"),
            ("key_ctrl", "term.key_ctrl() -> bool"),
        ),
    },
    StdModule {
        name: "sort",
        description: "Sorting helpers.",
        functions: fns!(
            ("sort", "sort.sort(a)"),
            ("sorted", "sort.sorted(a) -> array[T]"),
        ),
    },
    StdModule {
        name: "hash",
        description: "Hashing helpers.",
        functions: fns!(("hash", "hash.hash(s) -> int"),),
    },
    StdModule {
        name: "encoding",
        description: "Base64/hex encoding.",
        functions: fns!(
            ("base64_encode", "encoding.base64_encode(s) -> string"),
            ("base64_decode", "encoding.base64_decode(s) -> string"),
            ("hex_encode", "encoding.hex_encode(s) -> string"),
            ("hex_decode", "encoding.hex_decode(s) -> string"),
        ),
    },
    StdModule {
        name: "crypto",
        description: "Cryptographic hashes.",
        functions: fns!(
            ("sha256", "crypto.sha256(s) -> string"),
            ("md5", "crypto.md5(s) -> string"),
        ),
    },
    StdModule {
        name: "csv",
        description: "CSV parsing and rendering.",
        functions: fns!(
            ("parse", "csv.parse(s) -> array"),
            ("stringify", "csv.stringify(rows) -> string"),
        ),
    },
    StdModule {
        name: "yaml",
        description: "YAML parsing.",
        functions: fns!(("parse", "yaml.parse(s) -> any"),),
    },
    StdModule {
        name: "net",
        description: "Networking helpers.",
        functions: fns!(
            ("connect", "net.connect(host, port)"),
            ("listen", "net.listen(port)"),
        ),
    },
    StdModule {
        name: "regex",
        description: "Regular expressions.",
        functions: fns!(
            ("is_match", "regex.is_match(pattern, s) -> bool"),
            ("find", "regex.find(pattern, s) -> string"),
            ("find_all", "regex.find_all(pattern, s) -> array[string]"),
            ("replace", "regex.replace(pattern, s, rep) -> string"),
        ),
    },
    StdModule {
        name: "glob",
        description: "Glob file matching.",
        functions: fns!(("glob", "glob.glob(pattern) -> array[string]"),),
    },
    StdModule {
        name: "memory",
        description: "Memory utilities.",
        functions: fns!(("size_of", "memory.size_of(v) -> int"),),
    },
    StdModule {
        name: "slices",
        description: "Slice windowing over arrays.",
        functions: fns!(
            ("slice", "slices.slice(a, start, end) -> slice"),
            ("length", "slices.length(s) -> int"),
            ("get", "slices.get(s, i) -> T"),
        ),
    },
    StdModule {
        name: "testing",
        description: "Test assertions.",
        functions: fns!(
            ("assert", "testing.assert(cond)"),
            ("assert_eq", "testing.assert_eq(a, b)"),
            ("assert_ne", "testing.assert_ne(a, b)"),
        ),
    },
    StdModule {
        name: "archive",
        description: "Archive creation and extraction.",
        functions: fns!(
            ("zip", "archive.zip(src, dst)"),
            ("unzip", "archive.unzip(src, dst)"),
        ),
    },
    StdModule {
        name: "async",
        description: "Async task helpers.",
        functions: fns!(
            ("spawn", "async.spawn(f)"),
            ("await_all", "async.await_all(tasks)"),
        ),
    },
];

/// Look up a module by name.
pub fn module(name: &str) -> Option<&'static StdModule> {
    MODULES.iter().find(|module| module.name == name)
}

/// Language keywords offered by completion, with a short description.
pub const KEYWORDS: &[(&str, &str)] = &[
    ("var", "Declare a variable"),
    ("fun", "Declare a function"),
    ("class", "Declare a class"),
    ("enum", "Declare an enum"),
    ("type", "Declare a type alias"),
    ("trait", "Declare a trait"),
    ("impl", "Implement a trait for a type"),
    ("test", "Declare a test block"),
    ("if", "If statement"),
    ("else", "Else branch"),
    ("elif", "Else-if branch"),
    ("for", "For loop"),
    ("in", "For-in loop element binding"),
    ("while", "While loop"),
    ("do", "Do-while loop"),
    ("break", "Break out of a loop"),
    ("continue", "Continue to the next iteration"),
    ("return", "Return from a function"),
    ("match", "Match statement"),
    ("case", "Match case arm"),
    ("default", "Default match arm"),
    ("try", "Try block"),
    ("catch", "Catch handler"),
    ("finally", "Finally block"),
    ("throw", "Throw an exception"),
    ("retry", "Retry block"),
    ("use", "Import a module"),
    ("from", "Selective import source"),
    ("as", "Import alias"),
    ("say", "Print a value"),
    ("true", "Boolean literal"),
    ("false", "Boolean literal"),
    ("nil", "Nil literal"),
    ("and", "Logical and"),
    ("or", "Logical or"),
    ("this", "Current instance"),
    ("static", "Static variable"),
    ("shared", "Reference-counted shared value"),
    ("view", "Borrow declaration or expression"),
    ("mut", "Mutable borrow"),
    ("copy", "Copy expression"),
    ("own", "Owning allocation type"),
    ("async", "Async function or block"),
    ("await", "Await expression"),
    ("go", "Spawn a goroutine"),
    ("chan", "Channel type and constructor"),
    ("close", "Close a channel"),
    ("unsafe", "Unsafe block"),
    ("quiet", "Warning suppression block"),
    ("int", "Integer type"),
    ("float", "Float type"),
    ("string", "String type"),
    ("bool", "Boolean type"),
    ("array", "Array type"),
    ("object", "Object type"),
    ("option", "Nullable option type"),
    ("result", "Result type"),
    ("any", "Any type"),
    ("pointer", "Pointer type"),
    ("slice", "Slice type"),
];

/// Builtin values always in scope.
pub const BUILTINS: &[(&str, &str)] = &[
    ("say", "say(value) — print a value"),
    ("Ok", "Ok(payload) — result success constructor"),
    ("Err", "Err(payload) — result error constructor"),
    ("alloc", "alloc(size) — allocate memory"),
    ("wait_any", "wait_any(tasks) — wait for any task"),
    ("wait_all", "wait_all(tasks) — wait for all tasks"),
    ("chan", "chan — channel constructor namespace"),
];
