# Argmend

Argmend repairs broken tool-call arguments into JSON.

[![CI](https://github.com/argmend/argmend/actions/workflows/ci.yml/badge.svg?event=pull_request)](https://github.com/argmend/argmend/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/argmend?logo=rust)](https://crates.io/crates/argmend)
[![docs.rs](https://img.shields.io/docsrs/argmend?logo=docs.rs)](https://docs.rs/argmend)

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue)](./LICENSE)
[![OpenSSF Scorecard](https://api.securityscorecards.dev/projects/github.com/argmend/argmend/badge)](https://securityscorecards.dev/viewer/?uri=github.com/argmend/argmend)
[![OpenSSF Best Practices](https://www.bestpractices.dev/projects/16368/badge)](https://www.bestpractices.dev/projects/16368)
[![FOSSA Status](https://github.com/argmend/argmend/actions/workflows/fossa.yml/badge.svg)](https://github.com/argmend/argmend/actions/workflows/fossa.yml)

## Install

```toml
[dependencies]
argmend = "0.2"
```

Rust 1.95 or newer. The crate has no command-line program. Call it from your host.

## Repair one argument string

```rust
use argmend::{repair_json, RepairOutcome};

fn main() {
    match repair_json(r#"{file: "a.rs"}"#) {
        RepairOutcome::Repaired { value, repairs } => {
            println!("{value}");
            println!("{} rewrites", repairs.len());
        }
        RepairOutcome::Valid(value) => println!("{value}"),
        RepairOutcome::Unfixable(reason) | RepairOutcome::Ambiguous(reason) => {
            eprintln!("{reason}");
        }
        other => eprintln!("{other:?}"),
    }
}
```

Keep a wildcard arm on `RepairOutcome`, `RewriteKind`, and `RepairError`. A later release can add a variant. Build `RepairRequest` with `RepairRequest::new`.

That program is [examples/repair.rs](examples/repair.rs). Run it with:

```text
cargo run --example repair
```

### Outcomes

| Variant | Meaning |
| --- | --- |
| `Valid` | The text parsed, and Argmend did not rewrite it. |
| `Repaired` | Argmend rewrote the text, then parsed it. |
| `Unfixable` | Argmend could not produce one JSON value. |
| `Ambiguous` | More than one repair was possible, so none was chosen. |

A Windows path whose backslash was a JSON escape is a `Repaired` result.

### Numbers and paths

- A newline after a one-letter label, such as `Q:\n` or `Q:\n\n`, stays a newline.
- A number that is the last character, such as `{"a": 12`, stays unfixable.
- The same number followed by a space or a newline is closed.

### Text around the value

`repair_json` leaves text before the value unfixable:

- `Here is the call: {"a": 1}`
- a word before a markdown fence
- a `<tool_call>` tag around the value

A fence that starts the input is unwrapped. Text after one object or array is removed: `{"a": 1}` followed by `Done.` is repaired, and the note is `removed trailing prose`.

`leading_prose` extracts one object or array from a prefix, a word before a fence, or a `<tool_call>` tag. It stays off. `Here {"a":1} {"b":2}` stays unfixable. See [Turning a repair off](#turning-a-repair-off).

## Pass a schema

Pass a tool schema with `repair_args` when you have one. A value nested more than 128 arrays or objects is an error. `repair_args` uses that same limit.

Argmend reads the schema from:

- the object you pass, when that object is already a JSON Schema
- a `parameters` object, when the outer object has no `properties` map
- OpenAI `function.parameters`
- Anthropic `input_schema`
- MCP `inputSchema`

`if`, `then`, `not`, and `oneOf` still apply on a `parameters` object, and inside an extracted tool schema. `{"a":"1"}` against an integer property `a` becomes `{"a": 1}`.

A `type: function` object with no parameters object is unfixable. The message is `unrecognized OpenAI tool definition`. Blank input against `{"type":"function","parameters":{"type":"object"}}` is `{}`. The type name `function` is only the tool wrapper.

Any other object is still the schema itself. Blank input becomes `{}`, `[]`, or null, matching the schema type. A string, number, or boolean is not invented.

## Streaming preview

`preview_args` reads an argument while tokens are still arriving. It does not repair the text. `repair_json("[1, 2, 3")` stays `Unfixable`. The preview can show `[1, 2, 3]` with `complete: false`. That partial value must not be passed to a tool. `complete` is true only when the text is already one JSON value.

```rust
use argmend::preview_args;

fn main() {
    let preview = preview_args("[1, 2, 3");
    if let Some(value) = preview.value {
        println!("complete={} value={value}", preview.complete);
    }
}
```

## Already parsed JSON

`normalize` takes a parsed value and a schema. It does not read the original text.

`{"n": "5"}` with an integer property `n` becomes `{"n": 5}`. The rewrite kind is `NumberString`, the note is `integer string 5`, and the pointer is `/n`.

## Path names and aliases

These names rename to the one matching path property in the schema: `path`, `file`, `file_path`, `filepath`, and `path_name`.

`{"file": "a.rs"}` against a `path` string property becomes `{"path": "a.rs"}`. The rewrite kind is `Alias`, and the note is `renamed file to path`.

That rename is refused, and the result is ambiguous, when:

- a schema branch lists that key under `properties`
- `patternProperties` may own the key
- the key matches more than one property

`x-aliases` adds names for one property. A `content` property with `"x-aliases": ["text"]` turns `{"text": "hi"}` into `{"content": "hi"}`. The note is `renamed text to content`. The same three refusals apply. The alias and the real key with different values are also ambiguous.

## Turning a repair off

`RepairRequest::new` keeps path, alias, and truncation repairs on. `embedded_json`, `leading_prose`, and `wrap_scalar` stay off. Build `RepairOptions::new`, set one field, and pass it to `with_options`. Pass the same options to `repair_json_with`.

```rust
use argmend::{repair_json_with, RepairOptions};

fn main() {
    let mut options = RepairOptions::new();
    options.leading_prose = true;
    let outcome = repair_json_with(r#"Here is the call: {"a": 1}"#, options);
    println!("{outcome:?}");
}
```

| Field | Default | Effect when you change it |
| --- | --- | --- |
| `path_backslashes` | on | `false` leaves a Windows path escape as that JSON escape. A bare key in the same text is still quoted. |
| `aliases` | on | `false` leaves `file` and an `x-aliases` name in place. |
| `truncation` | on | `false` leaves `{"file": "a.rs"` unfixable. A finished `{file: "a.rs"}` still repairs. |
| `embedded_json` | off | `true` parses one JSON string when the schema type is `object` or `array` and `string` is not also allowed. |
| `leading_prose` | off | `true` extracts one object or array after a prefix, a word before a fence, or a `<tool_call>` tag. |
| `wrap_scalar` | off | `true` wraps one string, number, or bool when the schema type is `array` and not also `object`. |

`embedded_json` example: `{"a":"{\"b\":1}"}` against an object property `a` becomes `{"a":{"b":1}}`. The rewrite kind is `JsonString`, and the note is `parsed json object`. A string that is not JSON stays that string. `normalize` keeps this off.

`wrap_scalar` example: `"a"` against an array of strings becomes `["a"]`. The item schema still applies, so `"1"` against an array of integers becomes `[1]`. A value that fails the item schema is ambiguous. An array that is already an array stays that array.

## What success does not mean

`Ok` from `repair_args` means Argmend produced JSON. It does not mean the schema accepts that JSON. Validate the value before you run the tool.

These keywords are left for the host:

- `$ref` and `$dynamicRef` are not resolved
- `pattern`, `format`, and the content keywords are not evaluated

A value that already fails those stays in place when nothing else was rewritten.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). The repair passes are in [ARCHITECTURE.md](ARCHITECTURE.md). The plan is [ROADMAP.md](ROADMAP.md). Maintainer rules are [GOVERNANCE.md](GOVERNANCE.md). Security reports go to [SECURITY.md](SECURITY.md). Questions go to [SUPPORT.md](SUPPORT.md).

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE](LICENSE))

at your option.
