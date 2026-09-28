"""Mutation harness for MutShellSidebar. Run under the .mut.lock:
    flock <target-track>/.mut.lock python3 harness.py <batch>
Applies the batch's mutants to dx-tree, runs ONE cargo test, restores the
backups, checks the product files are clean, and prints per-mutant results."""
import json, os, re, shutil, signal, subprocess, sys

ROOT = "/home/almalinux/repos/roost-v3-web/target-track"
TREE = f"{ROOT}/dx-tree"
HERE = f"{ROOT}/tmp/mut-MutShellSidebar"
sys.path.insert(0, HERE)
from mutants import BATCHES, TEST_BINS  # noqa: E402

batch_name = sys.argv[1]
mutants = BATCHES[batch_name]
files = sorted({m["file"] for m in mutants})
backups = {}


def restore():
    for rel, bak in backups.items():
        shutil.copy2(bak, f"{TREE}/{rel}")


def on_signal(signum, frame):
    restore()
    sys.exit(f"interrupted by signal {signum}; restored")


signal.signal(signal.SIGTERM, on_signal)
signal.signal(signal.SIGINT, on_signal)

# My own test edits (status_bar.rs carries an inline test module I own).
ALLOWED_DIRTY = {"crates/roost-web/tests/" + n + ".rs" for n in TEST_BINS} | {
    "crates/roost-web/src/components/layout/status_bar.rs"}


def dirty_files():
    out = subprocess.run(["git", "-C", TREE, "status", "--porcelain"],
                         capture_output=True, text=True, check=True).stdout
    return {line[3:] for line in out.splitlines()
            if line.strip() and "/tests/" not in line[3:]}


foreign = dirty_files() - ALLOWED_DIRTY
if foreign:
    sys.exit(f"dx-tree dirty before my batch (someone else's mutant?): {sorted(foreign)}")
records = []
log_path = None
try:
    for rel in files:
        bak = f"{HERE}/backup/{rel.replace('/', '__')}"
        shutil.copy2(f"{TREE}/{rel}", bak)
        backups[rel] = bak
    for m in mutants:
        path = f"{TREE}/{m['file']}"
        snapshot = subprocess.run(
            ["git", "-C", TREE, "show", f"HEAD:{m['file']}"],
            capture_output=True, text=True, check=True).stdout
        idx = snapshot.find(m["old"])
        if idx < 0 or snapshot.count(m["old"]) != 1:
            raise SystemExit(f"{m['id']}: old text found {snapshot.count(m['old'])}x in snapshot")
        line = snapshot[:idx].count("\n") + 1
        text = open(path).read()
        if text.count(m["old"]) != 1:
            raise SystemExit(f"{m['id']}: old text not unique in working file")
        open(path, "w").write(text.replace(m["old"], m["new"], 1))
        records.append({**m, "line": line})
    cmd = ["flock", f"{ROOT}/.roost-build.lock", "/home/almalinux/repos/roost-build-slot",
           "cargo", "test", "-p", "roost-web", "--no-fail-fast", "--lib"]
    for name in TEST_BINS:
        cmd += ["--test", name]
    env = dict(os.environ)
    env.update({
        "PATH": f"{os.environ['HOME']}/.cargo/bin:{os.environ['PATH']}",
        "CARGO_INCREMENTAL": "0", "CARGO_BUILD_JOBS": "3", "CARGO_TARGET_DIR": ROOT,
    })
    env.pop("RUSTFLAGS", None)
    log_path = f"{HERE}/{batch_name}.log"
    with open(log_path, "w") as log:
        proc = subprocess.run(cmd, cwd=TREE, env=env, stdout=log, stderr=subprocess.STDOUT)
finally:
    restore()
    for rel, bak in backups.items():
        if open(bak, "rb").read() != open(f"{TREE}/{rel}", "rb").read():
            print(f"!!! {rel} differs from its backup after restore")
            sys.exit(2)
    leftover = dirty_files() - ALLOWED_DIRTY
    if leftover:
        print(f"!!! PRODUCT FILES NOT CLEAN: {sorted(leftover)}")
        sys.exit(2)
    print(f"restored {len(files)} file(s); product files clean")

results = {}
current = None
for raw in open(log_path):
    header = re.search(r"Running (?:unittests )?(\S+)", raw)
    if header:
        src = header.group(1)
        current = "lib" if src.endswith("lib.rs") else os.path.splitext(os.path.basename(src))[0]
        continue
    hit = re.match(r"test (\S+) \.\.\. (ok|FAILED|ignored)", raw)
    if hit and current:
        results[f"{current}::{hit.group(1)}"] = hit.group(2)

if not results:
    print("NO TEST RESULTS (compile error?) exit", proc.returncode)
    os.system(f"grep -n -B2 -A12 '^error' {log_path} | head -80")
    sys.exit(1)

expected = set()
out = []
for r in records:
    statuses = {t: results.get(t, "MISSING") for t in r["expect"]}
    primary = r["expect"][0]
    expected.update(r["expect"])
    verdict = "failed" if statuses[primary] == "FAILED" else (
        "survived" if statuses[primary] == "ok" else statuses[primary])
    out.append({"id": r["id"], "slice": r["slice"], "file": r["file"], "line": r["line"],
                "change": r["change"], "expected_test": primary, "result": verdict,
                "others": statuses})
unexpected = sorted(t for t, s in results.items() if s == "FAILED" and t not in expected)
print(json.dumps(out, indent=1))
print("unexpected failures:", json.dumps(unexpected, indent=1))
print(f"totals: {sum(1 for s in results.values() if s == 'ok')} ok, "
      f"{sum(1 for s in results.values() if s == 'FAILED')} failed")
json.dump({"records": out, "unexpected": unexpected}, open(f"{HERE}/{batch_name}.json", "w"), indent=1)
