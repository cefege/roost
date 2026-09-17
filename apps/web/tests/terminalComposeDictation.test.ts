// Pins the paint split the composer's dictation mirror renders: which glyphs
// belong to the settled prefix and which to the unsettled hypothesis. A wrong
// boundary dims settled words or the user's own typed base, which is exactly
// what the mirror exists to distinguish. Pure — no DOM, no Solid rendering.

import { describe, test, expect } from "bun:test";
import { paintDictation } from "../src/components/TerminalComposeDictation.ts";

describe("paintDictation", () => {
	test("a hypothesis with no settled words starts after the typed base", () => {
		const painted = paintDictation("typed base", "", "hello");
		expect(painted).toEqual({ text: "typed base hello", provisionalFrom: 11 });
		expect(painted.text.slice(11)).toBe("hello");
	});

	test("an empty base makes the whole draft provisional", () => {
		expect(paintDictation("", "", "hello")).toEqual({ text: "hello", provisionalFrom: 0 });
	});

	test("fully settled speech leaves nothing provisional", () => {
		expect(paintDictation("typed base", "Hello, world.", "")).toEqual({
			text: "typed base Hello, world.",
			provisionalFrom: null,
		});
	});

	test("an open mic with no speech does not append a separating space", () => {
		expect(paintDictation("typed base", "", "")).toEqual({
			text: "typed base",
			provisionalFrom: null,
		});
	});

	test("the settled phrase stays out of the provisional run", () => {
		const painted = paintDictation("typed base ", "Hi", "there");
		expect(painted.text).toBe("typed base Hi there");
		expect(painted.provisionalFrom).not.toBeNull();
		expect(painted.text.slice(painted.provisionalFrom!)).toBe("there");
	});
});
