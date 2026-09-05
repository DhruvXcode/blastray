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

## Mission 7 same-class `this` calls

Parsed methods retain whether they are static, and parsed call facts distinguish
a direct non-computed `this.member()` from other member calls. Resolution uses a
temporary per-build map keyed by file, class, member name, and staticness. One
unique matching method becomes a CALLS edge; no match is unresolved and more
than one is ambiguous. Cache schema 4 invalidates pre-`this` persisted facts.

## Mission 9 named re-exports

Parsed files retain relative named re-export bindings (`local` -> module-visible
`exported` name). Resolution maps a unique one-hop binding to the underlying
canonical Function symbol; it does not create a barrel symbol. The re-exporting
file has an IMPORTS dependency on a uniquely resolved source module, and refresh
re-resolves the reverse importer closure. Cache schema 5 invalidates prior
parsed artifacts.

## Mission 10 local export forwarding

Parsed files retain non-type `export { local as publicName }` facts separately
from canonical symbols. A local Function or a uniquely resolved direct relative
import binding can expose that public name; the exported name maps to the
underlying canonical Function rather than a synthetic barrel symbol. Missing,
non-callable, type-only, ambiguous, indirect, and unresolved source bindings
remain unresolved or ambiguous. Cache schema 6 invalidates prior parsed facts.

## Mission 11 constructor-bound receivers

Callable parse artifacts retain only immutable direct constructor bindings:
`const local = new Class()`. A direct non-computed `local.method()` can use that
fact to target the existing non-static Method canonical symbol, with ordinary
CALLS evidence and adjacency. The facts are scoped to the containing callable;
reassigned or competing bindings do not resolve. Cache schema 7 invalidates
prior parsed artifacts.

## Mission 13 provider facts

The cache now stores a provider-tagged parsed artifact plus common resolved
file facts. Providers keep syntax and resolution details private, while the
core materializes only common symbol facts, imports, calls, issues, and call
sites. Cache schema 8 invalidates pre-provider artifacts. A provider receives
the full parsed repository for context but resolves only the paths selected by
the incremental lifecycle.

## Mission 14 Python facts

Python parsed artifacts retain top-level Functions, Classes, direct Methods,
their source spans, callable-body shadow facts, imports, and call sites. A
relative `from .module import name` binding resolves only to one top-level
Function in one `.py` or package `__init__.py` target. Direct calls and
same-class calls through the declared first instance parameter use ordinary
common CALLS evidence. Absolute/package and wildcard imports, arbitrary
receivers, inheritance, and constructor ownership remain unresolved. Cache
schema 9 invalidates pre-Python artifacts.

## Mission 15 Rust facts

`Type` is a common symbol kind for Rust structs, enums, and traits; existing
Classes remain Classes. Rust parsed artifacts retain top-level functions/types,
methods from direct impl blocks, explicit external module declarations, rooted
use bindings, body shadow facts, and call sites. `mod name;` maps only to one
of the structurally valid `name.rs` or `name/mod.rs` files. Rooted
`crate`/`self`/`super` use paths resolve only through that declared local module
map. Direct free calls and direct `self.method()` calls in one inherent impl
produce ordinary CALLS evidence; traits, macros, associated calls, arbitrary
receivers, and external crates do not. Cache schema 10 invalidates pre-Rust
artifacts.

## Mission 16 Go facts

Go parsed artifacts retain declared package names, top-level functions/types,
direct receiver methods, imports, callable shadow facts, and `go.mod` source
context. The provider maps an import only when its path falls under the nearest
known module and exactly one indexed local package directory proves the target;
same-package direct calls and receiver calls likewise require a unique target.
Nested-module cross-package ownership, external packages, arbitrary receivers,
and Go overload-like uncertainty remain unresolved. Cache schema 11 invalidates
pre-Go provider/context state.

## Mission 18 Java facts

Java parsed artifacts retain declared package names, non-wildcard imports,
top-level classes plus interfaces/enums as `Type`, direct methods, modifiers
needed for static proof, callable shadow facts, and call sites. A repository
import is an IMPORTS edge only for one indexed type with the exact package and
name. CALLS require one same-owner direct/`this` method, one direct static
import, or one locally/importedly named Class and one static target method.
Overloaded candidates, local shadowing, non-Class type receivers, arbitrary
objects, inheritance, interfaces, and external/wildcard imports yield issues
instead of edges. Cache schema 12 invalidates pre-Java artifacts.
