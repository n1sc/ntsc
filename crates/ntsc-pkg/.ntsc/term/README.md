# term

Interactive terminal support for Neutron. The package keeps the public API in
Neutron while using the compiler's small native terminal substrate for raw
mode, alternate-screen control, and key polling.

## Install

```console
ntsc pkg install term
```

## Use

```neutron
use term

fun main() {
    if (!term.is_tty()) {
        say("not an interactive terminal")
        return
    }

    var handle = term.open()
    try {
        while (term.poll_key(handle, 100.0)) {
            say(term.key_code())
            if (term.key_code() == "escape") {
                break
            }
        }
    } finally {
        term.restore(handle)
    }
}
```

## API

| Function | Returns | Description |
| --- | --- | --- |
| `is_tty()` | `bool` | Whether stdout is an interactive terminal. |
| `open()` | `int` | Enters raw mode and the alternate screen. |
| `restore(handle)` | `bool` | Restores the terminal after `open`. |
| `poll_key(handle, timeout_ms)` | `bool` | Waits for a key press. |
| `key_code()` | `string` | Stable name of the last key. |
| `key_character()` | `string` | Character from the last key, if any. |
| `key_ctrl()` | `bool` | Whether the last key used Control. |
