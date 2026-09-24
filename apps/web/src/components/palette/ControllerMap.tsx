// ControllerMap — the Start-button overlay App.tsx mounts: a controller
// silhouette whose caps sit where they sit on the hardware, each naming its
// action and each lit while its physical button is held, so a user learns their
// own pad. Wording comes from PAD_CONTROL_GUIDE and geometry from this file, so
// the transient legend cannot disagree with the map. No text field anywhere —
// an autofocused filter is what makes HelpOverlay a dead end for a controller.

import { createMemo, For, Show } from "solid-js";
import { closeControllerMap, controllerMapOpen } from "../../lib/keyboardShortcuts.ts";
import { padInputSeen } from "../../lib/padMode.ts";
import { padHeldActions, padHeldButtons } from "../../browser/gamepadSource.ts";
import {
  PAD_CONTROL_GUIDE,
  type PadAction,
  type PadControlGuide,
} from "../../lib/padBindings.ts";
import { BindingChip, Chip, Surface } from "../Settings/md/primitives.tsx";
import { Sheet } from "../Settings/md/Sheet.tsx";

// ── Geometry ─────────────────────────────────────────────────────────────────

/** A cluster is a grid area of `.pad-map` in styles/gamepad.css. Placement
 *  lives in CSS, so this table carries no pixels. */
type PadCluster =
  | "shoulder-left" | "shoulder-right" | "stick-left"
  | "dpad" | "centre" | "face" | "stick-right";

interface PadMapSlot {
  readonly cap: string;
  readonly cluster: PadCluster;
  /** Standard-mapping indices that light this cap. The analogue sticks travel
   *  on axes, so only their click carries an index. */
  readonly buttons: readonly number[];
}

// Source order is DOM order within a cluster. The face diamond is the one
// exception: its four seats are placed by cap in CSS, a diamond being no stack.
const PAD_MAP_SLOTS: readonly PadMapSlot[] = [
  { cap: "LT", cluster: "shoulder-left", buttons: [6] },
  { cap: "LB", cluster: "shoulder-left", buttons: [4] },
  { cap: "RT", cluster: "shoulder-right", buttons: [7] },
  { cap: "RB", cluster: "shoulder-right", buttons: [5] },
  { cap: "L-stick", cluster: "stick-left", buttons: [] },
  { cap: "L3", cluster: "stick-left", buttons: [10] },
  { cap: "D-pad", cluster: "dpad", buttons: [12, 13, 14, 15] },
  { cap: "Back", cluster: "centre", buttons: [8] },
  { cap: "Start", cluster: "centre", buttons: [9] },
  { cap: "Y", cluster: "face", buttons: [3] },
  { cap: "X", cluster: "face", buttons: [2] },
  { cap: "B", cluster: "face", buttons: [1] },
  { cap: "A", cluster: "face", buttons: [0] },
  { cap: "R-stick", cluster: "stick-right", buttons: [] },
  { cap: "R3", cluster: "stick-right", buttons: [11] },
];

const PAD_MAP_CLUSTERS: readonly PadCluster[] = [
  "shoulder-left", "shoulder-right", "stick-left",
  "dpad", "centre", "face", "stick-right",
];

/** Indices a named cap claims; anything else this pad reports is an extra. */
const NAMED_BUTTONS = new Set(PAD_MAP_SLOTS.flatMap((slot) => slot.buttons));

const UNBOUND_LABEL = "Unbound";
const UNBOUND_DETAIL = "This pad reports this button; nothing is bound to it";
const IDLE_CAPTION = "Hold a button to read exactly what it does.";

// The sticks travel on axes, so no button index can ever light them. These are
// the intents that identify one: a scroll can only have come from the right
// stick, and a move with no D-pad index held can only have come from the left.
const STICK_MOVE_ACTIONS: readonly PadAction[] = [
  "move-up", "move-down", "move-left", "move-right",
];
const STICK_SCROLL_ACTIONS: readonly PadAction[] = ["scroll-up", "scroll-down"];

// ── Component ────────────────────────────────────────────────────────────────

export function ControllerMap() {
  // Open is not enough: this surface exists only when input is actually coming
  // from a controller, and padInputSeen() is the latch for real pad input — a
  // pad plugged in for games never asked for a controller UI.
  const visible = createMemo(() => controllerMapOpen() && padInputSeen());

  // The poll's held set is read ONCE per change here and folded into the shapes
  // the diagram needs; reading it per cap would hang fifteen subscriptions off
  // a set the poll loop republishes.
  const heldCaps = createMemo(() => {
    const indices = padHeldButtons();
    const actions = padHeldActions();
    const caps = new Set<string>();
    for (const slot of PAD_MAP_SLOTS) {
      if (slot.buttons.some((index) => indices.has(index))) caps.add(slot.cap);
    }
    if (STICK_SCROLL_ACTIONS.some((action) => actions.has(action))) caps.add("R-stick");
    if (!caps.has("D-pad") && STICK_MOVE_ACTIONS.some((action) => actions.has(action))) {
      caps.add("L-stick");
    }
    return caps;
  });

  // Lighting by raw index is what makes this a discovery tool: a button the map
  // does not name still reports itself instead of staying invisible.
  const unnamedHeld = createMemo(() =>
    [...padHeldButtons()]
      .filter((index) => !NAMED_BUTTONS.has(index))
      .sort((left, right) => left - right),
  );

  const caption = createMemo(() => {
    const caps = heldCaps();
    const slot = PAD_MAP_SLOTS.find((candidate) => caps.has(candidate.cap));
    const row = slot && PAD_CONTROL_GUIDE.find((entry) => entry.cap === slot.cap);
    return row ? `${row.cap} · ${row.label} — ${row.detail}` : IDLE_CAPTION;
  });

  return (
    <Sheet
      open={visible()}
      onClose={closeControllerMap}
      headline="Controller map"
      side="center"
      class="roost-dialog--controller-map"
    >
      <Show when={visible()}>
        <div class="pad-map-shell" data-testid="controller-map">
          <Surface level={2} elevation={1} radius="lg" pad={5} class="pad-map">
            <For each={PAD_MAP_CLUSTERS}>
              {(cluster) => (
                <div class="pad-map__cluster" data-cluster={cluster}>
                  <For each={PAD_MAP_SLOTS.filter((slot) => slot.cluster === cluster)}>
                    {(slot) => padCallout(
                      slot.cap,
                      () => heldCaps().has(slot.cap),
                      PAD_CONTROL_GUIDE.find((row) => row.cap === slot.cap),
                    )}
                  </For>
                </div>
              )}
            </For>
          </Surface>

          <p class="md-body-s pad-map__caption" data-testid="controller-map-caption">
            {caption()}
          </p>

          <Show when={unnamedHeld().length > 0}>
            <div class="pad-map__extras">
              <For each={unnamedHeld()}>
                {(index) => padCallout(`Button ${index}`, () => true, undefined)}
              </For>
            </div>
          </Show>

          <div class="pad-map__footer">
            <Chip label="B, Esc or Start closes" />
            <span class="md-label-s pad-map__note">
              Xbox labels — the same caps on any standard-mapping pad.
            </span>
          </div>
        </div>
      </Show>
    </Sheet>
  );
}

// ── private ──────────────────────────────────────────────────────────────────

/** One labelled callout. `held` stays an accessor so the highlight tracks the
 *  poll; a plain boolean would freeze it at first paint. The clause sits in the
 *  body AND in `title`: LB/RB and LT/RT share a verb by design, so the diagram
 *  is ambiguous without it, while gamepad.css hides it inside the face diamond,
 *  which has no column to spare and whose four verbs are already unique. */
function padCallout(cap: string, held: () => boolean, guide: PadControlGuide | undefined) {
  return (
    <div
      class="pad-map__callout"
      data-cap={cap}
      data-held={held() ? "true" : "false"}
      title={guide?.detail ?? UNBOUND_DETAIL}
    >
      <BindingChip>{cap}</BindingChip>
      <span class="md-title-s pad-map__label">{guide?.label ?? UNBOUND_LABEL}</span>
      <span class="md-label-s pad-map__detail">{guide?.detail ?? UNBOUND_DETAIL}</span>
    </div>
  );
}
