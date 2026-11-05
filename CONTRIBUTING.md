# Contributing to Argmend

Issues and pull requests are welcome. A good first change is a failing test for one wrong `repair_json` result, with the input and the value you expected.

## Set up

Install Rust 1.95 or newer. [rust-toolchain.toml](rust-toolchain.toml) pins the version CI uses.

```text
cargo test --locked --all-targets
```

## Checks

CI runs these commands, in this order:

```text
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo test --locked --doc
cargo deny check
```

Run them before you open a pull request.

## Commits

Every commit needs a `Signed-off-by` line. `git commit -s` adds it from `git config user.email`.

## Pull requests

Describe the input, the old result, and the result you want. Add a test that failed before the change. Do not publish a crates.io release from a laptop. A version tag on `main` publishes the crate.
