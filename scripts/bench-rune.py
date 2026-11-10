#!/usr/bin/env python3
"""Rune cold-start: fresh process to first GPU-complete offscreen pixels.

Replicates rune-src/scripts/bench-startup.py but wraps the binary in `script -qec` so the
child sees a tty on stdin (otherwise rune-launch's dmenu mode kicks in). Prints the median
and MAD of rune's own `headless_ms` (process start to GPU-readable pixels) and the wall-clock
`process_ms`, across `--runs` executions after one warm-up.

Usage: bench-rune.py --exe path/to/rune-launch [--runs 7]
"""
import argparse
import json
import selectors
import statistics
import subprocess
import time

p = argparse.ArgumentParser()
p.add_argument("--exe", required=True)
p.add_argument("--runs", type=int, default=7)
a = p.parse_args()

rows = []
for i in range(a.runs + 1):
    cmd = ["script", "-qec", f"{a.exe} --first-pixels --frames 1", "/dev/null"]
    start = time.perf_counter()
    proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    try:
        with selectors.DefaultSelector() as selector:
            selector.register(proc.stdout, selectors.EVENT_READ)
            if not selector.select(timeout=30):
                raise TimeoutError("first pixels not ready after 30s")
            line = proc.stdout.readline().decode()
        elapsed = (time.perf_counter() - start) * 1000
        proc.communicate(timeout=30)
        if proc.returncode or not line.startswith("rune-first-pixels "):
            raise RuntimeError(f"bad run: {line!r} rc={proc.returncode}")
        row = json.loads(line.removeprefix("rune-first-pixels "))
        row["process_ms"] = elapsed
        if i:
            rows.append(row)
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait()

keys = [k for k in rows[0] if isinstance(rows[0][k], (int, float))]
result = {}
for k in keys:
    vals = [r[k] for r in rows]
    med = statistics.median(vals)
    result[k] = {"median_ms": round(med, 2), "mad_ms": round(statistics.median(abs(v - med) for v in vals), 2)}
print(json.dumps(result, indent=2))
