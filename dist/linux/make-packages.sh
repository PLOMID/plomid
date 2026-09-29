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
# Builds plomid_<version>_{amd64,arm64}.{deb,rpm} with nfpm.
# Usage: make-packages.sh <version-tag>. Expects the x64 server binary at
# dist/artifacts (downloaded by CI from the linux-x64 job); arm64 debs/rpms
# are produced from the same layout when ARCH is overridden.
set -euo pipefail

VERSION_TAG="${1:?usage: make-packages.sh <version-tag>}"
ARCH="${ARCH:-amd64}"

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
# shellcheck disable=SC1091
. "$ROOT/dist/branding.sh"
STAGE="$ROOT/dist/stage/nfpm"
ARTIFACTS="$ROOT/dist/artifacts"

command -v nfpm >/dev/null || { echo "nfpm not found"; exit 1; }

BIN="${BIN:-}"
if [ -z "$BIN" ]; then
  if [ -x "$ARTIFACTS/plomid-server" ]; then
    BIN="$ARTIFACTS/plomid-server"
  else
    # CI downloads the linux tarball instead of a bare binary. The release
    # workflow names them linux-x64, local packaging historically amd64 —
    # accept both so either producer's output is found.
    case "$ARCH" in
      arm64) CANDIDATES="arm64" ;;
      *)     CANDIDATES="x64 amd64" ;;
    esac
    TARBALL=""
    for c in $CANDIDATES; do
      TARBALL="$(ls "$ARTIFACTS"/plomid-*-linux-"$c".tar.gz 2>/dev/null | head -1 || true)"
      [ -n "$TARBALL" ] && break
    done
    test -n "$TARBALL" || { echo "no linux binary or tarball in $ARTIFACTS (BIN=... to override)"; exit 1; }
    rm -rf "$STAGE/untar" && mkdir -p "$STAGE/untar"
    tar -xzf "$TARBALL" -C "$STAGE/untar"
    BIN="$STAGE/untar/plomid-server"
  fi
fi

rm -rf "$STAGE/files" && mkdir -p "$STAGE/files" "$ARTIFACTS"
cp "$BIN" "$STAGE/files/plomid-server"
cp "$ROOT/dist/linux/plomid.service" "$STAGE/files/plomid.service"
cp "$ROOT/dist/linux/plomid.desktop" "$STAGE/files/plomid.desktop"
cp "$ROOT/assets/plomid-emblem-copper.png" "$STAGE/files/plomid-emblem-copper.png"
cp "$ROOT/README.md" "$STAGE/files/README.txt"

cat > "$STAGE/files/postinstall.sh" <<'EOF'
#!/bin/sh
set -e
if ! id plomid >/dev/null 2>&1; then
    adduser --system --group --home /var/lib/plomid --no-create-home plomid 2>/dev/null || \
    useradd --system --gid nogroup --home-dir /var/lib/plomid --shell /usr/sbin/nologin plomid
fi
install -d -o plomid -g plomid -m 0750 /var/lib/plomid/data
if command -v systemctl >/dev/null 2>&1; then
    systemctl daemon-reload || true
fi
EOF

cat > "$STAGE/files/preremove.sh" <<'EOF'
#!/bin/sh
set -e
if command -v systemctl >/dev/null 2>&1; then
    systemctl stop plomid.service || true
    systemctl disable plomid.service || true
fi
EOF
chmod 0755 "$STAGE/files/postinstall.sh" "$STAGE/files/preremove.sh"

chmod 0755 "$STAGE/files/postinstall.sh" "$STAGE/files/preremove.sh"

# Render nfpm.yaml from the template (nfpm does not expand variables
# itself in all versions; sed keeps this working everywhere).
sed -e "s|@@VERSION@@|${VERSION_TAG#v}|g" \
    -e "s|@@ARCH@@|${ARCH}|g" \
    -e "s|@@STAGE@@|${STAGE}/files|g" \
    "$ROOT/dist/linux/nfpm.yaml" > "$STAGE/nfpm.gen.yaml"
(
  cd "$ROOT/dist/linux"
  nfpm package --config "$STAGE/nfpm.gen.yaml" --packager deb --target "$ARTIFACTS/"
  nfpm package --config "$STAGE/nfpm.gen.yaml" --packager rpm --target "$ARTIFACTS/"
)
ok "packages:"; ls -l "$ARTIFACTS" | grep -E 'deb|rpm' || true
