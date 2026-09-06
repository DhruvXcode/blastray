# Mission 2 studies
## GitNexus (commit `932d937085e14664f4ef97b06506bf01034497ab`)
- Finding: file content hashes classify unchanged, changed, added, and deleted
  files deterministically; incremental work starts from that diff.
  Why it matters: BlastRay's eventual persisted index should make content and
  file-set changes explicit. Mission 2 receives an explicit changed path, so it
  keeps the same conservative distinction through full-rebuild fallbacks.
  Reference: `gitnexus/src/storage/file-hash.ts`,
  `gitnexus/test/unit/incremental-file-hash.test.ts`.
- Finding: cached parse artifacts avoid reparsing unchanged files, but cache
  schema/version changes must invalidate safely.
  Why it matters: Mission 2 retains `ParsedFile` artifacts in memory and keeps
  persistence/versioning out of scope until it can be designed deliberately.
  Reference: `gitnexus/src/storage/parse-cache.ts`,
  `gitnexus/src/core/ingestion/pipeline-phases/parse-impl.ts`.
- Finding: an import reverse graph gives a bounded importer closure; the
  orchestration tests treat interrupted/uncertain incremental state as a full
  rebuild case.
  Why it matters: BlastRay re-resolves the edited file and its direct resolved
  importers only, and chooses a full rebuild for additions, deletions, renames,
  unsupported paths, or any path it did not previously index.
  Reference: `gitnexus/test/unit/incremental-orchestration.test.ts`,
  `gitnexus/src/core/ingestion/utils/graph-sort.ts`.
- Finding: warm parse caching alone can fail to improve latency when a later
  scope-resolution phase still re-extracts or performs workspace-wide derived
  work; the project documents and profiles those phases separately.
  Why it matters: BlastRay keeps its supported resolution subset narrow and
  separates retained parse artifacts, per-file resolution facts, and cheap
  rebuilt query adjacency. It must measure refresh separately from full build.
  Reference: `gitnexus/src/core/ingestion/pipeline-phases/parse-impl.ts`,
  `gitnexus/src/core/ingestion/scope-resolution/pipeline/run.ts`.
- Finding: GitNexus persists generated state under `.gitnexus/` and has broad
  MCP/editor integrations, which add lifecycle and startup complexity.
  Why it matters: BlastRay should not add state or integrations in Mission 2;
  any future state stays reconstructible under `.blastray/` and the public
  intelligence vocabulary stays small.
  Reference: `gitnexus/src/storage/repo-manager.ts`, `.mcp.json`,
  `gitnexus-claude-plugin/`.
## Mission 3 additions
- Finding: cache metadata and parsed artifacts are version-gated; a missing,
  malformed, or version-mismatched cache is deliberately equivalent to no
  cache, not a user-facing analysis failure.
  Why it matters: BlastRay validates a schema-tagged, checksummed `index.bin`
  before using it and otherwise rebuilds entirely from source.
  Reference: `gitnexus/src/storage/repo-manager.ts`,
  `gitnexus/src/storage/parse-cache.ts`.
- Finding: a completed temporary sibling file followed by rename protects the
  previously readable state from truncation during an interrupted write.
  Why it matters: BlastRay writes one temporary `.blastray/index.bin.tmp-*`
  file, syncs it, then renames it into place; temporary leftovers are never
  read, and a bad final file triggers a full rebuild.
  Reference: `gitnexus/src/storage/fs-atomic.ts`.
- Finding: deterministic hash diffs separate modifications from file-set
  changes before incremental orchestration begins.
  Why it matters: BlastRay hashes every currently selected source file with
  BLAKE3; additions, deletions, and path changes take the full-rebuild path.
  Reference: `gitnexus/src/storage/file-hash.ts`.
## Mission 4 additions
- Finding: zero-context diff parsing must preserve a deletion's old-side range
  or a deletion can be misreported as no change; an unparseable nonempty diff
  is partial analysis, never a clean result.
  Why it matters: BlastRay maps deleted lines against the matching `HEAD` file
  and labels unparseable or unmappable regions conservative/incomplete.
  Reference: `gitnexus/src/storage/git.ts` (`parseDiffHunks`),
  `gitnexus/src/mcp/local/local-backend.ts` (`detectChanges`).
- Finding: changed symbols and capped results need stable ordering, while a
  line-to-symbol match must be narrowed by source span rather than widened to
  every enclosing declaration.
  Why it matters: BlastRay selects the narrowest enclosing current function,
  class, or method, sorts canonical roots, and uses one deduplicated reverse
  traversal for all roots.
  Reference: `gitnexus/src/mcp/local/local-backend.ts` (`detectChanges`).
## Mission 6 addition
- Finding: TypeScript sources in GitNexus use runtime `.js` relative specifiers
  while the indexed implementation is `.ts`, and important exported lifecycle
  callables are arrow-function `const` bindings.
  Why it matters: a narrow exact-first ESM substitution plus callable-binding
  extraction restores confirmed edges without package or type-system guessing.
  Reference: `gitnexus/src/core/run-analyze.ts`,
  `gitnexus/src/storage/index-lock.ts`,
  `gitnexus/src/core/search/fts-indexes.ts`.
## Mission 7 addition
- Finding: the GitNexus unresolved census found 2,056 direct `this` member
  calls, of which 1,873 had a unique same-class indexed method candidate; only
  two unresolved calls used a relative namespace import.
  Why it matters: same-class `this.method()` provides high-value deterministic
  graph coverage without object/type inference, while namespace support would
  add complexity for little observed benefit.
  Reference: census over `gitnexus` commit `932d937085e14664f4ef97b06506bf01034497ab`;
  representative `gitnexus/src/cli/watch-queue.ts`.
## Mission 8 decision
- Finding: only six of 2,540 direct class-static-shaped calls resolved to a unique indexed Class/static method; globals (`Math`, `Number`, `JSON`) dominated.
  Why it matters: class-like spelling is not evidence; retain conservative receivers.
  Reference: Mission 8 census over GitNexus `932d937`; `src/mcp/local/local-backend.ts`.
## Mission 9 decision
- Finding: 287 of 374 named re-export bindings forwarded a unique direct callable; only three needed chains and wildcards had no observed direct call-site value.
  Why it matters: one-hop named forwarding is high-confidence and keeps canonical identity intact; export closure would add complexity without measured need.
  Reference: Mission 9 census; `src/core/auto-sync/index.ts`, `src/core/ingestion/pipeline-phases/index.ts` in GitNexus `932d937`.
## Mission 10 decision
- Finding: a local `export { name }` can forward a previously imported direct
  relative binding, a common compatibility barrel pattern; 135 such bindings
  accounted for 21 of the 41 observed callable-absent direct call sites.
  Why it matters: resolving the one direct import/export hop restores canonical
  CALLS targets without object inference or a general export closure. CommonJS,
  values, constructors, and parser-limited patterns dominated the remaining
  misses and were not safe to generalize.
  Reference: Mission 10 census; `src/storage/repo-manager.ts`,
  `src/storage/repo-meta.ts` in GitNexus `932d937`.
## Mission 11 decision
- Finding: 325 unresolved member calls had direct immutable constructor
  ownership evidence; 311 reached a unique non-static method candidate without
  type inference, and 221 new CALLS edges survived production scope checks.
  Why it matters: this is the last measured high-value receiver slice with a
  compact proof model. The remaining large receiver categories require flow,
  type, object-member, or external-package semantics.
  Reference: Mission 11 census; `src/cli/analyze-watch.ts`,
  `src/cli/watch-queue.ts` in GitNexus `932d937`.
## Provider lessons
- Python: explicit relative imports were common in package-oriented repositories, while absolute imports need environment/package knowledge; a filesystem-only provider must leave the latter explicit. Reference: Mission 14 OSS census.
- Rust: declared file modules and rooted use paths provide a small proof surface, while rustc method selection, macros, traits, and external crates do not. Reference: Mission 15 OSS audit.
- Go: package declaration plus the nearest source `go.mod` prove a compact local-import boundary; nested modules and external packages require more environment knowledge than the provider should assume. Reference: Mission 16 OSS audit.
- Java: package declarations and exact non-wildcard imports are enough to prove a small repository-local type map. Unique same-owner calls and simple static class calls survive source review; overloads, object receivers, and inheritance do not. Reference: Mission 18 OSS audit.
- Discovery: source comments and docstrings can make task wording useful without asserting any new graph fact; retain them as bounded per-symbol lexical evidence rather than copying whole files or inferring edges. Reference: Mission 19 frozen discovery corpus.
- Inspection: synchronized source can be read once at query time for a bounded definition packet; this avoids cache duplication while giving an agent the declaration, intent, and local control flow it would otherwise open a file to obtain. Reference: Mission 20 evaluation.

## Mission 23 onboarding lesson
- CodeGraph (commit `b9ca4b7981116909900368cc1686a1074cd4d4c1`) demonstrates that MCP initialization instructions and a strongly decision-oriented primary exploration surface can make an indexed capability legible without repository-file injection. GitNexus demonstrates that generated agent context, skills, and editor integration can increase teaching coverage but also create lifecycle and repository-footprint costs. BlastRay retains concise MCP guidance plus small redundancy in the four tool descriptions and schemas; it does not adopt generated repository instructions, hooks, plugins, aliases, or a broader tool surface.
- Mission 23B: MCP-native guidance alone produced 0/3 natural Codex selections in the controlled probe. One tiny user-scoped Codex skill produced 3/3 selections without repository injection, so a host-native discovery bridge can be warranted when measured; it should remain one optional user integration, not a repository lifecycle feature.

## Mission 24 action-economy lesson
- A discovery instruction must name both the first action and the stopping boundary: treating find, inspect, trace, and impact as equal steps causes synonym searches and unnecessary verification even when the graph has already located the implementation.

## Mission 25 utility lesson
- A structural result earns its transport overhead only when it displaces the follow-up work it invites: one decisive `find` plus one inspect helped node-fetch, while distributed paths triggered serial inspections and source rereads that made equal-quality answers more expensive.
