# Changelog

## [0.3.0](https://github.com/argmend/argmend/compare/v0.2.8...v0.3.0) (2026-11-06)


### Features

* accept OpenAI, Anthropic, and MCP tool definitions ([#185](https://github.com/argmend/argmend/issues/185)) ([603760e](https://github.com/argmend/argmend/commit/603760ec58a26a4dfcac144789712ffc6a93289b)), closes [#166](https://github.com/argmend/argmend/issues/166) [#157](https://github.com/argmend/argmend/issues/157)
* extract one JSON value after leading prose ([#187](https://github.com/argmend/argmend/issues/187)) ([ec96450](https://github.com/argmend/argmend/commit/ec96450fcf62ba33a7c52f2d79f022adfccc47a0)), closes [#169](https://github.com/argmend/argmend/issues/169)
* let callers turn off path, alias, and truncation repairs ([#183](https://github.com/argmend/argmend/issues/183)) ([08600e0](https://github.com/argmend/argmend/commit/08600e08f92a43223fd12d4c3f82aa8b70091657))
* mark public results non_exhaustive ([#182](https://github.com/argmend/argmend/issues/182)) ([c57b53c](https://github.com/argmend/argmend/commit/c57b53cd4ac0ed9a6b4fc59e757dbfc8e1453ca1)), closes [#158](https://github.com/argmend/argmend/issues/158)
* parse one JSON string when the schema asks for a container ([#186](https://github.com/argmend/argmend/issues/186)) ([87c1da3](https://github.com/argmend/argmend/commit/87c1da3ae0c8fe2341101d12d2329fde74fd1829)), closes [#167](https://github.com/argmend/argmend/issues/167)
* point schema rewrites and refusals at a JSON pointer ([#184](https://github.com/argmend/argmend/issues/184)) ([a80c617](https://github.com/argmend/argmend/commit/a80c61707d114ddaad5dcc0b6c33c7d870209491))
* preview a tool-call argument that is still streaming ([#206](https://github.com/argmend/argmend/issues/206)) ([79c8396](https://github.com/argmend/argmend/commit/79c8396ea7478f9b1fd60e337ed62c766b8112a7)), closes [#171](https://github.com/argmend/argmend/issues/171)
* wrap one scalar into an array when asked ([#205](https://github.com/argmend/argmend/issues/205)) ([feadb47](https://github.com/argmend/argmend/commit/feadb47984bbd9caf0e859f6d2e99cec7a6fcac3)), closes [#170](https://github.com/argmend/argmend/issues/170)


### Bug Fixes

* apply if, not, and oneOf inside parameters ([#175](https://github.com/argmend/argmend/issues/175)) ([da3883c](https://github.com/argmend/argmend/commit/da3883c998e5ef037304cf15e71798d0145d5df7)), closes [#156](https://github.com/argmend/argmend/issues/156) [#159](https://github.com/argmend/argmend/issues/159)
* close a number once whitespace ends it ([#174](https://github.com/argmend/argmend/issues/174)) ([f0415d6](https://github.com/argmend/argmend/commit/f0415d63a3d328a5a32a351a4f3067738ce90700)), closes [#163](https://github.com/argmend/argmend/issues/163)
* do not apply then when if fails allOf ([#181](https://github.com/argmend/argmend/issues/181)) ([891790e](https://github.com/argmend/argmend/commit/891790eeb740d5137ce26c4c5d2138f9b33b1a8d)), closes [#176](https://github.com/argmend/argmend/issues/176)
* keep a JSON escape after a one-letter label ([#172](https://github.com/argmend/argmend/issues/172)) ([2be0176](https://github.com/argmend/argmend/commit/2be0176f21795a139573018571158da2279e0e2b))
* refuse a value nested more than 128 levels ([#177](https://github.com/argmend/argmend/issues/177)) ([32b1230](https://github.com/argmend/argmend/commit/32b123035387645d60d0faf626c9bcfabaed02c4)), closes [#160](https://github.com/argmend/argmend/issues/160)

## 0.2.8

- A quoted Windows path keeps `!`, `?`, `*`, `[`, and `]`.
- An unquoted Windows path keeps `!`, `?`, `*`, and `[`. `]` still ends an array.
- A lone-backslash drive path inside a sentence keeps its backslashes.
- A newline that is not on that path stays a newline.
- Public docs, community health files, and the security workflows.

## 0.2.7

Tagged as `v0.2.7`. Earlier releases are the `v0.2` git tags.
