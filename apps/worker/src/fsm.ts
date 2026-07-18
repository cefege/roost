// Channel lifecycle FSM — hand-rolled per R0.5.
// States: spawned → attached → agent-running → agent-needs-input → agent-idle → closed.
// Invariants (R5.3):
//   - Cannot reach closed without passing through attached.
//   - closed is terminal: no further transitions.
//   - Every closure emits exactly one "closed" SessionEvent.
//
// Callers: session-manager.ts (owns one FsmChannel per session).
// Coord events are fired via the onTransition callback.

export type ChannelState =
  | "spawned"
  | "attached"
  | "agent-running"
  | "agent-needs-input"
  | "agent-idle"
  | "closed";

export type FsmEvent =
  | { kind: "attach" }
  | { kind: "detach" }
  | { kind: "agent-started" }
  | { kind: "agent-running" }
  | { kind: "agent-needs-input" }
  | { kind: "agent-idle" }
  | { kind: "close"; exitCode: number | null };

/** Transition table: [fromState, eventKind] → toState | null (invalid). */
const TRANSITIONS: Partial<Record<ChannelState, Partial<Record<FsmEvent["kind"], ChannelState>>>> = {
  spawned: {
    attach: "attached",
    close: "closed",       // keeper died before any attach (e.g. spawn failure)
  },
  attached: {
    detach: "spawned",
    "agent-started": "agent-running",
    "agent-running": "agent-running",
    "agent-needs-input": "agent-needs-input",
    "agent-idle": "agent-idle",
    close: "closed",
  },
  "agent-running": {
    detach: "spawned",
    "agent-needs-input": "agent-needs-input",
    "agent-idle": "agent-idle",
    attach: "agent-running",
    close: "closed",
  },
  "agent-needs-input": {
    detach: "spawned",
    "agent-running": "agent-running",
    "agent-idle": "agent-idle",
    attach: "agent-needs-input",
    close: "closed",
  },
  "agent-idle": {
    detach: "spawned",
    "agent-running": "agent-running",
    "agent-needs-input": "agent-needs-input",
    attach: "agent-idle",
    close: "closed",
  },
  closed: {
    // terminal — no valid transitions
  },
};

/** Result of a transition attempt. */
export type TransitionResult =
  | { ok: true; from: ChannelState; to: ChannelState }
  | { ok: false; reason: string };

export class FsmChannel {
  private _state: ChannelState = "spawned";
  private _hasEverAttached = false;
  private readonly _onTransition: (from: ChannelState, to: ChannelState, event: FsmEvent) => void;

  constructor(onTransition: (from: ChannelState, to: ChannelState, event: FsmEvent) => void) {
    this._onTransition = onTransition;
  }

  get state(): ChannelState { return this._state; }
  get hasEverAttached(): boolean { return this._hasEverAttached; }

  /** Apply an event. Returns ok=true on valid transition, ok=false otherwise. */
  send(event: FsmEvent): TransitionResult {
    if (this._state === "closed") {
      return { ok: false, reason: "closed is terminal" };
    }
    const table = TRANSITIONS[this._state];
    const next = table?.[event.kind];
    if (!next) {
      return { ok: false, reason: `no transition ${this._state} + ${event.kind}` };
    }
    const from = this._state;
    this._state = next;
    if (event.kind === "attach") this._hasEverAttached = true;
    this._onTransition(from, next, event);
    return { ok: true, from, to: next };
  }
}
