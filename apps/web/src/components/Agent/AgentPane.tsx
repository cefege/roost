// The /agent surface: picks a machine, lists that machine's Mecatl sessions,
// and drives one conversation through the coordinator relay. Mounted as an
// overlay by MainPane, so the terminal deck stays alive behind it. Owns the
// per-machine SDK client and the per-session controller lifetimes; every
// durable fact (sessions, transcript, approvals) belongs to Mecatl, not Roost.

import { useNavigate, useParams } from "@solidjs/router";
import {
  For,
  Show,
  createEffect,
  createMemo,
  createResource,
  createSignal,
  onCleanup,
  untrack,
} from "solid-js";
import type { Client } from "@stacklok-oss/mecatl-sdk";
import type { ListSessionsRequest } from "@stacklok-oss/mecatl-sdk/gen";
import { Button } from "../Settings/md/Button.tsx";
import { EmptyState } from "../Settings/md/primitives.tsx";
import { Surface } from "../Settings/md/Surface.tsx";
import { agentHref } from "../../routes.ts";
import { AgentComposer } from "./AgentComposer.tsx";
import { AgentMachinePicker, agentMachineOptions } from "./AgentMachinePicker.tsx";
import { AgentSessionList, type AgentSessionRow } from "./AgentSessionList.tsx";
import { AgentTranscript } from "./AgentTranscript.tsx";
import { lastAgentMachine, rememberAgentMachine } from "./agentMachineMemory.ts";
import {
  createAgentSessionController,
  type AgentSessionController,
} from "./agentSessionController.ts";
import {
  classifyRelayFailure,
  createMecatlClient,
  serverSupportsHttpSteer,
  type AgentRelayFailure,
} from "./mecatlClient.ts";

interface MachineConnection {
  client: Client;
  httpSteer: boolean;
}

export function AgentPane() {
  const params = useParams<{ workerFp?: string }>();
  const navigate = useNavigate();
  const [draft, setDraft] = createSignal("");
  const [creating, setCreating] = createSignal(false);
  const [selectedSessionId, setSelectedSessionId] = createSignal<string | null>(null);
  const [paneFailure, setPaneFailure] = createSignal<AgentRelayFailure | null>(null);

  // The URL owns the machine; an absent parameter resolves to the last machine
  // this browser used and then to the first paired one, so a bare /agent is a
  // working deep link rather than an empty screen.
  const machineFp = createMemo(() => {
    const fromUrl = params.workerFp;
    if (fromUrl) return fromUrl;
    const remembered = lastAgentMachine();
    const options = agentMachineOptions();
    if (remembered && options.some((option) => option.fp === remembered)) return remembered;
    return options[0]?.fp ?? null;
  });

  createEffect(() => {
    const fp = machineFp();
    if (fp) rememberAgentMachine(fp);
  });

  const [connection] = createResource<MachineConnection | null, string>(
    () => machineFp(),
    async (fp) => {
      const client = createMecatlClient(fp);
      try {
        const compatibility = await client.server.compatibility();
        return { client, httpSteer: serverSupportsHttpSteer(compatibility.features) };
      } catch (error) {
        await client.close().catch(() => undefined);
        setPaneFailure(classifyRelayFailure(error));
        return null;
      }
    },
  );

  createEffect(() => {
    const active = connection();
    if (!active) return;
    setPaneFailure(null);
    onCleanup(() => { void active.client.close().catch(() => undefined); });
  });

  const [sessions, { refetch: refetchSessions }] = createResource<
    readonly AgentSessionRow[],
    MachineConnection
  >(
    () => connection() ?? undefined,
    async (active) => {
      try {
        // The SDK bundles its own @bufbuild/protobuf major, so `create()` from
        // this workspace's copy produces a nominally different Message type.
        // The request carries only scalars, so the literal IS the message.
        const listRequest = {
          $typeName: "mecatl.v1.ListSessionsRequest",
          pageSize: 50,
          cursor: "",
        } as const satisfies ListSessionsRequest;
        const response = await active.client.sessions.list(listRequest);
        return response.sessions.map((summary) => ({
          sessionId: summary.sessionId,
          // `title` is deprecated in the SDK in favour of title_metadata; the
          // flat field remains the only value older servers populate.
          title: summary.titleMetadata?.title || summary.title || "Untitled session",
          state: summary.state,
          turns: summary.turns,
          modifiedAtMs: Number(summary.modifiedAtUnix) * 1000,
        }));
      } catch (error) {
        setPaneFailure(classifyRelayFailure(error));
        return [];
      }
    },
  );

  // One controller per selected session; switching sessions or machines
  // disposes the previous one so no detached stream keeps painting.
  const controller = createMemo<AgentSessionController | null>((previous) => {
    previous?.dispose();
    const active = connection();
    const sessionId = selectedSessionId();
    if (!active || !sessionId) return null;
    return createAgentSessionController({
      client: active.client,
      sessionId,
      httpSteer: active.httpSteer,
    });
  }, null);
  onCleanup(() => untrack(controller)?.dispose());

  createEffect(() => {
    // A machine change invalidates the selection: session IDs are per daemon.
    machineFp();
    setSelectedSessionId(null);
  });

  async function createSession(): Promise<void> {
    const active = connection();
    if (!active || creating()) return;
    setCreating(true);
    try {
      const session = await active.client.sessions.create({});
      setSelectedSessionId(session.id);
      void refetchSessions();
    } catch (error) {
      setPaneFailure(classifyRelayFailure(error));
    } finally {
      setCreating(false);
    }
  }

  async function submitDraft(): Promise<void> {
    const active = controller();
    const text = draft().trim();
    if (!active || text.length === 0) return;
    setDraft("");
    if (active.canSteer()) await active.steer(text);
    else await active.prompt(text);
    void refetchSessions();
  }

  const failure = createMemo(() => controller()?.failure() ?? paneFailure());

  return (
    <div
      data-testid="agent-pane"
      style={{
        position: "absolute",
        inset: "0",
        display: "flex",
        "flex-direction": "column",
        gap: "var(--md-space-3)",
        padding: "var(--md-space-4)",
        overflow: "hidden",
        background: "var(--surface-0)",
      }}
    >
      <AgentMachinePicker
        selectedFp={machineFp()}
        onSelect={(fp) => navigate(agentHref(fp))}
      />

      <Show when={machineFp()} fallback={null}>
        <Show
          when={failure() === null}
          fallback={<AgentFailureState failure={failure()!} onRetry={() => void refetchSessions()} />}
        >
          <div
            style={{
              display: "flex",
              gap: "var(--md-space-3)",
              flex: "1",
              "min-height": 0,
            }}
          >
            <Surface
              level={1}
              style={{ width: "280px", padding: "var(--md-space-3)", overflow: "auto" }}
            >
              <AgentSessionList
                rows={sessions() ?? []}
                selectedId={selectedSessionId()}
                creating={creating()}
                onSelect={setSelectedSessionId}
                onCreate={() => void createSession()}
              />
            </Surface>

            <Surface
              level={1}
              style={{
                flex: "1",
                display: "flex",
                "flex-direction": "column",
                "min-width": 0,
                padding: "var(--md-space-3)",
                gap: "var(--md-space-3)",
              }}
            >
              <Show
                when={controller()}
                fallback={
                  <EmptyState
                    icon="forum"
                    title="No session selected"
                    supporting="Pick a conversation on the left, or start a new one."
                  />
                }
              >
                {(active) => (
                  <>
                    <div style={{ flex: "1", "min-height": 0, overflow: "auto" }}>
                      <AgentTranscript
                        entries={active().timeline.entries}
                        asksActionable={active().ownsRun()}
                        onVerdict={(askId, verdict) => void active().answerAsk(askId, verdict)}
                      />
                    </div>
                    <AgentComposer
                      value={draft()}
                      status={active().status()}
                      canSteer={active().canSteer()}
                      onInput={setDraft}
                      onSubmit={() => void submitDraft()}
                      onCancel={() => void active().cancel()}
                    />
                  </>
                )}
              </Show>
            </Surface>
          </div>
        </Show>
      </Show>
    </div>
  );
}

const FAILURE_COPY: Record<AgentRelayFailure["kind"], { title: string; supporting: string }> = {
  unavailable: {
    title: "Mecatl is not running on this machine.",
    supporting:
      "Install mecated and configure a model provider, then set ROOST_MECATL=1 for this worker.",
  },
  offline: {
    title: "Machine offline",
    supporting: "This machine's worker is not connected to the coordinator right now.",
  },
  busy: {
    title: "Machine busy",
    supporting: "Too many agent requests are in flight for this machine. Try again shortly.",
  },
  unauthorized: {
    title: "Not authorized",
    supporting: "This browser is no longer trusted by the coordinator. Pair it again.",
  },
  unknown_machine: {
    title: "Unknown machine",
    supporting: "The coordinator has no live worker with this fingerprint.",
  },
  failed: { title: "Agent request failed", supporting: "" },
};

function AgentFailureState(props: { failure: AgentRelayFailure; onRetry: () => void }) {
  const copy = createMemo(() => {
    const base = FAILURE_COPY[props.failure.kind];
    if (props.failure.kind === "failed") {
      return { title: base.title, supporting: props.failure.message };
    }
    if (props.failure.kind === "unavailable") {
      return { title: base.title, supporting: `${base.supporting} (${props.failure.reason})` };
    }
    return base;
  });
  return (
    <EmptyState
      icon="cloud_off"
      title={copy().title}
      supporting={copy().supporting}
      action={<Button variant="secondary" onClick={props.onRetry}>Retry</Button>}
    />
  );
}
