import { afterAll, describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { relayOwnedValues } from "./coverage-relay";
import { sourceAudit } from "./coverage-lanes";
import { classifyRelayOwnedReplies } from "./inventory-relay-owned";
import { SourceIndex } from "./inventory-source";
import type { InventoryEntry } from "./inventory-types";

const root = mkdtempSync(path.join(tmpdir(), "parity-relay-"));
afterAll(() => rmSync(root, { recursive: true, force: true }));

const ENCODE = `export type TspVerb = "q" | "o" | "f" | "b" | "t" | "x" | "r" | "e";
export function encodeTspMessage(verb: TspVerb, body: string): string { return verb + body; }
export function encodeTspJson(verb: TspVerb, value: unknown): string { return encodeTspMessage(verb, JSON.stringify(value)); }
`;
const BACKEND = `import { encodeTspJson } from "./encode";
export interface NativeTransportMessage { readonly verb: "o" | "f" | "x" | "q" | "b" | "t"; readonly body: unknown }
export function send(message: NativeTransportMessage): string { return encodeTspJson(message.verb, message.body); }
`;
const RELAY = `import { encodeTspJson } from "../../../../tui/src/native/encode";
export const answer = (): string => encodeTspJson("r", { r: "blobs", have: [] });
`;

function tree(extra: Record<string, string> = {}, relay = RELAY): SourceIndex {
	const dir = mkdtempSync(path.join(root, "tree-"));
	for (const sub of ["packages/tui/src/native", "packages/coding-agent/src/modes/acp"]) mkdirSync(path.join(dir, sub), { recursive: true });
	writeFileSync(path.join(dir, "packages/tui/src/native/encode.ts"), ENCODE);
	writeFileSync(path.join(dir, "packages/tui/src/native/backend.ts"), BACKEND);
	writeFileSync(path.join(dir, "packages/coding-agent/src/modes/acp/relay.ts"), relay);
	for (const [name, text] of Object.entries(extra)) writeFileSync(path.join(dir, name), text);
	return new SourceIndex(dir);
}

const location = { source: "packages/wire/src/tsp.ts", line: 1 };
function entries(): InventoryEntry[] {
	const base = { source: location.source, line: 1 };
	return [
		{ id: "reply:blobs", category: "reply", ...base, details: { exposure: "native-contract", exposureProof: location, discriminator: { key: "r", value: "blobs" } } },
		{ id: "property:TspReply.have", category: "property", ...base, details: { declaringType: "TspReply", path: "have", exposure: "native-contract", declarations: [{ ...location, variants: ["r:blobs"] }] } },
		{ id: "property:TspReply.r", category: "property", ...base, details: { declaringType: "TspReply", path: "r", exposure: "native-contract", enumValues: ["hello", "blobs"], declarations: [{ ...location, variants: ["r:hello", "r:blobs"] }] } },
		{ id: "reply:hello", category: "reply", ...base, details: { exposure: "native-contract", exposureProof: location, discriminator: { key: "r", value: "hello" } } },
	];
}
const host = (...keys: string[]) => ({ keys: new Set(keys), files: 4 });
const exposures = (list: InventoryEntry[]) => Object.fromEntries(list.map(entry => [entry.id, entry.details.exposure]));

describe("replies only the ACP relay writes", () => {
	test("the relay's reply, its exclusive member and its discriminator value are relay-owned; other replies stay", () => {
		const list = entries();
		classifyRelayOwnedReplies(list, tree(), host());
		expect(exposures(list)).toEqual({ "reply:blobs": "unproduced", "property:TspReply.have": "unproduced", "property:TspReply.r": "native-contract", "reply:hello": "native-contract" });
		const [reply, , shared] = list;
		expect(sourceAudit(reply!)?.reason).toBe("unproduced-wire-member");
		expect(relayOwnedValues(shared!)).toEqual(["blobs"]);
		expect(sourceAudit(list[1]!)?.reason).toBe("unproduced-wire-member");
	});

	test("an encoder call outside the relay that could write a reply keeps every entry demanded", () => {
		for (const extra of [
			{ "packages/tui/src/other.ts": `import { encodeTspJson } from "./native/encode";\nexport const x = (): string => encodeTspJson("r", { r: "blobs", have: ["a"] });\n` },
			{ "packages/tui/src/other.ts": `import { encodeTspJson, type TspVerb } from "./native/encode";\nexport const x = (verb: TspVerb): string => encodeTspJson(verb, {});\n` },
			{ "packages/tui/src/other.ts": `import { encodeTspMessage } from "./native/encode";\nexport const x = (): string => encodeTspMessage("e", "{}");\n` },
		]) {
			const list = entries();
			classifyRelayOwnedReplies(list, tree(extra), host());
			expect(exposures(list)["reply:blobs"]).toBe("native-contract");
			expect(relayOwnedValues(list[2]!)).toEqual([]);
		}
	});

	test("a relay call that does not spell its tag, or a host that spells it, is not classified", () => {
		const opaque = `import { encodeTspJson } from "../../../../tui/src/native/encode";\nexport const answer = (body: object): string => encodeTspJson("r", body);\n`;
		const list = entries();
		classifyRelayOwnedReplies(list, tree({}, opaque), host());
		expect(exposures(list)["reply:blobs"]).toBe("native-contract");
		const spelled = entries();
		classifyRelayOwnedReplies(spelled, tree(), host("blobs"));
		expect(exposures(spelled)["reply:blobs"]).toBe("native-contract");
	});

	test("a forged proof outside the relay, or one without a write, is rejected by the audit", () => {
		const list = entries();
		classifyRelayOwnedReplies(list, tree(), host());
		const reply = list[0]!;
		const proof = reply.details.unproducedProof as { relay: { writes: Array<{ at: { source: string } }> } };
		proof.relay.writes[0]!.at.source = "packages/tui/src/native/backend.ts";
		expect(() => sourceAudit(reply)).toThrow(/lacks its key-write search proof/);
		proof.relay.writes = [];
		expect(() => sourceAudit(reply)).toThrow(/lacks its key-write search proof/);
		const shared = entries()[2]!;
		shared.details.relayOwnedValues = { values: ["blobs"], proof: "relay-owned-reply", reply: "reply:blobs", relay: { tag: "blobs", writes: [], calls: 0, verbDeclaration: location } };
		expect(relayOwnedValues(shared)).toEqual([]);
	});
});
