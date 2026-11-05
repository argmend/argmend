# Architecture

Argmend turns broken tool-call text into one JSON value.

`repair_json` runs the syntax passes in [src/syntax.rs](src/syntax.rs), then parses the result. A Windows path backslash is kept when the text is a drive path, including a path inside a sentence. If more than one repair is possible, the result is `Ambiguous` and nothing is chosen. If no single JSON value can be produced, the result is `Unfixable`.

`repair_args` does that work and then checks the value against the tool schema in [src/schema.rs](src/schema.rs). The schema can fill a blank value with `{}`, `[]`, or null when that is the declared type. It does not invent a string, number, or boolean. `$ref` and `$dynamicRef` are not resolved. `pattern`, `format`, and the content keywords are not evaluated.

The host still validates the JSON before it runs the tool.
