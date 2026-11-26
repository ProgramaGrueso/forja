#!/usr/bin/env bash
set -euo pipefail

FAKE_LOCAL_EXIT=7 FAKE_LOCAL_STDERR='fallo simulado local' \
  exec "$(dirname "$0")/fake_local.sh" "$@"
