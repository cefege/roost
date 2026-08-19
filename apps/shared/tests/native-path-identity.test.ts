// Path identity folds only what the host filesystem itself treats as one path:
// Windows case, and darwin's three system symlinks (/tmp, /var, /etc ARE
// /private/{tmp,var,etc}). Display must never be folded — that is normalize's
// job — so these assertions pin identity only.

import { describe, expect, test } from "bun:test";
import { nativePathIdentityKey, sameWorkerFolder } from "../src/native-path.ts";

describe("native path identity", () => {
	test("darwin folds its system symlinks so one directory has one key", () => {
		for (const root of ["tmp", "var", "etc"]) {
			expect(nativePathIdentityKey("darwin", `/${root}/proj`))
				.toBe(nativePathIdentityKey("darwin", `/private/${root}/proj`));
			// The bare root folds too: a session spawned at /tmp reports /private/tmp.
			expect(nativePathIdentityKey("darwin", `/${root}`))
				.toBe(nativePathIdentityKey("darwin", `/private/${root}`));
		}
	});

	test("the fold is exact, not a /private prefix heuristic", () => {
		// /private/other is a real directory, distinct from /other.
		expect(nativePathIdentityKey("darwin", "/private/other"))
			.not.toBe(nativePathIdentityKey("darwin", "/other"));
		// A path that merely starts with the root name is untouched.
		expect(nativePathIdentityKey("darwin", "/tmpfiles"))
			.not.toBe(nativePathIdentityKey("darwin", "/private/tmpfiles"));
	});

	test("linux has no such symlinks, so the same pair stays distinct", () => {
		expect(nativePathIdentityKey("linux", "/tmp/proj"))
			.not.toBe(nativePathIdentityKey("linux", "/private/tmp/proj"));
	});

	test("sameWorkerFolder folds a known os and falls back to equality otherwise", () => {
		expect(sameWorkerFolder("darwin", "/tmp/proj", "/private/tmp/proj")).toBe(true);
		expect(sameWorkerFolder("linux", "/tmp/proj", "/private/tmp/proj")).toBe(false);
		// Unknown or absent os: exact equality, never a false merge.
		expect(sameWorkerFolder(undefined, "/tmp/proj", "/private/tmp/proj")).toBe(false);
		expect(sameWorkerFolder("plan9", "/tmp/proj", "/tmp/proj")).toBe(true);
		// A path neither side can normalize also falls back instead of throwing.
		expect(sameWorkerFolder("darwin", "relative/proj", "relative/proj")).toBe(true);
		expect(sameWorkerFolder("darwin", "relative/proj", "/tmp/proj")).toBe(false);
	});
});
