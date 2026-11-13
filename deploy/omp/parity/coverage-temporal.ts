import { isDeepStrictEqual } from "node:util";
import type { ExecutedCheckpoint, ExecutedComparison } from "./coverage";

export function temporalSatisfied(comparison: ExecutedComparison, previous: readonly ExecutedCheckpoint[]): boolean {
  const temporal = comparison.temporal;
  if (!temporal || !["sample", "increase", "decrease"].includes(temporal.direction)) throw new Error("Invalid temporal direction");
  if (Object.hasOwn(comparison, "expected") || comparison.expectedByAdapter) throw new Error("Temporal comparisons cannot declare fixed or negative expectations");
  if (comparison.terminal.type !== "screen" || !["capture", "row", "column", "rowGap", "columnGap", "rightGap", "bottomGap"].includes(comparison.terminal.read ?? "capture")
    || comparison.browser.type !== "dom" || !["text", "value", "rowGap", "columnGap", "rectWidth", "rectHeight", "rectX", "rectY", "rightGap", "bottomGap", "scrollTop", "scrollHeight", "clientHeight"].includes(comparison.browser.read)) throw new Error("Temporal comparisons require actual current rendered numeric text or geometry on both adapters");
  if (temporal.direction === "sample") return [comparison.terminalValue, comparison.browserValue].every(value => typeof value === "number" && Number.isFinite(value));
  const checkpoint = previous.find(item => item.name === temporal.from?.checkpoint);
  const prior = checkpoint?.comparisons.find(item => item.name === temporal.from?.comparison);
  if (!checkpoint?.passed || !prior?.passed || prior.expectedByAdapter) throw new Error("Temporal comparison requires an earlier successful positive observation");
  if (!isDeepStrictEqual(prior.terminal, comparison.terminal) || !isDeepStrictEqual(prior.browser, comparison.browser)) throw new Error("Temporal observations must retain the same actual read descriptors");
  const direction = temporal.direction === "increase" ? 1 : -1;
  return (["terminalValue", "browserValue"] as const).every(key => {
    const current = comparison[key], before = prior[key];
    return typeof current === "number" && Number.isFinite(current) && typeof before === "number" && Number.isFinite(before)
      && direction * (current - before) > 0;
  });
}
