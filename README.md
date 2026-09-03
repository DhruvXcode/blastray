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

Mission 0 establishes the crate, project boundaries, and reference-study setup.
The intelligence engine is not implemented yet.

## Build and test

```sh
cargo build
cargo test
cargo run -- --help
```
