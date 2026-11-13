//! Session browser proof scenario: the real session selector over four sessions of different ages and
//! states, proving the picker properties its describe hook emits for rows and search hits. Registered by pickers.ts.
import { writeFile } from "node:fs/promises";
import type * as SessionSelector from "@omp-source/packages/tui/src/overlays/session-selector";
import type { FixtureCase } from "../scenario";
import { pickerClaims } from "./pickers-claims-session";
import { below, coversFor, literal } from "./pickers-support";

const heading = { terminal: "Fixture sessions \\(fixture folder\\)", browser: 'h2:text-is("Fixture sessions (fixture folder)")' };
const alphaRow = { terminal: "Alpha plan", browser: '[data-omp-picker-item]:has-text("Alpha plan")' };
const gammaRow = { terminal: "Gamma run", browser: '[data-omp-picker-item]:has-text("Gamma run")' };
const browse = pickerClaims["session-browse"]!;
const search = pickerClaims["session-search"]!;

export const fixtureCases: FixtureCase[] = [{
  id: "picker-session-browse",
  covers: coversFor(browse, search),
  async create(context) {
    const { SessionSelectorComponent } = await context.load<typeof SessionSelector>("packages/tui/src/overlays/session-selector.ts");
    const now = Date.now();
    const day = 86_400_000;
    const specs = [
      { id: "alpha", title: "Alpha plan", message: "Alpha prompt", modified: new Date(2026, 9, 24, 12, 0), status: "complete" as const },
      { id: "beta", title: "Beta fork", message: "Beta prompt", modified: new Date(now - 2 * day), status: "interrupted" as const, parent: "alpha" },
      { id: "gamma", title: "Gamma run", message: "Gamma prompt", modified: new Date(now - 3 * day), status: "aborted" as const },
      { id: "delta", title: "Delta wait", message: "Delta prompt", modified: new Date(now - 20 * day), status: "pending" as const },
    ];
    const sessions = specs.map(spec => ({
      path: `${context.workspace}/${spec.id}.jsonl`, id: spec.id, cwd: context.workspace, title: spec.title, modified: spec.modified, size: 5600,
      firstMessage: spec.message, allMessagesText: `${spec.message} ${spec.title}`, status: spec.status,
      ...(spec.parent ? { parentSessionPath: `${context.workspace}/${spec.parent}.jsonl` } : {}),
    }));
    await Promise.all(sessions.map(session => writeFile(session.path, JSON.stringify({ type: "session", version: 3, id: session.id, timestamp: "2026-11-01T12:00:00Z", cwd: "." }) + "\n")));
    const selector = new SessionSelectorComponent(sessions, () => context.done(), () => context.done(), () => context.done(), {
      title: "Fixture sessions", scopeLabel: "fixture folder", getTerminalRows: () => 40, pinnedIds: new Set(["alpha"]), currentSessionPath: sessions[2]!.path,
      onSetPinned: async () => undefined, onRename: async (_session, title) => title, onDelete: async () => true,
      loadAllSessions: async () => ({ sessions }),
    });
    selector.setOnRequestRender(() => context.tui.requestRender());
    return selector;
  },
  checkpoints: [
    {
      name: "browse",
      comparisons: [
        literal("title", "(Fixture sessions \\(fixture folder\\))", { selector: heading.browser }, "Fixture sessions (fixture folder)", []),
        literal("session", "(Alpha plan)", { selector: '[data-omp-picker-item] :text-is("Alpha plan")' }, "Alpha plan", []),
        below("session properties", alphaRow, heading, browse),
      ],
    },
    {
      name: "search",
      terminalInput: "gamma",
      browserActions: [{ type: "fill", selector: 'input[type="search"]', value: "gamma" }],
      comparisons: [
        literal("match", "(Gamma run)", { selector: '[data-omp-picker-item]:has-text("Gamma run") span.break-words' }, "Gamma run", []),
        below("search hits", { terminal: gammaRow.terminal, browser: `${gammaRow.browser} span.text-primary` }, heading, search),
      ],
    },
  ],
}];
