#!/usr/bin/env bash
# Build a self-contained mind67.app.
#
#   ./scripts/build-app.sh             build dist/mind67.app
#   ./scripts/build-app.sh --install   ...and put it in /Applications
#   ./scripts/build-app.sh --dev       build dist/mind67Dev.app instead
#
# The bundle carries the release binary, the icon and (via `include_bytes!`)
# the font, so it runs from anywhere — /Applications, another Mac, a stick.
# Nothing in it points back at this checkout.
#
# --dev builds the same binary under a different name, identity and icon. The
# executable ends in `-dev`, which is how the app knows to keep its maps,
# recents and keymap in `jotmind-dev` (see src/dev.rs). It is never installed:
# the whole point is that the copy holding real maps is not the copy being
# restarted every few minutes. Use ./dev.sh to build and launch one.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/target/release/mind67"
VERSION="$(awk -F'"' '/^version = /{print $2; exit}' "$ROOT/Cargo.toml")"

install=false
dev=false
for a in "$@"; do
  case "$a" in
    --install) install=true ;;
    --dev) dev=true ;;
    *)
      echo "usage: $(basename "$0") [--install] [--dev]" >&2
      exit 2
      ;;
  esac
done

# The bundle's identity. Everything that differs between the real app and the
# development one is these four lines; the rest of the script builds whichever
# it was handed.
if $dev; then
  NAME="mind67Dev"
  DISPLAY="mind67 Dev"
  EXE="mind67-dev"
  BUNDLE_ID="com.igorrrr.JotMind.dev"
  ICON="AppIconDev"
else
  NAME="mind67"
  DISPLAY="mind67"
  EXE="mind67"
  BUNDLE_ID="com.igorrrr.JotMind"
  ICON="AppIcon"
fi
APP="$ROOT/dist/$NAME.app"

# Installing a dev build into /Applications would put it exactly where the app
# holding real maps lives, under a name one letter away from it. Refuse.
if $dev && $install; then
  echo "build-app.sh: --dev is a local build; it is never installed" >&2
  exit 2
fi

cargo build --release --manifest-path "$ROOT/Cargo.toml"

# The dev icon is the master icon with a DEV ribbon stamped on it. Committed
# like AppIcon.icns, so a dev build does not need Pillow on the machine.
if $dev && [[ ! -f "$ROOT/assets/icon/$ICON.icns" ||
  "$ROOT/assets/icon/AppIcon-1024.png" -nt "$ROOT/assets/icon/$ICON.icns" ]]; then
  echo "rebuilding $ICON.icns…"
  "$ROOT/scripts/build-dev-icns.sh" >/dev/null
fi

# Assembled from scratch every time. A stale file left behind invalidates the
# signature, and that shows up only as a launch that dies without a window.
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

cp "$BIN" "$APP/Contents/MacOS/$EXE"
chmod +x "$APP/Contents/MacOS/$EXE"
cp "$ROOT/assets/icon/$ICON.icns" "$APP/Contents/Resources/$ICON.icns"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key>
  <string>en</string>
  <key>CFBundleExecutable</key>
  <string>$EXE</string>
  <key>CFBundleIconFile</key>
  <string>$ICON</string>
  <key>CFBundleIdentifier</key>
  <string>$BUNDLE_ID</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>CFBundleName</key>
  <string>$DISPLAY</string>
  <key>CFBundleDisplayName</key>
  <string>$DISPLAY</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>$VERSION</string>
  <key>CFBundleVersion</key>
  <string>$VERSION</string>
  <key>LSMinimumSystemVersion</key>
  <string>13.0</string>
  <key>NSHighResolutionCapable</key>
  <true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key>
  <true/>
</dict>
</plist>
PLIST

printf 'APPL????' > "$APP/Contents/PkgInfo"

# Ad-hoc signature: on Apple silicon an unsigned binary is killed on launch,
# and copying the bundle anywhere re-checks it. Not notarization — this is a
# locally built app, so Gatekeeper never sees a download to quarantine.
codesign --force --sign - "$APP"
codesign --verify --strict "$APP"

echo "Built $APP  (v$VERSION)"

if $install; then
  DEST="/Applications/$NAME.app"
  # ditto, not cp: it carries the signature and extended attributes across.
  rm -rf "$DEST"
  ditto "$APP" "$DEST"
  echo "Installed $DEST"
elif $dev; then
  echo "Data in ~/Library/Application Support/jotmind-dev — real maps untouched."
else
  echo "Drag it to /Applications, or re-run with --install."
fi
