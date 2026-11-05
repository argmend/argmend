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
    }
}
```

That program is [examples/repair.rs](examples/repair.rs). Run it with:

```text
cargo run --example repair
```

`Valid` means the text parsed and Argmend did not rewrite it. `Repaired` means Argmend rewrote the text and then parsed it. A Windows path whose backslash was a JSON escape is one of those rewrites. A newline after a one-letter label, such as `Q:\n` or `Q:\n\n`, stays a newline. A number that is the last character, such as `{"a": 12`, stays unfixable. The same number followed by a space or a newline is closed. `Unfixable` means Argmend could not produce one JSON value. `Ambiguous` means more than one repair was possible, so none was chosen.

Pass a tool schema with `repair_args` when you have one. Blank input becomes `{}`, `[]`, or null, matching the schema type. A string, number, or boolean is not invented.

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
