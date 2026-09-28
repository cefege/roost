#!/usr/bin/env python3
"""Batched mutation runner. mutbatch.py <spec.json>... : groups mutants so no two in a
batch share a file or a test binary, applies a batch, runs its test binaries once
(no-fail-fast), reports KILLED/SURVIVED per mutant, restores every file (also on signal).
A batch that fails to build is re-run one mutant at a time."""
import json, re, signal, subprocess, sys
ROOT = "/home/mike/repos/roost-v3-worker/"
CARGO = "/home/mike/wl/wcargo.sh"
saved = {}
def restore(*_):
    for path, text in saved.items():
        open(path, "w").write(text)
    saved.clear()
    if _: sys.exit(1)
for sig in (signal.SIGTERM, signal.SIGHUP, signal.SIGINT):
    signal.signal(sig, restore)

def bins(m): return {(m["pkg"], t) for t in m["tests"]}

def run(batch):
    for m in batch:
        path = ROOT + m["file"]
        text = saved.get(path) or open(path).read()
        saved.setdefault(path, text)
        cur = open(path).read()
        if cur.count(m["old"]) != 1:
            print(f"{m['id']} ANCHOR count={cur.count(m['old'])}", flush=True); m["skip"] = True; continue
        open(path, "w").write(cur.replace(m["old"], m["new"], 1))
    live = [m for m in batch if not m.get("skip")]
    results = {}  # (pkg, bin) -> {test: ok/FAILED}
    build_error = False
    try:
        for pkg in sorted({m["pkg"] for m in live}):
            args = []
            for m in live:
                if m["pkg"] != pkg: continue
                for t in m["tests"]:
                    args += ["--lib"] if t == "--lib" else ["--test", t]
            try:
                out = subprocess.run([CARGO, "test", "-p", pkg, "--no-fail-fast"] + args,
                                     stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                     text=True, timeout=2400).stdout
            except subprocess.TimeoutExpired as e:
                out = (e.stdout or b"").decode() if isinstance(e.stdout, bytes) else (e.stdout or "")
                print(f"TIMEOUT pkg={pkg}", flush=True)
            proc_err = subprocess.CompletedProcess
            full = out
            cur = None
            for line in full.splitlines():
                r = re.match(r"\s*Running (?:tests/(\S+)\.rs|unittests src/lib\.rs)", line)
                if r:
                    cur = (pkg, r.group(1) or "--lib"); results.setdefault(cur, {}); continue
                t = re.match(r"test (\S+) \.\.\. (ok|FAILED|ignored)", line)
                if t and cur: results[cur][t.group(1)] = t.group(2)
    finally:
        restore()
    for m in live:
        seen = {}
        for b in bins(m): seen.update(results.get(b, {}))
        if not seen:
            build_error = True; print(f"{m['id']} NO-RESULTS (build error?) bins={m['tests']}", flush=True); continue
        failed = [n for n, r in seen.items() if r == "FAILED" and (not m.get("name") or m["name"] in n)]
        verdict = "KILLED" if failed else "SURVIVED"
        print(f"{m['id']} {verdict} {m['file']} -> {m['tests']}::{failed or m.get('name') or '*'}", flush=True)
    return build_error

specs = [m for f in sys.argv[1:] for m in json.load(open(f))]
batches = []
for m in specs:
    for b in batches:
        if all(x["file"] != m["file"] and not (bins(x) & bins(m)) for x in b):
            b.append(m); break
    else:
        batches.append([m])
print(f"{len(specs)} mutants in {len(batches)} batches", flush=True)
for i, b in enumerate(batches):
    print(f"## batch {i}: {[m['id'] for m in b]}", flush=True)
    if run(b) and len(b) > 1:
        print(f"## batch {i} build error: one at a time", flush=True)
        for m in b:
            if not m.get("skip"): run([m])
print("## done", flush=True)
