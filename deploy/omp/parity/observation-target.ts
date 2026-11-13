import type { Page } from "playwright";
import type { BrowserTarget } from "./coverage";

/** Samples the element actually addressed by an observation, not a fixture-declared identity. */
export async function observedTarget(page: Page, selector: string): Promise<BrowserTarget | undefined> {
  const locator = page.locator(selector);
  if (await locator.count() !== 1) return undefined;
  return locator.evaluate(element => {
    const owner = element.closest("[data-omp-node]");
    const surfaceId = owner?.getAttribute("data-omp-surface") ?? element.closest("[data-omp-surface]")?.getAttribute("data-omp-surface");
    const owners: Array<{ node: string; surface: string }> = [];
    for (let ancestor: Element | null = element; ancestor; ancestor = ancestor.parentElement) {
      const node = ancestor.getAttribute("data-omp-node");
      const surface = ancestor.getAttribute("data-omp-surface");
      if (node && surface && surface === surfaceId) owners.push({ node, surface });
    }
    const surface = [...document.querySelectorAll("[data-omp-surface-sequence]")].find(candidate => candidate.getAttribute("data-omp-surface") === surfaceId);
    const sequence = surface?.getAttribute("data-omp-surface-sequence");
    const bounds = element.getBoundingClientRect();
    let left = Math.max(0, bounds.left), right = Math.min(innerWidth, bounds.right);
    let top = Math.max(0, bounds.top), bottom = Math.min(innerHeight, bounds.bottom);
    let visible = bounds.width > 0 && bounds.height > 0;
    for (let current: Element | null = element; current; current = current.parentElement) {
      const style = window.getComputedStyle(current);
      if (style.display === "none" || style.visibility === "hidden" || style.visibility === "collapse" || Number(style.opacity) === 0) visible = false;
      if (current !== element) {
        const clip = current.getBoundingClientRect();
        if (["hidden", "clip", "scroll", "auto"].includes(style.overflowX)) { left = Math.max(left, clip.left); right = Math.min(right, clip.right); }
        if (["hidden", "clip", "scroll", "auto"].includes(style.overflowY)) { top = Math.max(top, clip.top); bottom = Math.min(bottom, clip.bottom); }
      }
    }
    const attributes = Object.fromEntries([...element.attributes].filter(attribute => attribute.name !== "value").map(attribute => [attribute.name, attribute.value]));
    const computed = window.getComputedStyle(element);
    const state: Record<string, boolean> = { bold: Number(computed.fontWeight) >= 600, italic: /italic|oblique/.test(computed.fontStyle),
      underline: computed.textDecorationLine.includes("underline"), focused: document.activeElement === element };
    for (const [key, attribute] of [["selected", "aria-selected"], ["pressed", "aria-pressed"], ["expanded", "aria-expanded"]] as const) {
      const value = element.getAttribute(attribute);
      if (value === "true" || value === "false") state[key] = value === "true";
    }
    if (state.expanded !== undefined) state.collapsed = !state.expanded;
    if (state.pressed !== undefined) state.on = state.pressed;
    let parentBackground = "rgba(0, 0, 0, 0)";
    for (let parent = element.parentElement; parent; parent = parent.parentElement) {
      const background = window.getComputedStyle(parent).backgroundColor;
      if (background !== "rgba(0, 0, 0, 0)" && background !== "transparent") { parentBackground = background; break; }
    }
    const parentForeground = element.parentElement ? window.getComputedStyle(element.parentElement).color : undefined;
    state.inverse = computed.color === parentBackground && computed.backgroundColor === parentForeground;
    if (element instanceof HTMLInputElement || element instanceof HTMLTextAreaElement) state.readonly = element.readOnly;
    if (element instanceof HTMLInputElement) { state.checked = element.checked; state.mask = element.type === "password"; }
    if (element instanceof HTMLInputElement || element instanceof HTMLTextAreaElement || element instanceof HTMLButtonElement || element instanceof HTMLSelectElement) state.disabled = element.disabled;
    const identity: NonNullable<BrowserTarget["identity"]> = {};
    for (const [key, attribute] of [["setting", "data-omp-setting"], ["control", "data-omp-control"], ["page", "data-omp-page"],
      ["pageTab", "data-omp-page-tab"], ["pageActive", "data-omp-page-active"], ["item", "data-omp-item"], ["itemSelected", "data-omp-item-selected"]] as const) {
      const value = element.closest(`[${attribute}]`)?.getAttribute(attribute);
      if (value !== null && value !== undefined) identity[key] = value;
    }
    const prefsOwner = element.closest('[data-omp-kind="prefs"]') ?? owner?.querySelector('[data-omp-kind="prefs"]');
    const activePage = prefsOwner?.querySelector('[data-omp-page-active="true"]')
      ?? prefsOwner?.querySelector('[data-omp-page-tab][aria-selected="true"]');
    const page = activePage?.getAttribute("data-omp-page") ?? identity.page;
    const tab = activePage?.getAttribute("data-omp-page-tab") ?? identity.pageTab;
    const prefs = prefsOwner ? { ...(page ? { page } : {}), ...(tab ? { tab } : {}),
      rows: [...prefsOwner.querySelectorAll("[data-omp-setting]")].filter(row => {
        const rect = row.getBoundingClientRect(), style = getComputedStyle(row);
        return rect.width > 0 && rect.height > 0 && style.display !== "none" && style.visibility !== "hidden";
      }).map(row => row.getAttribute("data-omp-setting")!) } : undefined;
    return { visible: visible && right > left && bottom > top, attributes, state, identity, owners, ...(prefs ? { prefs } : {}),
      ...(owner?.getAttribute("data-omp-node") ? { node: owner.getAttribute("data-omp-node")! } : {}),
      ...(surfaceId ? { surface: surfaceId } : {}),
      ...(sequence && Number.isSafeInteger(Number(sequence)) ? { surfaceSequence: Number(sequence) } : {}) };
  });
}
