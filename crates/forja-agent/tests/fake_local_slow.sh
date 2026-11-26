#!/usr/bin/env bash
set -euo pipefail

FAKE_LOCAL_SLEEP=3 exec "$(dirname "$0")/fake_local.sh" "$@"
