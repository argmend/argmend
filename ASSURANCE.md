# Assurance

This is the argument for the current crate, not a certification.

Claim: `repair_json` returns a `RepairOutcome` for any Unicode string and does not panic.

Evidence: unit tests in `src/lib.rs`, property tests in `tests/repair_gaps.rs`, and the `repair_json` fuzz target. The fuzzer runs in CI for 20 seconds on changes under `src/` or `fuzz/`.

Claim: an ambiguous repair is not guessed.

Evidence: `RepairOutcome::Ambiguous` is returned when more than one repair is possible. Tests cover that result.

Claim: `normalize` and `repair_args` do not abort on a deeply nested value.

`normalize` walks the value on the heap and refuses more than 128 nested arrays or objects. The error is `value is nested too deeply`. `repair_args` calls `normalize`, so it shares that limit. A string still goes through `serde_json`, which stops at its own recursion limit before a `Value` exists. The `repair_json` claim above is only about parsing text.

Evidence: `normalize_stops_past_the_depth_limit`.

Claim: `Ok` from `repair_args` is JSON, not schema acceptance.

Evidence: the README and `repair_args` documentation say the host still validates. `$ref`, `pattern`, and `format` are not evaluated. Tests cover those refusals.

Limit: there is one maintainer. A review is not a second independent organization.
