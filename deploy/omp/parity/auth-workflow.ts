// Owns checkpoint-time participation by the isolated OAuth authorization peer.
// Reads actual source host-open/listener records and delivers only its owned callback.
// The real source PKCE engine, manual dialog and fixed callback listener stay unchanged.
import { randomUUID } from "node:crypto";
import { watch } from "node:fs";
import { appendFile, readFile, rename, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { objectValue } from "./builtin-capture-state";
import { authWorkflowHooks } from "./builtin-auth-workflow-requirements";
import { assertOwned, readProfile, type ProfileManifest } from "./profile-manifest";

export interface AuthWorkflowClient {
  beforeInput(checkpoint: number): Promise<Record<string, string>>;
  beforeObservation(checkpoint: number): Promise<void>;
}

interface ActualAuthListener {
  listenerId: string;
  pid: number;
  generation: number;
  origin: string;
}

async function traceRows(path: string): Promise<Record<string, unknown>[]> {
  const text = await readFile(path, "utf8");
  const lines = text.split("\n");
  lines.pop();
  return lines.filter(Boolean).map(line => objectValue(JSON.parse(line) as unknown, "Actual OAuth transport/host record"));
}

async function observeAuthEvent<Value>(directory: string, description: string, observe: () => Promise<Value | undefined>): Promise<Value> {
  return new Promise((resolveObservation, rejectObservation) => {
    let reading = false;
    let changed = false;
    let settled = false;
    const watcher = watch(directory, { persistent: false }, () => {
      changed = true;
      void inspect();
    });
    const deadline = setTimeout(() => finish(new Error(`Timed out awaiting actual source OAuth ${description}`)), 15000);
    function finish(error: unknown, value?: Value): void {
      if (settled) return;
      settled = true;
      clearTimeout(deadline);
      watcher.close();
      if (error !== undefined) rejectObservation(error);
      else if (value !== undefined) resolveObservation(value);
      else rejectObservation(new Error("OAuth event observer completed without an observation"));
    }
    async function inspect(): Promise<void> {
      if (reading || settled) return;
      reading = true;
      do {
        changed = false;
        try {
          const value = await observe();
          if (value !== undefined) finish(undefined, value);
        } catch (error) { finish(error); }
      } while (changed && !settled);
      reading = false;
    }
    watcher.on("error", error => finish(error));
    void inspect();
  });
}

export function createAuthWorkflow(profile: Pick<ProfileManifest, "home" | "workspace">, scenarioId: string): AuthWorkflowClient | undefined {
  const hook = authWorkflowHooks[scenarioId];
  const declaredTransport = hook?.oauthTransport;
  if (!declaredTransport) return undefined;
  const transport = declaredTransport;
  if (!hook.service?.oauth) throw new Error("OAuth workflow has no declared owned token endpoint");
  const oauth = hook.service.oauth;
  const directory = join(resolve(profile.home), ".parity");
  const paths = { host: join(directory, "host.jsonl"), network: join(directory, "network.jsonl"), generation: join(directory, "process-generation.json"), bindings: join(directory, "auth-bindings.json"), control: join(directory, "auth-control.jsonl") };
  for (const path of Object.values(paths)) assertOwned(profile.home, path);
  let listener: ActualAuthListener | undefined;
  let delivered = false;
  let bound = false;
  let busy = false;
  const bindings: Record<string, string> = {};

  async function actualAuthorization(): Promise<{ redirect: URL; listener: ActualAuthListener } | undefined> {
    const manifest = await readProfile(profile.workspace);
    if (manifest.scenario.id !== scenarioId || resolve(manifest.home) !== resolve(profile.home)) throw new Error("OAuth checkpoint belongs to a different owned scenario");
    const generation = objectValue(await Bun.file(paths.generation).json() as unknown, "Actual OAuth source process generation");
    if (typeof generation.pid !== "number" || !Number.isSafeInteger(generation.pid) || generation.pid < 2 || typeof generation.generation !== "number" || !Number.isSafeInteger(generation.generation) || generation.generation < 1 || generation.workspace !== manifest.workspace || generation.sourceRoot !== manifest.sourceRoot) throw new Error("OAuth process generation is not the current owned source process");
    const host = await traceRows(paths.host);
    const authorize = host.filter(event => event.channel === "builtin.host-open-url" && typeof event.value === "string" && URL.canParse(event.value) && event.pid === generation.pid && event.generation === generation.generation).map(event => new URL(String(event.value))).findLast(url => url.origin === "https://auth.openai.com" && url.pathname === "/oauth/authorize");
    if (!authorize) return undefined;
    if (authorize.username || authorize.password || authorize.hash) throw new Error("Actual source authorize URL contains unsupported credentials/fragment");
    const state = authorize.searchParams.get("state");
    const challenge = authorize.searchParams.get("code_challenge");
    if (!state || !challenge || authorize.searchParams.get("code_challenge_method") !== "S256" || authorize.searchParams.get("client_id") !== "app_EMoamEEZ73f0CkXaXp7hrann" || authorize.searchParams.get("response_type") !== "code") throw new Error("Actual source authorize URL lacks canonical state/PKCE/client fields");
    const redirect = new URL(authorize.searchParams.get("redirect_uri") ?? "");
    if (redirect.href !== "http://localhost:1455/auth/callback") throw new Error("Source OAuth redirect differs from its canonical fixed callback address");
    const events = await traceRows(paths.network);
    const registration = events.findLast((event, index) => event.type === "owned-listener-registered" && event.source === transport.callbackSource && event.origin === redirect.origin && event.pid === generation.pid && event.generation === generation.generation && typeof event.listenerId === "string" && !events.slice(index + 1).some(closed => closed.type === "owned-listener-closed" && closed.listenerId === event.listenerId && closed.pid === event.pid && closed.generation === event.generation));
    if (!registration || typeof registration.listenerId !== "string") return undefined;
    process.kill(generation.pid, 0);
    redirect.searchParams.set("code", oauth.expectedCode);
    redirect.searchParams.set("state", state);
    return { redirect, listener: { listenerId: registration.listenerId, pid: generation.pid, generation: generation.generation, origin: redirect.origin } };
  }

  async function bindActualAuthorization(): Promise<URL> {
    const actual = await observeAuthEvent(directory, "current-generation authorization/listener readiness", actualAuthorization);
    if (listener && (listener.listenerId !== actual.listener.listenerId || listener.pid !== actual.listener.pid || listener.generation !== actual.listener.generation)) throw new Error("Actual OAuth listener changed before checkpoint delivery");
    listener = actual.listener;
    if (bound && bindings[transport.redirectBinding] !== actual.redirect.href) throw new Error("Actual OAuth redirect binding changed");
    if (!bound) {
      bindings[transport.redirectBinding] = actual.redirect.href;
      const temporary = `${paths.bindings}.${randomUUID()}.tmp`;
      await writeFile(temporary, `${JSON.stringify(bindings)}\n`, { mode: 0o600, flag: "wx" });
      await rename(temporary, paths.bindings);
      bound = true;
    }
    return actual.redirect;
  }

  async function retired(): Promise<void> {
    if (!listener) throw new Error("OAuth flow completed without observing its actual listener");
    await observeAuthEvent(directory, "callback listener retirement", async () => {
      const events = await traceRows(paths.network);
      const closed = events.some(event => event.type === "owned-listener-closed" && event.listenerId === listener?.listenerId && event.pid === listener?.pid && event.generation === listener?.generation);
      const registrations = events.filter(event => event.type === "owned-listener-registered" && event.source === transport.callbackSource && event.pid === listener?.pid && event.generation === listener?.generation && typeof event.origin === "string" && new URL(event.origin).port === String(transport.callbackPort));
      const stillOpen = registrations.some(registration => !events.some(event => event.type === "owned-listener-closed" && event.listenerId === registration.listenerId && event.pid === registration.pid && event.generation === registration.generation));
      return closed && !stillOpen ? true : undefined;
    });
  }

  async function exclusive(checkpoint: number, action: () => Promise<void>): Promise<Record<string, string>> {
    if (!Number.isInteger(checkpoint) || checkpoint < 0 || busy) throw new Error("Invalid or overlapping OAuth checkpoint");
    busy = true;
    try { await action(); return { ...bindings }; }
    finally { busy = false; }
  }

  return {
    async beforeInput(checkpoint) {
      return exclusive(checkpoint, async () => {
        if (checkpoint !== transport.deliveryBeforeCheckpoint) return;
        const redirect = await bindActualAuthorization();
        if (transport.delivery !== "owned-http-callback") return;
        if (delivered) throw new Error("Owned OAuth HTTP callback was already delivered");
        delivered = true;
        const response = await fetch(redirect, { redirect: "manual", credentials: "omit", signal: AbortSignal.timeout(15000) });
        await response.body?.cancel();
        await appendFile(paths.control, `${JSON.stringify({ type: "actual-auth-callback-response", scenarioId, checkpoint, status: response.status, listenerId: listener?.listenerId, pid: listener?.pid, generation: listener?.generation, origin: redirect.origin, path: redirect.pathname })}\n`);
        if (response.status !== 200) throw new Error(`Actual canonical OAuth callback returned HTTP${response.status}`);
      });
    },
    async beforeObservation(checkpoint) {
      await exclusive(checkpoint, async () => {
        if (checkpoint === 0) await bindActualAuthorization();
        else await retired();
      });
    },
  };
}
