#!/usr/bin/env python3
"""Raw wire-protocol timing probe.

Answers "does PLOMID send the response promptly, and does it stream a
multi-statement simple-query batch or buffer it?" using socket-level
timestamps, with no Python DB driver in the way.

For each query it measures:
    query_sent_us, first_response_byte_us, ready_for_query_us, total_us
"""

import socket
import struct
import sys
import time

HOST = "127.0.0.1"
PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 16000
USER = "plomid"
DB = "plomid"


def startup(sock):
    params = [b"user", USER.encode(), b"database", DB.encode(),
              b"client_encoding", b"UTF8", b""]
    body = struct.pack("!i", 196608) + b"".join(params)
    sock.sendall(struct.pack("!i", len(body) + 4) + body)
    # Read until ReadyForQuery ('Z').
    while True:
        tag = recv_exact(sock, 1)
        (length,) = struct.unpack("!i", recv_exact(sock, 4))
        payload = recv_exact(sock, length - 4)
        if tag == b"Z":
            return


def recv_exact(sock, n):
    buf = b""
    while len(buf) < n:
        chunk = sock.recv(n - len(buf))
        if not chunk:
            raise EOFError("connection closed")
        buf += chunk
    return buf


def run_query(sock, sql):
    """Send one simple Query and measure byte-level timings."""
    payload = sql.encode() + b"\0"
    msg = b"Q" + struct.pack("!i", len(payload) + 4) + payload

    t_sent = time.perf_counter()
    sock.sendall(msg)

    first = None
    rowdesc = None
    commands = []
    while True:
        tag = recv_exact(sock, 1)
        (length,) = struct.unpack("!i", recv_exact(sock, 4))
        recv_exact(sock, length - 4)
        now = time.perf_counter()
        if first is None:
            first = now
        if tag == b"T":
            rowdesc = now
        if tag == b"C":
            commands.append((now - t_sent) * 1e6)
        if tag == b"Z":
            done = now
            break

    return {
        "first_response_us": ((first - t_sent) * 1e6) if first else None,
        "first_rowdesc_us": ((rowdesc - t_sent) * 1e6) if rowdesc else None,
        "command_complete_us": commands,
        "ready_for_query_us": (done - t_sent) * 1e6,
        "total_us": (done - t_sent) * 1e6,
    }


def main():
    sock = socket.create_connection((HOST, PORT), timeout=30)
    sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
    startup(sock)

    with socket.create_connection((HOST, PORT), timeout=30) as setup_sock:
        pass
    # Create a scratch table through the same raw socket.
    run_query(sock, "DROP TABLE IF EXISTS wiret")
    run_query(sock, "CREATE TABLE wiret (id BIGINT PRIMARY KEY, v INTEGER)")

    print("== single statement ==")
    for sql in ("SELECT 1", "INSERT INTO wiret VALUES (1, 1)"):
        reps = [run_query(sock, sql) for _ in range(5)]
        r = sorted(reps, key=lambda x: x["total_us"])[2]
        print(
            f"  {sql[:38]:<40} first_byte={r['first_response_us']:8.1f}us "
            f"ready={r['ready_for_query_us']:8.1f}us"
        )

    print("\n== multi-statement batch (20 INSERTs in ONE simple query) ==")
    batch = "; ".join(f"INSERT INTO wiret VALUES ({i}, {i})" for i in range(100, 120))
    r = run_query(sock, batch)
    print(f"  first_response_byte_us = {r['first_response_us']:.1f}")
    print(f"  ready_for_query_us     = {r['ready_for_query_us']:.1f}")
    print(f"  command_complete count = {len(r['command_complete_us'])}")
    if r["command_complete_us"]:
        print(
            "  command_complete timings (us after send): "
            + ", ".join(f"{c:.1f}" for c in r["command_complete_us"][:6])
            + (" ..." if len(r["command_complete_us"]) > 6 else "")
        )
        spread = r["command_complete_us"][-1] - r["command_complete_us"][0]
        print(
            f"  first->last CommandComplete spread = {spread:.1f}us "
            f"({'STREAMED as each statement completed' if spread > 200 else 'BUFFERED: all results emitted at once after the whole batch ran'})"
        )

    print("\n== mixed batch: SELECT then INSERT ==")
    r = run_query(sock, "SELECT 1; INSERT INTO wiret VALUES (500, 500); SELECT 2")
    print(f"  first_response_byte_us = {r['first_response_us']:.1f}")
    print(f"  ready_for_query_us     = {r['ready_for_query_us']:.1f}")
    print(f"  command_complete timings = {[round(c,1) for c in r['command_complete_us']]}")

    run_query(sock, "DROP TABLE wiret")
    sock.close()


if __name__ == "__main__":
    main()
