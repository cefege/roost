import { describe, expect, test } from "bun:test";
import { classifyHostUnproduced } from "./inventory-host-audit";
import type { InventoryEntry } from "./inventory-types";

const member = (declaringType: string, path: string, emitters: Record<string, unknown>[] = []): InventoryEntry => ({
	id: `property:${declaringType}.${path}`,
	category: "property",
	source: "packages/wire/src/tsp.ts",
	line: 1,
	details: { declaringType, path, exposure: "native-contract", declarations: [{ source: "packages/wire/src/tsp.ts", line: 1 }], emitters },
});

describe("host producer audit", () => {
	test("an inbound member no host source writes is unproduced, and so are its fields", () => {
		const cell = member("TspReply", "cell");
		const width = member("TspReply", "cell.w");
		classifyHostUnproduced([width, cell], { keys: new Set(["cols"]), files: 3 });
		expect(cell.details.exposure).toBe("unproduced");
		expect(cell.details.unproducedProof).toMatchObject({ proof: "host-never-writes", key: "cell" });
		expect(width.details.unproducedProof).toMatchObject({ proof: "parent-never-written", parent: cell.id, key: "w" });
	});

	test("a key the Rust host writes keeps its obligation", () => {
		const cols = member("TspReply", "cols");
		classifyHostUnproduced([cols], { keys: new Set(["cols"]), files: 3 });
		expect(cols.details.exposure).toBe("native-contract");
	});

	test("a write in OMP's ACP bridge keeps its obligation; a terminal-transport write does not", () => {
		const bridged = member("TspReply", "hour12", [{ role: "typed-property-emitter", source: "packages/coding-agent/src/modes/acp/remote-command-ui.ts", line: 9 }]);
		const terminal = member("TspReply", "apc", [{ role: "typed-property-emitter", source: "packages/tui/src/native/backend.ts", line: 233 }]);
		classifyHostUnproduced([bridged, terminal], { keys: new Set(), files: 3 });
		expect(bridged.details.exposure).toBe("native-contract");
		expect(terminal.details.unproducedProof).toMatchObject({ proof: "host-never-writes", nonHostWrites: ["packages/tui/src/native/backend.ts:233"] });
	});

	test("outbound members are never host-audited", () => {
		const outbound = member("TspNode", "k");
		classifyHostUnproduced([outbound], { keys: new Set(), files: 3 });
		expect(outbound.details.exposure).toBe("native-contract");
	});
});
