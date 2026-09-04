// Renders the persistent terminal deck from a dedicated layout model.
// Operation and swipe modules own mutations and gesture lifetimes so this file
// keeps keyed CellTerminal ordering, pane chrome, and render-layer precedence
// visible in one place. Hidden warm sessions remain mounted off-screen.

import {
  For,
  Index,
  Show,
  createMemo,
  createSignal,
} from "solid-js";
import type { Session } from "@roost/shared/wire";
import { rootStore } from "../store/root.ts";
import { CellTerminal } from "./CellTerminal.tsx";
import { PaneStrip } from "./PaneStrip.tsx";
import { MobileDeckBar } from "./MobileDeckBar.tsx";
import { PaneDivider } from "./PaneDivider.tsx";
import { ArrangeMenu } from "./ArrangeMenu.tsx";
import { TerminalDeckSpotlight } from "./TerminalDeckSpotlight.tsx";
import { TerminalDeckSwipeOverlay } from "./TerminalDeckSwipeOverlay.tsx";
import { isCompact } from "../lib/windowSizeClass.ts";
import { isResizeDragging } from "../lib/resizeDrag.ts";
import {
  swipeStyleFor,
  type Swipe,
} from "../lib/deckSwipe.ts";
import {
  createTerminalDeckModel,
  type TerminalDeckProps,
} from "./terminal-deck-model.ts";
import {
  MOBILE_TERMINAL_STRIP_HEIGHT,
  TERMINAL_STRIP_HEIGHT,
  sameTerminalSessionSlot,
} from "./terminal-deck-geometry.ts";
import { createTerminalDeckOperations } from "./terminal-deck-operations.ts";
import { bindTerminalDeckSwipe } from "./terminal-deck-swipe.ts";

export function TerminalDeck(props: TerminalDeckProps) {
  let deckElement: HTMLDivElement | undefined;
  const getDeckElement = () => deckElement;
  const [swipe, setSwipe] = createSignal<Swipe | null>(null);
  const model = createTerminalDeckModel(props, swipe, setSwipe, getDeckElement);
  const operations = createTerminalDeckOperations(props, model, getDeckElement);
  bindTerminalDeckSwipe(
    props,
    swipe,
    setSwipe,
    model,
    operations,
    getDeckElement,
  );

  return (
    <div
      ref={deckElement}
      data-testid="terminal-deck"
      data-multi-pane={model.view().panes.length > 1 ? "true" : "false"}
      data-resizing={isResizeDragging() ? "true" : undefined}
      onPointerDown={operations.onDeckPointerDown}
      onFocusIn={operations.onDeckFocusIn}
      style={{
        flex: "1",
        position: "relative",
        overflow: "hidden",
        background: "var(--term-bg)",
        "touch-action": isCompact() ? "pan-y" : "auto",
        transform: "translate3d(0, calc(0px - var(--term-chat-growth, 0px)), 0)",
      }}
    >
      <Show when={model.openSessions().length === 0}>
        <div style={{ position: "absolute", inset: "0", display: "flex", "align-items": "center", "justify-content": "center", color: "var(--text-lo)", "font-size": "13px" }}>
          No session selected.
        </div>
      </Show>

      {/* Primitive IDs key the deck so store snapshots cannot remount a warm
          renderer and discard its retained DOM or scroll position. */}
      <For each={model.mountedSessionIds()}>
        {(sessionId) => {
          const session = createMemo(() => rootStore.sessions[sessionId]);
          const slot = createMemo(
            () => model.slotBySession().get(sessionId) ?? null,
            undefined,
            { equals: sameTerminalSessionSlot },
          );
          return (
            <Show when={session()}>
              {(currentSession) => (
                <div
                  data-testid={`terminal-slot-${sessionId}`}
                  data-pane-slot
                  data-pane
                  data-pane-id={slot()?.paneId ?? ""}
                  data-focused={slot()?.focused ? "true" : "false"}
                  data-spotlit={slot()?.spotlit ? "true" : undefined}
                  style={{
                    ...model.termStyle(
                      slot(),
                      model.parkSizeBySession().get(sessionId),
                    ),
                    ...swipeStyleFor(swipe(), sessionId, model.size().w),
                  }}
                >
                  <CellTerminal
                    session={currentSession()}
                    inLayout={!!slot()}
                    focused={slot()?.focused ?? false}
                    spotlit={slot()?.spotlit ?? false}
                    surfaceVisible={props.surfaceVisible}
                    surfaceActive={
                      model.spotlightPane() === null || slot()?.spotlit === true
                    }
                  />
                </div>
              )}
            </Show>
          );
        }}
      </For>

      <Show when={isCompact() && model.mobileTabs().length > 0}>
        <div
          data-testid="mobile-strip-wrap"
          style={{
            position: "absolute",
            left: "0",
            top: "0",
            width: "100%",
            height: `${MOBILE_TERMINAL_STRIP_HEIGHT}px`,
            "z-index": "3",
            ...swipeStyleFor(swipe(), props.activeSessionId ?? "", model.size().w),
          }}
        >
          <MobileDeckBar
            tabs={model.mobileTabs()}
            selectedTab={props.activeSessionId ?? ""}
            onSelect={operations.select}
            onClose={operations.close}
            onNewTab={() => void operations.newTab(model.layout()?.focusedPaneId ?? "")}
          />
        </div>
        <Show when={model.barNeighborId()}>
          {(neighborId) => (
            <div
              data-testid="mobile-strip-wrap-neighbor"
              style={{
                position: "absolute",
                left: "0",
                top: "0",
                width: "100%",
                height: `${MOBILE_TERMINAL_STRIP_HEIGHT}px`,
                "z-index": "3",
                ...swipeStyleFor(swipe(), neighborId(), model.size().w),
              }}
            >
              <MobileDeckBar
                tabs={model.mobileTabs()}
                selectedTab={neighborId()}
                onSelect={operations.select}
                onClose={operations.close}
                onNewTab={() => void operations.newTab(model.layout()?.focusedPaneId ?? "")}
              />
            </div>
          )}
        </Show>
      </Show>

      <TerminalDeckSwipeOverlay
        swipe={swipe()}
        paneRect={model.view().panes[0]?.rect}
        deckWidth={model.size().w}
        stripH={model.stripH()}
        folderLabel={model.newTermFolder()}
      />

      <Show when={!isCompact()}>
        <For each={model.panes()}>
          {(pane) => {
            const tabs = createMemo(() =>
              pane.tabIds
                .map((id) => rootStore.sessions[id])
                .filter(Boolean) as Session[]);
            const rect = () => model.paneRectById().get(pane.paneId) ?? pane.rect;
            return (
              <Show when={pane.paneId !== model.spotlightPane()?.paneId}>
                <div
                  data-pane
                  data-pane-id={pane.paneId}
                  style={{
                    position: "absolute",
                    left: `${rect().x}px`,
                    top: `${rect().y}px`,
                    width: `${rect().w}px`,
                    height: `${TERMINAL_STRIP_HEIGHT}px`,
                    "z-index": "3",
                  }}
                >
                  <Show when={tabs().length > 0}>
                    <PaneStrip
                      paneId={pane.paneId}
                      tabs={tabs()}
                      selectedTab={pane.selectedTab}
                      focused={model.paneFocusById().get(pane.paneId) ?? false}
                      onSelect={operations.select}
                      onClose={operations.close}
                      onReorder={(ids) => operations.reorder(pane.paneId, ids)}
                      onNewTab={() => void operations.newTab(pane.paneId)}
                      onTabDragMove={(x, y) =>
                        operations.onTabDragMove(pane.paneId, x, y)}
                      onTabTileDrop={(tabId, x, y) =>
                        operations.onTabTileDrop(tabId, pane.paneId, x, y)}
                      onTabDragEnd={operations.onTabDragEnd}
                    />
                  </Show>
                </div>
              </Show>
            );
          }}
        </For>
        <Index each={model.view().dividers}>
          {(divider) => (
            <PaneDivider
              divider={divider}
              deckEl={getDeckElement}
              onDrag={operations.dividerDrag}
              onCommit={operations.dividerCommit}
            />
          )}
        </Index>
        <Show when={model.dropOverlay()}>
          {(overlay) => (
            <div
              class="pane-drop-overlay"
              data-zone={overlay().zone}
              style={{
                position: "absolute",
                left: `${overlay().rect.x}px`,
                top: `${overlay().rect.y}px`,
                width: `${overlay().rect.w}px`,
                height: `${overlay().rect.h}px`,
                "z-index": "5",
                "pointer-events": "none",
              }}
            />
          )}
        </Show>
        <Show when={model.liveIds().length >= 2}>
          <div style={{ position: "absolute", top: "0", right: "0", height: `${TERMINAL_STRIP_HEIGHT}px`, display: "flex", "align-items": "center", padding: "0 6px", "z-index": "4" }}>
            <ArrangeMenu onArrange={operations.arrange} />
          </div>
        </Show>
      </Show>
      <TerminalDeckSpotlight rect={model.spotlightRect()} />
    </div>
  );
}
