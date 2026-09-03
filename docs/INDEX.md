# Index

The index is a small code graph, not a general-purpose database.

Mission 1 entities:

- files
- symbols

Symbol identity is `repo-relative/path.ts::SymbolName`; methods use
`repo-relative/path.ts::ClassName.methodName`.

Mission 1 relationships:

- `DEFINES`
- `IMPORTS`
- `CALLS`

Every attempted import or call is resolved, ambiguous, or unresolved. Only
resolved relationships become graph edges.

## Mission 2 in-memory refresh

Parsed files and per-file resolution facts are keyed by repo-relative path.
Canonical symbol identity remains `path::name`; numeric vector positions are
short-lived materialized view IDs and are rebuilt deterministically after a
refresh. The graph stores sorted forward and reverse CALLS adjacency, so query
traversals do not scan every call edge at each step.

For a modified existing supported source file, BlastRay reparses that file and
re-resolves only it plus direct resolved importers. A full rebuild is used for
file-set changes and any uncertain invalidation.

## Mission 3 persisted state

`.blastray/index.bin` stores only canonical, reconstructible per-file state:
schema/checksum, BLAKE3 source hashes, parsed artifacts, and per-file resolution
facts. On load, BlastRay validates current hashes and reconstructs vector IDs,
lookup maps, and query adjacency in memory. A bad cache is discarded in favor
of a full source rebuild.
