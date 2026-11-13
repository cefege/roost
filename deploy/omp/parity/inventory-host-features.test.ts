import { describe, expect, test } from "bun:test";
import { provenanceReasons } from "./coverage-provenance";
import { classifyUnadvertisedFeatures, hostAdvertisedFeatures } from "./inventory-host-features";
import type { InventoryEntry } from "./inventory-types";
import type { ProbeVisit } from "./probe";

const source = "packages/tui/src/overlays/advisor-config.ts";
const HOST = { features: new Set(["send", "undo", "scroll"]), source: "crates/roost-web/host.rs", line: 19 };
const feature = (name: string): InventoryEntry => ({ id: `feature:${name}`, category: "feature", source, line: 474, details: { name, openVocabulary: true, checks: [{ source, line: 474, expression: `cx.feature("${name}")` }] } });
const check = (adapter: "terminal" | "browser", name: string, line = 474): ProbeVisit => ({ type: "visit", component: `${source}#AdvisorConfig`, method: "feature", source, line, adapter, pid: 1, generation: "g", sequence: 1, arguments: [name] });
const classified = (name: string) => { const entry = feature(name); classifyUnadvertisedFeatures([entry], HOST); return entry; };
const hello = new Set(["advertised-feature:send", "advertised-feature:undo", "advertised-feature:scroll"]);

describe("unadvertised host features", () => {
	test("only a feature the host's open action omits is marked, with that action's location", () => {
		expect(classified("aside").details.hostFeatures).toEqual({ advertised: ["scroll", "send", "undo"], source: HOST.source, line: HOST.line });
		expect(classified("send").details.hostFeatures).toBeUndefined();
	});

	test("the real Roost host advertises send, undo and scroll from its single open action", () => {
		const host = hostAdvertisedFeatures();
		expect(host.source).toBe("crates/roost-web/src/components/agent_chat/omp_ui_mount_transport.rs");
		expect([...host.features].sort()).toEqual(["scroll", "send", "undo"]);
	});

	test("an executed browser check against a hello that omits the feature is the evidence", () => {
		expect(provenanceReasons(classified("aside"), [check("browser", "aside")], hello)).toEqual([]);
	});

	test("a check no browser executed is not evidence, nor is a terminal-only visit", () => {
		expect(provenanceReasons(classified("aside"), [], hello)[0]).toContain("No browser execution of the unadvertised-feature check");
		expect(provenanceReasons(classified("aside"), [check("terminal", "aside")], hello)[0]).toContain("No browser execution");
		expect(provenanceReasons(classified("aside"), [check("browser", "aside", 999)], hello)[0]).toContain("No browser execution");
	});

	test("a run with no hello cannot show what the host negotiated", () => {
		expect(provenanceReasons(classified("aside"), [check("browser", "aside")], new Set())).toEqual(["No hello reply in the run shows what the host negotiated"]);
	});

	test("a hello that lists a feature the host does not advertise rejects the claim", () => {
		expect(provenanceReasons(classified("aside"), [check("browser", "aside")], new Set([...hello, "advertised-feature:popout"]))[0]).toContain("does not list");
	});

	test("a run that did negotiate the feature falls back to the ordinary negotiated-feature rule", () => {
		const negotiated = new Set([...hello, "advertised-feature:aside"]);
		expect(provenanceReasons(classified("aside"), [check("browser", "aside")], negotiated)).toEqual([]);
		expect(provenanceReasons(classified("aside"), [], negotiated)).toEqual(["No negotiated feature and executed canonical check with its actual name"]);
	});

	test("an entry whose recorded advertisement lists the name gets no exemption", () => {
		const forged = classified("aside");
		forged.details.hostFeatures = { advertised: ["aside", "send"], source: HOST.source, line: HOST.line };
		expect(provenanceReasons(forged, [check("browser", "aside")], hello)).toEqual(["No negotiated feature and executed canonical check with its actual name"]);
	});
});
