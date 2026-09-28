#!/usr/bin/env python3
"""WHeart mutation batches. Outer: `python3 /tmp/wheart-mut.py <batch>` takes the
worktree build lock (same lock + slot + env as /tmp/wcargo.sh), then inside it
applies the batch's mutations, builds the test binaries (--no-run), copies them
to /tmp/wheart-mut/<batch>/, and reverts each mutation by exact reverse replace."""
import os, re, shutil, subprocess, sys

ROOT = "/home/almalinux/repos/roost-v3-worker"
CR = ROOT + "/crates/roost-worker/"
LOCK = ROOT + "/target-track/.roost-build.lock"
SLOT = "/home/almalinux/repos/roost-build-slot"
TESTS = ["heartbeat", "heartbeat_host_metrics", "channel_creation_gate", "session_git_ports",
         "stray_reap", "strays", "host_samples", "host_pr_status"]

HB = CR + "src/runtime/heartbeat.rs"
HM = CR + "src/runtime/heartbeat_metrics.rs"
GATE = CR + "src/session/channel_creation_gate.rs"
STR = CR + "src/strays.rs"
SR = CR + "src/session/stray_reap.rs"
GP = CR + "src/session/git_ports.rs"
SAM = CR + "src/host/samples.rs"
PR = CR + "src/host/pr_status.rs"
RESP = CR + "src/session/respawn.rs"

BATCHES = {
    "B1": [
        ("M1", HB, "                    beat.attempt().await;\n                    next.set(tokio::time::sleep(HEARTBEAT_INTERVAL));\n",
                   "                    next.set(tokio::time::sleep(HEARTBEAT_INTERVAL));\n                    beat.attempt().await;\n"),
        ("M5a", HM, "            && self.clock.now_epoch_ms() - cached.sampled_at_ms < HOST_METRICS_INTERVAL_MS\n",
                    "            && self.clock.now_epoch_ms() - cached.sampled_at_ms < 0\n"),
        ("M6a", GATE, "            if counts.preparations > 0 {\n                return false;\n",
                      "            if false {\n                return false;\n"),
        ("M8a", STR, "                self.strikes.remove(&channel_id);\n                Verdict::Reap { channel_id }\n",
                     "                Verdict::Reap { channel_id }\n"),
        ("M10a", GP, "            if record.session_id() != session_id {\n                return false;\n            }\n            match reading {\n",
                     "            if false {\n                return false;\n            }\n            match reading {\n"),
        ("M11", SAM, "        .filter_map(|line| line.split_once(':'))\n        .find(|(name, _)| name.trim() == interface)\n",
                     "        .find_map(|line| line.split_once(':'))\n        .filter(|(name, _)| name.trim() == interface)\n"),
        ("M12", PR, "                .or(entry.state.as_deref())\n", ""),
    ],
    "B2": [
        ("M2", HB, "        if self.config.reconciliation.current() != reconciliation {\n",
                   "        if false {\n"),
        ("M5b", HM, "        if let Some(net) = sample.net {\n            self.previous_net = Some((net, sampled_at_ms));\n        }\n", ""),
        ("M6b", GATE, ".wait_for(|counts| counts.active_creations == 0)", ".wait_for(|_| true)"),
        ("M9a", SR, "                    .record_of_channel(*channel_id)\n                    .is_some()\n",
                    "                    .record_of_channel(*channel_id)\n                    .is_some()\n                    && false\n"),
        ("M10b", GP, "record.git_branch.as_ref().and_then(Option::as_ref) == branch.as_ref()",
                     "record.git_branch == Some(branch.clone())"),
    ],
    "B3": [
        ("M3", HB, "        let mut host_metrics = self.last_good_host_metrics.clone();\n",
                   "        let mut host_metrics = None;\n"),
        ("M6b", GATE, ".wait_for(|counts| counts.active_creations == 0)", ".wait_for(|_| true)"),
        ("M6c", GATE, "        if self.rolled_back.swap(true, Ordering::AcqRel) {\n",
                      "        if false && self.rolled_back.swap(true, Ordering::AcqRel) {\n"),
        ("M8b", STR, "                if strikes < STRAY_STRIKES {\n",
                     "                if strikes < STRAY_STRIKES - 1 {\n"),
    ],
    "B5": [
        ("M4", HB, "                self.consecutive_misses = 0;\n", ""),
    ],
    "B4": [
        ("M4", HB, "                self.consecutive_misses = 0;\n", ""),
        ("M6d", GATE, "                lanes.set_keeper_update_prepared(true);\n", ""),
        ("M7", RESP, "        let _lease = self.admit_channel_creation()?;\n        let cols = cols.unwrap_or(DEFAULT_COLS);\n",
                     "        let _ = self.admit_channel_creation()?;\n        let cols = cols.unwrap_or(DEFAULT_COLS);\n"),
        ("M9b", SR, "        if maintenance.is_some() {\n", "        if false {\n"),
    ],
}


def swap(path, old, new, tag):
    text = open(path).read()
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{tag}: expected exactly one match in {path}, found {count}")
    open(path, "w").write(text.replace(old, new))


def revert(path, old, new, tag):
    text = open(path).read()
    if new == "":
        raise SystemExit(f"{tag}: deletion mutations revert by anchor")
    count = text.count(new)
    if count != 1:
        print(f"REVERT PROBLEM {tag}: {count} matches of the mutated text in {path}", flush=True)
        return False
    open(path, "w").write(text.replace(new, old))
    return True


def cargo_env():
    env = dict(os.environ)
    env["PATH"] = os.path.expanduser("~/.cargo/bin") + ":" + env.get("PATH", "")
    env.update(CARGO_INCREMENTAL="0", CARGO_BUILD_JOBS="3", CARGO_TARGET_DIR=ROOT + "/target-track")
    env.pop("RUSTFLAGS", None)
    return env


def final(tests=None):
    extra = ["host_folder_facts", "host_listening_ports"]
    args = [SLOT, "cargo", "test", "-p", "roost-worker", "--no-run", "--message-format", "short"]
    for test in tests or (TESTS + extra):
        args += ["--test", test]
    build = subprocess.run(args, cwd=ROOT, env=cargo_env(), capture_output=True, text=True)
    out = build.stdout + build.stderr
    print("\n".join(l for l in out.splitlines() if "error" in l or "warning" in l or "Executable" in l), flush=True)
    print(f"final build exit={build.returncode}", flush=True)
    dest = "/tmp/wheart-mut/FINAL"
    os.makedirs(dest, exist_ok=True)
    for match in re.finditer(r"Executable tests/(\w+)\.rs \(([^)]+)\)", out):
        shutil.copy2(os.path.join(ROOT, match.group(2)), os.path.join(dest, match.group(1)))
    clippy = subprocess.run([SLOT, "cargo", "clippy", "-p", "roost-worker", "--all-targets", "--message-format", "short", "--", "-D", "warnings"],
                            cwd=ROOT, env=cargo_env(), capture_output=True, text=True)
    open("/tmp/wheart-clippy.log", "w").write(clippy.stdout + clippy.stderr)
    print(f"clippy exit={clippy.returncode}", flush=True)


def inner(batch):
    if batch == "SEQ2":
        sys.stdout = open("/tmp/wheart-mut-B5.log", "w")
        inner("B5")
        sys.stdout = open("/tmp/wheart-final.log", "w")
        final(TESTS + ["host_folder_facts", "host_listening_ports"])
        return
    if batch == "SEQ":
        for name in ("B3", "B4"):
            sys.stdout = open(f"/tmp/wheart-mut-{name}.log", "w")
            inner(name)
        sys.stdout = open("/tmp/wheart-final.log", "w")
        final()
        return
    muts = BATCHES[batch]
    applied = []
    try:
        for tag, path, old, new in muts:
            # Deletions carry a unique marker so the reverse replace is exact.
            mutated = new if new else f"// WHEART-MUTATION {tag}\n"
            swap(path, old, mutated, tag)
            applied.append((tag, path, old, mutated))
            print(f"applied {tag} in {path}", flush=True)
        env = cargo_env()
        args = [SLOT, "cargo", "test", "-p", "roost-worker", "--no-run", "--message-format", "short"]
        for test in TESTS:
            args += ["--test", test]
        build = subprocess.run(args, cwd=ROOT, env=env, capture_output=True, text=True)
        out = build.stdout + build.stderr
        print("\n".join(l for l in out.splitlines() if "error" in l or "Executable" in l), flush=True)
        print(f"build exit={build.returncode}", flush=True)
        dest = f"/tmp/wheart-mut/{batch}"
        os.makedirs(dest, exist_ok=True)
        for match in re.finditer(r"Executable tests/(\w+)\.rs \(([^)]+)\)", out):
            shutil.copy2(os.path.join(ROOT, match.group(2)), os.path.join(dest, match.group(1)))
    finally:
        for tag, path, old, mutated in applied:
            ok = revert(path, old, mutated, tag)
            print(f"reverted {tag}: {ok}", flush=True)


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[2] == "inner":
        inner(sys.argv[1])
    else:
        avail = shutil.disk_usage("/home").free // (1 << 30)
        if avail < 10:
            raise SystemExit(97)
        os.execvp("flock", ["flock", LOCK, sys.executable, __file__, sys.argv[1], "inner"])
