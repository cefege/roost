import type { Comparison } from "../scenario";

type Claim = NonNullable<Comparison["proves"]>[number];

/** The dark theme's `toneColor` palette as the browser computes it; `neutral` is the terminal's default foreground. */
export const toneRgb: Record<string, string | undefined> = {
  accent: "rgb(254, 188, 56)",
  info: "rgb(254, 188, 56)",
  success: "rgb(137, 210, 129)",
  warning: "rgb(228, 192, 15)",
  pending: "rgb(228, 192, 15)",
  error: "rgb(252, 58, 75)",
  muted: "rgb(119, 125, 136)",
  user: "rgb(178, 129, 214)",
};

/** One `layout` claim per emitted prop, each carrying the whole value the producer wrote. */
export function layoutClaims(type: string, props: Record<string, unknown>): Claim[] {
  return Object.entries(props).map(([key, value]) => ({ entry: `property:${type}.${key}`, effect: "layout" as const, value: value as never }));
}

export const escapePattern = (value: string): string => value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

const monthNames = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const addDays = (date: Date, days: number): Date => new Date(date.getFullYear(), date.getMonth(), date.getDate() + days);

/**
 * What the usage dashboard's heatmap chart must carry for one busy `today` and an
 * otherwise empty year: a Monday-first 53-week grid, intensities 0..1 and the month
 * labels GitHub keeps (a label survives only when its successor starts over two columns later).
 */
export function todayHeatmap(today = new Date(), weeks = 53): { cells: (number | null)[][]; cols: { at: number; label: string }[] } {
  const today0 = new Date(today.getFullYear(), today.getMonth(), today.getDate());
  const start = addDays(today0, -((today0.getDay() + 6) % 7) - (weeks - 1) * 7);
  const cells = Array.from({ length: 7 }, () => Array.from({ length: weeks }, (): number | null => null));
  const starts: { at: number; label: string }[] = [];
  let previous = -1;
  for (let week = 0; week < weeks; week++) {
    const weekStart = addDays(start, week * 7);
    if (weekStart.getMonth() !== previous) starts.push({ at: week, label: monthNames[weekStart.getMonth()]! });
    previous = weekStart.getMonth();
    for (let day = 0; day < 7; day++) {
      const date = addDays(weekStart, day);
      if (date <= today0) cells[day]![week] = date.getTime() === today0.getTime() ? 1 : 0;
    }
  }
  return { cells, cols: starts.filter((entry, index) => !starts[index + 1] || starts[index + 1]!.at - entry.at > 2) };
}
