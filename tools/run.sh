#!/usr/bin/env bash
# Build if needed, then run. Extra args go to chatport (e.g. -c config.toml --key ...).
# Usage: tools/run.sh [--release] [chatport args...]
set -euo pipefail
cd "$(dirname "$0")/.."

profile=debug
if [[ ${1:-} == --release ]]; then profile=release; shift; tools/build.sh --release; else tools/build.sh; fi
exec "./target/$profile/chatport" "$@"
