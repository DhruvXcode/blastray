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
