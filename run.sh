#!/usr/bin/env bash
# Build and launch mind67 (macOS app bundle with Dock icon).
#
#   ./run.sh              your saved map (autosaves)
#   ./run.sh --demo       scratch demo map (never written to disk)
#   ./run.sh --empty      scratch empty map (never written to disk)
#   ./run.sh --smoke      open, wait ~1.8s, quit (harness check)
#
# The bundle it launches is the same self-contained one you would ship; see
# scripts/build-app.sh, and `--install` there to put it in /Applications.
set -euo pipefail

cd "$(dirname "$0")"
ROOT="$(pwd)"
APP="$ROOT/dist/mind67.app"

ARGS=("$@")

for a in "${ARGS[@]+"${ARGS[@]}"}"; do
  if [[ "$a" == "--smoke" ]]; then
    exec cargo run --release -- "${ARGS[@]}"
  fi
done

./scripts/build-app.sh >/dev/null

open -n -a "$APP" --args "${ARGS[@]+"${ARGS[@]}"}"
