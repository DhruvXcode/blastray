#!/usr/bin/env bash
# Controlled one-run harness. Baselines and hidden checks live outside task trees.
set -euo pipefail

if [ "$#" -ne 3 ]; then
  echo "usage: $0 <task-id> <bare|blastray|gitnexus> <workspace>" >&2
  exit 64
fi

task=$1
condition=$2
workspace=$3
root=$(cd "$(dirname "$0")/../.." && pwd)
baseline="${M22_BASELINES:?set M22_BASELINES}/$task"
mkdir -p "$workspace"
run="$workspace/$task-$condition"
rm -rf "$run"
cp -a "$baseline" "$run"
# Baselines are sealed, clean repositories whose ignored dependency/build
# caches were prepared before the experiment. Preserve those caches so a
# condition is not measured on package-download availability.

# Each agent gets an isolated Codex home. The copied authentication is never
# retained in this repository; it only makes the three otherwise identical
# CLI invocations independently usable.
agent_home="$workspace/codex-home-$task-$condition"
rm -rf "$agent_home"
mkdir -p "$agent_home/.codex"
cp "$root/misc/agent-benchmark/codex-base.toml" "$agent_home/.codex/config.toml"
cp "${M22_CODEX_AUTH:?set M22_CODEX_AUTH}" "$agent_home/.codex/auth.json"
# Tool packages are installed before timed runs; share only npm's package cache,
# not GitNexus' index/configuration state.
ln -s "${M22_NPM_CACHE:?set M22_NPM_CACHE}" "$agent_home/.npm"

setup_start=$(date +%s%N)
case "$condition" in
  bare) config=()
    ;;
  blastray)
    (cd "$run" && "$M22_BLASTRAY" find '' >/dev/null)
    config=(-c "mcp_servers.blastray.command=\"$M22_BLASTRAY\"" -c 'mcp_servers.blastray.args=["mcp"]')
    ;;
  gitnexus)
    # GitNexus' documented Codex setup installs its MCP, skills, and hooks.
    # Keep that installation and its index state in this condition's home.
    (cd "$run" && HOME="$agent_home" CODEX_HOME="$agent_home/.codex" npx -y "gitnexus@$M22_GITNEXUS_VERSION" setup -c codex >/dev/null)
    (cd "$run" && HOME="$agent_home" CODEX_HOME="$agent_home/.codex" npx -y "gitnexus@$M22_GITNEXUS_VERSION" analyze >/dev/null)
    # The generated repository instructions are normal GitNexus setup state,
    # not agent edits. Make them the ready-to-work baseline.
    git -C "$run" add -A
    git -C "$run" -c user.name='M22 setup' -c user.email='m22@example.invalid' commit --no-verify -m 'benchmark: prepare GitNexus' >/dev/null
    config=()
    ;;
  *) echo "unknown condition: $condition" >&2; exit 64 ;;
esac
setup_ms=$(( ($(date +%s%N) - setup_start) / 1000000 ))

prompt=$(node -e 'const m=require(process.argv[1]); const t=m.tasks.find(x=>x.id===process.argv[2]); console.log(`Implement the requested fix in this repository. Use the available tools as you judge useful. Keep the patch focused and run relevant tests. Do not use network, GitHub, issue/PR lookup, or git history to locate an upstream solution.\n\n${t.prompt}`)' "$root/misc/agent-benchmark/tasks.json" "$task")
started=$(date +%s%N)
HOME="$agent_home" CODEX_HOME="$agent_home/.codex" JAVA_HOME="${M22_JAVA_HOME:?set M22_JAVA_HOME}" PATH="$M22_JAVA_HOME/bin:$PATH" codex exec --ephemeral --json --dangerously-bypass-approvals-and-sandbox \
  -m gpt-5.6-terra -c 'model_reasoning_effort="high"' -C "$run" "${config[@]}" "$prompt" \
  > "$workspace/$task-$condition.jsonl" 2>&1 || true
elapsed_ms=$(( ($(date +%s%N) - started) / 1000000 ))

git -C "$run" diff --no-ext-diff > "$workspace/$task-$condition.patch" || true
python3 "$root/misc/agent-benchmark/summarize.py" "$workspace/$task-$condition.jsonl" \
  "$task" "$condition" "$setup_ms" "$elapsed_ms" "$run" > "$workspace/$task-$condition.summary.json"
