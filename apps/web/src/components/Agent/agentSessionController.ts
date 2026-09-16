// Run lifecycle for one selected Mecatl session: load the authoritative
// transcript, stream a run's events into the timeline, answer permission asks,
// cancel, steer, and follow a run that was already in flight after a reload.
// Created and disposed by AgentPane.tsx; folds through agentTimeline.ts.
//
// Exactly one stream folds into the timeline at a time — a Run this pane owns,
// or a durable attachment to a run it does not — because both carry the same
// events and folding both renders every delta, tool card and ask twice.
//
// Every cursor and event lives in this closure only. Roost stores no agent
// conversation state, so a reload legitimately re-reads Mecatl.

import { createSignal, type Accessor } from "solid-js";
import { diag } from "@roost/shared/diag";
import {
  NoRunsError,
  type AttachedRun,
  type Client,
  type PermissionVerdict,
  type Run,
  type SdkCursor,
  type Session,
} from "@stacklok-oss/mecatl-sdk";
import { classifyRelayFailure, isAbortFailure, type AgentRelayFailure } from "./mecatlClient.ts";
import { createAgentTimeline, type AgentTimeline } from "./agentTimeline.ts";

export type AgentRunStatus = "loading" | "idle" | "running" | "cancelling";

// Mecatl's lifecycle states that mean a run is still in flight. A positive list
// on purpose: an unrecognized future state must NOT open a durable attachment
// that would then never see a terminal result.
const LIVE_SESSION_STATES: Record<string, true | undefined> = {
  running: true,
  awaiting: true,
};

export interface AgentSessionController {
  readonly timeline: AgentTimeline;
  readonly status: Accessor<AgentRunStatus>;
  readonly failure: Accessor<AgentRelayFailure | null>;
  /**
   * True when this pane started the live run. Permission verdicts and steering
   * are only possible then: the SDK refuses both on a durable attachment.
   */
  readonly ownsRun: Accessor<boolean>;
  /** True while a run this pane owns is live and the server accepts steering. */
  readonly canSteer: Accessor<boolean>;
  prompt(text: string): Promise<void>;
  steer(text: string): Promise<void>;
  cancel(): Promise<void>;
  answerAsk(askId: string, verdict: PermissionVerdict): Promise<void>;
  /** Drops local render state and re-reads Mecatl. Refused while a run is live. */
  refresh(): Promise<void>;
  dispose(): void;
}

export interface AgentSessionControllerOptions {
  client: Client;
  sessionId: string;
  /** Whether this deployment advertised the http_steer server feature. */
  httpSteer: boolean;
}

export function createAgentSessionController(
  options: AgentSessionControllerOptions,
): AgentSessionController {
  const { client, sessionId, httpSteer } = options;
  const timeline = createAgentTimeline();
  const [status, setStatus] = createSignal<AgentRunStatus>("loading");
  const [failure, setFailure] = createSignal<AgentRelayFailure | null>(null);
  const [ownsRun, setOwnsRun] = createSignal(false);
  const lifetime = new AbortController();

  let session: Session | null = null;
  let activeRun: Run | null = null;
  let attachment: AttachedRun | null = null;
  let cursor: SdkCursor | null = null;

  async function resolveSession(): Promise<Session> {
    if (session !== null) return session;
    const loaded = await client.sessions.get(sessionId, { signal: lifetime.signal });
    session = loaded;
    return loaded;
  }

  function reportFailure(error: unknown, stage: string): void {
    if (isAbortFailure(error) || lifetime.signal.aborted) return;
    const classified = classifyRelayFailure(error);
    setFailure(classified);
    diag("agent.relay_failed", { stage, sid: sessionId, kind: classified.kind });
  }

  // Follows a run this pane did not start (page reload, or another device).
  // Verdicts stay unavailable here by SDK contract, so the pane renders the
  // ask read-only rather than offering a button that cannot work.
  async function followLiveRun(): Promise<void> {
    let handle: AttachedRun;
    try {
      handle = await (await resolveSession()).attach(undefined, {
        from: cursor ?? "start",
        signal: lifetime.signal,
      });
    } catch (error) {
      if (!(error instanceof NoRunsError)) reportFailure(error, "attach");
      return;
    }
    attachment = handle;
    setOwnsRun(false);
    setStatus("running");
    try {
      for await (const envelope of handle) {
        if (lifetime.signal.aborted) return;
        if (envelope.kind === "gap") {
          timeline.appendNotice("info", "Some activity could not be replayed.");
          continue;
        }
        cursor = envelope.cursor;
        // `boundary` marks the replay-to-live transition and carries no event;
        // `unknown` is a forward-compatible phase whose event may be absent.
        if (envelope.kind === "boundary") continue;
        const activity = envelope.event;
        if (activity === undefined) continue;
        timeline.applyEvent(activity);
        if (activity.kind === "result") break;
      }
    } catch (error) {
      reportFailure(error, "attach_stream");
    } finally {
      attachment = null;
      timeline.settleStreaming();
      await handle.close().catch(() => undefined);
      if (!lifetime.signal.aborted) setStatus("idle");
    }
  }

  async function consumeRun(run: Run): Promise<void> {
    activeRun = run;
    setOwnsRun(true);
    setStatus("running");
    try {
      for await (const event of run) {
        if (lifetime.signal.aborted) return;
        timeline.applyEvent(event);
      }
    } catch (error) {
      reportFailure(error, "run_stream");
    } finally {
      activeRun = null;
      setOwnsRun(false);
      timeline.settleStreaming();
      if (!lifetime.signal.aborted) setStatus("idle");
    }
  }

  async function load(): Promise<void> {
    setStatus("loading");
    let live = false;
    try {
      const resolved = await resolveSession();
      const transcript = await resolved.transcript({ signal: lifetime.signal });
      timeline.loadTranscript(transcript);
      const snapshot = await resolved.snapshot({ signal: lifetime.signal });
      live = LIVE_SESSION_STATES[snapshot.state] === true;
      setFailure(null);
      diag("agent.session_loaded", {
        sid: sessionId,
        messages: transcript.messages.length,
        state: snapshot.state,
      });
    } catch (error) {
      reportFailure(error, "load");
    }
    if (lifetime.signal.aborted) return;
    if (!live) {
      setStatus("idle");
      return;
    }
    // One render source at a time: the transcript above is dropped and the
    // durable log replayed instead, so a live run's deltas, tool cards and
    // asks arrive exactly once.
    timeline.clear();
    await followLiveRun();
  }

  void load();

  return {
    timeline,
    status,
    failure,
    ownsRun,
    canSteer: () => httpSteer && ownsRun() && status() === "running",
    async prompt(text: string): Promise<void> {
      if (status() !== "idle") return;
      setFailure(null);
      timeline.appendPrompt(text);
      let run: Run;
      try {
        run = await (await resolveSession()).run(text, {}, { signal: lifetime.signal });
      } catch (error) {
        timeline.settleStreaming();
        reportFailure(error, "run_start");
        return;
      }
      diag("agent.run_started", { sid: sessionId, run: run.id });
      await consumeRun(run);
    },
    async steer(text: string): Promise<void> {
      const run = activeRun;
      if (run === null || !httpSteer) return;
      try {
        await run.steer(text);
        diag("agent.run_steered", { sid: sessionId, run: run.id });
      } catch (error) {
        reportFailure(error, "steer");
      }
    },
    async cancel(): Promise<void> {
      const owned = activeRun;
      const followed = attachment;
      if (owned === null && followed === null) return;
      setStatus("cancelling");
      try {
        if (owned !== null) await owned.cancel();
        else if (followed !== null) await followed.cancel();
        diag("agent.run_cancelled", {
          sid: sessionId,
          run: owned?.id ?? followed?.runId ?? "",
        });
      } catch (error) {
        reportFailure(error, "cancel");
        setStatus("running");
      }
    },
    async answerAsk(askId: string, verdict: PermissionVerdict): Promise<void> {
      const run = activeRun;
      if (run === null) return;
      // Reflect the verdict immediately: the server's own `approval` event
      // confirms it, but the buttons must stop inviting a second click.
      timeline.resolveAsk(askId, verdict);
      try {
        await run.resolveAsk(askId, verdict);
        diag("agent.ask_resolved", { sid: sessionId, verdict });
      } catch (error) {
        reportFailure(error, "resolve_ask");
      }
    },
    async refresh(): Promise<void> {
      if (status() === "running" || status() === "cancelling") return;
      timeline.clear();
      cursor = null;
      await load();
    },
    dispose(): void {
      lifetime.abort();
      activeRun = null;
      attachment = null;
      void session?.close().catch(() => undefined);
      session = null;
    },
  };
}
