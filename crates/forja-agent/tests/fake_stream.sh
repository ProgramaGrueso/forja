#!/usr/bin/env bash
# Imprime FAKE_STREAM_LINES líneas (por defecto 3) con una pausa corta entre ellas.
# FAKE_STREAM_TAG prefija cada línea; FAKE_STREAM_SLEEP es la pausa en segundos.
set -euo pipefail

n="${FAKE_STREAM_LINES:-3}"
tag="${FAKE_STREAM_TAG:-linea}"
pause="${FAKE_STREAM_SLEEP:-0.05}"

for ((i = 1; i <= n; i++)); do
  printf '%s %d\n' "${tag}" "${i}"
  sleep "${pause}"
done

if [[ -n "${FAKE_STREAM_STDERR:-}" ]]; then
  printf '%s\n' "${FAKE_STREAM_STDERR}" >&2
fi
if [[ -n "${FAKE_STREAM_SECRET:-}" ]]; then
  printf 'clave %s\n' "${FAKE_STREAM_SECRET}"
fi
