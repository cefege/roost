import { resolve } from "node:path";
import type { BuiltinRequirements } from "./builtin-requirements";
import type { BuiltinCaptureContext, CaptureState } from "./builtin-capture-state";
import { builtinSeedManifestPath, readBuiltinSeedManifest } from "./builtin-seed";
import { boundaryValue, ownedPath, requestedPointers } from "./builtin-capture-state";
import { captureApprovals, captureFiles, captureRules, captureSettings } from "./builtin-capture-files";
import { captureMarketplaces, capturePlugins, captureSecurity, captureSkills, captureSmithery, captureSsh } from "./builtin-capture-packages";
import { captureBackground, captureBtw, captureRecording, captureSession } from "./builtin-capture-session";
import { captureGit, captureHttp, captureMcp } from "./builtin-capture-boundaries";
import { captureMemory } from "./builtin-capture-memory";
import { captureCollab } from "./builtin-capture-collab";
import { captureAuth } from "./builtin-capture-auth";
import { probe } from "./probe";
export type { BuiltinCaptureContext, BuiltinCaptureRequest } from "./builtin-capture-state";

export async function captureBuiltin(requirement: BuiltinRequirements, ctx: BuiltinCaptureContext): Promise<void> {
  const home = process.env.HOME;
  if (!home) throw new Error("Builtin capture requires an isolated HOME");
  const state: CaptureState = { requirement, context: ctx, home, ownedRoots: [resolve(home), resolve(ctx.workspace)] };
  await ownedPath(state, builtinSeedManifestPath(home));
  const manifest = await readBuiltinSeedManifest(home);
  const session = probe.requireSession();
  await ownedPath(state, session.settings.getAgentDir());
  if (ctx.extensionContext.sessionManager.getSessionId() !== session.sessionManager.getSessionId()) {
    throw new Error("Builtin capture extension context does not belong to the actual SDK session");
  }
  const channels = new Set(requirement.captures.map(capture => capture.channel));
  for (const channel of channels) {
    let value: unknown;
    switch (channel) {
      case "builtin.session": value = await captureSession(state, session, manifest); break;
      case "builtin.persisted-settings": value = await captureSettings(state, session, manifest.settingsPath); break;
      case "builtin.todos": value = session.getTodoPhases(); break;
      case "builtin.goal": value = session.getGoalModeState()?.goal ?? null; break;
      case "builtin.btw": value = await captureBtw(state, session); break;
      case "builtin.background": value = await captureBackground(state, session); break;
      case "builtin.workspace-file": value = await captureFiles(state, session, false); break;
      case "builtin.local-file": value = await captureFiles(state, session, true); break;
      case "builtin.plugins": value = await capturePlugins(state); break;
      case "builtin.marketplaces": value = await captureMarketplaces(state); break;
      case "builtin.plugin-reloads": value = probe.pluginReloadsCompleted; break;
      case "builtin.skills": value = await captureSkills(state); break;
      case "builtin.security": value = await captureSecurity(state); break;
      case "builtin.collab": value = await captureCollab(state, manifest); break;
      case "builtin.auth": value = await captureAuth(state); break;
      case "builtin.auth-http": value = await boundaryValue(state, "OMP_PARITY_SERVICE_TRACE", channel); break;
      case "builtin.memory": value = await captureMemory(state, session, manifest.settingsPath); break;
      case "builtin.memory-http": value = await boundaryValue(state, "OMP_PARITY_SERVICE_TRACE", channel); break;
      case "builtin.ssh": value = await captureSsh(state); break;
      case "builtin.mcp": value = await captureMcp(state); break;
      case "builtin.smithery": value = await captureSmithery(state); break;
      case "builtin.recording": value = await captureRecording(state); break;
      case "builtin.approvals": value = await captureApprovals(state, session); break;
      case "builtin.rules": value = await captureRules(state); break;
      case "builtin.git": value = await captureGit(state); break;
      case "builtin.http": value = await captureHttp(state); break;
      case "builtin.provider": value = await boundaryValue(state, "OMP_PARITY_PROVIDER_TRACE", channel); break;
      case "builtin.process": value = await boundaryValue(state, "OMP_PARITY_PROCESS_TRACE", channel); break;
      case "builtin.live": value = await boundaryValue(state, "OMP_PARITY_LIVE_TRACE", channel); break;
      case "builtin.clipboard":
      case "builtin.host-open-url": value = await boundaryValue(state, "OMP_PARITY_HOST_TRACE", channel); break;
      default: throw new Error(`Unsupported builtin capture source: ${channel}`);
    }
    if (value === undefined) throw new Error(`Actual builtin capture returned undefined: ${channel}`);
    for (const parts of requestedPointers(state, channel)) {
      let current: unknown = value;
      for (const part of parts) {
        if (typeof current !== "object" || current === null || !Object.hasOwn(current, part)) throw new Error(`Missing actual builtin capture field ${channel}/${parts.join("/")}`);
        current = (current as Record<string, unknown>)[part];
      }
      if (current === undefined) throw new Error(`Undefined actual builtin capture field ${channel}/${parts.join("/")}`);
    }
    ctx.record(channel, value);
  }
}
