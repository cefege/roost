import { writeFile } from "node:fs/promises";
import type * as GitApp from "@omp-source/packages/tui/src/apps/git/git-tui";
import type { ChangedFile, FileContents, GitTuiModel } from "@omp-source/packages/tui/src/apps/git/state";
import type { FixtureCase, FixtureContext } from "../scenario";

const component = (file: string, name: string) => `component:packages/tui/src/${file}.ts#${name}`;
const pixelPng = Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aN5sAAAAASUVORK5CYII=", "base64");

async function mountGit(context: FixtureContext, files: ChangedFile[], contents: FileContents) {
  const { showGitOverlay } = await context.load<typeof GitApp>("packages/tui/src/apps/git/git-tui.ts");
  const model: GitTuiModel = {
    cwd: context.workspace, branch: "fixture-branch", clean: false, unstaged: files, staged: [], headCommit: null,
    refresh: async () => false, loadChangeStats: async () => false, loadHeadFiles: async () => false,
    streamContents: async () => contents,
    stage: async () => undefined, unstage: async () => undefined, discard: async () => undefined,
    commit: async () => undefined, applyPatch: async () => undefined,
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
}

export const fixtureCases: FixtureCase[] = [
  {
    id: "primitive-git-diff-view-modes",
    covers: [component("apps/git/git-tui", "GitTuiComponent")],
    create: context => mountGit(context, [{ path: "src/mode.ts", kind: "modified", area: "unstaged", additions: 1, deletions: 1 }],
      { kind: "text", oldText: "const alpha = 1;\nconst beta = 1;\n", newText: "const alpha = 2;\nconst beta = 1;\n", streamResult: undefined as never }),
    checkpoints: [
      { name: "split view", comparisons: [{ name: "added line", terminal: { type: "screen", pattern: "(const alpha = 2;)", group: 1 }, browser: { type: "dom", selector: '[data-kind="added"]', read: "text", pattern: "(const alpha = 2;)", group: 1 }, expected: "const alpha = 2;" }] },
    ],
  },
  {
    id: "primitive-git-asset-compare-media",
    covers: [component("apps/git/git-tui", "GitTuiComponent")],
    async create(context) {
      await writeFile(`${context.workspace}/before.png`, pixelPng);
      await writeFile(`${context.workspace}/after.png`, pixelPng);
      return mountGit(context, [{ path: "after.png", kind: "modified", area: "unstaged", additions: 0, deletions: 0 }], {
        kind: "asset", old: { kind: "binary", byteLength: 70 }, new: { kind: "binary", byteLength: 70 },
        oldRef: { path: `${context.workspace}/before.png`, rev: "HEAD" }, newRef: { path: `${context.workspace}/after.png` },
      });
    },
    checkpoints: [{ name: "asset compare", comparisons: [{ name: "file path", terminal: { type: "screen", pattern: "(after\\.png)", group: 1 }, browser: { type: "dom", selector: '[data-omp-role="omp.app.git.path"]', read: "text", pattern: "(after\\.png)", group: 1 }, expected: "after.png" }] }],
  },
];
