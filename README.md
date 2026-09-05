# BlastRay

> Know what your agent can break before it edits.

BlastRay is a native structural intelligence engine for coding agents. It answers four questions: `find`, `inspect`, `trace`, and `impact`.

## Install

Download your platform archive from [GitHub Releases](https://github.com/DhruvXcode/blastray/releases), unpack it, and place `blastray` on `PATH`. Rust users can build from source with `cargo install --path .`.

## Quick start

Run from a repository. The first query creates generated `.blastray/` state automatically; no initialization is needed.

```sh
blastray find refreshSession
blastray find "where are failed HTTP requests retried"
blastray inspect src/auth/session.ts::refreshSession
blastray trace src/a.ts::start src/d.ts::finish
blastray impact src/auth/session.ts::refreshSession
blastray impact --diff
```

`find` accepts an exact symbol or a task/concept-style query. Exact canonical
and symbol-name matches stay deterministic; task queries use compact local
source evidence to suggest a small set of places to inspect.

`inspect` returns a compact definition slice alongside confirmed callers,
callees, call-site evidence, imports, and any relevant unresolved boundaries.

`impact` walks only source-proven symbol dependencies in reverse. Alongside
confirmed `CALLS`, TypeScript and Java currently contribute exact local or
relative/imported `EXTENDS` and `IMPLEMENTS` contracts, with declaration-site
evidence. File imports, text matches, framework conventions, and unresolved
types never become impact edges.

## MCP

`blastray mcp` is a stdio MCP server. Configure an MCP-capable client to launch it in the repository working directory. It exposes exactly `find`, `inspect`, `trace`, and `impact`; `impact` accepts `@diff` for working-tree impact.

## Supported languages

JavaScript, TypeScript, Python, Rust, Go, and Java have conservative structural support. Dynamic, external-package, and compiler-dependent relationships remain unresolved rather than guessed. Java needs no JVM, JDK, Maven, Gradle, compiler, LSP, classpath, or package-manager execution. Go nested-module cross-package imports are not currently modeled.

## Development

```sh
cargo test
cargo run -- --help
```

BlastRay is early beta software. Deleting `.blastray/` is always safe; it is reconstructible local state.
