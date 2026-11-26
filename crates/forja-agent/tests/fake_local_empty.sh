#!/usr/bin/env bash
set -euo pipefail

FAKE_LOCAL_STDOUT='' exec "$(dirname "$0")/fake_local.sh" "$@"
