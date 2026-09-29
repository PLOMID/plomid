#!/bin/bash
# 
# © 2026 PLOMID Technology Solutions
#
# PLOMID
# Platform for Modern Intelligence and Data
#
# Author: Sainath Sapa
# GitHub: https://github.com/sainathsapa
#
# Licensed under the Apache License, Version 2.0;
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     https://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.
# Builds a drag-to-Applications .dmg for macOS from the release binary.
# Usage: make-dmg.sh <version-tag> <arm64|x64>
#
# Layout inside the image:
#   /PLOMID.app           (double-clickable launcher, runs plomid-server)
#   /PLOMID.app/.../MacOS/plomid-server
#   README.txt
#
# Unsigned by default (Gatekeeper: right-click > Open on first launch).
# To sign + notarize, export APPLE_DEVELOPER_ID, APPLE_APP_PASSWORD and
# APPLE_TEAM_ID before running; the script signs when they are present and
# skips otherwise. The .app wrapper keeps the one-file binary installable
# without touching its bytes either way.
set -euo pipefail

VERSION="${1:?usage: make-dmg.sh <version-tag> <arm64|x64>}"
ARCH_LABEL="${2:?usage: make-dmg.sh <version-tag> <arm64|x64>}"
# CFBundleVersion allows dots and digits only: strip any -beta/-rc suffix
# from the tag so prereleases still produce a valid bundle version.
NUM_VERSION="${VERSION#v}"
NUM_VERSION="${NUM_VERSION%%-*}"

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
# shellcheck disable=SC1091
. "$ROOT/dist/branding.sh"
STAGE="$ROOT/dist/stage/plomid"
ARTIFACTS="$ROOT/dist/artifacts"
APP="$STAGE/PLOMID.app"
BIN_SRC="${BIN:-$ROOT/target/release/plomid-server}"

test -x "$BIN_SRC" || { echo "missing $BIN_SRC; run cargo build --release first"; exit 1; }
command -v hdiutil >/dev/null || { echo "hdiutil not found (macOS only)"; exit 1; }

rm -rf "$STAGE" "$ARTIFACTS"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$ARTIFACTS"

cp "$BIN_SRC" "$APP/Contents/MacOS/plomid-server"
chmod 0755 "$APP/Contents/MacOS/plomid-server"
cp "$ROOT/README.md" "$STAGE/README.txt"
cp "$ROOT/assets/plomid-lockup-copper.png" "$STAGE/" 2>/dev/null || true

cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>PLOMID</string>
    <key>CFBundleIdentifier</key><string>in.plomid.server</string>
    <key>CFBundleVersion</key><string>${NUM_VERSION}</string>
    <key>CFBundleShortVersionString</key><string>${NUM_VERSION}</string>
    <key>CFBundleExecutable</key><string>plomid-server</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
</dict>
</plist>
EOF

if [ -n "${APPLE_DEVELOPER_ID:-}" ]; then
    section "signing with $APPLE_DEVELOPER_ID"
    codesign --force --options runtime --timestamp \
        --sign "$APPLE_DEVELOPER_ID" "$APP/Contents/MacOS/plomid-server"
    if [ -n "${APPLE_APP_PASSWORD:-}" ] && [ -n "${APPLE_TEAM_ID:-}" ]; then
        echo "==> notarization requested: submit the .dmg after this script finishes:"
        echo "    xcrun notarytool submit <dmg> --apple-id <id> --password \$APPLE_APP_PASSWORD --team-id \$APPLE_TEAM_ID --wait"
    fi
else
    warn "no APPLE_DEVELOPER_ID: shipping unsigned (right-click > Open on first launch)"
fi
# Flat copy beside the .app for the tarball artifact: taken from the bundle
# so both packages ship the identical (possibly signed) bytes.
cp "$APP/Contents/MacOS/plomid-server" "$STAGE/plomid-server"

# Branded app icon: copper emblem converted to .icns through an iconset
# when the macOS tools exist; skipped silently elsewhere (the installer
# still works, iconless).
if command -v sips >/dev/null && command -v iconutil >/dev/null \
    && [ -f "$ROOT/assets/plomid-emblem-copper.png" ]; then
    ICONSET="$STAGE/PLOMID.iconset"
    rm -rf "$ICONSET" && mkdir -p "$ICONSET"
    if for size in 16 32 128 256 512; do
        sips -z "$size" "$size" "$ROOT/assets/plomid-emblem-copper.png" \
            --out "$ICONSET/icon_${size}x${size}.png" >/dev/null 2>&1 || break
        sips -z "$((size * 2))" "$((size * 2))" "$ROOT/assets/plomid-emblem-copper.png" \
            --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null 2>&1 || break
    done && iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/PLOMID.icns" >/dev/null 2>&1; then
        # Point the bundle at the icon (insert before the closing dict tag).
        python3 - "$APP/Contents/Info.plist" <<'EOF'
import sys
p = sys.argv[1]
s = open(p).read()
s = s.replace("    <key>CFBundlePackageType</key>",
              "    <key>CFBundleIconFile</key><string>PLOMID</string>\n    <key>CFBundlePackageType</key>")
open(p, "w").write(s)
EOF
    fi
    rm -rf "$ICONSET"
fi

DMG="$ARTIFACTS/plomid-${VERSION}-macos-${ARCH_LABEL}.dmg"
rm -f "$DMG"
hdiutil create -volname "PLOMID ${VERSION}" -srcfolder "$STAGE" -ov -format UDZO "$DMG" >/dev/null
ok "artifact: $DMG ($(du -h "$DMG" | cut -f1))"
