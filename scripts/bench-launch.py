#!/usr/bin/env python3
"""Original launch cold-start: fresh process on the live X display until its window maps.

The launcher maps its window once the first frame is drawn, so window-map is the closest
proxy for "launcher visible and responsive". The polling loop (xdotool) adds sub-ms to a
handful of ms of measurement noise, reported here as-is.

Usage: bench-launch.py --exe path/to/launch [--runs 7] [--name launch]
"""
import argparse
import os
import signal
import statistics
import subprocess
import time

p = argparse.ArgumentParser()
p.add_argument("--exe", required=True)
p.add_argument("--runs", type=int, default=7)
p.add_argument("--name", default="launch")
a = p.parse_args()

listing = ["xdotool", "search", "--onlyvisible", "--name", f"^{a.name}$"]


def mapped() -> bool:
    r = subprocess.run(listing, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    return bool(r.stdout.strip())


def find_pid() -> int | None:
    r = subprocess.run(["pgrep", "-f", f"^{a.exe}$"], stdout=subprocess.PIPE)
    out = r.stdout.split()
    # Discard our own shell wrapper; every spawn below is a direct exec.
    return int(out[0]) if out else None


rows = []
for i in range(a.runs + 1):
    start = time.perf_counter()
    proc = subprocess.Popen([a.exe], env={**os.environ, "DISPLAY": os.environ.get("DISPLAY", ":0")})
    try:
        t = time.perf_counter()
        while time.perf_counter() - t < 20:
            if mapped():
                break
            time.sleep(0.001)
        elapsed = (time.perf_counter() - start) * 1000
        if not mapped():
            raise RuntimeError(f"window {a.name!r} never mapped")
        # Give it two frames to settle, then close via the launcher's own escape isn't scriptable,
        # so terminate the process and reap it.
        time.sleep(0.3)
    finally:
        proc.send_signal(signal.SIGTERM)
        try:
            proc.wait(timeout=2)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()
    if i:
        rows.append(elapsed)

med = statistics.median(rows)
mad = statistics.median(abs(v - med) for v in rows)
print(f"window_map_ms median={med:.2f} mad={mad:.2f} runs={rows}")
