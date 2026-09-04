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
