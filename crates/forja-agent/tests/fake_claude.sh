#!/usr/bin/env bash
# CLI falso de Claude Code para los tests de forja-agent.
# Nunca llama a la red ni al claude real: el modo se elige con el archivo
# ./fake_claude_mode.txt dentro del worktree (cwd del proceso).
set -euo pipefail

mode=""
if [[ -f "./fake_claude_mode.txt" ]]; then
  mode="$(cat ./fake_claude_mode.txt)"
fi

case "${mode}" in
  sleep)
    sleep 3
    ;;
  exit1)
    printf '%s\n' "fallo simulado de claude" >&2
    exit 1
    ;;
  empty)
    printf '\n'
    exit 0
    ;;
  broken-json)
    printf '%s\n' '{roto'
    exit 0
    ;;
esac

if [[ -n "${FAKE_CLAUDE_STDERR:-}" ]]; then
  printf '%s\n' "${FAKE_CLAUDE_STDERR}" >&2
fi

printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"num_turns":1,"result":"ok desde fake_claude"}'

exit "${FAKE_CLAUDE_EXIT:-0}"
