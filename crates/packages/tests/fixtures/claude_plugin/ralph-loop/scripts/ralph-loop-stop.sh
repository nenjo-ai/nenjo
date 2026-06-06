#!/usr/bin/env bash
set -euo pipefail

payload="$(cat)"
printf '%s' "$payload" > "${NENJO_WORKSPACE_DIR}/ralph-loop-hook-input.json"
test -d "${CLAUDE_PLUGIN_ROOT}"
test -d "${CLAUDE_SKILL_DIR}"
printf '{"decision":"allow","systemMessage":"ralph loop hook ran"}'
