# Benchmarks

BlastRay will eventually measure:

- cold indexing time
- incremental update time
- query latency
- peak and resident memory
- index size
- relationship-resolution accuracy
- effect on coding-agent repository exploration and tool usage

## Mission 1 baseline

Measured before the Mission 2 change at commit `0178b5e`, in release mode:

```text
cd tests/fixtures/basic
TIMEFORMAT='basic wall=%3R seconds'; time ../../../target/release/blastray find ''

cd misc/references/gitnexus
TIMEFORMAT='gitnexus wall=%3R seconds'; time ../../../target/release/blastray find ''
```

One run measured 0.006 s for `tests/fixtures/basic` (13 source files, 23
symbols) and 5.156 s for the pinned GitNexus checkout (2,600 indexed source
files, 7,795 symbols). The environment did not provide `/usr/bin/time`, so peak
RSS was unavailable. The Mission 1 CLI did not expose import/call/issue counts.

## Mission 2 in-memory build versus refresh

Run the manual release benchmark from the repository root:

```text
cargo test --release --lib index::tests::release_build_and_refresh_benchmarks -- --ignored --nocapture
```

The test copies only the source files selected by BlastRay to a temporary
directory; copy/setup and compilation are outside the timed region. It builds
the in-memory index, appends whitespace to one existing file, then refreshes it.
On this environment:

| repository | files | symbols | imports | calls | issues | full build | refresh |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `tests/fixtures/basic` | 13 | 23 | 3 | 10 | 2 | 977 µs | 167 µs |
| pinned GitNexus | 2,600 | 7,795 | 386 | 3,961 | 62,588 | 3,909,360 µs | 64,932 µs |

The changed paths were respectively `src/consumer.ts` and
`eval/workflow_bench/oracles/cross-module-parse-retry.oracle.test.ts`.

## Mission 3 persistent CLI lifecycle

Build release mode, make a disposable repository copy, then time ordinary
queries. The first query creates `.blastray/index.bin`; the second validates
hashes and loads it; appending whitespace exercises refresh without changing
graph meaning.

```text
cargo build --release
benchmark_dir=$(mktemp -d /tmp/blastray-mission3-XXXXXX)
cp -a tests/fixtures/basic "$benchmark_dir/basic"
cd "$benchmark_dir/basic"
TIMEFORMAT='cold=%3R seconds'; time /workspaces/blastray/target/release/blastray find '' >/dev/null
TIMEFORMAT='warm=%3R seconds'; time /workspaces/blastray/target/release/blastray find '' >/dev/null
printf '\n' >> src/local.ts
TIMEFORMAT='modified=%3R seconds'; time /workspaces/blastray/target/release/blastray find '' >/dev/null
wc -c < .blastray/index.bin
rm -rf "$benchmark_dir"
```

The GitNexus run uses the same sequence with a disposable copy of
`misc/references/gitnexus` and changes
`eval/workflow_bench/oracles/cross-module-parse-retry.oracle.test.ts`.

| repository | indexed files | symbols | cache size | cold | warm unchanged | warm modified |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `tests/fixtures/basic` | 13 | 23 | 6,299 bytes | 0.005 s | 0.003 s | 0.004 s |
| pinned GitNexus | 2,600 | 7,795 | 17,507,666 bytes | 4.635 s | 0.300 s | 0.501 s |

Measurements are one release-mode run on this environment. `/usr/bin/time` was
unavailable, so peak RSS was not recorded.

## Mission 4 warm `impact --diff`

Build release mode, create a disposable Git repository for the fixture, warm
the cache with `find`, then make one existing-function edit before timing the
diff query. The GitNexus copy already has a `HEAD`; its edit is deliberately in
one modeled function.

```text
cargo build --release
benchmark_dir=$(mktemp -d /tmp/blastray-mission4-XXXXXX)
cp -a tests/fixtures/basic "$benchmark_dir/basic"
git -C "$benchmark_dir/basic" init -q
git -C "$benchmark_dir/basic" config user.name benchmark
git -C "$benchmark_dir/basic" config user.email benchmark@example.invalid
git -C "$benchmark_dir/basic" add . && git -C "$benchmark_dir/basic" commit -qm initial
(cd "$benchmark_dir/basic" && /workspaces/blastray/target/release/blastray find '' >/dev/null)
# Edit src/local.ts inside leaf(), then:
(cd "$benchmark_dir/basic" && time /workspaces/blastray/target/release/blastray impact --diff)

cp -a misc/references/gitnexus "$benchmark_dir/gitnexus"
(cd "$benchmark_dir/gitnexus" && /workspaces/blastray/target/release/blastray find '' >/dev/null)
# Edit gitnexus/src/utils/process-identity.ts inside isProcessAlive(), then:
(cd "$benchmark_dir/gitnexus" && time /workspaces/blastray/target/release/blastray impact --diff)
```

| repository | indexed files | symbols | cache size | changed files | mapped changed symbols | confirmed affected symbols | warm diff wall |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `tests/fixtures/basic` | 13 | 23 | 6,483 bytes | 1 | 1 | 4 | 0.023 s |
| pinned GitNexus | 2,600 | 7,795 | 17,570,026 bytes | 1 | 1 | 0 | 2.440 s |

These are single release-mode runs on this environment. The fixture result is
conservative/incomplete because the changed file contains an unrelated
unresolved receiver call; the GitNexus result is conservative/incomplete due to
unsupported/import and receiver relationships in its changed file.

## Mission 5 MCP and diff timing probe

The Mission 4 2.440 s GitNexus result was investigated before optimization.
On a warm disposable copy, a fresh CLI `find` (load/check the 17,570,026-byte
cache plus hash validation) took 0.333 s and a changed-file CLI `impact --diff`
took 0.690 s. Direct Git portions of that latter run were small: name-status
0.020 s, unified diff 0.019 s, untracked status 0.060 s, and `HEAD:path`
retrieval 0.003 s. In a live MCP process, changed-file synchronization took
0.325 s; after that synchronization, `impact("@diff")` took 0.252 s. That
post-sync time includes another whole-tree hash validation; its direct Git work
was about 0.102 s, leaving a small old-file parse/mapping/traversal remainder.

The measured major avoidable repeated-agent cost was reloading/deserializing
the cache for each process, not Git itself. Mission 5 therefore makes no risky
Git/cache redesign: the MCP process holds one index and uses `Index::sync`.

Release-mode MCP measurements used JSON-RPC stdio against one persistent
process, after a normal initialize request:

| repository | startup/index open | unchanged `find` | unchanged `impact` | `impact("@diff")` after one edit |
| --- | ---: | ---: | ---: | ---: |
| `tests/fixtures/basic` | 0.009 s | 0.001 s | 0.001 s | 0.019 s |
| pinned GitNexus | 4.489 s | 0.127 s | 0.122 s | 2.266 s |

The first GitNexus changed-diff result includes an initially cold persistent
cache write. A subsequent source edit in the same live process measured 0.325
s for sync and 0.252 s for post-sync diff analysis. These are one-run,
environment-specific measurements, not public performance claims.

## Mission 6 GitNexus structural coverage and ranked find

Build release mode, use a disposable copy of the pinned reference, then invoke
the same query twice so the second invocation uses the persistent cache:

```text
cargo build --release
benchmark_dir=$(mktemp -d /tmp/blastray-mission6-XXXXXX)
cp -a misc/references/gitnexus/gitnexus/. "$benchmark_dir"
(cd "$benchmark_dir" && /workspaces/blastray/target/release/blastray find analyze)
(cd "$benchmark_dir" && TIMEFORMAT='warm=%3R seconds'; time /workspaces/blastray/target/release/blastray find analyze)
wc -c < "$benchmark_dir/.blastray/index.bin"
```

The baseline was the Mission 5 binary and the after measurement was Mission 6,
both against the same 2,430-file source copy. Relationship counts are graph
edges; unresolved and ambiguous counts are explicit issues. The MCP timings
are one warmed JSON-RPC `find("analyze")` call in a live stdio server.

| metric | Mission 5 | Mission 6 |
| --- | ---: | ---: |
| symbols | 7,543 | 9,376 |
| resolved IMPORTS | 187 | 5,739 |
| resolved CALLS | 3,833 | 8,284 |
| unresolved issues | 60,120 | 50,058 |
| ambiguous issues | 1 | 1 |
| `.blastray/index.bin` | 16,072,148 bytes | 16,680,879 bytes |
| warm CLI `find analyze` | 0.246 s | 0.279 s |
| warm MCP `find analyze` | 0.111 s | 0.116 s |
| `find analyze` emitted / total matches | 179 / 179 | 20 / 240 |
| CLI find output | 180 lines, 17,522 bytes | 22 lines, 2,182 bytes |

A read-only Mission 6 MCP pass resolved and traced all three previously missing
direct calls from `runFullAnalysis`: `acquireIndexLock`,
`initialiseSearchFTSStemmer`, and `resetDegradedParseCounter`. These are
single-run environment-specific measurements, not public performance claims.

## Mission 7 unresolved census and same-class `this` calls

The Mission 6 release binary built the pinned GitNexus source set (2,430 files)
and a temporary in-crate census grouped every unresolved issue by exact detail,
context, and source-token receiver shape. The temporary census code was removed
after measurement. Counts below are issue/call-site counts, not unique edges.

| unresolved context | count | share |
| --- | ---: | ---: |
| receiver/member | 30,866 | 61.66% |
| imported binding | 13,306 | 26.58% |
| import/module | 3,782 | 7.55% |
| shadowing | 1,086 | 2.17% |
| direct identifier | 919 | 1.84% |
| other (namespace-import diagnostic) | 99 | 0.20% |

The receiver census found 21,018 local/imported-object receivers, 4,486
chained/dynamic receivers, 2,571 class/static-looking receivers, 2,056 `this`
members, 644 computed/dynamic receivers, 89 non-relative namespace members,
and 2 relative namespace members. Of the `this` calls, 1,873 had a unique
same-class indexed method candidate and 183 had none. This made same-class
`this.method()` the selected deterministic slice; package/import categories and
general receiver shapes require unsupported resolution machinery.

Build release mode and use a disposable reference copy to reproduce the
post-change measurement:

```text
cargo build --release
benchmark_dir=$(mktemp -d /tmp/blastray-mission7-XXXXXX)
cp -a misc/references/gitnexus/gitnexus/. "$benchmark_dir"
(cd "$benchmark_dir" && TIMEFORMAT='cold=%3R seconds'; time /workspaces/blastray/target/release/blastray find analyze)
(cd "$benchmark_dir" && TIMEFORMAT='warm=%3R seconds'; time /workspaces/blastray/target/release/blastray find analyze)
wc -c < "$benchmark_dir/.blastray/index.bin"
```

| metric | Mission 6 | Mission 7 |
| --- | ---: | ---: |
| source files | 2,430 | 2,430 |
| symbols | 9,376 | 9,376 |
| resolved IMPORTS | 5,739 | 5,739 |
| resolved CALLS edges | 8,284 | 9,738 |
| unresolved issues | 50,058 | 48,187 |
| ambiguous issues | 1 | 1 |
| cache size | 16,680,879 bytes | 16,698,099 bytes |
| cold `find analyze` | 4.451 s | 4.088 s |
| warm `find analyze` | 0.279 s | 0.274 s |

The chosen category changed from 2,056 unresolved `this` receiver call sites
to 1,871 resolved, 169 explicitly missing same-class methods, and 16 remaining
unsupported dynamic/private forms. A warm live-MCP `find("watch queue")` took
0.115 s in one release-mode run. These are environment-specific measurements,
not public performance claims.

## Mission 8 class/static receiver census (no semantic change selected)

The Mission 7 release index built the same pinned GitNexus source set in
release mode. A temporary in-crate census joined each unresolved receiver issue
to its originating parsed call, then applied only the proposed direct,
non-computed class-receiver rules: a unique same-file indexed Class or a unique
relative named/default import resolving to an exported indexed Class, followed
by a unique indexed static method. The probe was removed after measurement.
Counts are call sites, not graph edges.

| direct class/static-shaped category | count |
| --- | ---: |
| A. same-file indexed `Class.staticMethod()` | 3 |
| B. relative named imported Class | 3 |
| C. relative aliased imported Class | 0 |
| D. relative default imported Class | 0 |
| E. uppercase-looking receiver not proven to be an indexed Class | 2,487 |
| F. package/external receiver | 25 |
| G. chained/prototype or other non-direct shape | 22 |
| total direct class/static-shaped subset | 2,540 |

The prior broader syntax census reported 2,571 class/static-looking sites; 31
of those were not direct identifier-receiver forms under the Mission 8 rules.
Only 6 direct sites (0.24% of the direct subset) were deterministically
resolvable, so BlastRay intentionally made no Mission 8 semantic change. The
same-file examples were `LocalBackend.formatGroupResourcePayload` (two sites)
and `McpRepositoryPolicy.unrestricted`; the named-import examples include
`McpRepositoryPolicy.unrestricted` in `src/mcp/server.ts`. The remaining
dominant forms are globals such as `Math.floor`, `Number.parseInt`, and
`JSON.stringify`, which are not indexed project Classes.

## Mission 9 imported-binding census and named re-exports

The Mission 8 release index was joined to its parsed imports and target-module
export syntax. Counts are unresolved import bindings; the 858 downstream
"imported binding is not uniquely resolved" call diagnostics were separately
attributed to those bindings. A temporary in-crate probe was removed after
measurement.

| cause | bindings | affected direct call sites |
| --- | ---: | ---: |
| non-relative module | 6,720 | 778 |
| type-only import | 2,815 | 0 |
| callable absent from resolved module | 2,031 | 41 |
| named one-hop re-export | 373 | 35 |
| existing non-callable symbol | 341 | 0 |
| type-only export | 115 | 0 |
| wildcard re-export | 31 | 0 |
| relative module not found | 19 | 2 |
| local export list / alias | 2 | 2 |
| aliased named re-export | 1 | 0 |

Of the 374 named/aliased re-exports, 287 forwarded a unique direct callable in
one hop; 3 needed an indirect chain, 45 had no indexed source module, and 39
had a missing or unsupported source export. This selected one-hop named
re-exports, including aliases; it deliberately excludes chains and wildcards.

Build release mode and run against a disposable reference copy:

```text
cargo build --release
benchmark_dir=$(mktemp -d /tmp/blastray-mission9-XXXXXX)
cp -a misc/references/gitnexus/gitnexus/. "$benchmark_dir"
(cd "$benchmark_dir" && TIMEFORMAT='cold=%3R seconds'; time /workspaces/blastray/target/release/blastray find analyze)
(cd "$benchmark_dir" && TIMEFORMAT='warm=%3R seconds'; time /workspaces/blastray/target/release/blastray find analyze)
wc -c < "$benchmark_dir/.blastray/index.bin"
```

| metric | Mission 8 | Mission 9 |
| --- | ---: | ---: |
| source files | 2,430 | 2,430 |
| symbols | 9,376 | 9,376 |
| resolved IMPORTS | 5,739 | 5,914 |
| resolved CALLS | 9,738 | 9,768 |
| unresolved issues | 48,187 | 47,865 |
| ambiguous issues | 1 | 1 |
| cache size | 16,698,099 bytes | 16,708,417 bytes |
| cold `find analyze` | 4.088 s | 4.089 s |
| warm `find analyze` | 0.274 s | 0.292 s |

The selected forwarding facts changed from 374 unresolved named-re-export
bindings to 287 proven forwardings, 0 new ambiguous forwardings, and 87
unsupported/missing/indirect cases. They restored 30 confirmed CALLS edges.
These are single-run environment measurements, not public performance claims.

## Mission 10 callable-absent census and local export forwarding

The Mission 9 `callable absent` count (2,031 bindings / 41 direct call sites)
was a source-symbol-absence bucket. Mission 10 joined every current
`the imported symbol was not uniquely exported by the resolved module`
diagnostic to its import and target source, yielding 2,607 exact diagnostic
instances. The broader exact join includes values, classes, parser-limited
declarations, and export forms outside the older bucket; it was used so no
direct call diagnostic was silently omitted.

| exact target-module cause | bindings | direct call sites |
| --- | ---: | ---: |
| explicit local callable export | 2 | 2 |
| unsupported declaration | 412 | 0 |
| object literal | 24 | 0 |
| class constructor-like import | 338 | 0 |
| exported non-callable value | 1,133 | 12 |
| CommonJS assignment | 184 | 6 |
| unsupported re-export chain | 119 | 0 |
| wildcard re-export | 28 | 0 |
| local export without indexed local binding | 37 | 0 |
| other/parser-limited shape | 195 | 2 |
| local export forwarding an imported binding | 135 | 21 |

Forwarding the last category was selected: it covers 21 of the 41 original
call sites with direct relative imports and unique direct callable exports.
The nearest alternatives either cover only two sites or require object-member,
CommonJS, class-constructor, or wider export-closure semantics.

Release-mode measurements used the existing `find analyze` disposable-copy
procedure from Mission 9. The explicit importer-closure scan was also timed
inside a release build; the first no-importer path was
`bench/impact-pdg/fixtures/inter-dispatcher-thin/src/dispatcher.ts`.

| metric | Mission 9 | Mission 10 |
| --- | ---: | ---: |
| source files | 2,430 | 2,430 |
| symbols | 9,376 | 9,376 |
| resolved IMPORTS | 5,914 | 5,914 |
| resolved CALLS | 9,768 | 9,788 |
| unresolved issues | 47,865 | 47,729 |
| ambiguous issues | 1 | 1 |
| cache size | 16,708,417 bytes | 16,889,808 bytes |
| cold `find analyze` | 4.089 s | 3.960 s |
| warm `find analyze` | 0.292 s | 0.290 s |

All 135 selected binding diagnostics resolved; their 21 direct call sites
restore 20 deduplicated CALLS edges (two sites share one source/target edge).
The full reference build took 7.181 s in the release in-crate measurement.
Reverse importer closure took 89 us for the no-importer path (size 1) and
1,556 us for `src/storage/repo-meta.ts` (size 245), so Mission 10 leaves the
simple scan unchanged. These are single-run environment measurements, not
public performance claims.

## Mission 11 receiver ownership and constructor-bound calls

A release-mode Tree-sitter census joined every remaining unresolved direct
non-computed member call to its nearest indexed callable and lexical receiver
binding. The probe was removed after measurement. The initial 28,645 call-site
classification found 45 same-file and 280 direct-relative-import constructor
sites: 325 sites over 144 receiver bindings. Of those, 311 had a unique
non-static Class method candidate (222 source/target pairs), enough to select
the narrow immutable-constructor slice.

| remaining receiver category after the slice | call sites |
| --- | ---: |
| parameter or local-shadowed receiver | 4,933 |
| same-file local value | 4,670 |
| unresolved/global receiver | 4,017 |
| other dynamic receiver | 3,641 |
| local explicit type annotation | 2,727 |
| function-return/chained receiver | 2,248 |
| relative imported object/value | 2,198 |
| class field or `this` property | 1,549 |
| package/external receiver | 323 |
| object literal | 9 |
| mutable/reassigned or unindexed constructor | 4 |

Build release mode and use the same disposable-copy command as Mission 10.
The post-change run measured 302 resolved constructor call sites and 221 new
deduplicated CALLS edges; nine syntactically promising sites stayed unresolved
because the production model rejects competing lexical bindings.

| metric | Mission 10 | Mission 11 |
| --- | ---: | ---: |
| source files | 2,430 | 2,430 |
| symbols | 9,376 | 9,376 |
| resolved IMPORTS | 5,914 | 5,914 |
| resolved CALLS | 9,788 | 10,009 |
| unresolved issues | 47,729 | 47,427 |
| ambiguous issues | 1 | 1 |
| cache size | 16,889,808 bytes | 17,353,479 bytes |
| full build | 7.181 s | 6.805 s |
| cold `find analyze` | 3.960 s | 4.100 s |
| warm `find analyze` | 0.290 s | 0.290 s |

Examples include `startWatchFileLoop` creating `WatchRefreshQueue` before
calling `runInitial`, `evalServerCommand` creating `LocalBackend` before
`init`, and `wikiCommandImpl` creating `WikiGenerator` before `run`. These are
single-run environment measurements, not public performance claims.

## Mission 12 multi-repository pre-beta audit

Release-mode first and second `blastray find audit` queries were run in
disposable shallow clones. The first query creates `.blastray/index.bin`; the
second validates hashes and loads the warm state. Source trees honor their own
Git ignore rules and no dependency installation or generated vendor tree was
included. Wall time is Bash's `time`; `/usr/bin/time` was unavailable here.

```text
git clone --depth 1 https://github.com/expressjs/morgan.git audit-repo
cd audit-repo
TIMEFORMAT='cold=%3R'; time /workspaces/blastray/target/release/blastray find audit
TIMEFORMAT='warm=%3R'; time /workspaces/blastray/target/release/blastray find audit
wc -c < .blastray/index.bin
```

| repository | pinned commit | supported files | symbols (function/class/method) | imports | calls | unresolved | ambiguous | unresolved/symbol | cache bytes | cold | warm |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| [GitNexus](https://github.com/abhigyanpatwari/GitNexus) | `932d937085e14664f4ef97b06506bf01034497ab` | 2,430 | 9,376 (7,525/307/1,544) | 5,914 | 10,009 | 47,427 | 1 | 5.06 | 17,353,479 | 4.354 s | 0.300 s |
| [morgan](https://github.com/expressjs/morgan) | `286b000228cacba362bfa89791c6268663f86610` | 3 | 25 (25/0/0) | 0 | 7 | 43 | 0 | 1.72 | 13,504 | 0.020 s | 0.003 s |
| [p-queue](https://github.com/sindresorhus/p-queue) | `180ab9e25cd10b6f548767d7176076b50d25e188` | 14 | 59 (6/2/51) | 17 | 3 | 174 | 0 | 2.95 | 45,717 | 0.038 s | 0.003 s |
| [zustand](https://github.com/pmndrs/zustand) | `b57db4f86ef179285da216eeb291266da82c361c` | 48 | 59 (59/0/0) | 33 | 9 | 416 | 0 | 7.05 | 104,282 | 0.062 s | 0.006 s |
| [changesets](https://github.com/changesets/changesets) | `d7e4a2d7e60f963d858ac31068fe8cddecc6ca2d` | 163 | 334 (305/8/21) | 210 | 229 | 2,747 | 0 | 8.22 | 768,009 | 0.166 s | 0.015 s |

Top unresolved categories, by exact diagnostic detail, show the same honest
coverage boundary across unrelated codebases: receiver/dynamic syntax is the
largest category except where non-relative imports dominate.

| repository | top unresolved categories (count) |
| --- | --- |
| GitNexus | receiver/dynamic (26,498); imported binding waits for module (6,739); non-relative imports (3,758); type-only imports (2,815); export not uniquely callable (2,494) |
| morgan | receiver/dynamic (28); no matching local/imported function (13); possible local shadowing (2) |
| p-queue | receiver/dynamic (76); imported binding waits for module (35); non-relative imports (33); no same-class method (12); export not uniquely callable (10) |
| zustand | imported binding waits for module (173); non-relative imports (95); receiver/dynamic (59); type-only imports (40); imported binding not uniquely resolved (24) |
| changesets | receiver/dynamic (1,085); imported binding waits for module (632); non-relative imports (510); imported binding not uniquely resolved (175); type-only imports (170) |

The audit source-checked the first five deterministic CALLS edges from each
repository where available (three for p-queue): all 23 call sites and canonical
targets were verified against source; incorrect 0, uncertain 0. This is a
small correctness sample, not a coverage claim.

Three disposable working-tree edits also exercised `impact --diff`: morgan
`index.js::clfdate` and zustand `src/vanilla/shallow.ts::shallow` mapped
correctly with zero confirmed downstream callers; p-queue
`source/lower-bound.ts::lowerBound` mapped correctly with confirmed callers
`PriorityQueue.enqueue` then `PriorityQueue.setPriority`. All three results
remained explicitly conservative/incomplete because the changed files contain
unresolved calls.

Live stdio MCP protocol passes used one process per repository and the existing
four tools. One lexical find located a useful target in p-queue (`priority
queue`, 10 results), zustand (`shallow`, 6), and changesets (`release plan`,
capped at 20 of 55). `inspect`, `trace`, and `impact` then produced useful
confirmed neighborhoods and paths for `PriorityQueue.enqueue -> lowerBound`,
`shallow -> compareEntries`, and `applyReleasePlan -> getNewChangelogEntry`.
No protocol stdout noise appeared; unresolved receiver/package edges remain the
visible limit.

The requested private Kael Chess clone was inaccessible (HTTP 403), so the
public Dart repository [dart-lang/language](https://github.com/dart-lang/language)
at `91fa646beb384eaa41257f52e3b4ac9434504578` was used as the
unsupported-language control. At that time it received: `No supported source
files found. BlastRay currently indexes .ts, .tsx, .js, and .jsx.`

## Mission 13 provider-boundary equivalence

The pre-refactor `41e1ec7` binary and the provider-boundary binary were run
against the same disposable combined fixture. `find`, `inspect`, `trace`, and
`impact` outputs were byte-identical for direct imports, one-hop re-exports,
same-class `this` calls, and constructor-bound calls. A controlled body edit
also produced byte-identical `impact --diff` output.

The pinned GitNexus reference remained exactly structurally equivalent:
2,430 files, 9,376 symbols, 5,914 IMPORTS, 10,009 CALLS, 47,427 unresolved
issues, and one ambiguous issue. A release build measured 6.962 s.

Cold/warm measurements below use two disposable GitNexus copies with existing
state removed; the second pair reverses run order to distinguish cache/host
noise from the refactor. The provider enum adds one serialized tag per parsed
file; it does not add a dependency or separate per-language cache.

| metric | Mission 12 / pre-provider | Mission 13 |
| --- | ---: | ---: |
| cold `find analyze` (first order) | 4.465 s | 5.081 s |
| cold `find analyze` (reversed order) | 4.422 s | 4.470 s |
| warm `find analyze` | 0.310 s | 0.307 s |
| cache size | 17,353,479 bytes | 17,363,199 bytes |
| warm live-MCP `find("analyze")` | not re-baselined | 135 ms |

The cold first-order outlier did not reproduce after reversal; warm behavior
and structural counts were unchanged. These are single-machine measurements,
not public performance claims.

## Mission 14 Python-provider audit

The release binary was built before and after adding only
`tree-sitter-python 0.25.0`: 11,283,696 bytes -> 11,840,888 bytes. Four
disposable shallow clones were indexed without installed dependencies or
generated vendor state. The first release `find` creates the cache and the
second validates it. Metrics are one-run wall-clock measurements.

| repository | pinned commit | `.py` files | symbols (function/class/method) | imports | calls | unresolved | ambiguous | unresolved/symbol | cache bytes | cold | warm |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| [Requests](https://github.com/psf/requests) | `dae7ef63b4df6eded86637f251fc4e3a06c3b479` | 37 | 707 (146/72/489) | 60 | 168 | 2,727 | 20 | 3.86 | 775,434 | 0.177 s | 0.049 s |
| [Click](https://github.com/pallets/click) | `36baa15ff831b939a22bc527cd76ce653ef6f66d` | 79 | 1,321 (787/113/421) | 43 | 321 | 4,247 | 14 | 3.22 | 1,226,274 | 0.340 s | 0.061 s |
| [Starlette](https://github.com/encode/starlette) | `c9176b57af56eb0f0906e47e0eea95de43bbf38f` | 84 | 1,460 (772/166/522) | 0 | 201 | 6,714 | 0 | 4.60 | 1,753,662 | 0.306 s | 0.045 s |
| [Pytest](https://github.com/pytest-dev/pytest) | `51e9a9f148cd2509a31e3fa0d2b1b3204c2b0dd7` | 272 | 6,364 (2,002/544/3,818) | 18 | 1,454 | 27,626 | 13 | 4.34 | 7,857,398 | 0.723 s | 0.114 s |

Top unresolved categories were explicit uncertainty, not missing graph edges:
arbitrary/dynamic receivers dominated all four; unresolved external or absolute
imports followed. Simple import-line census respectively found relative explicit
imports in Requests/Click/Starlette/Pytest of 78/191/0/66 lines, while absolute
`from` imports were 133/207/666/2,448. Wildcard imports were absent except one
Pytest line, so they remain unsupported.

The first ten deterministic CALLS edges from each repository were source
checked (40 total): every call site named the recorded target and every target
definition existed at its canonical file/symbol; verified 40, incorrect 0,
uncertain 0. The sample includes direct local calls, explicit relative imports,
and same-class methods. This is a correctness sample, not a coverage claim.

Live release MCP sessions used the unchanged four tools on Requests
(`HTTPAdapter.build_response -> extract_cookies_to_jar`), Click
(`blur_cmd -> copy_filename`), and Starlette
(`make_payload -> make_json_payload`). Each workflow found, inspected, traced,
and impacted confirmed structure with call-site evidence. Package/absolute
imports and arbitrary receivers were the visible limits. An isolated Codex
read-only run did not load its temporary MCP registration, so it was not used
as acceptance evidence; direct MCP protocol sessions were successful.

Python `impact --diff` tests map both a function-body edit and a method-body
edit to the narrowest common symbol and traverse their confirmed callers. A
release mixed JS/TS+Python fixture indexed 2 files, 4 symbols, and 2 CALLS in
1,641 us cold / 262 us warm with a 1,011-byte single cache.

GitNexus contains 244 `.py` files. Its JS/TS subset remains exactly the
Mission 13 baseline: 2,430 files, 9,376 symbols, 5,914 IMPORTS, 10,009 CALLS,
47,427 unresolved issues, and one ambiguous issue. The combined two-provider
index is 2,674 files, 9,823 symbols, 5,933 IMPORTS, 10,028 CALLS, 47,984
unresolved issues, and one ambiguous issue. A release cold/warm `find analyze`
on a disposable copy measured 4.156 s / 0.305 s with a 17,684,843-byte cache.

## Mission 15 Rust-provider audit

The release binary grew from 11,840,888 to 12,994,648 bytes after adding only
`tree-sitter-rust 0.24.2`. The first query/build and second unchanged query were
run against disposable shallow clones with no dependencies installed. Counts
are provider-neutral graph facts; `Type` is the Rust struct/enum/trait kind.

| repository | pinned commit | `.rs` files | symbols (function/class/method/type) | imports | calls | unresolved | ambiguous | unresolved/symbol | cache bytes | cold | warm |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| [ripgrep](https://github.com/BurntSushi/ripgrep) | `3fce3b5bb0236da2df6d99672afb8a719642eca7` | 110 | 2,498 (327/0/1,784/387) | 103 | 435 | 7,213 | 2 | 2.89 | 2,485,403 | 0.369 s | 0.019 s |
| [fd](https://github.com/sharkdp/fd) | `1765d0817b4e706141115b81c29a5965630e243a` | 24 | 313 (185/0/97/31) | 54 | 96 | 1,921 | 0 | 6.14 | 569,549 | 0.055 s | 0.004 s |
| [serde](https://github.com/serde-rs/serde) | `a874a1b1bb1cc16cf5ee3b1b7b527af5705742bb` | 208 | 1,279 (658/0/208/413) | 114 | 277 | 6,063 | 0 | 4.74 | 1,813,030 | 0.242 s | 0.014 s |
| [mini-redis](https://github.com/tokio-rs/mini-redis) | `3d93b42bc363220f85af4fc9e1bebd35b588a4a3` | 28 | 181 (40/0/109/32) | 34 | 46 | 1,070 | 0 | 5.91 | 296,358 | 0.039 s | 0.003 s |

The leading unresolved categories were deliberately explicit: arbitrary,
associated, macro, or dynamic receiver calls (5,535/1,588/3,825/763 in the
table order); unrooted or external `use` paths (406/169/538/155); then local
name/binding uncertainty. The first ten resolved calls from each repository
were source checked: 40 verified, 0 incorrect, 0 uncertain. They were direct
local/imported calls or same-inherent-type receiver calls.

Live release MCP sessions completed find -> inspect -> trace -> impact against
ripgrep (`build.rs::main -> set_git_revision_hash`), fd
(`Opts.search_paths -> Opts.normalize_path`), and serde
(`serde/build.rs::main -> rustc_minor_version`). The unmodeled receiver and
external-crate portions remained visible in `inspect` instead of becoming
edges. Common `impact --diff` tests map Rust free-function and impl-method body
edits to their narrowest symbols and traverse confirmed callers.

BlastRay self-dogfood indexed 28 source files, 331 symbols, 259 CALLS, and a
725,251-byte cache in 0.405 s cold / 0.007 s warm. `find resolve`, `inspect
src/languages/rust.rs::resolve_file`, `trace resolve -> resolve_file`, and
`impact resolve_file` all exposed the provider's direct implementation path;
associated standard-library calls and arbitrary receivers remained the main
source-reading fallback. On the pinned GitNexus nested source set, the JS/TS
subset stayed exactly 2,430 files, 9,376 symbols, 5,914 IMPORTS, 10,009 CALLS,
47,427 unresolved issues, and one ambiguous issue. A fresh combined index had
2,883 files, 10,281 symbols, 6,070 IMPORTS, 10,064 CALLS, 48,267 unresolved
issues, one ambiguous issue, a 17,954,748-byte cache, and 4.520 s / 0.253 s
cold/warm timing. These are single-machine measurements, not public claims.
