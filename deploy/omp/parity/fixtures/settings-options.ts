import type { BrowserAction, Checkpoint, Comparison } from "../scenario";
import { visibleText } from "../scenario";
import type { SettingDef } from "@omp-source/packages/tui/src/overlays/settings-defs";
import { attribute, escapePattern, labelControl } from "./settings-support";
import { settingRow } from "./settings-proofs";

/** Rows the terminal select list draws at once for a settings choice (`createSubmenu`, settings-selector.ts). */
const terminalWindowRows = 10;
const down = "\x1b[B";

export type OptionObservation = { checkpoint: string; comparison: string };

/** First and last option index the terminal list shows with `selected` highlighted (menu-selection.ts getMenuWindow). */
function terminalWindow(total: number, selected: number): [number, number] {
  let start = Math.max(0, selected - Math.floor((terminalWindowRows - 1) / 2));
  const end = Math.min(total, start + terminalWindowRows);
  start = Math.max(0, end - terminalWindowRows);
  return [start, end];
}

/** Selection positions whose windows together show every option, ending on the last one. */
function pagePositions(total: number): number[] {
  const positions: number[] = [];
  for (let position = Math.floor((terminalWindowRows - 1) / 2); ; position += terminalWindowRows) {
    if (position >= total - 1) break;
    positions.push(position);
    if (terminalWindow(total, position)[1] >= total) return positions;
  }
  positions.push(total - 1);
  return positions;
}

function optionComparison(definition: SettingDef, label: string, index: number, picker: boolean): Comparison {
  return visibleText(`option ${index}`, `(?<![\\w.-])(${escapePattern(label)})(?![\\w.-])`,
    picker ? `[data-omp-picker-item]:has(:text-is(${attribute(label)})) :text-is(${attribute(label)})`
      : `${settingRow(definition.path)} [role=option] span:text-is(${attribute(label)})`, label);
}

/** Checkpoints that open the real option list and read every option on both legs, paging the terminal window. */
export function optionEnumeration(definition: SettingDef, labels: readonly string[], picker: boolean): { checkpoints: Checkpoint[]; observations: OptionObservation[] } {
  const open: BrowserAction = { type: "click", selector: `button${labelControl(definition.label)}` };
  const positions = labels.length <= terminalWindowRows ? [] : pagePositions(labels.length);
  const checkpoints: Checkpoint[] = [];
  const observations: OptionObservation[] = [];
  const observe = (name: string, indexes: number[]) => {
    for (const index of indexes) observations.push({ checkpoint: name, comparison: `option ${index}` });
    return indexes.map(index => optionComparison(definition, labels[index] ?? "", index, picker));
  };
  if (positions.length === 0) {
    const name = "open option list";
    checkpoints.push({ name, terminalInput: "\r", browserActions: [open], comparisons: observe(name, labels.map((_, index) => index)) });
    return { checkpoints, observations };
  }
  const seen = new Set<number>();
  positions.forEach((position, page) => {
    const [start, end] = terminalWindow(labels.length, position);
    const indexes = Array.from({ length: end - start }, (_, offset) => start + offset).filter(index => !seen.has(index));
    indexes.forEach(index => seen.add(index));
    const name = `option page ${page}`;
    const step = page === 0 ? position : position - (positions[page - 1] ?? 0);
    checkpoints.push({ name, terminalInput: page === 0 ? `\r\x1b[H${down.repeat(step)}` : down.repeat(step), ...(page === 0 ? { browserActions: [open] } : {}),
      comparisons: observe(name, indexes) });
  });
  return { checkpoints, observations };
}
