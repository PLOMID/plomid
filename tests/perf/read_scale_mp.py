#!/usr/bin/env python3
"""PLOMID read-scaling probe with a process-split client.

`read_scale_probe.py` drives load from Python *threads* in one process. When a
read-path probe plateaus well below the core count, the first question is
whether the ceiling is the server or the client: CPython's GIL serializes the
parts of a psycopg2 request that are not inside the C driver call, so a
single-process client can cap out before the server does.

This script runs the identical workload two ways at the same total concurrency:

    threads   -- N threads in one process (same as read_scale_probe.py)
    procs     -- P processes x U threads each, so the GIL is split

If `procs` scales meaningfully past `threads`, the plateau was client-side and
the thread probe must not be reported as a server limit. If both plateau at the
same point, the limit is on the server and reproducible.

Usage:
    python3 tests/perf/read_scale_mp.py --data /tmp/plomid-mp --port 16200
"""

import argparse
import multiprocessing as mp
import os
import shutil
import signal
import socket
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
    print("RESULT " + name + " " + " ".join(f"{k}={v}" for k, v in fields.items()), flush=True)


def wait_ready(host, port, timeout=60.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        with socket.socket() as sock:
            sock.settimeout(0.5)
            if sock.connect_ex((host, port)) == 0:
                return True
        time.sleep(0.2)
    return False


def seed(dsn, size, batch=2000):
    conn = psycopg2.connect(dsn, connect_timeout=60)
    conn.autocommit = True
    with conn.cursor() as cur:
        cur.execute("DROP TABLE IF EXISTS probe_mp")
        cur.execute(
            """
            CREATE TABLE probe_mp (
                id  BIGINT PRIMARY KEY,
                k   INTEGER NOT NULL,
                val INTEGER NOT NULL
            )
            """
        )
    with conn.cursor() as cur:
        for base in range(0, size, batch):
            cur.execute("BEGIN")
            for i in range(base, min(base + batch, size)):
                cur.execute("INSERT INTO probe_mp VALUES (%s,%s,%s)", (i, i % 1000, i))
            cur.execute("COMMIT")
    conn.close()


def cpu_seconds(pid):
    """Cumulative CPU seconds for `pid` from `ps`, or None if unavailable.

    macOS `ps -o time=` prints `MM:SS.ss` or `HH:MM:SS.ss`. Diffing this across
    a level and dividing by wall gives the *average cores* the server consumed
    while that level ran, which is what distinguishes a CPU-bound ceiling from a
    lock-serialized one.
    """
    try:
        out = subprocess.run(
            ["ps", "-o", "time=", "-p", str(pid)],
            capture_output=True, text=True, check=True,
        ).stdout.strip()
    except Exception:
        return None
    if not out:
        return None
    parts = out.split(":")
    try:
        if len(parts) == 3:
            return int(parts[0]) * 3600 + int(parts[1]) * 60 + float(parts[2])
        if len(parts) == 2:
            return int(parts[0]) * 60 + float(parts[1])
    except ValueError:
        return None
    return None


def worker(dsn, wid, ops, size, out, lock):
    lats = []
    try:
        conn = psycopg2.connect(dsn, connect_timeout=60)
        conn.autocommit = True
        with conn.cursor() as cur:
            for i in range(ops):
                key = (wid * 7919 + i * 31) % size
                start = time.perf_counter()
                cur.execute("SELECT id,val FROM probe_mp WHERE id = %s", (key,))
                cur.fetchall()
                lats.append(time.perf_counter() - start)
        conn.close()
    except Exception as exc:  # record, never hide
        lats.append(-1.0)
        print("worker error:", repr(exc), flush=True)
    with lock:
        out.extend(lats)


def run_single_process(dsn, users, ops, size):
    lats, lock = [], threading.Lock()
    threads = [threading.Thread(target=worker, args=(dsn, w, ops, size, lats, lock)) for w in range(users)]
    start = time.perf_counter()
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    return lats, time.perf_counter() - start


def proc_entry(dsn, users, ops, size, out_queue):
    lats, wall = run_single_process(dsn, users, ops, size)
    out_queue.put((lats, wall))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--data", default="target/read-mp")
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=16200)
    ap.add_argument("--rows", type=int, default=100000)
    ap.add_argument("--ops", type=int, default=150)
    ap.add_argument("--totals", default="1,2,4,8,16,32", help="concurrency levels to sweep")
    ap.add_argument("--procs", type=int, default=0,
                    help="processes per level (0 = one process per client, GIL-free)")
    ap.add_argument("--server", default="target/release/plomid-server")
    args = ap.parse_args()

    root = os.path.abspath(args.data)
    shutil.rmtree(root, ignore_errors=True)
    os.makedirs(root, exist_ok=True)
    log = open(os.path.join(root, "server.log"), "w")
    server = subprocess.Popen(
        [
            args.server, "--host", args.host, "--port", str(args.port), "--data", root,
            "--username", "plomid", "--password", "plomid", "--auth", "scram",
            "--log-level", "warn",
        ],
        stdout=log, stderr=subprocess.STDOUT,
    )
    dsn = f"postgresql://plomid:plomid@{args.host}:{args.port}/plomid"
    try:
        if not wait_ready(args.host, args.port):
            raise SystemExit("server did not become ready")
        seed(dsn, args.rows)

        for total in [int(v) for v in args.totals.split(",") if v.strip()]:
            procs = args.procs if args.procs > 0 else total
            per = max(1, total // procs)
            q = mp.Queue()
            children = [
                mp.Process(target=proc_entry, args=(dsn, per, args.ops, args.rows, q))
                for _ in range(procs)
            ]
            cpu_before = cpu_seconds(server.pid)
            for c in children:
                c.start()
            all_lats = []
            # Wall is the longest worker span, measured inside the workers, so
            # fork/import cost is excluded and throughput reflects the load
            # phase only.
            wall = 0.0
            for _ in children:
                lats_i, wall_i = q.get()
                all_lats.extend(lats_i)
                wall = max(wall, wall_i)
            for c in children:
                c.join()
            cpu_after = cpu_seconds(server.pid)
            cores = "n/a"
            if cpu_before is not None and cpu_after is not None and wall > 0:
                cores = f"{(cpu_after - cpu_before) / wall:.2f}"
            ms = [v * 1000 for v in all_lats if v >= 0]
            result(
                "mp_read", users=total, procs=procs, per_proc=per, ops=len(all_lats),
                throughput=f"{len(all_lats) / max(wall, 1e-9):.0f}",
                p50_ms=f"{pct(ms, 50):.3f}", p95_ms=f"{pct(ms, 95):.3f}",
                p99_ms=f"{pct(ms, 99):.3f}", server_cores=cores,
            )
        print("PROBE COMPLETE", flush=True)
    finally:
        server.send_signal(signal.SIGTERM)
        try:
            server.wait(timeout=20)
        except subprocess.TimeoutExpired:
            server.kill()
        log.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
