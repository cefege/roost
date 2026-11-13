import { join, resolve } from "node:path";
import type { ExtensionAPI, ExtensionContext } from "@omp-source/packages/coding-agent/src/extensibility/extensions/types";
import { loadCases } from "./cases";
import { captureBuiltin } from "./builtin-runtime";
import { readProfile } from "./profile-manifest";
import { sourceLoader } from "./source-load";
import { probe } from "./probe";
import { objectValue } from "./builtin-capture-state";
import { createSecurityControlPoller } from "./builtin-capture-security-control";

export default async function parityExtension(api: ExtensionAPI): Promise<void> {
  const ownedWorkspace = process.env.OMP_PARITY_WORKSPACE;
  if (!ownedWorkspace) throw new Error("Trusted parity extension requires an explicitly owned workspace");
  const workspace: string = ownedWorkspace;
  const profile = await readProfile(workspace);
  if (process.env.HOME !== profile.home) throw new Error("Fixture extension escaped its isolated HOME");
  const load = sourceLoader(profile.sourceRoot);
  const providerUrl = process.env.OMP_PARITY_PROVIDER_URL;
  if (!providerUrl || new URL(providerUrl).hostname !== "127.0.0.1") throw new Error("Fixture provider must be the owned loopback HTTP adapter");
  const requirement = profile.scenario.requirements;
  const ids = requirement?.models.ids ?? ["parity-coder", "parity-small"];
  api.registerProvider("openai", {
    baseUrl: providerUrl, api: "openai-completions", apiKey: "parity-local-unused", authHeader: true,
    models: ids.map(id => ({ id, name: id, reasoning: requirement?.models.reasoning ?? true,
      input: requirement?.models.imageInput === false ? ["text"] : ["text", "image"],
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, contextWindow: 32768, maxTokens: 4096,
    })),
  });
  const cases = loadCases();
  const selected = cases.all.find(candidate => candidate.scenario.id === profile.scenario.id && candidate.kind === profile.scenario.kind);
  if (!selected) throw new Error(`Missing actual parity case ${profile.scenario.id}`);
  let currentContext: ExtensionContext | undefined;
  let capturing = false;
  let timer: NodeJS.Timeout | undefined;
  let securityTimer: NodeJS.Timeout | undefined;
  async function capture(): Promise<void> {
    if (!currentContext || selected?.kind !== "builtin" || capturing) return;
    capturing = true;
    try {
      const request = objectValue(await Bun.file(join(profile.home, ".parity/capture-request.json")).json() as unknown, "Current checkpoint capture request");
      if (!Array.isArray(request.channels) || request.channels.some(channel => typeof channel !== "string") || !Array.isArray(request.requests)) throw new Error("Invalid current checkpoint capture request");
      const requested = new Set<string>(request.channels as string[]);
      const observations: unknown[] = request.requests;
      const captures = observations.map(value => {
        const item = objectValue(value, "Current checkpoint capture observation");
        if (typeof item.channel !== "string" || !requested.has(item.channel) || (item.pointer !== undefined && typeof item.pointer !== "string") || (item.occurrence !== undefined && (typeof item.occurrence !== "number" || !Number.isInteger(item.occurrence) || item.occurrence < 0))) throw new Error("Invalid current checkpoint capture observation");
        const capture = selected.requirements.captures.find(capture => capture.channel === item.channel && capture.pointer === item.pointer);
        if (!capture) throw new Error(`Unauthored current checkpoint capture: ${item.channel}${item.pointer ?? ""}`);
        return capture;
      });
      if ([...requested].some(channel => !captures.some(capture => capture.channel === channel))) throw new Error("Current checkpoint capture channels lack observation requests");
      await captureBuiltin({ ...selected.requirements, captures }, {
        extensionContext: currentContext, workspace, load, record: (channel, value) => probe.record(channel, value),
      });
    } catch (error) {
      probe.record("builtin.capture-error", error instanceof Error ? error.message : String(error));
    } finally { capturing = false; }
  }
  api.on("session_start", async (_event, context) => {
    currentContext = context;
    if (selected.kind === "builtin") {
      clearInterval(securityTimer);
      const pollSecurity = createSecurityControlPoller({
        requirement: selected.requirements,
        context: { extensionContext: context, workspace, load, record: (channel, value) => probe.record(channel, value) },
        home: profile.home, ownedRoots: [resolve(profile.home), resolve(workspace)],
      }, selected.scenario.id);
      if (pollSecurity) {
        securityTimer = setInterval(() => { void pollSecurity().catch(error => probe.record("builtin.security-control-error", error instanceof Error ? error.message : String(error))); }, 25);
        securityTimer.unref();
      }
      clearInterval(timer);
      timer = setInterval(() => { void capture(); }, 100);
      timer.unref();
      await capture();
    }
    probe.record("fixture.session-ready", { id: selected.scenario.id });
  });
  api.on("session_shutdown", async () => { clearInterval(timer); clearInterval(securityTimer); timer = undefined; securityTimer = undefined; currentContext = undefined; });
  api.registerCommand("parity-fixture", {
    description: "Mount the owned differential parity fixture",
    async handler(argument, context) {
      if (selected.kind !== "fixture" || argument.trim() !== selected.scenario.id) throw new Error(`Fixture command does not match this owned case: ${argument}`);
      await context.ui.custom<unknown>(async (tui, theme, keybindings, done) => {
        const component = await selected.scenario.create({ tui, theme, keybindings, extensionContext: context, workspace, load,
          record: (channel, value) => probe.record(channel, value),
          done: value => { probe.record("fixture.done", { id: selected.scenario.id, value: value ?? null }); done(value); },
        });
        probe.record("fixture.ready", { id: selected.scenario.id });
        return component;
      }, selected.scenario.presentation ?? { overlay: true });
    },
  });
}
