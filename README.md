# BlastRay

> Know what your agent can break before it edits.

BlastRay is a tiny native code-intelligence engine that lets coding agents see
how code connects and what a change can break.

It will provide four primitives:

- `find` — locate a symbol or code concept
- `inspect` — show the structural neighborhood around a target
- `trace` — explain how one target connects to another
- `impact` — show what a symbol, file, or current diff can affect

## Status

Mission 11 provides a deterministic TypeScript/JavaScript slice for `.ts`,
`.tsx`, `.js`, and `.jsx` files, with a persistent local index and
`impact --diff` for staged plus unstaged Git changes. It indexes named
top-level functions, callable top-level `const` bindings, classes, and class
methods; resolves only confirmed local or relative-imported function calls; and
reports uncertainty instead of guessing. `find` is compact ranked lexical
search, not semantic search. Direct `this.method()` calls resolve only to a
uniquely matching method on the same indexed class.
Relative one-hop named re-exports, including aliases, resolve only when they
uniquely expose an already indexed callable; calls retain the declaration's
original canonical identity. Explicit local export lists can also forward one
uniquely resolved direct relative callable import without changing that identity.
An immutable local `const x = new Class()` can call a uniquely resolved
non-static method on that indexed local or relative-imported Class.

The same four primitives are available to MCP-capable coding agents through
the stdio server: `blastray mcp`.

## Build and test

```sh
cargo build
cargo test
cargo run -- --help
```

Run from a repository root:

```sh
blastray find refreshSession
blastray inspect src/auth/session.ts::refreshSession
blastray trace src/a.ts::start src/d.ts::finish
blastray impact src/auth/session.ts::refreshSession
blastray impact --diff
blastray mcp
```
