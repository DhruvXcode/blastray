#!/usr/bin/env bash
# Hidden checks run outside an agent's conversation and only after its patch.
set -euo pipefail

if [ "$#" -ne 3 ]; then
  echo "usage: $0 <task-id> <candidate-tree> <result-file>" >&2
  exit 64
fi

task=$1
candidate=$2
result=$3
root=$(cd "$(dirname "$0")/../.." && pwd)
seed=${M22_SEEDS:?set M22_SEEDS}
work=$(mktemp -d "${TMPDIR:-/tmp}/m22-eval.XXXXXX")
trap 'rm -rf "$work"' EXIT
cp -a "$candidate" "$work/tree"
tree="$work/tree"

run() {
  local name=$1
  shift
  if (cd "$tree" && "$@") >"$work/$name.log" 2>&1; then
    printf 'PASS' >"$work/$name.status"
  else
    printf 'FAIL' >"$work/$name.status"
  fi
}

case "$task" in
  js-uuid)
    git -C "$seed/js-uuid" show b1da338:src/test/v1.test.ts >"$tree/src/test/v1.test.ts"
    git -C "$seed/js-uuid" show b1da338:src/test/v6.test.ts >"$tree/src/test/v6.test.ts"
    run hidden npm test
    run existing npm test
    ;;
  go-uuid)
    cp "$root/misc/agent-benchmark/hidden/go_uuid_v6_time_test.go" "$tree/zz_m22_hidden_test.go"
    run hidden go test ./...
    run existing go test ./...
    ;;
  aho-corasick)
    mkdir -p "$tree/tests"
    cp "$root/misc/agent-benchmark/hidden/match_offset_overflow.rs" "$tree/tests/zz_m22_hidden.rs"
    cat >>"$tree/Cargo.toml" <<'EOF'

[[test]]
name = "zz_m22_hidden"
path = "tests/zz_m22_hidden.rs"
EOF
    run hidden cargo test --test zz_m22_hidden
    run existing cargo test --lib
    ;;
  gson)
    cp "$tree/gson/src/test/java/com/google/gson/stream/JsonReaderTest.java" "$work/JsonReaderTest.candidate.java"
    git -C "$seed/gson" show 004e7a49:gson/src/test/java/com/google/gson/stream/JsonReaderTest.java >"$tree/gson/src/test/java/com/google/gson/stream/JsonReaderTest.java"
    # Candidate trees carry warmed build outputs. Force recompilation so Maven
    # cannot run an older class merely because copied source timestamps predate
    # the cached target directory.
    run hidden env JAVA_HOME="${M22_JAVA_HOME:?set M22_JAVA_HOME}" PATH="$M22_JAVA_HOME/bin:$PATH" mvn -q -pl gson clean test -Dtest=JsonReaderTest
    cp "$work/JsonReaderTest.candidate.java" "$tree/gson/src/test/java/com/google/gson/stream/JsonReaderTest.java"
    run existing env JAVA_HOME="$M22_JAVA_HOME" PATH="$M22_JAVA_HOME/bin:$PATH" mvn -q -pl gson clean test -Dtest=JsonReaderTest
    ;;
  *) echo "unknown task: $task" >&2; exit 64 ;;
esac

python3 - "$task" "$work" "$result" <<'PY'
import json, pathlib, sys
task, work, result = sys.argv[1:]
path = pathlib.Path(work)
def outcome(name):
    return (path / (name + '.status')).read_text().strip()
def tail(name):
    return (path / (name + '.log')).read_text(errors='replace')[-2000:]
json.dump({
    'task': task,
    'hidden_regression': outcome('hidden'),
    'existing_tests': outcome('existing'),
    'hidden_log_tail': tail('hidden'),
    'existing_log_tail': tail('existing'),
}, open(result, 'w'), indent=2, sort_keys=True)
PY
