#!/usr/bin/env bash
set -euo pipefail
for path in .claude/rules/example.md .claude/skills/example/SKILL.md .claude/agents/example.md; do
  if git check-ignore -q "$path"; then echo "unexpectedly ignored: $path" >&2; exit 1; fi
done
for path in .claude/settings.local.json .claude/worktrees/w/state .claude/logs/run.log; do
  git check-ignore -q "$path" || { echo "runtime file is not ignored: $path" >&2; exit 1; }
done
echo "Claude source/runtime ignore check passed"
