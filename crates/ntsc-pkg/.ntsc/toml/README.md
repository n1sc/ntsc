# toml

TOML parsing and serialization written in Neutron. `parse` returns compact JSON
text so it composes with the standard `json` module; `stringify` accepts that
JSON representation and emits TOML.

## Install

```console
ntsc pkg install toml
```

## Use

```ntsc
use toml
use json

fun main() {
    var document = toml.parse("name = 'neutron'\n[build]\nopt = true")
    say(json.get(document, "name"))
    say(toml.stringify(document))
}
```

## API

| Function | Returns |
| --- | --- |
| `parse(text)` | A JSON object containing the TOML document. |
| `stringify(document)` | TOML text for a JSON object. |
