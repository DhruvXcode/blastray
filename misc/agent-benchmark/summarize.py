#!/usr/bin/env python3
"""Derive compact, auditable interaction metrics from Codex JSONL events."""
import json
import pathlib
import subprocess
import sys

events = []
for line in pathlib.Path(sys.argv[1]).read_text(errors="replace").splitlines():
    try:
        events.append(json.loads(line))
    except json.JSONDecodeError:
        pass

commands, mcp_tools, first_edit, usage = [], [], None, None
for event in events:
    if event.get("type") == "turn.completed":
        usage = event.get("usage")
    item = event.get("item", {})
    if item.get("type") == "command_execution" and event.get("type") == "item.started":
        commands.append(item.get("command", ""))
    if item.get("type") in {"mcp_tool_call", "mcp_tool_result"} and event.get("type") == "item.started":
        mcp_tools.append(item.get("server", "mcp"))
    if item.get("type") == "file_change" and first_edit is None:
        first_edit = len(commands) + len(mcp_tools)

def contains(command, words):
    return any(word in command for word in words)

read_actions = sum(contains(c, ("sed ", "cat ", "head ", "tail ", "less ", "awk ")) for c in commands)
search_actions = sum(contains(c, ("rg ", "grep ", "find ", "git grep")) for c in commands)
run = sys.argv[6]
changed = subprocess.run(
    ["git", "-C", run, "diff", "--numstat"], text=True, capture_output=True, check=False
).stdout.splitlines()
print(json.dumps({
    "task": sys.argv[2],
    "condition": sys.argv[3],
    "setup_ms": int(sys.argv[4]),
    "agent_elapsed_ms": int(sys.argv[5]),
    "event_records": len(events),
    "command_actions": len(commands),
    "file_read_actions": read_actions,
    "search_actions": search_actions,
    "mcp_actions": len(mcp_tools),
    "mcp_tools": mcp_tools,
    "actions_before_first_edit": first_edit,
    "major_tool_actions": len(commands) + len(mcp_tools),
    "changed_numstat": changed,
    "token_usage": usage,
}, indent=2, sort_keys=True))
