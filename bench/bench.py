#!/usr/bin/env python3
"""Benchmark matrix for typed pipes.

    cargo tools && cargo build --release && bench/bench.py [--lines 2000000] [--rows 1000000] [--repeats 3]

Runs every scenario `repeats` times, interleaved (so drift on a noisy machine
hits all scenarios alike), after one warm-up round. Reports the median and
best wall time, throughput, peak RSS, and for `wp` runs the host
allocations, `realloc` calls into components, bytes staged by the host pump
and the time the pump spent copying (blank where Wasmtime copies directly).
Every implementation of the same query must print byte-identical output.

Writes bench/results/latest.md and bench/results/latest.json.
"""

import argparse
import datetime
import json
import os
import pathlib
import platform
import statistics
import subprocess
import sys
import time

ROOT = pathlib.Path(__file__).resolve().parent.parent
BIN = ROOT / "target" / "release"
WP = str(BIN / "wp")
DATA = ROOT / "bench" / "data"
RESULTS = ROOT / "bench" / "results"


def run(cmd, shell=False):
    """Run one command; return (wall seconds, stdout, stderr)."""
    start = time.perf_counter()
    p = subprocess.run(
        cmd, shell=shell, cwd=ROOT, capture_output=True,
        env={**os.environ, "LC_ALL": "C", "RUST_BACKTRACE": "0"},
    )
    wall = time.perf_counter() - start
    if p.returncode != 0:
        raise RuntimeError(f"command failed ({p.returncode}): {cmd}\n{p.stderr.decode(errors='replace')}")
    return wall, p.stdout, p.stderr.decode(errors="replace")


def wp_stats(stderr):
    """The JSON line `wp --json` prints last on stderr."""
    for line in reversed(stderr.strip().splitlines()):
        if line.startswith("{"):
            return json.loads(line)
    return None


def scenarios(log, rows):
    """(group, name, command, shell?, check-group) for every run."""
    q1 = f"cat {log} | parse-log | errors | top-paths 10"
    q1c = f"cat {log} | parse-log-cols | errors-cols | top-paths-cols 10"
    q2 = f"cat {log} | parse-log | top-paths 10"
    q2full = f"cat {log} | parse-log | top-paths-full 10"
    q2c = f"cat {log} | parse-log-cols | top-paths-cols 10"
    q2cfull = f"cat {log} | parse-log-cols | top-paths-cols-full 10"
    awk1 = (f"mawk '$9 >= 500 {{ c[$7]++ }} END {{ for (p in c) printf \"%9d %s\\n\", c[p], p }}' {log}"
            " | sort -k1,1nr -k2,2 | head -10")
    awk2 = (f"mawk '{{ c[$7]++ }} END {{ for (p in c) printf \"%9d %s\\n\", c[p], p }}' {log}"
            " | sort -k1,1nr -k2,2 | head -10")
    native = str(BIN / "native-logs")
    return [
        # Pipe cost on synthetic rows: 3 strings + 1 int per row.
        ("Pipe cost: gen → count (synthetic commits)", "rows · Wasmtime copy", [WP, "--json", "--copy", "wasmtime", f"gen {rows} 256 | count 256"], False, None),
        ("Pipe cost: gen → count (synthetic commits)", "rows · host pump", [WP, "--json", "--serial", f"gen {rows} 256 | count 256"], False, None),
        ("Pipe cost: gen → count (synthetic commits)", "rows · host pump · parallel", [WP, "--json", f"gen {rows} 256 | count 256"], False, None),
        ("Pipe cost: gen → count (synthetic commits)", "columnar · host pump", [WP, "--json", "--serial", f"gen-cols {rows} 4096 | count-cols"], False, None),
        ("Pipe cost: gen → count (synthetic commits)", "columnar · host pump · parallel", [WP, "--json", f"gen-cols {rows} 4096 | count-cols"], False, None),
        ("Pipe cost: gen → count (synthetic commits)", "columnar · Wasmtime copy", [WP, "--json", "--copy", "wasmtime", f"gen-cols {rows} 4096 | count-cols"], False, None),
        # Q1: a selective query; most rows die in `errors`.
        ("Q1: top 10 paths returning 5xx", "wp rows · Wasmtime copy", [WP, "--json", "--copy", "wasmtime", q1], False, "q1"),
        ("Q1: top 10 paths returning 5xx", "wp rows", [WP, "--json", "--serial", q1], False, "q1"),
        ("Q1: top 10 paths returning 5xx", "wp rows · parallel", [WP, "--json", q1], False, "q1"),
        ("Q1: top 10 paths returning 5xx", "wp columnar", [WP, "--json", "--serial", q1c], False, "q1"),
        ("Q1: top 10 paths returning 5xx", "wp columnar · parallel", [WP, "--json", q1c], False, "q1"),
        ("Q1: top 10 paths returning 5xx", "native Rust (same parser, 1 process)", [native, "top-paths", str(log), "10", "500"], False, "q1"),
        ("Q1: top 10 paths returning 5xx", "mawk | sort | head", awk1, True, "q1"),
        # Q2: every row crosses the pipe.
        ("Q2: top 10 paths, all requests", "wp rows · whole rows · Wasmtime copy", [WP, "--json", "--copy", "wasmtime", q2full], False, "q2"),
        ("Q2: top 10 paths, all requests", "wp rows · whole rows", [WP, "--json", "--serial", q2full], False, "q2"),
        ("Q2: top 10 paths, all requests", "wp rows · projected", [WP, "--json", "--serial", q2], False, "q2"),
        ("Q2: top 10 paths, all requests", "wp rows · projected · parallel", [WP, "--json", q2], False, "q2"),
        ("Q2: top 10 paths, all requests", "wp columnar · whole batches", [WP, "--json", "--serial", q2cfull], False, "q2"),
        ("Q2: top 10 paths, all requests", "wp columnar · projected", [WP, "--json", "--serial", q2c], False, "q2"),
        ("Q2: top 10 paths, all requests", "wp columnar · projected · parallel", [WP, "--json", q2c], False, "q2"),
        ("Q2: top 10 paths, all requests", "native Rust (same parser, 1 process)", [native, "top-paths", str(log), "10"], False, "q2"),
        ("Q2: top 10 paths, all requests", "mawk | sort | head", awk2, True, "q2"),
    ]


def measure_rss(cmd, shell):
    """Peak RSS of one fresh run (a separate child, so maxima don't mix)."""
    code = ("import resource,subprocess,sys,json,os;"
            "subprocess.run(json.loads(sys.argv[1]),shell=json.loads(sys.argv[2]),stdout=subprocess.DEVNULL,"
            "stderr=subprocess.DEVNULL,cwd=sys.argv[3],env={**os.environ,'LC_ALL':'C'});"
            "print(resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss*1024)")
    out = subprocess.run([sys.executable, "-c", code, json.dumps(cmd), json.dumps(shell), str(ROOT)],
                         capture_output=True, text=True, check=True)
    return int(out.stdout.strip())


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--lines", type=int, default=2_000_000, help="access-log lines (default 2M, ~420 MB)")
    ap.add_argument("--rows", type=int, default=1_000_000, help="synthetic rows for pipe-cost runs")
    ap.add_argument("--repeats", type=int, default=3)
    ap.add_argument("--only", help="run only groups/names containing this text")
    args = ap.parse_args()

    DATA.mkdir(parents=True, exist_ok=True)
    log = DATA / f"access-{args.lines}.log"
    if not log.exists():
        print(f"generating {log.name} ...", file=sys.stderr)
        subprocess.run([str(BIN / "loggen"), str(args.lines), str(log)], check=True)
    log_bytes = log.stat().st_size

    plan = scenarios(log.relative_to(ROOT), args.rows)
    if args.only:
        plan = [s for s in plan if args.only.lower() in (s[0] + " " + s[1]).lower()]

    samples = {i: [] for i in range(len(plan))}
    stats = {}
    outputs = {}
    for rnd in range(args.repeats + 1):
        for i, (group, name, cmd, shell, check) in enumerate(plan):
            print(f"\r[{rnd}/{args.repeats}] {group[:24]:<24} {name[:44]:<44}", end="", file=sys.stderr, flush=True)
            wall, out, err = run(cmd, shell)
            if rnd == 0:  # warm-up: fills the compile cache; record output
                if check:
                    outputs.setdefault(check, {})[name] = out
                continue
            samples[i].append(wall)
            s = wp_stats(err) if not shell and cmd[0] == WP else None
            if s:
                stats[i] = s
    print(file=sys.stderr)

    # Every implementation of a query must agree exactly.
    for check, outs in outputs.items():
        ref_name, ref = next(iter(outs.items()))
        for name, out in outs.items():
            if out != ref:
                sys.exit(f"output mismatch in {check}: `{name}` differs from `{ref_name}`")

    rows = []
    for i, (group, name, cmd, shell, check) in enumerate(plan):
        rss = measure_rss(cmd, shell)
        s = stats.get(i, {})
        med, best = statistics.median(samples[i]), min(samples[i])
        n_rows = s.get("rows") if s else None
        is_log = check is not None
        if "Pipe cost" in group:
            n_rows = n_rows or args.rows
        pipes = s.get("pipes", [])
        rows.append({
            "group": group, "name": name, "median_s": med, "best_s": best,
            "mb_per_s": (log_bytes / 1e6 / med) if is_log else None,
            "m_rows_per_s": (n_rows / 1e6 / med) if (n_rows and not is_log) else None,
            "peak_rss_mb": rss / 1e6,
            "host_allocs": s.get("host_allocs"),
            "reallocs": sum(p["reallocs"] for p in pipes) if pipes else None,
            "staged_mb": sum(p["bytes"] for p in pipes) / 1e6 if pipes else None,
            "copy_ms": sum(p["lift_ms"] + p["lower_ms"] for p in pipes) if pipes else None,
            "command": cmd if isinstance(cmd, str) else " ".join(map(str, cmd)),
        })

    meta = {
        "date": datetime.datetime.now().isoformat(timespec="seconds"),
        "machine": f"{platform.machine()}, {os.cpu_count()} cores, {platform.system()} {platform.release()}",
        "log": {"lines": args.lines, "bytes": log_bytes},
        "synthetic_rows": args.rows,
        "repeats": args.repeats,
    }
    RESULTS.mkdir(parents=True, exist_ok=True)
    (RESULTS / "latest.json").write_text(json.dumps({"meta": meta, "results": rows}, indent=2))

    def fmt(v, f):
        return "" if v is None else f.format(v)

    lines = [
        f"Ran {meta['date']} on {meta['machine']}; median of {args.repeats} interleaved runs after a warm-up.",
        f"Logs: {args.lines:,} lines, {log_bytes / 1e6:.0f} MB. Synthetic: {args.rows:,} rows.",
        "Every implementation of Q1 and Q2 printed byte-identical output.",
        "",
    ]
    for group in dict.fromkeys(r["group"] for r in rows):
        lines += [f"### {group}", "",
                  "| | median | best | throughput | peak RSS | host allocs | reallocs | staged | host copy time |",
                  "|---|---:|---:|---:|---:|---:|---:|---:|---:|"]
        for r in (r for r in rows if r["group"] == group):
            tput = fmt(r["mb_per_s"], "{:.0f} MB/s") or fmt(r["m_rows_per_s"], "{:.2f} M rows/s")
            name = r["name"].replace("|", "\\|")
            lines.append(
                f"| {name} | {r['median_s'] * 1e3:,.0f} ms | {r['best_s'] * 1e3:,.0f} ms | {tput} | "
                f"{r['peak_rss_mb']:.0f} MB | {fmt(r['host_allocs'], '{:,}')} | {fmt(r['reallocs'], '{:,}')} | "
                f"{fmt(r['staged_mb'], '{:,.0f} MB')} | {fmt(r['copy_ms'], '{:,.0f} ms')} |"
            )
        lines.append("")
    md = "\n".join(lines)
    (RESULTS / "latest.md").write_text(md)
    print(md)


if __name__ == "__main__":
    main()
