#!/usr/bin/env bash
set -euo pipefail

FAKE_LOCAL_ECHO_ARGS=1 exec "$(dirname "$0")/fake_local.sh" "$@"
