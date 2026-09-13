// Folder group filtering matches the Spaces projection's visible folder metadata.
// The helper must preserve group order and require every normalized query term.

import { describe, expect, test } from "bun:test";
import { asWorkerFp } from "@roost/shared/wire";
import {
  filterFolderGroups,
  type FolderGroup,
} from "../src/lib/folderGroups.ts";

const WORKER_FP = asWorkerFp("a".repeat(64));

function folder(overrides: Partial<FolderGroup> = {}): FolderGroup {
  return {
    key: "folder",
    name: "Web Console",
    server: "Build Host",
    spawnFp: WORKER_FP,
    spawnCwd: "/srv/roost/apps/web",
    online: true,
    subtitle: "",
    latestActivity: 1,
    leadId: "session",
    sessionIds: ["session"],
    pr: null,
    branch: null,
    ports: [],
    reachAddr: null,
    agentStatus: {
      level: "unknown",
      counts: { blocked: 0, done: 0, working: 0, idle: 0, unknown: 0 },
      total: 0,
    },
    ...overrides,
  };
}

describe("filterFolderGroups", () => {
  test("matches normalized terms across folder name, server, and path", () => {
    const web = folder();
    const api = folder({
      key: "api",
      name: "API",
      server: "Deploy Host",
      spawnCwd: "/srv/roost/apps/api",
    });

    expect(filterFolderGroups([web, api], " ＷＥＢ\n build /apps/web ")).toEqual([web]);
    expect(filterFolderGroups([web, api], "deploy /apps/api")).toEqual([api]);
  });

  test("retains order for an empty query and excludes folders missing any term", () => {
    const web = folder();
    const api = folder({ key: "api", name: "API", spawnCwd: "/srv/roost/apps/api" });
    const groups = [web, api];

    expect(filterFolderGroups(groups, "   ")).toBe(groups);
    expect(filterFolderGroups(groups, "web deploy")).toEqual([]);
  });
});
