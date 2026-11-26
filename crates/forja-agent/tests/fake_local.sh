#!/usr/bin/env bash
# Programa falso que imita a `opencode run` para los tests de LocalAgent.
# Nunca llama a un modelo real ni a la red.
set -euo pipefail

if [[ -n "${FAKE_LOCAL_SLEEP:-}" ]]; then
  sleep "${FAKE_LOCAL_SLEEP}"
fi

if [[ -n "${FAKE_LOCAL_STDERR:-}" ]]; then
  printf '%s\n' "${FAKE_LOCAL_STDERR}" >&2
fi

if [[ "${FAKE_LOCAL_ECHO_ARGS:-0}" == "1" ]]; then
  printf '%s\n' "$@"
else
  printf '%s\n' "${FAKE_LOCAL_STDOUT-ok desde fake_local}"
fi

exit "${FAKE_LOCAL_EXIT:-0}"
