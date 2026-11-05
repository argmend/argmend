# Roadmap

## Near term

Fix repair results that a real tool-call string gets wrong. Each fix ships with a test that failed before the change.

Keep the crate a library. There is no command-line program and no hosted service in this plan.

## Later

`1.0` waits until the public contract stops changing. That contract is `repair_json` and `repair_args`, including the cases they refuse: unresolved `$ref`, and `pattern`, `format`, and the content keywords.

## Not planned

Argmend does not resolve JSON Schema references, and it does not evaluate `pattern` or `format`. A successful result stays JSON for the host to validate.
