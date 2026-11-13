import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import type { BrowserLeg } from "./browser-leg";
import type { ProbeDocument, ProbeVisit, WireObservation } from "./probe";
import type { LegProfile } from "./profile";
import type { TerminalLeg } from "./pty";
import { objectValue } from "./runtime";
import type { ExecutedCheckpoint, ExecutedComparison, ProbeCursor } from "./coverage";
import type { ExecutedNativeContract } from "./coverage-native";

export interface SerializedError { name: string; message: string; stack?: string; cause?: SerializedError; errors?: SerializedError[] }
export function serializeError(error: unknown): SerializedError {
  if (!(error instanceof Error)) return { name: "ThrownValue", message: String(error) };
  return { name: error.name, message: error.message, stack: error.stack,
    ...(error.cause === undefined ? {} : { cause: serializeError(error.cause) }),
    ...(error instanceof AggregateError ? { errors: [...error.errors].map(serializeError) } : {}) };
}

export async function readTrace(path: string): Promise<Record<string, unknown>[]> {
  if (!await Bun.file(path).exists()) return [];
  const content = await Bun.file(path).text();
  const complete = content.endsWith("\n") ? content : content.slice(0, content.lastIndexOf("\n") + 1);
  return complete.split("\n").filter(Boolean).map((line, index) => objectValue(JSON.parse(line), `${path}:${index + 1}`));
}

// The probe records every call of every instrumented callable, so hot helpers
// repeat byte-identical rows hundreds of thousands of times per case. Every
// coverage query asks whether a matching visit exists in a sequence window
// bounded by a non-visit row or a checkpoint cursor of the same process, so a
// repeated identical visit (all fields but `sequence`) inside one such window
// carries no evidence; the first occurrence per window is kept, and so is
// every cursor row.
function compactVisits(rows: Record<string, unknown>[], cursors: readonly ProbeCursor[]): Record<string, unknown>[] {
  const cursorSequences = new Map<string, Set<number>>();
  for (const cursor of cursors) {
    const owner = `${cursor.pid}:${cursor.generation}`;
    cursorSequences.set(owner, (cursorSequences.get(owner) ?? new Set()).add(cursor.sequence));
  }
  const windows = new Map<string, Set<string>>();
  return rows.filter(row => {
    const owner = `${String(row.pid)}:${String(row.generation)}`;
    const isCursor = typeof row.sequence === "number" && cursorSequences.get(owner)?.has(row.sequence) === true;
    if (row.type !== "visit" || isCursor) {
      windows.delete(owner);
      return true;
    }
    const { sequence: _sequence, ...identity } = row;
    const key = JSON.stringify(identity);
    const seen = windows.get(owner) ?? new Set<string>();
    windows.set(owner, seen);
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

export async function readProvenance(profiles: readonly LegProfile[], checkpoints: readonly ExecutedCheckpoint[]): Promise<{ visits: ProbeVisit[]; wire: WireObservation[]; documents: ProbeDocument[]; raw: Record<string, unknown>[] }> {
  const visits: ProbeVisit[] = [];
  const wire: WireObservation[] = [];
  const documents: ProbeDocument[] = [];
  const raw: Record<string, unknown>[] = [];
  const cursors = checkpoints.flatMap(checkpoint => checkpoint.probeCursors ? [checkpoint.probeCursors.terminal, checkpoint.probeCursors.browser] : []);
  for (const profile of profiles) {
    for (const row of compactVisits(await readTrace(profile.probeTracePath), cursors)) {
      raw.push(row);
      // Retained rows stay byte-identical to the append-only source; the independent coverage gate validates every field.
      if (row.type === "visit") visits.push(row as unknown as ProbeVisit);
      if (row.type === "wire") wire.push(row as unknown as WireObservation);
      if (row.type === "document") documents.push(row as unknown as ProbeDocument);
    }
  }
  return { visits, wire, documents, raw };
}

export async function captureCheckpoint(
  name: string, comparisons: ExecutedComparison[], profiles: readonly LegProfile[], stimulus: NonNullable<ExecutedCheckpoint["stimulus"]>, nativeContracts: ExecutedNativeContract[] = [],
): Promise<ExecutedCheckpoint> {
  const cursors = new Map<string, ProbeCursor>();
  for (const profile of profiles) {
    const adapter = profile.environment.OMP_PARITY_ADAPTER;
    const rows = await readTrace(profile.probeTracePath);
    const latest = rows.findLast(row => row.adapter === adapter);
    if (!latest || !Number.isInteger(latest.pid) || typeof latest.generation !== "string" || !Number.isInteger(latest.sequence)) throw new Error(`Missing actual ${adapter} source cursor after checkpoint observations`);
    cursors.set(adapter!, { pid: latest.pid as number, generation: latest.generation, sequence: latest.sequence as number });
  }
  if (!cursors.has("terminal") || !cursors.has("browser")) throw new Error("Both real adapter cursors are required");
  return { name, passed: true, comparisons, stimulus, nativeContracts, probeCursors: { terminal: cursors.get("terminal")!, browser: cursors.get("browser")! } };
}

export async function captureCaseState(directory: string, terminal?: TerminalLeg, browser?: BrowserLeg): Promise<void> {
  await mkdir(directory, { recursive: true });
  const errors: unknown[] = [];
  if (terminal) {
    try {
      const snapshot = await terminal.snapshot(true);
      await writeFile(join(directory, "terminal.txt"), snapshot.text);
      await writeFile(join(directory, "terminal.json"), JSON.stringify(snapshot, null, 2));
      await writeFile(join(directory, "terminal.ansi"), await terminal.readOutput());
    } catch (error) { errors.push(error); }
  }
  if (browser) {
    try { await browser.snapshot(join(directory, "browser")); }
    catch (error) { errors.push(error); }
  }
  if (errors.length) throw new AggregateError(errors, "Capturing actual parity state failed");
}

export async function assertProjectionHealthy(browser: BrowserLeg): Promise<void> {
  const errors = browser.page.locator('[data-omp-projection-error="true"]');
  if (await errors.count()) throw new Error(`Canonical OMP transcript projection failed: ${JSON.stringify(await errors.allTextContents())}`);
  const renderErrors = browser.page.locator("[data-omp-render-error]");
  if (await renderErrors.count()) throw new Error(`Native OMP rendering failed: ${JSON.stringify(await renderErrors.evaluateAll(elements => elements.map(element => ({ reason: element.getAttribute("data-omp-render-error"), text: element.textContent }))))}`);
}
