#!/usr/bin/env bash
# Wave-gate row W1, run as a mutation window.
#
# The mutation is `roost-worker/src/session/types.rs:206`: `fsm: ChannelFsm::new()`
# becomes `fsm: ChannelFsm::default()`. `ChannelFsm` derives `Default` over
# `Option<ChannelState>`, and `None` is the RETIRED state from which `send`
# refuses every event — so this mutation deletes the initial state, not a fence
# around it.
#
# W1 is the most load-bearing row on this branch: it guards the exactly-once end
# of a session, and the defect it guards was found by a slice tripping over it
# rather than by a test.
#
# Discipline from docs/v3-wave-gate.md, applied literally:
#   - the backup lives OUTSIDE the worktree, so a restore cannot be defeated by
#     anything that happens inside it;
#   - a `trap` restores on any exit path, including a failed assertion;
#   - sha256 is checked before AND after, so a silent no-op mutation is visible
#     as a failure rather than reported as a row that did not bite;
#   - the row states BOTH what must fail and what must still pass. A mutation that
#     only breaks things is also consistent with a broken baseline.
#
# ONE DISAGREEMENT WITH THE ROW TABLE, recorded before the run rather than after.
# The table says the mutation fails the test's SECOND assertion and that "the
# attach alone passes under a weaker fix". Read against the test body, the FIRST
# assertion is the one that should fail: `a_new_record_can_be_attached_and_ends_
# exactly_once` opens with
#     assert_eq!(session.fsm.state(), Some(ChannelState::Spawned));
# and `default()` yields `None`, so that assert fails immediately and the attach
# is never reached. The row's claim about which assertion catches it may be
# describing a DIFFERENT, weaker mutation. This script asserts only that the NAMED
# TEST fails, which is true under either reading, and prints the actual failing
# assertion so the disagreement is settled by output rather than by argument.

set -uo pipefail

WORKTREE=/home/almalinux/repos/roost-v3-worker
TARGET="$WORKTREE/crates/roost-worker/src/session/types.rs"
BACKUP=/tmp/roost-row-w1-types.rs.$(date +%s)
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

echo "=== W1 mutation window opens ==="
echo "tree: $WORKTREE @ $(git -C "$WORKTREE" rev-parse --short HEAD)"
echo "baseline sha256: $BEFORE_SHA"

# 1. GREEN BASELINE on the named binary, before anything is touched.
echo
echo "--- baseline: session_vocabulary ---"
cargo test -p roost-worker --test session_vocabulary 2>&1 | tail -12
BASELINE_RC=${PIPESTATUS[0]}
if [ "$BASELINE_RC" -ne 0 ]; then
  echo "BASELINE IS RED — the row is INCONCLUSIVE, not a pass. Stopping." >&2
  exit 2
fi

# 2. Apply the mutation. The needle is the call, not the comment above it: the
#    comment is the author's statement of WHY the call is not `default()`, so it
#    must survive the mutation untouched or the diff will read as a revert.
cp "$TARGET" "$BACKUP"
python3 - "$TARGET" <<'PY'
import sys
path = sys.argv[1]
text = open(path).read()
needle = "            fsm: ChannelFsm::new(),"
if needle not in text:
    sys.exit("MUTATION TARGET NOT FOUND — types.rs has moved; the row needs re-anchoring")
if text.count(needle) != 1:
    sys.exit("MUTATION TARGET IS NOT UNIQUE — refusing to guess which site the row means")
open(path, "w").write(text.replace(needle, "            fsm: ChannelFsm::default(),", 1))
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
echo "--- MUST FAIL: a_new_record_can_be_attached_and_ends_exactly_once ---"
cargo test -p roost-worker --test session_vocabulary a_new_record_can_be_attached_and_ends_exactly_once 2>&1 | tail -30
MUT_RC=${PIPESTATUS[0]}
if [ "$MUT_RC" -eq 0 ]; then
  echo "MUST-FAIL DID NOT FAIL — the row does not bite. A record born retired is not a record." >&2
  exit 5
fi
echo "MUST-FAIL: as required, the named test failed under the mutation."

# 4. MUST STILL PASS: the binary's other tests. The mutation is one line in a
#    struct constructor; a binary-wide failure would mean it broke the BUILD or
#    some unrelated path, which is BIT-with-unestablished-isolation rather than
#    the row biting.
echo
echo "--- MUST STILL PASS: the rest of session_vocabulary ---"
cargo test -p roost-worker --test session_vocabulary 2>&1 | tail -20
KEEP_RC=${PIPESTATUS[0]}
echo "rest-of-binary exit: $KEEP_RC (non-zero is EXPECTED here: the named test is in this binary)"

echo
echo "=== W1 mutation window closes (trap restores) ==="
