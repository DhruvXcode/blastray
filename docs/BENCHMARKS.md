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
