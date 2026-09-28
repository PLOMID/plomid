#!/usr/bin/env python3
"""PLOMID read-scaling probe: does read throughput grow with concurrent users?

Boots a fresh server on a scratch directory, seeds a table over PGWire, then
runs point and range reads at increasing concurrency. Every printed number is
measured against that live server; nothing is estimated.

Usage:
    python3 tests/perf/read_scale_probe.py --data /tmp/plomid-probe

The point of the probe is the *shape* of the curve, not any single number: a
read path that serialises on a global lock shows flat throughput (or a
throughput drop from lock hand-off) as users increase, while a read path with
concurrent readers scales until a real hardware limit.
"""

import argparse
import os
import shutil
import signal
import socket
import statistics
import subprocess
import sys
import threading
import time

import psycopg2


def pct(values, p):
    if not values:
        return 0.0
    ordered = sorted(values)
    pos = (len(ordered) - 1) * (p / 100.0)
    lo = int(pos)
    hi = min(lo + 1, len(ordered) - 1)
    return ordered[lo] + (ordered[hi] - ordered[lo]) * (pos - lo)


def result(name, **fields):
    print(
        "RESULT " + name + " " + " ".join(f"{k}={v}" for k, v in fields.items()),
        flush=True,
    )


def wait_ready(host, port, timeout=60.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        with socket.socket() as sock:
            sock.settimeout(0.5)
            if sock.connect_ex((host, port)) == 0:
                return True
        time.sleep(0.2)
    return False


def connect(dsn):
    return psycopg2.connect(dsn, connect_timeout=60)


def seed(dsn, size, batch=2000):
    conn = connect(dsn)
    conn.autocommit = True
    with conn.cursor() as cur:
        cur.execute("DROP TABLE IF EXISTS probe_read")
        cur.execute(
            """
            CREATE TABLE probe_read (
                id      BIGINT PRIMARY KEY,
                k       INTEGER NOT NULL,
                val     INTEGER NOT NULL,
                tex     VARCHAR(24)
            )
            """
        )
        cur.execute("CREATE INDEX probe_read_k_idx ON probe_read (k)")
    start = time.perf_counter()
    with conn.cursor() as cur:
        for base in range(0, size, batch):
            cur.execute("BEGIN")
            for i in range(base, min(base + batch, size)):
                cur.execute(
                    "INSERT INTO probe_read VALUES (%s,%s,%s,%s)",
                    (i, i % 1000, i * 3 % 100000, f"t{i % 50}"),
                )
            cur.execute("COMMIT")
    elapsed = time.perf_counter() - start
    conn.close()
    result("seed", rows=size, seconds=f"{elapsed:.2f}", rows_per_sec=f"{size / elapsed:.0f}")
    return size


def run_level(dsn, mode, users, ops, size):
    """One concurrency level; returns (throughput, latency percentiles)."""
    lats = []
    errors = []
    lock = threading.Lock()

    def worker(wid):
        try:
            conn = connect(dsn)
            conn.autocommit = True
            local = []
            with conn.cursor() as cur:
                # Prepared on the first use, then re-executed: this is the
                # extended-protocol path, so the measurement includes bind +
                # execute and not just parse.
                for i in range(ops):
                    key = (wid * 7919 + i * 31) % size
                    start = time.perf_counter()
                    if mode == "point":
                        cur.execute(
                            "SELECT id,val FROM probe_read WHERE id = %s", (key,)
                        )
                    elif mode == "range":
                        lo = (i * 13) % (size - 200)
                        cur.execute(
                            "SELECT id,val FROM probe_read WHERE id BETWEEN %s AND %s",
                            (lo, lo + 100),
                        )
                    else:  # indexed range
                        cur.execute(
                            "SELECT id,val FROM probe_read WHERE k BETWEEN %s AND %s",
                            (100, 110),
                        )
                    cur.fetchall()
                    local.append(time.perf_counter() - start)
            conn.close()
            with lock:
                lats.extend(local)
        except Exception as exc:  # record, never hide
            with lock:
                errors.append(repr(exc))

    threads = [threading.Thread(target=worker, args=(w,)) for w in range(users)]
    start = time.perf_counter()
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    wall = time.perf_counter() - start
    total = users * ops
    ms = [v * 1000 for v in lats]
    result(
        f"read_scale_{mode}",
        users=users,
        ops=total,
        wall_s=f"{wall:.3f}",
        throughput=f"{total / wall:.0f}",
        p50_ms=f"{pct(ms, 50):.3f}",
        p95_ms=f"{pct(ms, 95):.3f}",
        p99_ms=f"{pct(ms, 99):.3f}",
        max_ms=f"{max(ms) if ms else 0:.3f}",
        errors=len(errors),
    )
    return total / wall


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--data", default="target/read-probe")
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=16111)
    ap.add_argument("--rows", type=int, default=100000)
    ap.add_argument("--ops", type=int, default=120)
    ap.add_argument("--users", default="1,2,4,8,16,32")
    ap.add_argument("--modes", default="point,range")
    ap.add_argument("--server", default="target/release/plomid-server")
    ap.add_argument("--keep", action="store_true")
    args = ap.parse_args()

    root = os.path.abspath(args.data)
    shutil.rmtree(root, ignore_errors=True)
    os.makedirs(root, exist_ok=True)

    log = open(os.path.join(root, "server.log"), "w")
    server = subprocess.Popen(
        [
            args.server,
            "--host", args.host,
            "--port", str(args.port),
            "--data", root,
            "--username", "plomid",
            "--password", "plomid",
            "--auth", "scram",
            "--log-level", "warn",
        ],
        stdout=log,
        stderr=subprocess.STDOUT,
    )
    dsn = f"postgresql://plomid:plomid@{args.host}:{args.port}/plomid"
    try:
        if not wait_ready(args.host, args.port):
            raise SystemExit("server did not become ready")
        # Readiness is a completed query, not just an open socket.
        for _ in range(60):
            try:
                conn = connect(dsn)
                conn.close()
                break
            except Exception:
                time.sleep(0.5)
        else:
            raise SystemExit("server never accepted a query")
        result("server_up", pid=server.pid)

        size = seed(dsn, args.rows)
        for mode in args.modes.split(","):
            for users in [int(u) for u in args.users.split(",") if u.strip()]:
                run_level(dsn, mode, users, args.ops, size)
        print("PROBE COMPLETE", flush=True)
    finally:
        server.send_signal(signal.SIGTERM)
        try:
            server.wait(timeout=20)
        except subprocess.TimeoutExpired:
            server.kill()
        log.close()
        if not args.keep:
            pass  # keep the directory: storage forensics reads it afterwards
    return 0


if __name__ == "__main__":
    sys.exit(main())
