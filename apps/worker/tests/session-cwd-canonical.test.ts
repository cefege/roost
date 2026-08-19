// A session's recorded cwd must be the PHYSICAL directory, because the SPA keys
// folder groups off it and the shell reports the physical path via OSC 7 moments
// after the spawn. A symlinked request (/tmp on macOS is /private/tmp) that stays
// unresolved splits one directory into two folder keys, and TerminalDeck's
// liveIds filter then drops every session that landed under the other key —
// observed as a missing pane tab on macOS CI.

import { describe, expect, test } from "bun:test";
import { mkdtempSync, mkdirSync, rmSync, symlinkSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { canonicalSessionCwd } from "../src/util/path.ts";

describe("session cwd canonicalization", () => {
	test("a symlinked spawn cwd records the directory it points at", () => {
		const root = mkdtempSync(join(tmpdir(), "roost-cwd-canon-"));
		try {
			const real = join(root, "real");
			const link = join(root, "link");
			mkdirSync(real);
			symlinkSync(real, link);
			expect(canonicalSessionCwd(link)).toBe(canonicalSessionCwd(real));
			expect(canonicalSessionCwd(link)).toBe(canonicalSessionCwd(join(link, ".")));
			// The physical path is the one OSC 7 will report, so it is what the
			// record must carry — asserting equality with `real` alone would pass
			// even if both sides stayed symlinked.
			expect(canonicalSessionCwd(link).endsWith("/real")).toBe(true);
		} finally {
			rmSync(root, { recursive: true, force: true });
		}
	});

	test("~ expands and a missing directory keeps the expanded request", () => {
		const home = mkdtempSync(join(tmpdir(), "roost-cwd-home-"));
		try {
			expect(canonicalSessionCwd("~", "linux", home)).toBe(canonicalSessionCwd(home));
			const missing = join(home, "absent");
			// Fallback, not a throw: the spawn itself surfaces the real ENOENT.
			expect(canonicalSessionCwd(missing, "linux", home)).toBe(missing);
		} finally {
			rmSync(home, { recursive: true, force: true });
		}
	});
});
