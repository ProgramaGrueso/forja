#!/usr/bin/env bash
# CLI falso para los tests del orquestador (claude, opencode, agy y local).
# Sin red ni agentes reales. Detecta qué agente imita por sus flags y responde
# "ECO[modelo=<m>]" seguido del prompt recibido, en el formato de salida de cada uno:
#   claude (--permission-mode) -> {"result": "..."}
#   agy    (--mode)            -> {"status":"SUCCESS","response":"..."}
#   opencode / local (run ...) -> texto plano
# Si el mensaje del usuario (lo que sigue al último "Mensaje del usuario:") contiene
# líneas "EMIT: <texto>", responde "Delegando." y esas líneas sin el prefijo (así los
# tests simulan un orquestador que emite bloques [[forja:...]]); si no, hace eco.
# Con --effort X añade " ESFUERZO[X]" tras ECO[modelo=...].
# FAKE_ORCH_FAIL=1 -> error en stderr y código 1.
set -euo pipefail

if [[ -n "${FAKE_ORCH_FAIL:-}" ]]; then
  printf '%s\n' "fallo simulado del orquestador" >&2
  exit 1
fi

args=("$@")
model=""
effort=""
prompt=""
flavor="plain"
for ((i = 0; i < ${#args[@]}; i++)); do
  case "${args[$i]}" in
    --model | -m) model="${args[$((i + 1))]:-}" ;;
    --effort) effort="${args[$((i + 1))]:-}" ;;
    -p) prompt="${args[$((i + 1))]:-}" ;;
    --permission-mode) flavor="claude" ;;
    --mode) flavor="agy" ;;
  esac
done
if [[ -z "${prompt}" ]]; then
  prompt="${args[$((${#args[@]} - 1))]}"
fi

text="ECO[modelo=${model}]"
if [[ -n "${effort}" ]]; then
  text="${text} ESFUERZO[${effort}]"
fi
text="${text}"$'\n'"${prompt}"

marker=$'Mensaje del usuario:\n'
if [[ "${prompt}" == *"${marker}"* ]]; then
  last="${prompt##*"${marker}"}"
  emit=""
  while IFS= read -r line; do
    if [[ "${line}" == EMIT:* ]]; then
      emit+="${line#EMIT: }"$'\n'
    fi
  done <<<"${last}"
  if [[ -n "${emit}" ]]; then
    text="Delegando."$'\n'"${emit%$'\n'}"
  fi
fi

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
