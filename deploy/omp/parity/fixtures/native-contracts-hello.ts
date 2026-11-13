import { writeFileSync } from "node:fs";
import type * as Ui from "@omp-source/packages/tui/src/index";
import type * as Describe from "@omp-source/packages/tui/src/native/describe";
import type { NativeAssertion, NativeContract } from "../coverage-native";
import type { BrowserAction, Checkpoint, FixtureCase, FixtureContext } from "../scenario";
import { domText, eventContract, eventProperty, fileState } from "./native-contracts-support";

const probeSelector = '[data-omp-node$=".probe"]';
const statePath = "native-hello.json";
const defaultFeatures = ["send", "undo", "scroll"];

interface Negotiated { undo: boolean; reduceMotion: boolean; dark: boolean }
const label = (state: Negotiated) => `undo=${state.undo} reduceMotion=${state.reduceMotion} dark=${state.dark}`;

async function createHelloHost(context: FixtureContext) {
  const { Text } = await context.load<typeof Ui>("packages/tui/src/index.ts");
  const { node } = await context.load<typeof Describe>("packages/tui/src/native/describe.ts");
  class HelloProbe extends Text {
    describe(cx: { dark: boolean; reduceMotion: boolean; hour12?: boolean; feature(name: string): boolean; supports(kind: string): boolean }) {
      const snapshot = {
        undo: cx.feature("undo"), send: cx.feature("send"), media: cx.supports("media"), dark: cx.dark, reduceMotion: cx.reduceMotion,
        hour12: typeof cx.hour12 === "boolean" ? cx.hour12 : null,
      };
      writeFileSync(`${context.workspace}/${statePath}`, JSON.stringify(snapshot));
      context.record("native-hello", snapshot);
      return node("col", {}, [node("text", { text: label(snapshot), wrap: "none" }, undefined, "probe")]);
    }
  }
  return new HelloProbe("native hello probe", 0, 0);
}

const helloRequest = (match: Record<string, unknown>, pointer?: string) =>
  ({ verb: "r", direction: "in", occurrence: "last", match: { "/r": "hello", ...match }, ...(pointer ? { pointer } : {}) }) as const;

function helloContracts(match: Record<string, unknown>, fields: Record<string, unknown>, assertions: NativeAssertion[]): NativeContract[] {
  return [
    { entry: "reply:hello", request: helloRequest(match), assertions },
    ...Object.entries(fields).map(([field, value]): NativeContract => ({
      entry: `property:TspReply.${field}`, request: helloRequest(match, `/${field}`), ...(value === undefined ? {} : { value }), assertions,
    })),
  ];
}

const negotiated = (state: Negotiated): NativeAssertion[] => [
  domText(probeSelector, label(state)),
  fileState(statePath, "/undo", state.undo), fileState(statePath, "/reduceMotion", state.reduceMotion), fileState(statePath, "/dark", state.dark), fileState(statePath, "/hour12", true),
];

export const helloCases = (): FixtureCase[] => {
  const checkpoints: Checkpoint[] = [];
  const step = (name: string, browserActions: BrowserAction[], contracts: NativeContract[]) =>
    checkpoints.push({ name, browserActions, comparisons: [], nativeContracts: contracts });
  const full: Negotiated = { undo: true, reduceMotion: false, dark: true };
  step("the client's opening hello negotiates version, terminal, vocabulary and presentation", [], helloContracts(
    { "/features": defaultFeatures, "/reduceMotion": false },
    { v: 1, term: "acp", cols: undefined, kinds: undefined, features: defaultFeatures, dark: true, reduceMotion: false, hour12: true },
    negotiated(full),
  ));
  const narrowed: Negotiated = { ...full, undo: false };
  step("a re-sent hello narrows the negotiated features and re-describes the surface", [{ type: "nativeOpen", features: ["send"] }], helloContracts(
    { "/features": ["send"] }, { features: ["send"] }, negotiated(narrowed),
  ));
  const reduced: Negotiated = { ...full, reduceMotion: true };
  step("a client that prefers reduced motion reports it in its hello", [{ type: "emulateMedia", reducedMotion: "reduce" }, { type: "reload" }], helloContracts(
    { "/features": defaultFeatures, "/reduceMotion": true }, { reduceMotion: true }, negotiated(reduced),
  ));
  const light: Negotiated = { ...reduced, dark: false };
  step("a browser that follows a light system theme reports a light hello after the real settings switch", [
    { type: "press", key: "Control+," },
    { type: "click", selector: 'span[title^="Theme, color scheme"]' },
    { type: "click", selector: '[data-testid="theme-base-select"]' },
    { type: "click", selector: '[role="option"][data-key="neutral"]' },
    { type: "click", selector: '[data-testid="theme-mode-select"]' },
    { type: "click", selector: '[role="option"][data-key="light"]' },
    { type: "click", selector: '[data-testid^="sidebar-agent-conversation-"]' },
  ], helloContracts({ "/dark": false }, { dark: false }, negotiated(light)));
  const themeEvent = (name: string, scheme: "dark" | "light") => step(name, [{ type: "emulateMedia", colorScheme: scheme }], [
    eventContract("theme", negotiated({ ...reduced, dark: scheme === "dark" }), { "/dark": scheme === "dark" }),
    eventProperty("theme", "dark", { "/dark": scheme === "dark" }, scheme === "dark", negotiated({ ...reduced, dark: scheme === "dark" })),
  ]);
  themeEvent("the real system theme turning dark is reported as a theme event", "dark");
  themeEvent("the real system theme turning light is reported as a theme event", "light");
  const twentyFour = { id: "native-hello-twenty-four-hour-clock-reply", covers: [], browserLocale: "en-GB", create: createHelloHost, checkpoints: [{
    name: "a browser in a 24-hour locale reports hour12 false and the canonical context receives it",
    comparisons: [], nativeContracts: helloContracts({ "/hour12": false }, { hour12: false }, [
      domText(probeSelector, label(full)), fileState(statePath, "/hour12", false), fileState(statePath, "/dark", true),
    ]),
  }] } satisfies FixtureCase;
  return [{ id: "native-hello-reply-negotiation", covers: [], create: createHelloHost, checkpoints }, twentyFour];
};
