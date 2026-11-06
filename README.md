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

`Valid` means the text parsed and Argmend did not rewrite it. `Repaired` means Argmend rewrote the text and then parsed it. A Windows path whose backslash was a JSON escape is one of those rewrites. A newline after a one-letter label, such as `Q:\n` or `Q:\n\n`, stays a newline. A number that is the last character, such as `{"a": 12`, stays unfixable. The same number followed by a space or a newline is closed. Text before the value stays unfixable, as in `Here is the call: {"a": 1}`. A word before a markdown fence stays unfixable. A fence that starts the input is unwrapped. A `<tool_call>` tag around the value stays unfixable. Text after one object or array is removed: `{"a": 1}` followed by `Done.` is repaired, and the note is `removed trailing prose`. `Unfixable` means Argmend could not produce one JSON value. `Ambiguous` means more than one repair was possible, so none was chosen.

Pass a tool schema with `repair_args` when you have one. A value nested more than 128 arrays or objects is an error. `repair_args` uses that same limit. A `parameters` object is that schema when the outer object has no `properties` map, and `if`, `then`, `not`, and `oneOf` on it still apply. Blank input becomes `{}`, `[]`, or null, matching the schema type. A string, number, or boolean is not invented.

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

`RepairRequest::new` keeps every repair on. Build `RepairOptions::new`, set one field to `false`, and pass it to `with_options`.

`path_backslashes` set to `false` leaves a Windows path escape as that JSON escape. A bare key in the same text is still quoted. `aliases` set to `false` leaves `file` and an `x-aliases` name in place. `truncation` set to `false` leaves `{"file": "a.rs"` unfixable. A finished `{file: "a.rs"}` still repairs.

## What success does not mean

`Ok` from `repair_args` means Argmend produced JSON. It does not mean the schema accepts that JSON. Validate the value before you run the tool.

`$ref` and `$dynamicRef` are not resolved. `pattern`, `format`, and the content keywords are not evaluated. A value that already fails those stays in place when nothing else was rewritten.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). The repair passes are in [ARCHITECTURE.md](ARCHITECTURE.md). The plan is [ROADMAP.md](ROADMAP.md). Maintainer rules are [GOVERNANCE.md](GOVERNANCE.md). Security reports go to [SECURITY.md](SECURITY.md). Questions go to [SUPPORT.md](SUPPORT.md).

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE](LICENSE))

at your option.
