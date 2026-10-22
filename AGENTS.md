# Argmend

Every commit needs `git commit -s`. The sign-off email is `git config user.email`.

CI runs these commands, in this order:

- `cargo fmt --check`
- `cargo clippy --locked --all-targets -- -D warnings`
- `cargo test --locked --all-targets`
- `cargo deny check`
