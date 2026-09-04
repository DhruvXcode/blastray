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

## Mission 4 source spans

Parsed symbols retain inclusive one-based start and end lines from Tree-sitter.
`impact --diff` maps a changed line to the narrowest enclosing current symbol;
class methods therefore win over their enclosing class. The persisted span is
canonical parse metadata, while graph IDs and adjacency remain reconstructed
views. Cache schema 2 invalidates caches written before spans existed.

## Mission 5 live synchronization

`Index::sync` compares the live index with current source hashes without
reloading `.blastray/index.bin`. It keeps unchanged in-memory artifacts,
incrementally refreshes modified existing supported files, or rebuilds and
persists on a source file-set change or uncertain refresh. MCP uses this before
each tool call.

## Mission 6 callable and call-site facts

Top-level identifier bindings with arrow-function or function-expression
initializers are Function symbols, including named exports. Relative `.js`
specifier substitution checks an exact file first, then compatible `.ts`/`.tsx`
or `.jsx` candidates; more than one candidate remains ambiguous. Per-file
resolved CALLS facts retain one or more one-based call-site line/column pairs.
The materialized graph still has one edge per source/target pair, with sorted
forward/reverse adjacency and separate evidence lookup. Cache schema 3
invalidates pre-evidence persisted facts.
