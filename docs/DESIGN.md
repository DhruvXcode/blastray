# Design

## Purpose

BlastRay helps coding agents understand how repository code connects and what a
change can affect before they edit.

## Principles

- One native binary and one Rust crate.
- Small, direct, measurable implementation.
- Plain functions, structs, and data flow before abstractions.
- Minimal dependencies and no unearned architecture.

## Non-goals

BlastRay is not a web UI, AI/chat product, LLM client, embedding or vector
database, plugin system, cloud backend, telemetry system, database server, raw
query language, refactoring engine, or multi-repository system.

## Direction

The future pipeline is:

```text
source -> parse -> resolve -> graph -> query -> CLI/MCP
```

Each stage must earn its complexity through a current user-facing need.

BlastRay is a sugar layer: it should improve an existing repository without
making that repository feel like a BlastRay project. Future generated state is
reconstructible under `.blastray/`; BlastRay must not silently edit tracked
`.gitignore` or agent instruction files.

## Mission 1 boundary

Mission 1 builds the graph in memory on each CLI invocation. Only confirmed
relationships become edges. Unresolved and ambiguous imports or calls are kept
as explicit diagnostics and never participate in `trace` or `impact`.

## Mission 2 boundary

`Index::build` retains parsed artifacts in memory. `Index::refresh` reparses a
modified existing supported file, re-resolves it and its direct resolved
importers, then deterministically rebuilds the lightweight graph views and
adjacency. Added, deleted, renamed, unsupported, or otherwise uncertain paths
fall back to a full rebuild. There is no persistence, watcher, daemon, or new
public command.

## Mission 3 boundary

The ordinary query commands automatically maintain one reconstructible local
cache at `.blastray/index.bin`. A cold, corrupt, incompatible, or file-set
changed cache rebuilds from source; unchanged state loads directly; modified
existing supported files refresh through the Mission 2 path. BlastRay never
requires initialization, edits tracked `.gitignore`, or makes persisted state
authoritative over current source files.
