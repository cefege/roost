#!/usr/bin/env bash
# Wave-gate row W2, run as a mutation window.
#
# The mutation is `roost-term/src/emitter.rs:127`: `> LIVE_DELTA_SCROLLBACK_ROWS_CAP`
# becomes `> u64::MAX`, which makes `live_delta_exceeds_cap` permanently false and
# therefore deletes the cap rather than the fence around it.
#
# Discipline from docs/v3-wave-gate.md, applied literally:
#   - the backup lives OUTSIDE the worktree, so a restore cannot be defeated by
#     anything that happens inside it;
#   - a `trap` restores on any exit path, including a failed assertion;
#   - sha256 is checked before AND after, so a silent no-op mutation is visible
#     as a failure rather than reported as a row that did not bite;
#   - the row states BOTH what must fail and what must still pass. A mutation that
#     only breaks things is also consistent with a broken baseline.

set -uo pipefail

WORKTREE=/home/almalinux/repos/roost-v3-worker
TARGET="$WORKTREE/crates/roost-term/src/emitter.rs"
BACKUP=/tmp/roost-row-w2-emitter.rs.$(date +%s)
BEFORE_SHA=$(sha256sum "$TARGET" | cut -d' ' -f1)

export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2
export CARGO_TARGET_DIR="$WORKTREE/target-track"

restore() {
  if [ -f "$BACKUP" ]; then
    cp "$BACKUP" "$TARGET"
    local after
    after=$(sha256sum "$TARGET" | cut -d' ' -f1)
    if [ "$after" = "$BEFORE_SHA" ]; then
      echo "RESTORED  sha256 $after  (matches pre-mutation)"
    else
      echo "RESTORE FAILED: $after != $BEFORE_SHA" >&2
      return 1
    fi
  fi
}
trap restore EXIT INT TERM

echo "=== W2 mutation window opens ==="
echo "tree: $WORKTREE @ $(git -C "$WORKTREE" rev-parse --short HEAD)"
echo "baseline sha256: $BEFORE_SHA"

# 1. GREEN BASELINE on the named binary, before anything is touched.
echo
echo "--- baseline: a_delta_past_the_row_cap_becomes_a_viewport_only_full ---"
cargo test -p roost-term --test emitter_row_cap 2>&1 | tail -12
BASELINE_RC=${PIPESTATUS[0]}
if [ "$BASELINE_RC" -ne 0 ]; then
  echo "BASELINE IS RED — the row is INCONCLUSIVE, not a pass. Stopping." >&2
  exit 2
fi

# 2. Apply the mutation, outside the worktree copy first.
cp "$TARGET" "$BACKUP"
python3 - "$TARGET" <<'PY'
import sys
path = sys.argv[1]
text = open(path).read()
needle = "mono_total.saturating_sub(state.last_scrollback_total) > LIVE_DELTA_SCROLLBACK_ROWS_CAP"
if needle not in text:
    sys.exit("MUTATION TARGET NOT FOUND — emitter.rs has moved; the row needs re-anchoring")
open(path, "w").write(text.replace(needle, "mono_total.saturating_sub(state.last_scrollback_total) > u64::MAX", 1))
PY
if [ $? -ne 0 ]; then
  echo "MUTATION NOT APPLIED" >&2
  exit 3
fi

MUT_SHA=$(sha256sum "$TARGET" | cut -d' ' -f1)
if [ "$MUT_SHA" = "$BEFORE_SHA" ]; then
  echo "MUTATION WAS A NO-OP — sha256 unchanged. The row would report a pass it did not earn." >&2
  exit 4
fi
echo "mutated sha256: $MUT_SHA  (differs from baseline: mutation is live)"

# 3. MUST FAIL: the named test.
echo
echo "--- MUST FAIL: a_delta_past_the_row_cap_becomes_a_viewport_only_full ---"
cargo test -p roost-term --test emitter_row_cap 2>&1 | tail -25
MUT_RC=${PIPESTATUS[0]}
if [ "$MUT_RC" -eq 0 ]; then
  echo "MUST-FAIL DID NOT FAIL — the row does not bite. A cap that cannot be deleted is not a cap." >&2
  exit 5
fi
echo "MUST-FAIL: as required, the named test failed under the mutation."

# 4. MUST STILL PASS: the other direction. If the cap were removed by DELETING the
#    comparison's whole arm rather than by widening it, this is the test that
#    would notice, and it is the reason the file has two tests.
echo
echo "--- MUST STILL PASS: a_delta_within_the_row_cap_stays_a_delta ---"
cargo test -p roost-term --test emitter_row_cap a_delta_within_the_row_cap_stays_a_delta 2>&1 | tail -12
KEEP_RC=${PIPESTATUS[0]}
if [ "$KEEP_RC" -ne 0 ]; then
  echo "MUST-STILL-PASS FAILED — the mutation took out more than the cap. Row is BIT-with-unestablished-isolation." >&2
  exit 6
fi
echo "MUST-STILL-PASS: as required, the in-cap delta still builds as a delta."

# 5. The rest of the crate must be untouched by the mutation.
echo
echo "--- MUST STILL PASS: the rest of roost-term ---"
cargo test -p roost-term --no-fail-fast 2>&1 | grep -E '^(test result|running|error)' | tail -20
REST_RC=${PIPESTATUS[0]}
echo "rest-of-crate exit: $REST_RC"

echo
echo "=== W2 mutation window closes (trap restores) ==="
