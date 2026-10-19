# Standard library reference

The standard library is exposed as modules callable from Neutron source.
Import a module with `use module` (for example, `use arrays`) before calling
its functions. The `async` and `chan` namespaces are always in scope.
Functions follow the `module.func` naming
convention. Return types below are as seen by NTSC source; runtime `bool`
results are normalized to `bool` and scalar results to `int`/`float`.

Unless noted, a function that fails throws an exception whose message starts
with `module.func:`. Functions that previously returned a default value on
failure now throw instead.

## Arrays

The `arrays` module operates on the runtime heap array type produced by array
literals. Mutating operations take the array in place and return `void`;
functional operations take a view and return a new owned array. A `shared
array[T]` argument is borrowed for the operation.

| Function | Returns | Behavior |
| --- | --- | --- |
| `arrays.new()` | `array[any]` | Creates an empty untyped array. |
| `arrays.length(a)` | `int` | Number of elements. |
| `arrays.isEmpty(a)` | `bool` | `true` when the array is empty. |
| `arrays.get(a, i)` / `arrays.at(a, i)` | `T` | Element at index; bounds-checked. |
| `arrays.push(a, v)` | `void` | Appends `v` in place. |
| `arrays.pop(a)` | `T` | Removes and returns the last element. |
| `arrays.remove_at(a, i)` | `array[T]` | New array without the element at `i`; `a` is unchanged, so assign the result (`a = arrays.remove_at(a, i)`). |
| `arrays.remove(a, v)` | `array[T]` | New array without the first element equal to `v`; `a` is unchanged, so assign the result. |
| `arrays.clear(a)` | `void` | Removes all elements. |
| `arrays.reverse(a)` | `array[T]` | New array with elements reversed; `a` is unchanged, so assign the result. |
| `arrays.sort(a, dir)` | `array[T]` | New array sorted in direction `dir`; `a` is unchanged, so assign the result. |
| `arrays.shuffle(a)` | `void` | Randomizes in place. |
| `arrays.contains(a, v)` | `bool` | Whether `v` is present. |
| `arrays.index_of(a, v)` | `int` | First index of `v`, or `-1`. |
| `arrays.slice(a, start, end)` | `array[T]` | New array of elements `start..end`. |
| `arrays.clone(a)` | `array[T]` | New array with the same elements. |
| `arrays.flat(a)` | `array[T]` | Nested arrays flattened one level. |
| `arrays.range(start, end)` | `array[int]` | `[start, start+1, ..., end-1]`. |
| `arrays.fill(v, count)` | `array[T]` | `count` copies of `v`. |

`arrays.new()` and `arrays.fill(v, 0)` both produce an `array[any]`, and an
`any` element carries no static type: reading one as a number or a `bool`
does not do arithmetic, it reinterprets the value. Declare the element type
and seed the array with a typed empty — `var array[int] xs = arrays.fill(0, 0)`
— and reserve the untyped form for values that are genuinely mixed.

## Strings

Strings are owned UTF-8 values. String indexing is byte-based.

| Function | Returns | Behavior |
| --- | --- | --- |
| `strings.length(s)` | `int` | Length in bytes. |
| `strings.is_empty(s)` | `bool` | Whether the string is empty. |
| `strings.substring(s, start, end)` | `string` | Bytes `start..end`, snapped to UTF-8 boundaries. A negative `start` clamps to `0`; a negative `end` means the rest of the string; empty when `start >= end`. |
| `strings.index_of(s, sub, start)` | `int` | First byte offset of `sub` at or after byte `start` (negative clamps to `0`), or `-1`. An empty `sub` returns `-1`. |
| `strings.last_index_of(s, sub)` | `int` | Last byte offset of `sub`, or `-1`. |
| `strings.contains(s, sub)` | `bool` | Whether `s` contains `sub`. |
| `strings.starts_with(s, prefix)` | `bool` | Prefix test. |
| `strings.ends_with(s, suffix)` | `bool` | Suffix test. |
| `strings.count(s, sub)` | `int` | Number of non-overlapping occurrences; `0` for an empty `sub`. |
| `strings.replace(s, from, to)` | `string` | Replace all occurrences. |
| `strings.replace_first(s, from, to)` | `string` | Replace the first occurrence. |
| `strings.trim(s)` / `trim_left` / `trim_right` | `string` | Strip Unicode whitespace. |
| `strings.upper(s)` / `strings.lower(s)` | `string` | Full Unicode case conversion. |
| `strings.reverse(s)` | `string` | Reversed by Unicode character. |
| `strings.repeat(s, n)` | `string` | `s` repeated `n` times; empty when `n <= 0`. |
| `strings.split(s, delim)` | `string` | Split by `delim` (or per Unicode character when empty), joined by newlines. |
| `strings.join(a, b, delim)` | `string` | `a` + `delim` + `b`. |
| `strings.join_all(arr, sep)` | `string` | The elements of a string array joined by `sep`; the inverse of `strings.split`. |
| `strings.char_at(s, i)` | `string` | The whole character starting at byte `i` (snapped to a UTF-8 boundary); empty past the end. |
| `strings.char_code(s, i)` | `int` | Raw byte value at byte `i`; `-1` past the end. |
| `strings.from_char_code(code)` | `string` | String for an ASCII code (`0`–`127`); empty for anything else. |
| `strings.is_alpha(s)` / `is_digit` / `is_alnum` | `bool` | Whether **every** character is ASCII alphabetic / ASCII digit / ASCII alphanumeric; `false` for empty strings. |

## Formatting

| Function | Returns | Behavior |
| --- | --- | --- |
| `fmt.to_int(s)` | `int` | Parses an integer; throws on failure. |
| `fmt.to_float(s)` | `float` | Parses a float; throws on failure. |
| `fmt.i64_to_str(n)` | `string` | Integer to string. |
| `fmt.f64_to_str(f)` | `string` | Float to string. |
| `fmt.to_hex(n)` | `string` | Hexadecimal representation. |
| `fmt.to_oct(n)` | `string` | Octal representation. |
| `fmt.is_int(s)` | `bool` | Whether `s` parses as an integer. |
| `fmt.is_float(s)` | `bool` | Whether `s` parses as a float. |
| `fmt.type_name(tag)` | `string` | Name of the type for a runtime type tag. |
| `fmt.pad_left(s, width, pad)` | `string` | Left-pad to `width` with the character code `pad` (e.g. `48` for `0`). |
| `fmt.pad_right(s, width, pad)` | `string` | Right-pad to `width` with the character code `pad`. |

## Math

All `math` functions operate on and return `float`. `math.sqrt` throws on a
negative argument.

| Function | Behavior |
| --- | --- |
| `math.sqrt(x)` | Square root. |
| `math.pow(base, exp)` | Power. |
| `math.abs(x)` | Absolute value. |
| `math.ceil(x)` / `math.floor(x)` / `math.round(x)` | Rounding. |
| `math.sin(x)` / `math.cos(x)` / `math.tan(x)` | Trigonometry. |

## System

| Function | Returns | Behavior |
| --- | --- | --- |
| `sys.read(path)` | `string` | Reads a file; throws on failure. |
| `sys.write(path, content)` | `bool` | Writes a file; throws on failure. |
| `sys.append(path, content)` | `bool` | Appends to a file. |
| `sys.exists(path)` | `bool` | Whether the path exists. |
| `sys.mkdir(path)` | `bool` | Creates a directory, including missing parents. |
| `sys.listdir(path)` | `string` | Directory entries. |
| `sys.cwd()` | `string` | Current working directory. |
| `sys.env(name)` | `string` | Environment variable value. |
| `sys.args()` | `array[string]` | Command-line arguments; element `0` is the program name (like Go's `os.Args`). |
| `sys.exit(code)` | `void` | Terminates the process. |
| `sys.sleep(ms)` | `void` | Sleeps for `ms` milliseconds (`ms` may be fractional; negatives sleep `0`). |
| `sys.exec(command)` | `int` | Runs a command; returns its exit status. |
| `sys.cp(src, dst)` | `bool` | Copies a file. |
| `sys.rm(path)` | `bool` | Removes a file. |
| `sys.walk(root)` | `string` | Recursively lists all files and directories under `root`, newline-separated. |
| `sys.symlink(target, link)` | `bool` | Creates a symbolic link at `link` pointing to `target`. Throws on failure. |
| `sys.readlink(path)` | `string` | Returns the target of a symbolic link. Throws if not a symlink. |
| `sys.is_symlink(path)` | `bool` | Whether the path is a symbolic link. |

## Terminal

The `term` module provides the platform-sensitive terminal primitives needed
by interactive Neutron applications. Rendering is application-owned: use
`io.write` with ANSI sequences or another output format around these calls.

| Function | Returns | Behavior |
| --- | --- | --- |
| `term.is_tty()` | `bool` | Whether stdout is an interactive terminal. |
| `term.open()` | `int` | Enables raw input and the alternate screen; returns a session handle. |
| `term.restore(handle)` | `bool` | Restores cooked input and the normal screen; safe to repeat. |
| `term.poll_key(handle, timeout_ms)` | `bool` | Waits up to `timeout_ms` for a key press and stores it as the last key. |
| `term.key_code()` | `string` | Stable code for the last key (`character`, `enter`, `up`, `ctrl_c`, and so on). |
| `term.key_character()` | `string` | Character for the last character key. |
| `term.key_ctrl()` | `bool` | Whether the last key had the Control modifier. |

## Time

| Function | Returns | Behavior |
| --- | --- | --- |
| `time.now()` | `float` | Milliseconds since the Unix epoch. |
| `time.sleep(ms)` | `void` | Sleeps for `ms` milliseconds (`ms` may be fractional; negatives sleep `0`). |
| `time.format(timestamp_ms, fmt)` | `string` | Formats a timestamp. |

## JSON

`json` operates on JSON text strings and returns JSON text or plain strings.

| Function | Returns | Behavior |
| --- | --- | --- |
| `json.parse(s)` | `string` | Validates and normalizes; throws on invalid input. Non-ASCII text is preserved. |
| `json.stringify(v)` | `string` | Serializes a value. |
| `json.is_valid(s)` | `bool` | Whether the text is valid JSON. |
| `json.get(json, key)` | `string` | Value of an *immediate* member, or `"null"`. |
| `json.has(json, key)` | `bool` | Whether a key exists. |
| `json.keys(json)` | `string` | Key list. |
| `json.length(array)` | `int` | Element count of a JSON array; `0` for anything else. |
| `json.at(array, i)` | `string` | Element `i` of a JSON array as JSON text; `"null"` when out of range. |
| `json.stringify_pretty(json)` | `string` | Pretty-prints. |
| `json.escape_string(s)` | `string` | Escapes a string for embedding in JSON, surrounding quotes included. |

`json.get` only looks at the members of the object it is given: a nested
object never shadows a member of the same name in its parent, so
`json.get(json, "mbid")` on `{"artist":{"mbid":"a"},"mbid":"b"}` answers
`"b"`. The result is *JSON text*, so a string value keeps its quotes — compare
against `json.escape_string(want)` or strip them, and remember that
`json.get(doc, "error")` is the text `10`, not the number 10.

There is no pointer syntax: reach a nested value by taking the object out
first (`json.get(json.get(doc, "lrc"), "lyric")`) or by splitting an array
into its elements and indexing those.

## CSV

`csv` parses and serializes comma-separated value text.  `csv.parse` returns a
JSON array of objects (each row becomes an object keyed by the header row);
`csv.stringify` accepts the same format and produces CSV text.

| Function | Returns | Behavior |
| --- | --- | --- |
| `csv.parse(s)` | `string` | Parses CSV text into a JSON array of objects; the first row is used as keys. Throws on malformed input. |
| `csv.stringify(v)` | `string` | Serializes a JSON array of objects into CSV text. Throws when `v` is not an array of objects. |

An empty input produces `[]`.

```ntsc
var data = csv.parse("name,age\nAlice,30\nBob,25")
// [{"name":"Alice","age":"30"},{"name":"Bob","age":"25"}]

var text = csv.stringify(data)
// "name,age\nAlice,30\nBob,25"
```

## YAML

`yaml` parses and serializes YAML text.  `yaml.parse` returns a JSON object
(or array for top-level sequences); `yaml.stringify` accepts a JSON value and
produces YAML text.

| Function | Returns | Behavior |
| --- | --- | --- |
| `yaml.parse(s)` | `string` | Parses YAML text into a JSON value. Throws on malformed input. |
| `yaml.stringify(v)` | `string` | Serializes a JSON value into YAML text. |

An empty input produces `{}`.

```ntsc
var data = yaml.parse("name: Alice\nage: 30\nactive: true")
// {"name":"Alice","age":30,"active":true}

var text = yaml.stringify(data)
// name: Alice\nage: 30\nactive: true
```

## HTTP

`http` performs blocking requests. On network or HTTP failure the request
functions throw. `http://` URLs are sent over a plain TCP connection;
`https://` URLs are encrypted with TLS (rustls) and verified against Mozilla's
bundled CA root certificates. A `3xx` response is followed (up to five hops,
resolving a relative `Location` against the URL that answered), and a `301`,
`302`, or `303` hop continues as a `GET` without a body. `http.request_full`
can turn that off. Without `timeout_ms` a request waits on the peer
indefinitely.

| Function | Returns | Behavior |
| --- | --- | --- |
| `http.get(url)` | `string` | GET request body. |
| `http.post(url, data)` | `string` | POST request. |
| `http.put(url, data)` | `string` | PUT request. |
| `http.delete(url)` | `string` | DELETE request. |
| `http.head(url)` | `string` | HEAD request. |
| `http.patch(url, data)` | `string` | PATCH request. |
| `http.request(method, url, data)` | `string` | Arbitrary method. |
| `http.request_full(method, url, data, options)` | `string` | Arbitrary method with per-request options; `options` is a JSON object and may be empty. |
| `http.status_code(response)` | `int` | Status code of a response string. |
| `http.get_range(url, start, len)` | `string` | Fetches a byte range from a URL. Throws on failure. |
| `http.get_file(url, dest)` | `string` | Downloads a URL to a file. Returns JSON with status and byte count. Throws on failure. |
| `http.download_with_progress(url, dest, chunk_size)` | `string` | Downloads in chunks. Returns JSON with status, bytes, and chunk count. Throws on failure. |
| `http.concurrent_download(urls, dest, max_parallel)` | `string` | Downloads multiple URLs in parallel. Returns JSON with per-URL results. Throws on failure. |

`http.request_full` options (all optional, an empty object behaves like
`http.request`):

| Option | Type | Behavior |
| --- | --- | --- |
| `headers` | object | Extra request headers, e.g. a `Referer` or `Authorization`. |
| `user_agent` | string | Replaces the default `Neutron/1.0` user agent. |
| `timeout_ms` | int | Read and write timeout in milliseconds; `0` or absent means no timeout. |
| `follow_redirects` | bool | Defaults to `true`; a redirect is followed at most `max_redirects` times. |
| `max_redirects` | int | Redirect budget, `5` by default. |

The `_async` variants run the blocking request on a worker-thread pool and
hand back a future: `await` parks the calling goroutine without tying up a
scheduler thread, so many fetches can overlap on few threads.

| Function | Returns | Behavior |
| --- | --- | --- |
| `http.get_async(url)` | `string` | Awaitable GET; response JSON (`status`, `body`). |
| `http.post_async(url, data)` | `string` | Awaitable POST. |
| `http.put_async(url, data)` | `string` | Awaitable PUT. |
| `http.delete_async(url)` | `string` | Awaitable DELETE. |
| `http.head_async(url)` | `string` | Awaitable HEAD. |
| `http.patch_async(url, data)` | `string` | Awaitable PATCH. |

## Collections

### Sets

| Function | Returns | Behavior |
| --- | --- | --- |
| `collections.set_new()` | `object` | New empty set. |
| `collections.set_add(set, v)` | `bool` | Adds a value. |
| `collections.set_has(set, v)` | `bool` | Membership test. |
| `collections.set_remove(set, v)` | `bool` | Removes a value. |
| `collections.set_size(set)` | `int` | Number of elements. |
| `collections.set_to_array(set)` | `string` | Elements as a list. |
| `collections.set_union(a, b)` | `object` | Union. |
| `collections.set_intersection(a, b)` | `object` | Intersection. |
| `collections.set_difference(a, b)` | `object` | Difference. |

### Stacks and queues

| Function | Returns | Behavior |
| --- | --- | --- |
| `collections.stack_new()` | `object` | New empty stack. |
| `collections.stack_push(s, v)` | `bool` | Push. |
| `collections.stack_pop(s)` | `bool` | Pop. |
| `collections.stack_peek(s)` | `bool` | Inspect top. |
| `collections.stack_size(s)` | `int` | Size. |
| `collections.stack_is_empty(s)` | `bool` | Empty test. |
| `collections.queue_new()` | `object` | New empty queue. |
| `collections.queue_enqueue(q, v)` | `bool` | Enqueue. |
| `collections.queue_dequeue(q)` | `bool` | Dequeue. |
| `collections.queue_peek(q)` | `bool` | Inspect front. |
| `collections.queue_size(q)` | `int` | Size. |
| `collections.queue_is_empty(q)` | `bool` | Empty test. |

### Channels

See [Concurrency](../guide/concurrency.md) for usage.

| Function | Returns | Behavior |
| --- | --- | --- |
| `collections.channel(capacity)` | `int` | Creates a queue; returns the receiver handle. |
| `collections.channel_sender(rx)` | `int` | Creates the sender for a receiver. |
| `collections.channel_send(tx, msg)` | `bool` | Copies the string `msg` into the queue, blocking when full. |
| `collections.channel_recv(rx)` | `string` | Receives, blocking when empty. |
| `collections.channel_try_recv(rx)` | `string` | Non-blocking receive; the empty string when nothing is pending. |
| `collections.channel_close(handle)` | `void` | Closes a queue end. |

## Regular expressions

Patterns follow Rust `regex` syntax, including Unicode classes such as
`\p{L}`. Since `\p` stays as the two characters it is written as (it is not
a recognized escape), a pattern only needs a single backslash in source:
`"[^\p{L}]"`. Every function throws on an invalid pattern.

| Function | Returns | Behavior |
| --- | --- | --- |
| `regex.test(text, pattern)` | `bool` | Whether the pattern matches. |
| `regex.search(text, pattern)` | `bool` | Whether the pattern appears. |
| `regex.find(text, pattern)` | `string` | JSON object with `matched`, `position`, and `length`; `"null"` when nothing matches. |
| `regex.find_all(text, pattern)` | `string` | JSON array of match objects. |
| `regex.replace(text, pattern, rep)` | `string` | Replace matches. |
| `regex.split(text, pattern)` | `string` | Split on matches, joined by newlines. |
| `regex.is_valid(pattern)` | `bool` | Pattern validity. |
| `regex.escape(text)` | `string` | Escapes literal text. |

## I/O

File handles are integers. `io.open` throws on failure.

| Function | Returns | Behavior |
| --- | --- | --- |
| `io.input()` | `string` | Reads one line from standard input without its line ending; returns `""` at EOF. |
| `io.stdin()` | `int` | Stable standard-input handle for `io.read*`. |
| `io.stdout()` | `int` | Stable standard-output handle for `io.write*` and `io.flush`. |
| `io.stderr()` | `int` | Stable standard-error handle for `io.write*` and `io.flush`. |
| `io.open(path, mode)` | `int` | Opens a file (`"r"`, `"w"`, `"w+"`, ...). |
| `io.close(handle)` | `bool` | Closes a file. |
| `io.read(handle, count)` | `string` | Reads up to `count` bytes. |
| `io.read_line(handle)` | `string` | Reads one line. |
| `io.read_all(handle)` | `string` | Reads the rest of the file. |
| `io.write(handle, data)` | `int` | Writes bytes; returns the count written. |
| `io.write_line(handle, data)` | `int` | Writes a line. |
| `io.flush(handle)` | `bool` | Flushes buffers. |
| `io.eof(handle)` | `bool` | End-of-file test. |
| `io.seek(handle, offset, whence)` | `bool` | Seeks (`whence` 0 = start). |
| `io.tell(handle)` | `int` | Current position. |

### Reading standard input

`io.input()` blocks until it reads a line from the process's standard input or
reaches end-of-file. It removes the trailing line ending (`\n` or `\r\n`) and
returns the remaining text as a string:

```ntsc
fun main() {
    say("What is your name?")
    var name = io.input()
    say("Hello, " + name)
}
```

An empty string is returned for both a blank line and end-of-file; `io.input()`
does not distinguish between them. If standard input cannot be read, it throws
an exception whose message starts with `io.input:`:

```ntsc
try {
    var line = io.input()
    say("Read: " + line)
} catch (err) {
    say(err)
}
```

Each call reads one line. The returned string is an ordinary owned NTSC string
and requires no manual cleanup.

### Standard streams

`io.stdin()`, `io.stdout()`, and `io.stderr()` return process-owned stream
handles. They use the same operations as file handles:

```ntsc
fun main() {
    var input = io.stdin()
    var output = io.stdout()
    var errors = io.stderr()

    io.write(output, "Name: ")
    io.flush(output)
    var name = strings.trim(io.read_line(input))
    io.write_line(output, "Hello, " + name)
    io.write_line(errors, "This line is written to stderr")
}
```

Standard stream handles are stable, allocate no registry objects, and are
owned by the process. Calling `io.close` on one is a successful no-op. Standard
input supports `io.read`, `io.read_line`, and `io.read_all`; standard output
and standard error support `io.write`, `io.write_line`, and `io.flush`.
Operations unsupported by the stream direction throw an `io.*` exception.
Standard streams are not seekable, so `io.seek`, `io.tell`, and `io.eof` throw
when given one of these handles.

## Networking

Sockets are integer handles. `net.tcp_connect` throws on connection failure.
Every connected stream has `TCP_NODELAY` enabled (matching Go's default), so a
small response is not delayed waiting for an ACK.

| Function | Returns | Behavior |
| --- | --- | --- |
| `net.tcp_connect(host, port)` | `int` | Connects; returns a socket. |
| `net.tcp_listen(port)` | `int` | Listens; port `0` picks an ephemeral port. |
| `net.local_port(handle)` | `int` | Bound port of a listener. |
| `net.tcp_accept(listener)` | `int` | Accepts a connection (blocks the calling thread). |
| `net.accept_poll(listener, timeout_ms)` | `int` | Accepts a connection, giving up after `timeout_ms` with the null handle. The listener is flipped to non-blocking for the wait and restored, so `net.tcp_accept` still works on it. |
| `net.send(handle, data)` | `int` | Sends bytes. |
| `net.send_line(handle, data)` | `int` | Sends a line. |
| `net.recv(handle, count)` | `string` | Receives up to `count` bytes. |
| `net.recv_line(handle)` | `string` | Receives a line (blocks the calling thread). |
| `net.close(handle)` | `bool` | Closes a socket. |
| `net.udp_bind(port)` | `int` | Binds a UDP socket. |
| `net.udp_send(handle, host, port, data)` | `int` | Sends a datagram. |
| `net.udp_recv(handle, count)` | `string` | Receives a datagram. |

The awaitable variants park the goroutine on the reactor instead of holding a
thread: each poll tries a non-blocking accept/read first, and the goroutine
sleeps on socket readiness until a client connects or bytes arrive. Because
nothing is queued to a worker pool, one accept loop can serve an unbounded
rate of connections, and slow clients never pin a scheduler thread.

| Function | Returns | Behavior |
| --- | --- | --- |
| `net.accept_async(listener)` | `int` | Awaitable accept; completes with the client socket. |
| `net.recv_line_async(sock)` | `string` | Awaitable line read; `""` at end of stream. |

## Operating system

| Function | Returns | Behavior |
| --- | --- | --- |
| `os.getenv(name)` | `string` | Environment variable (empty when unset). |
| `os.setenv(name, value)` | `bool` | Sets an environment variable. |
| `os.unsetenv(name)` | `bool` | Removes an environment variable. |
| `os.has_env(name)` | `bool` | Environment variable test. |
| `os.path_join(a, b)` | `string` | Joins path components. |
| `os.path_dirname(p)` | `string` | Directory part. |
| `os.path_basename(p)` | `string` | Final component. |
| `os.path_ext(p)` | `string` | Extension without the dot. |
| `os.path_stem(p)` | `string` | Filename without extension. |
| `os.path_abs(p)` | `string` | Absolute path. |
| `os.is_abs(p)` | `bool` | Absolute path test. |
| `os.separator()` | `string` | Path separator. |
| `os.temp_dir()` | `string` | Temporary directory. |
| `os.temp_path(prefix)` | `string` | A unique temp path. |
| `os.temp_file(prefix)` | `string` | Creates and returns a temp file path. |
| `os.file_lock(path)` | `int` | Acquires an advisory file lock; returns a handle > 0. Throws on failure. |
| `os.file_unlock(path)` | `bool` | Releases a file lock. |
| `os.rename(from, to)` | `bool` | Moves a file or directory; `false` when the move is not possible, leaving the target alone. |
| `os.executable_path()` | `string` | The running program's own path, or `""` when the platform cannot report it. |

## Crypto

`crypto.base64_decode`, `crypto.hex_decode`, and the hash functions throw on
invalid input.

| Function | Returns | Behavior |
| --- | --- | --- |
| `crypto.base64_encode(s)` | `string` | Base64 encode. |
| `crypto.base64_decode(s)` | `string` | Base64 decode. |
| `crypto.hex_encode(s)` | `string` | Hex encode. |
| `crypto.hex_decode(hex)` | `string` | Hex decode. |
| `crypto.sha256(s)` | `string` | SHA-256 digest (hex). |
| `crypto.sha512(s)` | `string` | SHA-512 digest (hex). |
| `crypto.sha384(s)` | `string` | SHA-384 digest (hex). |
| `crypto.sha224(s)` | `string` | SHA-224 digest (hex). |
| `crypto.md5(s)` | `string` | MD5 digest (hex). |
| `crypto.hmac_sha256(key, msg)` | `string` | HMAC-SHA256 (hex). |
| `crypto.hmac_sha512(key, msg)` | `string` | HMAC-SHA512 (hex). |
| `crypto.verify_sha256(data, hash)` | `bool` | Verifies a SHA-256 hash. |
| `crypto.verify_sha512(data, hash)` | `bool` | Verifies a SHA-512 hash. |
| `crypto.file_sha256(path)` | `string` | SHA-256 hash of a file's contents. |
| `crypto.file_sha512(path)` | `string` | SHA-512 hash of a file's contents. |
| `crypto.random_bytes(count)` | `string` | Random bytes. |
| `crypto.random_string(length, alphabet)` | `string` | Random string. |
| `crypto.xor_cipher(data, key)` | `string` | XOR cipher. |

## Encoding

| Function | Returns | Behavior |
| --- | --- | --- |
| `encoding.base64_encode(s)` | `string` | Base64 encode. |
| `encoding.base64_decode(s)` | `string` | Base64 decode. |
| `encoding.hex_encode(s)` | `string` | Hex encode. |
| `encoding.hex_decode(hex)` | `string` | Hex decode. |
| `encoding.utf8_valid(s)` | `bool` | Whether the string is valid UTF-8. |

## Hash

| Function | Returns | Behavior |
| --- | --- | --- |
| `hash.sha256(s)` | `string` | SHA-256 digest (hex). |
| `hash.crc32(s)` | `int` | CRC-32 checksum. |

## Random

| Function | Returns | Behavior |
| --- | --- | --- |
| `random.seed(seed)` | `bool` | Seeds the generator. |
| `random.int(min, max)` | `int` | Integer in `[min, max)`; throws on inverted bounds. |
| `random.float()` | `float` | Float in `[0.0, 1.0)`. |
| `random.bool()` | `bool` | Random boolean. |
| `random.shuffle(a)` | `array[T]` | New array, elements shuffled. |
| `random.weighted(weights)` | `int` | Weighted index; throws when weights are all zero. |

## Sorting

`sort` clones its input into a new array; the input is never consumed.

| Function | Returns | Behavior |
| --- | --- | --- |
| `sort.stable_sort(a)` | `array[T]` | Stable ascending sort. |
| `sort.sort_by(a, cmp)` | `array[T]` | Sort with a comparator lambda. |
| `sort.binary_search(a, v)` | `int` | Index of `v`, or `-1`. |

`sort.sort_by` takes a lambda `fun(T a, T b) -> bool` returning whether `a`
should precede `b`.

## Slices

A `slice[T]` is a bounds-checked window over an `array[T]`. It holds the source
array handle plus the window rather than a pointer, so every operation
re-validates that the array is still registered and that the index lies inside
the window. Ranges are half-open (`start..end`), and every function throws when
its range or index is out of bounds.

| Function | Returns | Behavior |
| --- | --- | --- |
| `slices.of(a, start, end)` | `slice[T]` | Window over `a`; throws when the range leaves the array. |
| `slices.sub(s, start, end)` | `slice[T]` | Narrows `s`; bounds are relative to `s`, so it cannot widen. |
| `slices.length(s)` | `int` | Number of elements in the window. |
| `slices.get(s, i)` | `T` | Element at `i` within the window. `s[i]` is equivalent. |
| `slices.set(s, i, v)` | `bool` | Writes `v` through to the underlying array. |
| `slices.to_array(s)` | `array[T]` | Fresh owned array of the spanned elements. |
| `slices.fill(s, v)` | `bool` | Sets every element in the window to `v`. |
| `slices.copy_from(dst, src)` | `bool` | Element-wise copy; requires equal lengths. |
| `slices.equal(a, b)` | `bool` | Whether both windows hold equal elements. |

A slice owns its own registry entry and is reclaimed at scope exit; it never
frees the array it borrows. `copy(s)` is equivalent to `slices.to_array(s)`.
Slices cannot cross a thread boundary.

## Memory

The `memory` module provides bounds-checked byte-buffer capabilities. A
`pointer` value is an opaque capability into a runtime-managed byte region, not
a machine address: it cannot be converted to an integer, dereferenced with `*`,
or used to reach outside the allocation it points into. All access goes through
the functions below, each of which bounds-checks its pointer and throws on a
stale or invalid capability.

| Function | Returns | Behavior |
| --- | --- | --- |
| `memory.alloc(size)` | `pointer` | Allocates a zeroed region of `size` bytes (`0..=16777216`). |
| `memory.offset(p, delta)` | `pointer` | A derived capability at `p + delta`; throws when out of bounds. |
| `memory.clone(p)` | `pointer` | A second capability into the same allocation. |
| `memory.drop(p)` | `void` | Releases one capability; the allocation lives until all capabilities are dropped. |
| `memory.load8(p)` | `int` | Reads one byte (0..255). |
| `memory.load64(p)` | `int` | Reads eight bytes, little-endian. |
| `memory.store8(p, v)` | `bool` | Writes one byte; `v` must be in 0..255. |
| `memory.store64(p, v)` | `bool` | Writes eight bytes, little-endian. |

Capabilities follow ordinary ownership: assignment moves them, `copy(p)`
clones a capability, and the value is dropped at scope exit. The allocation's
storage is freed only when its last capability is dropped, so an offset
capability keeps the whole region alive.

Capability access resolves the region through a per-thread cache, so a load or
store in a loop does not take the registry lock on every access; any handle
removal invalidates the cache, so a dropped capability is never served from it.
Typed pointers (`own T`, `&T`, `*T`) bypass the registry entirely and compile to
a plain load or store.

`memory.raw_address(reference)` is the typed-pointer escape hatch: it converts
`&T` to `*const T` and `&mut T` to `*mut T`. It is only allowed inside an
`unsafe` block, and the resulting pointer keeps its pointee type. See
[Pointers and references](language.md#pointers-and-references).

## Process

| Function | Returns | Behavior |
| --- | --- | --- |
| `process.exec(command)` | `int` | Runs a command; returns its exit status. |
| `process.exec_output(command)` | `string` | Runs a command; returns its output. |
| `process.spawn(command, args)` | `string` | Spawns a process. |
| `process.pid()` | `int` | Current process id. |
| `process.spawn_thread(body, arg)` | `int` | Starts an OS thread; returns its handle. `arg` must be thread-safe. |
| `process.thread_join(id)` | `bool` | Waits for a thread to finish. |

Inside an `async fun`, the `http.*_async` calls run on the offload pool
instead of the calling thread: the goroutine parks until the response is
ready, so a slow request never pins a scheduler thread. `process.exec` and
`process.exec_output` have no awaitable variant yet — they block the calling
thread; the runtime's offload machinery exists but the `await` front-end does
not lower process calls.

Rust dependency adaptors also expose an offloaded `<function>_async` alias for
each synchronous bridge. For example, `await @ureq.get_async(url)` runs the
blocking `ureq::get` call on the runtime's bounded offload pool; the original
`@ureq.get(url)` API remains synchronous.

`process.spawn_thread` takes a lambda with one `int` parameter. Its payload
must be a scalar or a stdlib handle: owned heap values, `shared` values, and
views are rejected at compile time. See
[Concurrency](../guide/concurrency.md#threading-rules) for the full
classification and the channel-handle pattern it implies.

## Testing

Assertions throw on failure. The test runner also uses them.

| Function | Returns | Behavior |
| --- | --- | --- |
| `testing.assert_true(b)` | `bool` | Passes when `b` is `true`. |
| `testing.assert_false(b)` | `bool` | Passes when `b` is `false`. |
| `testing.assert_eq(a, b)` | `bool` | Passes when `a == b` (`int`, `float`, `bool`, or `string`). |
| `testing.assert_ne(a, b)` | `bool` | Passes when `a != b`. |
| `testing.bench(f, n, warmup)` | `float` | Benchmarks `f` over `n` iterations (with an optional `warmup` phase); returns the average microseconds per iteration. `f` is a function reference taking one `int` and returning any value. |

## Async

| Function | Returns | Behavior |
| --- | --- | --- |
| `async.sleep(ms)` | `void` | Suspends the coroutine for approximately `ms` milliseconds. |

`async fun`/`await` and `go`/`chan[T]` run on the same M:N task scheduler
(one work-stealing thread pool, one reactor): goroutines and coroutines are
the same machinery, so there is one concurrency model, not two. See the
[concurrency guide](../guide/concurrency.md).

## Goroutines and channels (built in)

`go`, `chan[T]`, `close`, `<|`, and `|>` are language syntax rather than a
module; `chan.new(capacity)` is the one constructor function:

| Expression | Behavior |
| --- | --- |
| `go worker(args)` / `go { ... }` | Spawn a goroutine on the scheduler. Arguments and captures are classified by the ownership checker (scalars copy, handles share, owned values move, `view`/`shared` are `NTSC-E0501`). |
| `chan.new(capacity)` | Create a `chan[T]`; the element type comes from the annotated variable (`var chan[string] jobs = chan.new(4)`). |
| `v \|> ch` | Send: moves `v` into the channel; parks while the channel is full. |
| `v <\| ch` | Receive: binds a fresh variable owning the value; parks while empty. Top-level statement of an `async fun` only. |
| `close(ch)` | Forbid further sends; receivers drain the queued values, then get the zero value. |
| `for v in ch { ... }` | Receive until the channel is closed and drained. |

The blocking `collections.channel` family and `process.spawn_thread` remain
available as lower-level alternatives.

## Glob

The `glob` module provides filesystem pattern matching using shell-style
glob syntax: `*` matches any characters except `/`, `**` matches any
characters including `/`, `?` matches a single character, and `[abc]`
matches a character class.

| Function | Returns | Behavior |
| --- | --- | --- |
| `glob.matches(pattern, path)` | `bool` | Whether `path` matches the glob `pattern`. |
| `glob.find(root, pattern)` | `string` | Finds files under `root` matching `pattern`, newline-separated. |
| `glob.is_match(pattern, path)` | `bool` | Alias for `glob.matches`. |

## Paths

The `paths` module provides cross-platform path manipulation without
requiring an `os` module dependency.

| Function | Returns | Behavior |
| --- | --- | --- |
| `paths.join(a, b)` | `string` | Joins two path components. |
| `paths.parent(path)` | `string` | Directory part of the path. |
| `paths.file_name(path)` | `string` | Final component (file or directory name). |
| `paths.extension(path)` | `string` | File extension without the dot. |
| `paths.with_extension(path, ext)` | `string` | Replaces the extension. |
| `paths.stem(path)` | `string` | Filename without extension. |
| `paths.absolute(path)` | `string` | Converts to an absolute path. |
| `paths.relative(path, base)` | `string` | Computes `path` relative to `base`. |
| `paths.is_absolute(path)` | `bool` | Whether the path is absolute. |
| `paths.components(path)` | `string` | Splits the path into components, newline-separated. |
| `paths.normalize(path)` | `string` | Resolves `.` and `..` segments. |

## Archive

The `archive` module extracts tar, tar.gz, and zip archives. All functions
accept file paths (not raw data) since binary content cannot travel through
the string registry.

| Function | Returns | Behavior |
| --- | --- | --- |
| `archive.extract_tar_gz(path, dest)` | `string` | Extracts a `.tar.gz` file to `dest`. Returns the number of files extracted. Throws on failure. |
| `archive.extract_tar(path, dest)` | `string` | Extracts an uncompressed `.tar` file. |
| `archive.extract_zip(path, dest)` | `string` | Extracts a `.zip` file. |
| `archive.list_tar(path)` | `string` | Lists entry names in a tar file, newline-separated. |
| `archive.list_zip(path)` | `string` | Lists entry names in a zip file, newline-separated. |

## Unused legacy `arrays` module

A separate newline-delimited string implementation of the `arrays` module
exists in the runtime for backwards compatibility, but it is not used: every
`arrays.*` call in NTSC source is routed to the heap array operations above.
