#!/usr/bin/env python3
"""Plans (default) or makes (--commit) the per-slice commit series from commits2/.
A path shared by several slices goes to the FIRST slice in ORDER that lists it; every
changed path outside target-track/ lands in exactly one commit (else exit 2)."""
import subprocess, sys
R = "/home/mike/repos/roost-v3-worker"
C2 = R + "/target-track/pause/commits2/"
MSG = "/home/mike/wl/msgs/"
ORDER = ["ProtoDoor", "ProtoAttach", "WResume", "WKUpdate", "WHeart", "WDurable", "WBootOrder",
         "WDoorHttp", "WDoorTerm", "WPeer", "WAttach", "WAttachDirect", "WCapture",
         "WAgentsReport", "WAgentsDetect", "WAgentsInstall", "WAgentsPrompt"]
EXTRA = {"ProtoDoor": ["crates/roost-protocol/src/local_ui_door.rs"],
         "ProtoAttach": ["crates/roost-protocol/src/lib.rs",
                         "crates/roost-protocol/src/attachment_transfer/mod.rs",
                         "crates/roost-protocol/src/attachment_transfer/packets.rs",
                         "crates/roost-protocol/src/attachment_transfer/packet_queue.rs",
                         "crates/roost-protocol/tests/attachment_transfer_packets.rs"],
         "WResume": ["crates/roost-worker/tests/session_spawn.rs"],
         "WBootOrder": ["crates/roost-worker/tests/worker_boot_order.rs",
                        "crates/roost-worker/tests/worker_retire_authorization.rs"]}
st = subprocess.run(["git", "status", "--porcelain", "--untracked-files=all"], cwd=R,
                    capture_output=True, text=True).stdout.splitlines()
changed = {l[3:] for l in st if not l[3:].startswith("target-track/")}
taken, plan = set(), []
for s in ORDER:
    try: listed = open(C2 + s + ".paths").read().split()
    except FileNotFoundError: listed = []
    paths = [p for p in EXTRA.get(s, []) + listed if p in changed and p not in taken]
    taken.update(paths); plan.append((s, sorted(set(paths))))
left = changed - taken
for s, p in plan: print(f"{s}: {len(p)} paths")
if left: print("UNASSIGNED:", sorted(left)); sys.exit(2)
if "--commit" in sys.argv:
    for s, paths in plan:
        if not paths: continue
        msg = MSG + s + ".msg"
        subprocess.run(["git", "add", "--"] + paths, cwd=R, check=True)
        subprocess.run(["git", "commit", "-q", "-F", msg], cwd=R, check=True)
        print(subprocess.run(["git", "log", "--oneline", "-1"], cwd=R, capture_output=True, text=True).stdout.strip())
