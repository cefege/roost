---
description: Check on the v2 rewrite progress. Reports which phases shipped, which are in flight, which are blocked.
---

Run in parallel:
- ls "$(git rev-parse --show-toplevel)"/apps/coord/src/ 2>/dev/null | head -10
- ls "$(git rev-parse --show-toplevel)"/apps/worker/src/ 2>/dev/null | head -10
- ls "$(git rev-parse --show-toplevel)"/apps/web/src/ 2>/dev/null | head -10
- find "$(git rev-parse --show-toplevel)"/apps -name "BLOCKED.md" 2>/dev/null
- bun test apps/shared/tests 2>&1 | tail -3
- bun test smoke 2>&1 | tail -3
- bun x tsc -p tsconfig.base.json --noEmit 2>&1 | tail -10

Output one-line per phase:
- R4.-1 BUN-SMOKE-TEST: PASS|FAIL|PENDING
- R4.0 SPEC-THE-WIRE: PASS|FAIL|PENDING (look for apps/shared/src/wire/ files)
- R4.1 NEW-COORD: PASS|FAIL|IN-PROGRESS (look for apps/coord/src/router/)
- R4.2 NEW-WORKER: PASS|FAIL|IN-PROGRESS (look for apps/worker/src/keeper/, apps/worker/src/main.ts)
- R4.3 NEW-WEB: PASS|FAIL|IN-PROGRESS (look for apps/web/src/main.tsx, apps/web/src/store/)
- R4.4 INVARIANTS-IN-CI: PASS|PENDING (look for .github/workflows/ci.yml)
- R4.5 CUTOVER: PASS|PENDING (look for apps/roost-cli/src/cutover.ts)

Apply L1-L4. Dense. No prose.
