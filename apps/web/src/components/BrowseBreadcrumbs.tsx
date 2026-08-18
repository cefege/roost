// Browse-page breadcrumb trail: the visible (already-collapsed) crumb strip
// with its overflow menu, plus the hidden measurement mirror that BrowsePage's
// width-aware collapse reads. Split out of BrowsePage.tsx; the page owns the
// collapse math, both element refs, and the menu signals — this file only
// paints them.
//
// Callers: BrowsePage.tsx (WorkerBrowsePage).

import { For, Show } from "solid-js";
import type { Crumb, CrumbView } from "../lib/folderPalette.ts";

export function BrowseBreadcrumbs(props: {
  /** Collapsed views the strip paints (collapseCrumbsTo output). */
  crumbViews: CrumbView[];
  /** Full, uncollapsed trail the hidden mirror measures. */
  crumbs: Crumb[];
  menuOpen: boolean;
  menuPos: { top: number; left: number };
  setMenuOpen: (open: boolean) => void;
  setMenuPos: (pos: { top: number; left: number }) => void;
  onNavigate: (path: string) => void;
  setStripRef: (el: HTMLDivElement) => void;
  setMirrorRef: (el: HTMLDivElement) => void;
}) {
  let crumbOverflowBtn: HTMLButtonElement | undefined;
  return (
    <>
      <div class="df-browse-crumbs" ref={props.setStripRef} data-testid="browse-crumbs">
        <For each={props.crumbViews}>
          {(v, i) => (
            <>
              <Show when={i() > 0}>
                <span class="df-browse-crumb-sep" aria-hidden="true">▸</span>
              </Show>
              <Show
                when={v.kind === "crumb"}
                fallback={
                  <div style={{ position: "relative", "flex-shrink": "0" }}>
                    <button type="button" class="df-browse-crumb df-browse-crumb-overflow"
                      ref={crumbOverflowBtn}
                      data-testid="browse-crumb-overflow" aria-label="Show hidden folders"
                      aria-haspopup="menu" aria-expanded={props.menuOpen}
                      onClick={(e) => {
                        e.stopPropagation();
                        const willOpen = !props.menuOpen;
                        if (willOpen && crumbOverflowBtn) {
                          const r = crumbOverflowBtn.getBoundingClientRect();
                          props.setMenuPos({ top: r.bottom + 6, left: r.left });
                        }
                        props.setMenuOpen(willOpen);
                      }}
                    >…</button>
                    <Show when={props.menuOpen}>
                      <div onClick={() => props.setMenuOpen(false)}
                        style={{ position: "fixed", inset: "0", "z-index": "1" }} />
                      <div data-testid="browse-crumb-menu"
                        style={{ position: "fixed", top: `${props.menuPos.top}px`, left: `${props.menuPos.left}px`, "min-width": "180px", "max-height": "50vh", overflow: "auto", "z-index": "2", display: "flex", "flex-direction": "column", padding: "4px", background: "var(--md-sys-color-surface-container-high)", border: "1px solid var(--md-sys-color-outline-variant)", "border-radius": "var(--md-shape-md)", "box-shadow": "var(--md-elev-2)" }}
                      >
                        <For each={(v as Extract<CrumbView, { kind: "ellipsis" }>).hidden}>
                          {(h) => (
                            <button type="button" data-testid="browse-crumb-menu-item"
                              onClick={(e) => { e.stopPropagation(); props.setMenuOpen(false); props.onNavigate(h.path); }}
                              title={h.path}
                              style={{ display: "flex", "align-items": "center", gap: "8px", width: "100%", padding: "6px 10px", "border-radius": "var(--md-shape-sm)", border: "none", background: "transparent", color: "var(--md-sys-color-on-surface)", "font-size": "var(--md-body-s-size)", "font-family": "inherit", cursor: "pointer", "text-align": "left", "white-space": "nowrap", overflow: "hidden", "text-overflow": "ellipsis" }}
                            >{h.label}</button>
                          )}
                        </For>
                      </div>
                    </Show>
                  </div>
                }
              >
                <button type="button" class="df-browse-crumb" data-testid="browse-crumb"
                  data-current={i() === props.crumbViews.length - 1 ? "true" : "false"}
                  onClick={() => props.onNavigate((v as Extract<CrumbView, { kind: "crumb" }>).path)}
                  title={(v as Extract<CrumbView, { kind: "crumb" }>).path}
                >{(v as Extract<CrumbView, { kind: "crumb" }>).label}</button>
              </Show>
            </>
          )}
        </For>
      </div>
      <div class="df-browse-crumbs-measure" ref={props.setMirrorRef} aria-hidden="true">
        <For each={props.crumbs}>
          {(c, i) => (
            <>
              <Show when={i() > 0}>
                <span class="df-browse-crumb-sep" data-mirror-sep aria-hidden="true">▸</span>
              </Show>
              <button type="button" class="df-browse-crumb" data-mirror-crumb tabindex="-1">{c.label}</button>
            </>
          )}
        </For>
        {/* one sample of each non-crumb token so its width is measurable */}
        <span class="df-browse-crumb-sep" aria-hidden="true">▸</span>
        <button type="button" class="df-browse-crumb df-browse-crumb-overflow" data-mirror-overflow tabindex="-1">…</button>
      </div>
    </>
  );
}
