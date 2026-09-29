#!/usr/bin/env bash
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
set -euo pipefail

# Start the PLOMID server.
# Usage: ./start.sh [data_dir] [host] [port] [user] [password]
#
# The host defaults to 0.0.0.0 so the server is reachable from other machines
# and from containers. PLOMID requires an explicit opt-in for that (see
# --allow-remote-plaintext below) because remote clients send credentials
# unencrypted until TLS lands.

DATA_DIR="${PLOMID_DATA_DIR:-${1:-./data}}"
HOST="${PLOMID_HOST:-${2:-0.0.0.0}}"
PORT="${PLOMID_PORT:-${3:-5432}}"
USER="${PLOMID_USER:-${4:-plomid}}"
PASSWORD="${PLOMID_PASSWORD:-${5:-secret}}"
LOG_LEVEL="${PLOMID_LOG_LEVEL:-info}"

echo "Starting PLOMID server..."
echo "  Data directory: $DATA_DIR"
echo "  Host: $HOST"
echo "  Port: $PORT"
echo "  User: $USER"

mkdir -p "$DATA_DIR"

# A loopback listener needs nothing extra; anything else needs the opt-in,
# which the server refuses to assume on the operator's behalf.
LISTEN_ARGS=()
case "$HOST" in
    127.*|::1|localhost)
        ;;
    *)
        LISTEN_ARGS+=(--allow-remote-plaintext)
        echo "  WARNING: binding $HOST without TLS; clients on this network send" \
             "credentials in the clear. Use only on a trusted network." >&2
        ;;
esac

exec cargo run --bin plomid-server -- \
    --data "$DATA_DIR" \
    --host "$HOST" \
    --port "$PORT" \
    --username "$USER" \
    --password "$PASSWORD" \
    --log-level "$LOG_LEVEL" \
    "${LISTEN_ARGS[@]}"
