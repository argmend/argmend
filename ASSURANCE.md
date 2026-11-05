# Assurance

This is the argument for the current crate, not a certification.

Claim: `repair_json` returns a `RepairOutcome` for any Unicode string and does not panic.

Evidence: unit tests in `src/lib.rs`, property tests in `tests/repair_gaps.rs`, and the `repair_json` fuzz target. The fuzzer runs in CI for 20 seconds on changes under `src/` or `fuzz/`.

Claim: an ambiguous repair is not guessed.

Evidence: `RepairOutcome::Ambiguous` is returned when more than one repair is possible. Tests cover that result.

Claim: `Ok` from `repair_args` is JSON, not schema acceptance.

Evidence: the README and `repair_args` documentation say the host still validates. `$ref`, `pattern`, and `format` are not evaluated. Tests cover those refusals.

Limit: there is one maintainer. A review is not a second independent organization.
