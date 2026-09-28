# `roost deploy` dry-run plan — target `roosttarget@localhost`

## Target

A real account created for this purpose, so the deploy's own path resolution
runs against a home that is not a live install:

    sudo useradd -m -d /home/roosttarget -s /bin/bash roosttarget
    # authorized_keys = the existing id_ed25519_roost_deploy.pub
    # ~/.ssh/config: `Host localhost` -> that key, so `roost deploy` (which
    #   passes no -i) resolves a default identity.

Verified reachable: `ssh -o BatchMode=yes roosttarget@localhost` →
`REMOTE_OK / roosttarget / /home/roosttarget / Linux / x86_64`, and
`systemctl --user is-system-running` → `running`, so a unit can really start.

The v2 install on this box (`roost-coord.service`, `roost-worker.service`) is
**not** touched: v3 labels its units `roost3-coord` / `roost3-worker`.

## Environment for the deploy

    ROOST_COORDINATOR_URL=http://127.0.0.1:4113   # a fleet key; ambient fallback is correct
    ROOST_BOOTSTRAP_TOKEN=scratch-dry-run-token   # authorises enrolling a fresh worker
    --label scratch-target

A fresh target needs no coordinator process: `keeper_step::local_inventory`
reads the **deploying box's** coordinator database, and with none it is empty,
which is the documented fresh-target path.

## Steps

1. **Happy path** — `deploy` to the fresh target. Expect exit 0, one line on
   stdout, journal written and settled, definition replaced, unit active.
2. **Second deploy** — same SHA. Expect exit 0, "definition already current",
   and no second release directory.
3. **Show the journal** and the installed unit and the release root on the
   target.

## Forced rollback — and why it needs a fault-injection commit

Deploy #1 and #2 differ **only** in the SHA. Every other thing the unit names —
label, log directory, `StandardOutput=append:` target, `Environment=` — is
shared. So any lever that breaks deploy #2 also breaks the prior definition
that #2 has to roll back *to*, and the rollback itself fails:

| Lever tried | Why it cannot work |
| --- | --- |
| Bad `ROOST_COORDINATOR_URL` in the target's installed unit | The worker refuses to boot (`boot.rs:check_endpoint` → `CoordinatorEndpoint::new` → `BadCoordinatorUrl`) — good — but `capture_rollback_point` saves the *edited* definition, so the rollback restores a unit that fails identically. Result: `RollbackOutcome::Failed` → **exit 8**, not 5. |
| Make the log directory unwritable | `StandardOutput=append:` is in both definitions. Same poisoning. |
| Make the staged release non-executable | `install_release` chmods `0o755` after the rename. No window. |
| Tamper with the staged release on the target | Caught first by the digest check → `Refused`, exit 3. A real guard, but not a rollback. |

So the rollback is forced the way the defect actually occurs: **ship a release
that does not come up.** A throwaway local commit makes `roost worker` exit
non-zero at boot, deploy #2 ships that build, the unit fails to start,
`await_active` times out, `roll_back` restores deploy #1's definition — built
from the good SHA — and the worker comes back up on it. Expect **exit 5** and
`ApplyOutcome::RolledBack`.

The commit is never pushed; `git reset --hard` restores the branch afterwards,
and the report names the SHA on both sides of the experiment.

## Exit codes to observe

| Code | How to produce it |
| --- | --- |
| 0 | Steps 1–2 above. |
| 1 | `deploy` with no host; `--label ""`; `--expected-sha zz`. |
| 2 | `deploy` at a host that does not answer; a control character in the host. |
| 3 | `--source-root` naming a non-POSIX target is not reachable; instead: refuse with 3 by making `remote_arch` report something unsupported is not possible on this host. **May be unobservable here** — report as such rather than inventing it. |
| 4 | `--expected-manifest-sha256 <wrong 64-hex>` → the release that built hashes to something else. |
| 5 | The forced rollback above; and a target whose service is running with a keeper holding channels. |
| 6 | Deploy with no `ROOST_COORDINATOR_URL` and no prior install. |
| 7 | `ROOST_ALLOW_DIRTY` unset with an uncommitted change; `--expected-sha` that is not HEAD. |
| 8 | A rollback that cannot restore — see the poisoning table above; reachable by poisoning the prior unit. |
| 9 | Kill the ssh connection mid-apply; or a target that dies during the transaction. |
