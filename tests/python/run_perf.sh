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
# Reproducible PLOMID benchmark runner.
#
# Boots a fresh PLOMID server against a scratch data directory, waits for the
# port to accept connections, runs a python benchmark harness against it, and
# shuts the server down.  Keeping boot and benchmark in one shell keeps the
# server inside the caller's process group, so CI shells always clean up.
#
# Usage:
#   tests/python/run_perf.sh [benchmark.py ...] [-- extra env/args]
#
# Environment:
#   PLOMID_BIN   server binary               (default target/release/plomid-server)
#   PLOMID_DIR   scratch root                (default target/bench-plomid)
#   PLOMID_PORT  port                        (default 5432)
#   KEEP_DATA=1  reuse an existing data dir  (default: wipe for a clean run)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BIN="${PLOMID_BIN:-$ROOT/target/release/plomid-server}"
SCRATCH="${PLOMID_DIR:-$ROOT/target/bench-plomid}"
PORT="${PLOMID_PORT:-5432}"
DATA="$SCRATCH/data"
LOG="$SCRATCH/server.log"

mkdir -p "$SCRATCH"
if [[ "${KEEP_DATA:-0}" != "1" ]]; then
    rm -rf "$DATA"
fi
mkdir -p "$DATA"

"$BIN" --data "$DATA" --host 127.0.0.1 --port "$PORT" \
    --username plomid --password plomid --log-level "${PLOMID_LOG_LEVEL:-warn}" \
    >"$LOG" 2>&1 &
SERVER=$!
trap 'kill "$SERVER" 2>/dev/null || true; wait "$SERVER" 2>/dev/null || true' EXIT

# Wait for the listener instead of sleeping a fixed amount: startup includes
# WAL recovery, which grows with the retained WAL.
for _ in $(seq 1 300); do
    if nc -z 127.0.0.1 "$PORT" 2>/dev/null; then
        break
    fi
    if ! kill -0 "$SERVER" 2>/dev/null; then
        echo "plomid-server exited during startup" >&2
        tail -30 "$LOG" >&2
        exit 1
    fi
    sleep 0.1
done

export DB_DSN="${DB_DSN:-postgresql://plomid:plomid@127.0.0.1:$PORT/plomid}"
for script in "$@"; do
    echo "=== $script  ($DB_DSN) ==="
    (cd "$ROOT/tests/python" && python3 "$script")
done
