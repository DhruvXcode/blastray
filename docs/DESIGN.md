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

Agent discoverability is part of the interface. BlastRay should use standard
MCP server instructions, tool descriptions, input schemas, and annotations to
teach a connected agent when the existing four operations help. It must not
modify a repository merely to teach agents how to use BlastRay.

Portable MCP guidance is the first layer. Where a host demonstrably does not
select that guidance, BlastRay may offer one small host-native, user-scoped
discovery bridge. The bridge registers the same MCP server and teaches the same
four-operation decision model; it is not repository initialization, a skill
framework, or an extra intelligence surface.

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

## Mission 4 boundary

`impact --diff` asks Git for the staged and unstaged working-tree diff against
`HEAD`, maps changed source spans to the narrowest current symbol, and runs one
merged reverse-CALLS traversal. Deleted lines consult only the matching HEAD
file, never a historical graph. Lines outside indexed symbols use explicit
file-level roots; unsupported, added, deleted, renamed, untracked, unresolved,
ambiguous, or truncated portions make completeness conservative/incomplete.

## Mission 5 boundary

`blastray mcp` is one stdio MCP process for the repository in its working
directory. It exposes only `find`, `inspect`, `trace`, and `impact`, and holds
one live `Index` behind a mutex. Each tool call synchronizes source hashes and
uses the existing incremental/full-rebuild rules before answering; no watcher,
agent files, prompts, resources, or alternate graph format is added.

## Mission 6 boundary

Relative TypeScript imports may use Node ESM runtime `.js` or `.jsx` specifiers
when no exact source file exists: BlastRay checks the compatible TypeScript
source candidates and reports multiple matches as ambiguous. It indexes only
plain top-level identifier bindings whose initializer is an arrow function or
function expression. `find` remains deterministic lexical ranking with a small
display cap, and resolved CALLS retain call-site locations for existing
`inspect`, `trace`, and direct `impact` evidence.

## Mission 7 boundary

Inside an indexed direct class method, BlastRay resolves a non-computed
`this.method()` only when the same class defines exactly one method with that
name and matching staticness. It does not infer inheritance, external objects,
computed properties, chained receivers, or aliases of `this`.

## Mission 9 boundary

BlastRay forwards a relative one-hop named re-export only when its source module
and already-direct exported callable are each unique. The forwarding module adds
an internal file dependency, but the CALLS edge targets the original canonical
symbol. Wildcards, chains, default forwarding, type-only exports, missing
exports, and ambiguous source modules remain unresolved or ambiguous. Refresh
uses the reverse importer closure so edits to a re-export source re-resolve its
barrel consumers.

## Mission 10 boundary

BlastRay also forwards a local explicit export list when its local identifier
uniquely names a Function or a direct relative import that uniquely exposes a
direct callable. The public module name never changes the underlying canonical
symbol. This is not CommonJS support, object-member resolution, default-export
inference, wildcard closure, or multi-hop export linking.

## Mission 11 boundary

Inside an indexed callable, BlastRay resolves `x.method()` only when `x` has
one preceding immutable direct `const x = new Class()` binding, the Class is a
unique indexed local or direct relative import, and the Class has exactly one
non-static indexed method with that name. Reassignment, shadowing, computed or
inline receivers, conditional initializers, inheritance, and all other object
flows remain unresolved.

## Mission 12 boundary

An empty supported-source set is not treated as meaningful structural evidence.
CLI and MCP queries state the current `.ts`, `.tsx`, `.js`, and `.jsx` language
boundary; a mixed repository still indexes its supported files normally.

## Mission 13 boundary

`language.rs` owns a small compile-time provider table. The core asks it which
files are supported, parses source into provider-owned artifacts, requests
common resolved facts for selected files, and derives the supported-language
message from registered extensions. The JS/TS provider owns its Tree-sitter
grammar, AST extraction, module/export resolution, and receiver rules; index
lifecycle, graph materialization, queries, diff orchestration, CLI, and MCP do
not branch on JS/TS syntax.

## Mission 14 boundary

Python is the second compiled provider and emits the same common facts. It owns
`.py` grammar selection, extraction, and conservative Python resolution; core
indexing, graph/query/diff/MCP code remains language-independent. The first
slice proves only top-level functions, classes/direct methods, explicit
relative function imports, and direct same-class instance receiver calls.

## Mission 15 boundary

Rust is a third compiled provider. It owns `.rs` grammar selection, Rust item
and module extraction, and conservative resolution; core indexing, graph/query,
diff, CLI, and MCP remain provider-neutral. `Type` is the one new common symbol
kind, used for Rust structs, enums, and traits without relabeling existing
JavaScript/TypeScript or Python Classes. The first Rust slice proves only
same-file free calls, uniquely declared local `mod` files, rooted local `use`
bindings, and direct same-inherent-type `self.method()` calls.

## Mission 16 boundary

Go is the fourth compiled provider. Its provider-owned facts cover declared
packages, top-level functions/types, direct receiver methods, declared imports,
and source-only `go.mod` module context. Resolution proves same-package direct
calls, receiver calls, and uniquely local module imports without running Go or
reading a package cache. The generic core remains provider-neutral; schema 11
invalidates caches before the Go parsed artifact and provider-context facts.

## Mission 18 boundary

Java is the fifth compiled provider. It extracts declared packages, top-level
classes, interfaces/enums as `Type`, and direct methods with source spans.
Only uniquely proven local-package or explicit repository-local imports become
file dependencies. CALLS require exact non-varargs syntactic arity and are
limited to hierarchy-free owners, except that a unique own zero-argument
direct/`this` method is safe across a hierarchy: a unique direct or `this`
method in the declaring type, or a unique indexed class's simple static method. Local,
parameter, and member-field shadowing, overload/type selection uncertainty,
inheritance/interface dispatch, wildcard imports, reflection, lambdas, method
references, and all classpath/framework semantics remain unresolved. Overloads
use a normalized syntactic parameter signature in their canonical selector;
non-overloaded methods keep the concise selector. Java parsing is native only;
no JVM/JDK, build tool, compiler, LSP, or dependency manager is invoked.
Schema 13 invalidates pre-hardened Java parsed artifacts.

## Mission 19 boundary

`find` has a separate, provider-neutral discovery layer. It ranks compact
source-derived identifier, path, declaration, comment/docstring, string,
body, and test-context terms deterministically; exact identity and exact name
remain dominant. A bounded confirmed-CALLS neighborhood boost can reorder
already textual candidates, but never adds a candidate or graph edge. Search
relevance is therefore not graph truth: IMPORTS and CALLS remain only the
conservative facts proven by providers. There are no embeddings, model calls,
vector storage, daemon, or second indexing lifecycle.

## Mission 20 boundary

`inspect` reads the already-synchronized defining source file at query time and
renders a bounded declaration/body slice with immediately preceding comment or
doc context. Source is not duplicated in `.blastray`; a missing or stale span
is reported as unavailable rather than fabricated. Confirmed callers, callees,
call sites, imports, and unresolved boundaries remain graph facts. A small
`Likely relevant tests` section may use test-path/name proximity or confirmed
call-neighborhood evidence, and is explicitly relevance context—not a TESTS
edge or structural claim.

## Mission 21 impact propagation

Impact traverses a provider-neutral set of source-proven symbol dependencies,
always directed `dependent -> dependency`. `CALLS` remains a confirmed call-flow
fact used by `trace`; `EXTENDS` and `IMPLEMENTS` are impact-only structural
contract facts. Reverse traversal returns direct and transitive dependents with
the relationship kind and source location that admitted each symbol.

Only a provider may emit a dependency fact. The common graph, impact, diff,
CLI, and MCP layers do not infer language semantics. TypeScript emits a fact
only for a direct class/interface heritage name that resolves uniquely to an
indexed same-file or direct relative-imported type. Java applies the analogous
rule to a simple superclass/interface name uniquely resolved in its declared
package or an explicit repository-local import. Ambiguous, external, wildcard,
qualified, generic-argument, framework, runtime, and type-system-dispatch
relationships remain boundaries. File IMPORTS and discovery relevance never
enter impact.

Completeness therefore describes only the supported proven-dependency subset:
it lists target-name-relevant unresolved/ambiguous facts when present and
otherwise explicitly retains the dynamic/unsupported boundary. `impact --diff`
uses the same traversal after its existing conservative changed-symbol mapping.

## Mission 23 boundary

MCP initialization returns a short local-analysis playbook and the four
existing tools carry decision-oriented descriptions, parameter affordances, and
truthful annotations. `find` is the entry point for unfamiliar natural-language
tasks; `inspect` supplies bounded synchronized source context; `trace` requires
known endpoints; and `impact` is the pre-/post-edit structural check.

These are interface metadata only. The tools may refresh reconstructible
`.blastray` cache state, so their `readOnlyHint` is deliberately false; they
are nevertheless non-destructive, idempotent local operations with no open
world interaction. No repository instruction files, resources, prompts, tools,
graph semantics, retrieval, or source-language behavior are added.
