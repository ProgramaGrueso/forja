#!/usr/bin/env bash
# CLI falso para los tests del orquestador (claude, opencode, agy y local).
# Sin red ni agentes reales. Detecta qué agente imita por sus flags y responde
# "ECO[modelo=<m>]" seguido del prompt recibido, en el formato de salida de cada uno:
#   claude (--permission-mode) -> {"result": "..."}
#   agy    (--mode)            -> {"status":"SUCCESS","response":"..."}
#   opencode / local (run ...) -> texto plano
# FAKE_ORCH_FAIL=1 -> error en stderr y código 1.
set -euo pipefail

if [[ -n "${FAKE_ORCH_FAIL:-}" ]]; then
  printf '%s\n' "fallo simulado del orquestador" >&2
  exit 1
fi

args=("$@")
model=""
prompt=""
flavor="plain"
for ((i = 0; i < ${#args[@]}; i++)); do
  case "${args[$i]}" in
    --model | -m) model="${args[$((i + 1))]:-}" ;;
    -p) prompt="${args[$((i + 1))]:-}" ;;
    --permission-mode) flavor="claude" ;;
    --mode) flavor="agy" ;;
  esac
done
if [[ -z "${prompt}" ]]; then
  prompt="${args[$((${#args[@]} - 1))]}"
fi

text="ECO[modelo=${model}]"$'\n'"${prompt}"

esc() {
  local s="$1"
  s="${s//\\/\\\\}"
  s="${s//\"/\\\"}"
  s="${s//$'\n'/\\n}"
  s="${s//$'\t'/\\t}"
  printf '%s' "$s"
}

case "${flavor}" in
  claude) printf '{"result":"%s"}\n' "$(esc "${text}")" ;;
  agy) printf '{"status":"SUCCESS","response":"%s"}\n' "$(esc "${text}")" ;;
  *) printf '%s\n' "${text}" ;;
esac
