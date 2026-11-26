#!/usr/bin/env bash
set -euo pipefail

# Script falso para pruebas: nunca llama a OpenCode ni toca la red.
# El modo se controla con archivos marcadores en el directorio de trabajo
# (o con variables de entorno equivalentes).

if [[ -f ./fake_opencode_record ]]; then
  printf '%s\n' "$PWD" > ./fake_opencode_cwd
  : > ./fake_opencode_args
  for arg in "$@"; do
    printf '%s\0' "$arg" >> ./fake_opencode_args
  done
fi

# Modo "sin variantes": falla como opencode real si el modelo lleva `#variante`.
if [[ -f ./fake_opencode_no_variants || -n "${FAKE_OPENCODE_NO_VARIANTS:-}" ]]; then
  for arg in "$@"; do
    if [[ "$arg" == */*#* ]]; then
      printf 'Error: Variant unavailable for %s: %s\n' "${arg%%#*}" "${arg#*#}" >&2
      exit 1
    fi
  done
fi

sleep_for="${FAKE_OPENCODE_SLEEP:-}"
if [[ -z "${sleep_for}" && -f ./fake_opencode_sleep ]]; then
  sleep_for="$(cat ./fake_opencode_sleep)"
fi
if [[ -n "${sleep_for}" ]]; then
  sleep "${sleep_for}"
fi

stderr_msg="${FAKE_OPENCODE_STDERR:-}"
if [[ -z "${stderr_msg}" && -f ./fake_opencode_stderr ]]; then
  stderr_msg="$(cat ./fake_opencode_stderr)"
fi
if [[ -n "${stderr_msg}" ]]; then
  printf '%s\n' "${stderr_msg}" >&2
fi

if [[ -n "${FAKE_OPENCODE_STDOUT+x}" ]]; then
  printf '%s\n' "${FAKE_OPENCODE_STDOUT}"
elif [[ -f ./fake_opencode_stdout ]]; then
  cat ./fake_opencode_stdout
else
  printf '%s\n' 'trabajo listo desde fake_opencode'
fi

exit_code="${FAKE_OPENCODE_EXIT:-}"
if [[ -z "${exit_code}" && -f ./fake_opencode_exit ]]; then
  exit_code="$(cat ./fake_opencode_exit)"
fi

exit "${exit_code:-0}"
