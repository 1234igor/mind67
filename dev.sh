#!/usr/bin/env bash
# Build and launch the development copy of mind67.
#
#   ./dev.sh          build dist/mind67Dev.app and launch it
#   ./dev.sh --wipe   ...after throwing the dev data away first
#   ./dev.sh --demo   the scratch demo map, as run.sh takes it
#
# This is the copy to use while working on the app. It carries a DEV ribbon on
# its icon, says "mind67 Dev" in its title bar, and reads and writes only
# ~/Library/Application Support/jotmind-dev — so editing, deleting or
# corrupting its maps cannot touch a real one.
#
# The real app is built and installed with ./scripts/build-app.sh --install. Do
# that when shipping a finished change, not while trying one out.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
DATA="$HOME/Library/Application Support/jotmind-dev"
APP="$ROOT/dist/mind67Dev.app"

wipe=0
args=()
for arg in "$@"; do
  case "$arg" in
    --wipe) wipe=1 ;;
    *) args+=("$arg") ;;
  esac
done

"$ROOT/scripts/build-app.sh" --dev

if [[ "$wipe" -eq 1 ]]; then
  echo "jotmind-dev: wiping $DATA"
  rm -rf "$DATA"
fi

# `open -n` starts a fresh copy every time, so without this the Dock fills up
# with dev instances over a working session. Match the bundle path: the
# executable is `mind67-dev`.
pkill -f "mind67Dev.app" 2>/dev/null || true
sleep 1

echo "jotmind-dev: launching…"
exec open -n -a "$APP" --args "${args[@]+"${args[@]}"}"
