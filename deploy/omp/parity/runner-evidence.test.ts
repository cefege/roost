import { expect, test } from "bun:test";
import { mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { ExecutedCheckpoint } from "./coverage";
import type { LegProfile } from "./profile";
import { readProvenance } from "./runner-evidence";

const visit = (sequence: number, pid = 7, method = "isRecord") => ({ type: "visit", source: "packages/utils/src/type-guards.ts", line: 3,
  pid, generation: "g1", adapter: "terminal", sequence, method, component: "packages/utils/src/type-guards.ts#isRecord" });
const wire = (sequence: number) => ({ type: "wire", source: "packages/tui/src/native/backend.ts", line: 9,
  pid: 7, generation: "g1", adapter: "terminal", sequence, direction: "out", verb: "s", body: {} });

async function provenance(rows: object[], cursorSequences: number[]): Promise<number[]> {
  const path = join(await mkdtemp(join(tmpdir(), "parity-evidence-")), "probe.jsonl");
  await writeFile(path, rows.map(row => `${JSON.stringify(row)}\n`).join(""));
  const checkpoints = cursorSequences.map(sequence => ({ name: `c${sequence}`, passed: true, comparisons: [],
    probeCursors: { terminal: { pid: 7, generation: "g1", sequence }, browser: { pid: 99, generation: "g9", sequence: 1 } } })) as unknown as ExecutedCheckpoint[];
  const { raw } = await readProvenance([{ probeTracePath: path } as LegProfile], checkpoints);
  return raw.map(row => Number(row.sequence));
}

test("identical visits collapse only inside one process window", async () => {
  const rows = [visit(1), visit(2), visit(3, 7, "other"), visit(4), visit(5, 8), wire(6), visit(7), visit(8)];
  expect(await provenance(rows, [])).toEqual([1, 3, 5, 6, 7]);
});

test("a checkpoint cursor row is retained and starts a new window", async () => {
  const rows = [visit(1), visit(2), visit(3), visit(4)];
  expect(await provenance(rows, [3])).toEqual([1, 3, 4]);
});
