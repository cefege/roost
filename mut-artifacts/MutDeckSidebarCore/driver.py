#!/usr/bin/env python3
"""Mutation batch driver for MutDeckSidebarCore. Run ONLY under
flock target-track/.mut.lock. Usage: driver.py <batch>"""
import json, os, re, shutil, subprocess, sys, time

ROOT = "/home/almalinux/repos/roost-v3-web/target-track/dx-tree"
TMP = "/home/almalinux/repos/roost-v3-web/target-track/tmp/mut-MutDeckSidebarCore"
sys.path.insert(0, TMP)
from specs import BATCHES, CRATE_CMD  # noqa: E402

def main():
    name = sys.argv[1]
    batch = BATCHES[name]
    crate = batch["crate"]
    muts = batch["mutants"]
    files = sorted({m["file"] for m in muts})
    originals = {}
    for f in files:
        with open(os.path.join(ROOT, f)) as fh:
            originals[f] = fh.read()
    # validate + compute lines on originals
    mutated = dict(originals)
    for m in muts:
        src = originals[m["file"]]
        count = src.count(m["old"])
        if count != 1:
            print(f"ABORT {m['id']}: old occurs {count}x in {m['file']}")
            sys.exit(2)
        m["line"] = src[: src.index(m["old"])].count("\n") + 1
        if mutated[m["file"]].count(m["old"]) != 1:
            print(f"ABORT {m['id']}: overlaps another mutant")
            sys.exit(2)
        mutated[m["file"]] = mutated[m["file"]].replace(m["old"], m["new"], 1)
    bdir = os.path.join(TMP, "backup-" + name)
    os.makedirs(bdir, exist_ok=True)
    for f in files:
        dst = os.path.join(bdir, f.replace("/", "__"))
        shutil.copy2(os.path.join(ROOT, f), dst)
    log = os.path.join(TMP, f"{name}.log")
    rc = None
    try:
        for f in files:
            with open(os.path.join(ROOT, f), "w") as fh:
                fh.write(mutated[f])
        env = dict(os.environ)
        env["PATH"] = os.path.expanduser("~/.cargo/bin") + ":" + env["PATH"]
        env.update(CARGO_INCREMENTAL="0", CARGO_BUILD_JOBS="3",
                   CARGO_TARGET_DIR="/home/almalinux/repos/roost-v3-web/target-track")
        env.pop("RUSTFLAGS", None)
        cmd = ["flock", "/home/almalinux/repos/roost-v3-web/target-track/.roost-build.lock",
               "/home/almalinux/repos/roost-build-slot"] + CRATE_CMD[crate]
        t0 = time.time()
        with open(log, "w") as fh:
            rc = subprocess.run(cmd, cwd=ROOT, env=env, stdout=fh, stderr=subprocess.STDOUT).returncode
        print(f"cargo rc={rc} in {time.time()-t0:.0f}s log={log}")
    finally:
        for f in files:
            shutil.copy2(os.path.join(bdir, f.replace("/", "__")), os.path.join(ROOT, f))
        diff = subprocess.run(["git", "-C", ROOT, "diff", "--stat", "--"] + files,
                              capture_output=True, text=True).stdout
        print("RESTORE diff-stat:", repr(diff))
        if diff.strip():
            print("!!! RESTORE NOT CLEAN")
            sys.exit(3)
    text = open(log).read()
    status = {}
    for mt in re.finditer(r"^test (\S+) \.\.\. (ok|FAILED|ignored)", text, re.M):
        status[mt.group(1)] = mt.group(2)
    if "error[E" in text or "could not compile" in text:
        print("COMPILE ERROR present")
        for line in text.splitlines():
            if line.startswith("error") or "-->" in line:
                print("  ", line)
    failed = sorted(k for k, v in status.items() if v == "FAILED")
    print(f"tests seen={len(status)} failed={len(failed)}")
    results = []
    expected_all = set()
    for m in muts:
        exp = m["expect"]
        expected_all.update(exp)
        got = {t: status.get(t, "MISSING") for t in exp}
        res = "failed" if all(v == "FAILED" for v in got.values()) else (
            "survived" if all(v == "ok" for v in got.values()) else f"mixed {got}")
        results.append(dict(id=m["id"], file=m["file"], line=m["line"], change=m["change"],
                            expected_test=exp, result=res))
        print(f"{m['id']:6} {m['file']}:{m['line']} -> {res}  {got}")
    extra = [t for t in failed if t not in expected_all]
    print("unexpected failures:", extra)
    with open(os.path.join(TMP, f"{name}.json"), "w") as fh:
        json.dump(dict(results=results, failed=failed, unexpected=extra), fh, indent=1)

if __name__ == "__main__":
    main()
