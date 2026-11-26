#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${FAKE_AGY_SLEEP:-}" ]]; then
  sleep "${FAKE_AGY_SLEEP}"
fi

if [[ -n "${FAKE_AGY_STDERR:-}" ]]; then
  printf '%s\n' "${FAKE_AGY_STDERR}" >&2
fi

if [[ -z "${FAKE_AGY_STDOUT+x}" ]]; then
  stdout_msg='{"status":"SUCCESS","response":"ok desde fake_agy"}'
else
  stdout_msg="${FAKE_AGY_STDOUT}"
fi

printf '%s\n' "${stdout_msg}"

exit "${FAKE_AGY_EXIT:-0}"
