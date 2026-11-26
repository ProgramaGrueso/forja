#!/usr/bin/env bash
# Falso `hermes` para los tests de forja-agent. Imita la forma
#   hermes -p <perfil> chat --query-file <archivo> -Q --oneshot
# sin llamar a la red ni a ningún LLM: el comportamiento se dirige por el
# contenido del propio prompt (que llega íntegro por el archivo temporal):
#   FAKE:fail     -> imprime un error en stderr y sale con código 1
#   FAKE:timeout  -> duerme 30 s (el test usa un timeout corto)
#   FAKE:empty    -> stdout vacío y código 0
#   cualquier otra cosa -> responde con el contenido literal del archivo
# Además informa en stderr del archivo de prompt que vio (para que el test
# compruebe que Forja lo borra al terminar) y del directorio de trabajo.

set -euo pipefail

# Forma esperada del argv (la arma build_command de hermes.rs):
#   $0 -p <perfil> chat --query-file <archivo> -Q --oneshot
if [[ "${1:-}" != "-p" || "${3:-}" != "chat" || "${4:-}" != "--query-file" \
      || "${6:-}" != "-Q" || "${7:-}" != "--oneshot" ]]; then
  printf 'argv inesperado para fake_hermes: %s\n' "$*" >&2
  exit 64
fi

query_file="${5:-}"
if [[ ! -f "${query_file}" ]]; then
  printf 'fake_hermes: no existe el archivo de prompt: %s\n' "${query_file}" >&2
  exit 65
fi

# El test usa estas líneas para verificar cwd y limpieza del archivo temporal.
printf 'query_file: %s\n' "${query_file}" >&2
printf 'cwd: %s\n' "$(pwd)" >&2

content="$(cat "${query_file}")"

case "${content}" in
  FAKE:fail*)
    printf 'fallo simulado desde fake_hermes\n' >&2
    exit 1
    ;;
  FAKE:timeout*)
    sleep 30
    exit 0
    ;;
  FAKE:empty*)
    printf 'session_id: fake-hermes-session\n' >&2
    exit 0
    ;;
  *)
    # Como el hermes real en modo -Q: la respuesta final va a stdout.
    printf '%s\n' "${content}"
    printf 'session_id: fake-hermes-session\n' >&2
    exit 0
    ;;
esac
