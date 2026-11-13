import { writeFile } from "node:fs/promises";
import type * as GitApp from "@omp-source/packages/tui/src/apps/git/git-tui";
import type { ChangedFile, GitTuiModel } from "@omp-source/packages/tui/src/apps/git/state";
import type * as PsTop from "@omp-source/packages/tui/src/apps/ps-top";
import type { PsScope } from "@omp-source/packages/tui/src/apps/ps-data";
import type { DaemonSnapshot } from "@omp-source/packages/tui/src/tools/daemon";
import { callbackValue, visibleText, type FixtureCase } from "../scenario";

export const fixtureCases: FixtureCase[] = [
  {
    id: "app-process-monitor-stop",
    covers: ["component:packages/tui/src/apps/ps-top.ts#PsTopComponent"],
    async create(context) {
      const { PsTopComponent } = await context.load<typeof PsTop>("packages/tui/src/apps/ps-top.ts");
      const scope: PsScope = { kind: "project", projectDir: context.workspace, runtimeDir: `${context.workspace}/runtime` };
      const snapshot: DaemonSnapshot = { name: "fixture-api", id: "fixture-api", state: "running", pid: 100, createdAt: 1794000000000, startedAt: 1794000000000, restartCount: 0, outputBytes: 0, persist: false, detached: false };
      const component = new PsTopComponent(context.tui, { all: false }, {
        collectReports: async () => [{ scope, daemons: [{ snapshot, command: "fixture-never-executed", supervised: true }] }],
        act: async (_scope, name, verb) => { context.record("process-action", { name, verb }); Object.assign(snapshot, { state: "exited", exitCode: 0 }); return snapshot; },
        describe: async () => ({ daemon: snapshot, spec: { name: "fixture-api", application: "fixture-never-executed", args: [], env: {}, cwd: context.workspace, pty: false, restart: "no", persist: false, detached: false } }),
        logs: async () => ({ text: "Fixture daemon output", state: snapshot.state }), close: () => context.record("host-close", true),
      });
      void component.run().then(() => { component.dispose(); context.record("close", true); context.done(); });
      return component;
    },
    checkpoints: [
      { name: "process information", comparisons: [visibleText("process", "(fixture-api)", 'text="fixture-api"', "fixture-api")] },
      { name: "stop through host", terminalInput: "s", browserActions: [{ type: "click", selector: 'button:has-text("Stop")' }], comparisons: [callbackValue("host operation", "process-action", { name: "fixture-api", verb: "stop" })] },
      { name: "quit", terminalInput: "q", browserActions: [{ type: "click", selector: 'button:has-text("Quit")' }], comparisons: [callbackValue("closed", "close", true)] },
    ],
  },
  {
    id: "app-git-overlay-close",
    covers: ["component:packages/tui/src/apps/git/git-tui.ts#GitTuiComponent", "component:packages/tui/src/apps/git/sidebar.ts#Sidebar"],
    async create(context) {
      const { showGitOverlay } = await context.load<typeof GitApp>("packages/tui/src/apps/git/git-tui.ts");
      const files: ChangedFile[] = [{ path: "fixture.bin", kind: "modified", area: "unstaged", additions: 1, deletions: 0 }];
      const model: GitTuiModel = {
        cwd: context.workspace, branch: "fixture-branch", clean: false, unstaged: files, staged: [], headCommit: null,
        refresh: async () => false, loadChangeStats: async () => false, loadHeadFiles: async () => false,
        streamContents: async () => ({ kind: "asset", old: { kind: "binary", byteLength: 12 }, new: { kind: "binary", byteLength: 14 } }),
        stage: async selected => {
          const paths = (selected ?? files).map(file => file.path);
          await writeFile(`${context.workspace}/git-index.json`, JSON.stringify({ paths }));
          context.record("stage", paths);
        },
        unstage: async selected => context.record("unstage", selected?.map(file => file.path) ?? "all"),
        discard: async selected => context.record("discard", selected.map(file => file.path)),
        commit: async message => context.record("commit", message), applyPatch: async patch => context.record("patch", patch),
      };
      let opened = false;
      const open = () => {
        if (opened) return;
        opened = true;
        queueMicrotask(() => {
          void showGitOverlay(context.tui, { model, createAvatarSource: () => ({ get: () => null }),
            aiStage: async () => { throw new Error("Fixture forbids AI staging"); }, generateCommitMessage: async () => { throw new Error("Fixture forbids model generation"); },
          }).then(() => { context.record("close", true); context.done(); });
        });
      };
      return { render: () => { open(); return []; }, invalidate: () => undefined, describe: () => { open(); return null; } };
    },
    checkpoints: [
      { name: "repository information", comparisons: [visibleText("file", "(fixture\\.bin)", '[data-omp-role="omp.app.git.path"]', "fixture.bin")] },
      { name: "stage selected file", terminalInput: "s", browserActions: [{ type: "click", selector: 'button:has-text("Stage file")' }], comparisons: [callbackValue("staged path", "stage", ["fixture.bin"]), { name: "persisted host index", terminal: { type: "file", path: "git-index.json", pointer: "/paths/0" }, browser: { type: "file", path: "git-index.json", pointer: "/paths/0" }, expected: "fixture.bin" }] },
      { name: "close nested overlay", terminalInput: "q", browserActions: [{ type: "press", key: "q" }], comparisons: [callbackValue("closed", "close", true)] },
    ],
  },
];
