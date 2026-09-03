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

Mission 1 provides a deterministic TypeScript/JavaScript vertical slice for
`.ts`, `.tsx`, `.js`, and `.jsx` files. It indexes named top-level functions,
classes, and class methods; resolves only confirmed local or relative-imported
function calls; and reports uncertainty instead of guessing.

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
```
