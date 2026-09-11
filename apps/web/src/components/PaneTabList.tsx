/*
 * Filterable overflow menu for tabs that do not fit in a PaneStrip.
 * It owns only menu focus, filtering, and selection presentation; PaneStrip
 * retains the anchor, selected tab, and every layout mutation callback.
 */

import { For, Show, createMemo, createSignal, onCleanup, onMount } from "solid-js";
import { Portal } from "solid-js/web";
import type { Session } from "@roost/shared/wire";
import { sessionTitle } from "../lib/sessionTitle.ts";
import {
  anchoredMenuSurfaceStyle,
  CtxMenuItem,
  focusMenuEdge,
  handleMenuKeyboardNavigation,
  trackFloatingMenuDismiss,
  type AnchoredMenuPos,
} from "./contextMenuPrimitives.tsx";
import { Icon } from "./Settings/md/Icon.tsx";

export interface PaneTabListProps {
  position: AnchoredMenuPos;
  tabs: Session[];
  selectedTab: string;
  trigger: () => HTMLElement | undefined;
  tabBar: () => HTMLElement | undefined;
  onSelect: (sessionId: string) => void;
  onClose: () => void;
}

export function PaneTabList(props: PaneTabListProps) {
  const [filter, setFilter] = createSignal("");
  const [highlightedIndex, setHighlightedIndex] = createSignal(0);
  let popupElement: HTMLDivElement | undefined;
  let menuElement: HTMLDivElement | undefined;
  let inputElement: HTMLInputElement | undefined;
  let cancelPendingFocus: (() => void) | null = null;
  let focusRequest = 0;
  let selectionFocusPending = false;
  const matchingTabs = createMemo(() => {
    const normalizedFilter = filter().trim().toLowerCase();
    return props.tabs.filter((session) =>
      sessionTitle(session).toLowerCase().includes(normalizedFilter));
  });

  function cancelQueuedFocus(): void {
    cancelPendingFocus?.();
    cancelPendingFocus = null;
    focusRequest++;
  }

  function closeList(restoreTriggerFocus = false): void {
    cancelQueuedFocus();
    props.onClose();
    if (restoreTriggerFocus) queueMicrotask(() => props.trigger()?.focus());
  }

  function chooseTab(session: Session): void {
    cancelQueuedFocus();
    const request = focusRequest;
    selectionFocusPending = true;
    props.onSelect(session.id);
    props.onClose();
    queueMicrotask(() => {
      if (request !== focusRequest) return;
      const tab = document.querySelector<HTMLElement>(`[data-testid="tab-${session.id}"]`);
      const select = tab?.querySelector<HTMLElement>(".workbench-pane-tab__select");
      if (tab && select) {
        select.focus({ preventScroll: true });
        tab.scrollIntoView({ inline: "nearest", block: "nearest" });
      }
      selectionFocusPending = false;
    });
  }

  function focusMatchingEdge(edge: "first" | "last", index: number): void {
    setHighlightedIndex(index);
    cancelPendingFocus?.();
    cancelPendingFocus = focusMenuEdge(() => menuElement, edge);
  }

  function handleFilterKeyDown(event: KeyboardEvent): void {
    if (event.key === "Escape" || event.key === "Tab") {
      handleMenuKeyboardNavigation(
        event,
        menuElement,
        () => closeList(true),
        () => closeList(),
      );
      return;
    }

    const tabs = matchingTabs();
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      event.stopPropagation();
      if (tabs.length === 0) return;
      focusMatchingEdge(
        event.key === "ArrowDown" ? "first" : "last",
        event.key === "ArrowDown" ? 0 : tabs.length - 1,
      );
      return;
    }
    if (event.key === "Enter") {
      event.preventDefault();
      event.stopPropagation();
      if (tabs.length === 0) return;
      const selected = tabs[Math.min(highlightedIndex(), tabs.length - 1)] ?? tabs[0];
      chooseTab(selected);
    }
  }

  function handleMenuKeyDown(event: KeyboardEvent): void {
    handleMenuKeyboardNavigation(
      event,
      menuElement,
      () => closeList(true),
      () => closeList(),
    );
  }

  trackFloatingMenuDismiss({
    within: [props.trigger, () => popupElement],
    onClose: () => closeList(),
    onEscape: () => closeList(true),
  });

  onMount(() => inputElement?.focus());
  onCleanup(() => {
    if (!selectionFocusPending) cancelQueuedFocus();
  });

  return (
    <Portal>
      <div
        ref={popupElement}
        id="tab-list-popup"
        data-testid="tab-list-popup"
        class="df-menu-enter workbench-tab-list"
        aria-label="Open terminals in this pane"
        style={anchoredMenuSurfaceStyle(props.position, {
          minWidth: "var(--workbench-tab-menu-min-width)",
        })}
      >
        <div class="workbench-tab-list__filter">
          <Icon name="search" class="workbench-tab-list__filter-icon" size="sm" />
          <input
            ref={inputElement}
            class="workbench-tab-list__input"
            type="text"
            value={filter()}
            aria-label="Filter terminals in this pane"
            onKeyDown={handleFilterKeyDown}
            onInput={(event) => {
              setFilter(event.currentTarget.value);
              setHighlightedIndex(0);
            }}
            placeholder="Filter terminals…"
            data-testid="tab-list-filter"
          />
        </div>
        <div
          ref={menuElement}
          class="workbench-tab-list__items"
          role="menu"
          aria-label="Open terminals in this pane"
          onKeyDown={handleMenuKeyDown}
        >
          <Show
            when={matchingTabs().length > 0}
            fallback={<div class="workbench-tab-list__empty">No matches</div>}
          >
            <For each={matchingTabs()}>
              {(session, index) => (
                <CtxMenuItem
                  class="workbench-tab-list__item"
                  testid={`tab-list-item-${session.id}`}
                  selected={session.id === props.selectedTab}
                  highlighted={highlightedIndex() === index()}
                  onMouseEnter={() => setHighlightedIndex(index())}
                  onFocus={() => setHighlightedIndex(index())}
                  onClick={() => chooseTab(session)}
                >
                  <Icon name="terminal" class="workbench-tab-list__item-icon" size="sm" />
                  <span class="workbench-tab-list__item-label">{sessionTitle(session)}</span>
                  <Show when={session.id === props.selectedTab}>
                    <Icon name="check" class="workbench-tab-list__item-check" size="sm" />
                  </Show>
                </CtxMenuItem>
              )}
            </For>
          </Show>
        </div>
      </div>
    </Portal>
  );
}
