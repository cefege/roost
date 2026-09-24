// Pins the controller's folder cycle: one stick click must always land on a
// DIFFERENT folder's newest session, wrap at the end, and refuse to re-open the
// folder already showing. Getting this wrong is a pad that either dead-ends at
// the last folder or reloads the pane deck the user is already looking at.
//
// DOM-free and store-free: the helper takes a plain FolderGroup list.

import { describe, expect, test } from "bun:test";
import { asWorkerFp } from "@roost/protocol/wire";
import type { FolderGroup } from "../src/lib/folderGroups.ts";
import { nextFolderSessionId } from "../src/lib/padFolders.ts";

const WORKER_FP = asWorkerFp("a".repeat(64));

function folder(key: string, leadId: string, sessionIds: string[]): FolderGroup {
	return {
		key,
		name: key,
		server: "Build Host",
		spawnFp: WORKER_FP,
		spawnCwd: `/srv/roost/${key}`,
		online: true,
		subtitle: "",
		latestActivity: 1,
		leadId,
		sessionIds,
		pr: null,
		branch: null,
		ports: [],
		reachAddr: null,
		agentStatus: {
			level: "unknown",
			counts: { blocked: 0, done: 0, working: 0, idle: 0, unknown: 0 },
			total: 0,
		},
	};
}

// buildFolderGroups() hands these over ordered by latest activity, newest first.
const WEB = folder("web", "web-new", ["web-old", "web-new"]);
const API = folder("api", "api-new", ["api-old", "api-new"]);
const DOCS = folder("docs", "docs-new", ["docs-new"]);

describe("nextFolderSessionId", () => {
	test("steps to the next folder and lands on its newest session", () => {
		expect(nextFolderSessionId([WEB, API, DOCS], "web")).toBe("api-new");
		expect(nextFolderSessionId([WEB, API, DOCS], "api")).toBe("docs-new");
	});

	test("wraps from the last folder back to the first", () => {
		expect(nextFolderSessionId([WEB, API, DOCS], "docs")).toBe("web-new");
	});

	test("a lone folder is not a cycle", () => {
		// The button exists to move BETWEEN folders: with one folder there is
		// nowhere to go, wherever the pad happens to be standing.
		expect(nextFolderSessionId([WEB], "web")).toBeNull();
		expect(nextFolderSessionId([DOCS], null)).toBeNull();
		expect(nextFolderSessionId([], "web")).toBeNull();
		expect(nextFolderSessionId([], null)).toBeNull();
	});

	test("an unresolved current folder lands on the most recent one", () => {
		expect(nextFolderSessionId([WEB, API], null)).toBe("web-new");
		expect(nextFolderSessionId([WEB, API], "closed-folder")).toBe("web-new");
	});
});
