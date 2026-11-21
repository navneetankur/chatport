#!/usr/bin/env bash
# Build frontend (only if web/ sources changed since last dist) then the Rust binary.
# Usage: tools/build.sh [--release] [--force]
set -euo pipefail
cd "$(dirname "$0")/.."

release=""; force=0
for a in "$@"; do
  case "$a" in
    --release) release="--release" ;;
    --force) force=1 ;;
    *) echo "unknown arg: $a" >&2; exit 2 ;;
  esac
done

if [[ ! -d web/node_modules || web/package-lock.json -nt web/node_modules ]]; then
  echo "==> npm install"
  (cd web && npm install --no-fund --no-audit)
  touch web/node_modules
fi

stamp=web/dist/index.html
if [[ $force == 1 || ! -f $stamp ]] || \
   [[ -n "$(find web -path web/node_modules -prune -o -path web/dist -prune -o -type f -newer "$stamp" -print -quit)" ]]; then
  echo "==> web build"
  (cd web && npm run build)
else
  echo "==> web up to date"
fi

echo "==> cargo build $release"
cargo build $release
