// Channel lifecycle FSM — hand-rolled per R0.5.
// States: spawned → attached → closed.
// Invariants:
//   - close is valid from spawned (pre-attach failure) or attached.
//   - closed is terminal: no further transitions.
//   - Every closure emits exactly one "closed" SessionEvent.
//
// Callers: session-manager.ts (owns one FsmChannel per session).
// Coord events are fired via the onTransition callback.

export type ChannelState = "spawned" | "attached" | "closed";

export type FsmEvent =
  | { kind: "attach" }
  | { kind: "detach" }
  | { kind: "close"; exitCode: number | null };

/** Transition table: [fromState, eventKind] → toState | null (invalid). */
const TRANSITIONS: Partial<Record<ChannelState, Partial<Record<FsmEvent["kind"], ChannelState>>>> = {
  spawned: {
    attach: "attached",
    close: "closed",       // keeper died before any attach (e.g. spawn failure)
  },
  attached: {
    detach: "spawned",
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
  private readonly _onTransition: (from: ChannelState, to: ChannelState, event: FsmEvent) => void;

  constructor(onTransition: (from: ChannelState, to: ChannelState, event: FsmEvent) => void) {
    this._onTransition = onTransition;
  }

  get state(): ChannelState { return this._state; }

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
    this._onTransition(from, next, event);
    return { ok: true, from, to: next };
  }
}
