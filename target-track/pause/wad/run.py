#!/usr/bin/env python3
"""WAgentsDetect phase-2 verification: baselines, mutations (applied and
reverted under the worktree build lock), clippy. Throwaway; logs to wad/."""
import os, subprocess, sys, pathlib, re

ROOT = "/home/almalinux/repos/roost-v3-worker"
W = ROOT + "/crates/roost-worker/"
P = ROOT + "/crates/roost-protocol/"
OUT = ROOT + "/target-track/wad/"
LOCK = ROOT + "/target-track/.roost-build.lock"
ENV = dict(os.environ, PATH=os.path.expanduser("~/.cargo/bin") + ":" + os.environ["PATH"],
           CARGO_INCREMENTAL="0", CARGO_BUILD_JOBS="3", CARGO_TARGET_DIR=ROOT + "/target-track")
ENV.pop("RUSTFLAGS", None)
KEEP = re.compile(r"^error|error\[|^test |test result|panicked|called `|left:|right:|assertion|^warning: .*(agents/|agent_occupancy|link_loop/agent_status|terminal_changed|agent_owners|records\.rs|agent_status_retirement)|^(crates/\S+):\d+:\d+: (error|warning)")

WORKER_TESTS = ["agent_status_registry_identity", "agent_status_exit_completion", "agent_status_arbitration",
                "agent_status_stable_transitions", "agent_process_identity", "agent_status_screen_gate",
                "agent_status_osc_transition", "agent_status_reference_clear", "agent_status_peer_process_id",
                "agent_status_link_e2e", "link_agent_status"]

MUT = {
    "P1": (P + "src/proto_adapters/coord_worker_proto/records.rs", "    if status.active {\n        return AgentStatus::parse(value);", "    if status.active || !status.active {\n        return AgentStatus::parse(value);"),
    "M1": (W + "src/agents/registry_recompute.rs", "matches!(agent_id, BuiltinAgentId::Omp | BuiltinAgentId::Pi)", "matches!(agent_id, BuiltinAgentId::Omp)"),
    "M2": (W + "src/agents/registry_recompute.rs", "let retain = loss == CandidateLoss::Exit", "let retain = false && loss == CandidateLoss::Exit"),
    "M3": (W + "src/agents/registry.rs", "if !entry.screen_absence_observed {", "if false {"),
    "M4": (W + "src/agents/stable_detection.rs", "const PENDING_IDLE_CONFIRMATIONS: u32 = 3;", "const PENDING_IDLE_CONFIRMATIONS: u32 = 1;"),
    "M5": (W + "src/runtime/link_loop/agent_status.rs", "let keep_retirement = retirement.filter(|_| current.as_deref() != possibly_sent);", "let keep_retirement: Option<usize> = retirement.filter(|_| false);"),
    "M6": (W + "src/runtime/link_loop/agent_status.rs", "self.repair_replay.push(retirement.clone());", "let _ = retirement;"),
    "M7": (W + "src/agents/detector/scan.rs", "now_ms - last_read < SCREEN_RESCAN_MIN_MS", "now_ms - last_read <= SCREEN_RESCAN_MIN_MS"),
    "M8": (W + "src/agents/detector/scan.rs", "deps.sessions.clear_osc_evidence(session_id);", "let _ = session_id;"),
    "M9": (W + "src/agents/process_scan.rs", "if !refreshed || held.misses < 1 {", "if !refreshed {"),
    "M10": (W + "src/runtime/link_drain.rs", "if let Some(bytes) = loop_state.agent_statuses.next_bytes() {", "if let Some(bytes) = None::<&[u8]> {"),
    "M11": (W + "src/runtime/link_loop/agent_status.rs", "Some(index) => self.queues[index].1 = items.into(),", "Some(index) => {\n                self.queues.remove(index);\n                self.queues.push((session_id, items.into()));\n            }"),
    "M12": (W + "src/runtime/link_loop/agent_status.rs", "self.agent_statuses.send(item, can_write_direct);", "self.agent_statuses.send(item, can_write_direct && false);"),
}


def swap(name, forward):
    path, old, new = MUT[name]
    text = pathlib.Path(path).read_text()
    a, b = (old, new) if forward else (new, old)
    if text.count(a) != 1:
        raise SystemExit(f"{name}: expected one '{a[:50]}' in {path}, found {text.count(a)}")
    pathlib.Path(path).write_text(text.replace(a, b))


def cargo(log, args, mutations=()):
    """Run cargo under the worktree lock; mutations live only while it is held."""
    names = " ".join(mutations)
    script = (f"cd {ROOT} && python3 {OUT}run.py --swap on {names} && "
              f"/home/almalinux/repos/roost-build-slot cargo {args} ; rc=$?; "
              f"python3 {OUT}run.py --swap off {names}; exit $rc") if mutations else \
             f"cd {ROOT} && /home/almalinux/repos/roost-build-slot cargo {args}"
    avail = int(subprocess.run(["df", "--output=avail", "-B1G", "/home"], capture_output=True, text=True).stdout.split()[-1])
    if avail < 10:
        pathlib.Path(OUT + log).write_text(f"DISK LOW {avail}G\n")
        raise SystemExit(97)
    proc = subprocess.run(["flock", LOCK, "bash", "-c", script], env=ENV, capture_output=True, text=True)
    lines = [l for l in (proc.stdout + proc.stderr).splitlines() if KEEP.search(l) and not l.endswith("... ok")]
    pathlib.Path(OUT + log).write_text(f"rc={proc.returncode} mutations={names}\n" + "\n".join(lines) + "\n")


def tests(names):
    return " ".join(f"--test {name}" for name in names)


if len(sys.argv) > 2 and sys.argv[1] == "--swap":
    for name in sys.argv[3:]:
        swap(name, sys.argv[2] == "on")
    sys.exit(0)

steps = sys.argv[1:] or ["proto", "worker", "A", "B", "clippy"]
for step in steps:
    if step == "proto":
        cargo("proto-base.txt", "test -p roost-protocol --no-fail-fast --test agent_status_retirement_proto --test coord_worker_proto --message-format short")
        cargo("proto-P1.txt", "test -p roost-protocol --test agent_status_retirement_proto --message-format short", ["P1"])
    elif step == "worker":
        cargo("worker-base.txt", f"test -p roost-worker --no-fail-fast {tests(WORKER_TESTS)} --message-format short")
    elif step == "A":
        cargo("mut-A.txt", "test -p roost-worker --no-fail-fast --test agent_status_arbitration --test agent_status_registry_identity --test agent_status_stable_transitions --test link_agent_status --test agent_status_screen_gate --test agent_process_identity --message-format short",
              ["M1", "M3", "M4", "M5", "M7", "M9", "M11", "M12"])
    elif step == "B":
        cargo("mut-B.txt", "test -p roost-worker --no-fail-fast --test agent_status_exit_completion --test link_agent_status --test agent_status_osc_transition --test agent_status_link_e2e --message-format short",
              ["M2", "M6", "M8", "M10"])
    elif step == "clippy":
        cargo("clippy.txt", "clippy --keep-going -p roost-worker -p roost-protocol --all-targets --message-format short -- -D warnings")
    pathlib.Path(OUT + "progress.txt").write_text(f"done {step}\n")
pathlib.Path(OUT + "progress.txt").write_text("ALL DONE\n")
