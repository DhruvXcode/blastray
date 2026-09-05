# BlastRay

> Know what your agent can break before it edits.

BlastRay is a native structural intelligence engine for coding agents. It answers four questions: `find`, `inspect`, `trace`, and `impact`.

## Install

Download your platform archive from [GitHub Releases](https://github.com/DhruvXcode/blastray/releases), unpack it, and place `blastray` on `PATH`. Rust users can build from source with `cargo install --path .`.

## Quick start

Run from a repository. The first query creates generated `.blastray/` state automatically; no initialization is needed.

```sh
blastray find refreshSession
blastray inspect src/auth/session.ts::refreshSession
blastray trace src/a.ts::start src/d.ts::finish
blastray impact src/auth/session.ts::refreshSession
blastray impact --diff
```

## MCP

`blastray mcp` is a stdio MCP server. Configure an MCP-capable client to launch it in the repository working directory. It exposes exactly `find`, `inspect`, `trace`, and `impact`; `impact` accepts `@diff` for working-tree impact.

## Supported languages

JavaScript, TypeScript, Python, Rust, and Go have conservative structural support. Dynamic, external-package, and compiler-dependent relationships remain unresolved rather than guessed. Go nested-module cross-package imports are not currently modeled.

## Development

```sh
cargo test
cargo run -- --help
```

BlastRay is early beta software. Deleting `.blastray/` is always safe; it is reconstructible local state.
