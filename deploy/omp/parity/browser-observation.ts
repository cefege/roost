import type { Locator, Page } from "playwright";
import type { Observation } from "./scenario";

export type DOMObservation = Extract<Observation, { type: "dom" }>;

export async function uniqueElement(page: Page, selector: string, wait: boolean): Promise<Locator | undefined> {
  const element = page.locator(selector);
  if (wait) await element.waitFor({ state: "attached", timeout: 15_000 });
  const count = await element.count();
  if (count > 1) throw new Error(`Ambiguous browser selector (${count} matches): ${selector}`);
  if (count === 0) return undefined;
  return element;
}

export async function readDOM(element: Locator, observation: DOMObservation, reference?: Locator): Promise<unknown> {
  if (observation.read === "visible") return element.isVisible();
  const explicitRoot = reference ? await reference.elementHandle() : null;
  if (reference && !explicitRoot) throw new Error(`Missing geometry reference: ${observation.relativeTo}`);
  try {
    return await element.evaluate((node, { request, explicitRoot }) => {
      const input = node instanceof HTMLInputElement || node instanceof HTMLTextAreaElement;
      switch (request.read) {
        case "text": return node instanceof HTMLElement ? node.innerText : node.textContent ?? "";
        case "value":
          if (!(input || node instanceof HTMLSelectElement)) throw new Error("value observation requires a real input, textarea, or select");
          return node.value;
        case "attribute":
          if (!request.attribute) throw new Error("attribute observation requires an attribute name");
          return node.getAttribute(request.attribute);
        case "css":
          if (!request.property) throw new Error("css observation requires a computed property name");
          return window.getComputedStyle(node).getPropertyValue(request.property);
        case "checked":
          if (!(node instanceof HTMLInputElement)) throw new Error("checked observation requires a real input");
          return node.checked;
        case "selectionStart": case "selectionEnd": case "selectionDirection":
          if (!input) throw new Error(`${request.read} observation requires a real input or textarea`);
          return node[request.read];
        case "scrollTop": case "scrollHeight": case "clientHeight":
          if (!request.unit || request.unit === "pixels") return node[request.read];
          break;
        case "rectWidth": case "rectHeight":
          if (!request.unit || request.unit === "pixels") {
            const rect = node.getBoundingClientRect();
            return request.read === "rectWidth" ? rect.width : rect.height;
          }
          break;
        case "count": throw new Error("count must be read from the locator, not one element");
      }
      const rect = node.getBoundingClientRect();
      let surface = node.closest("[data-omp-surface]")?.getAttribute("data-omp-surface");
      if (!surface && !explicitRoot) {
        const overlay = node.closest('[role="dialog"]') ?? node;
        const surfaces = [...new Set([...overlay.querySelectorAll("[data-omp-surface]")]
          .map(candidate => candidate.getAttribute("data-omp-surface"))
          .filter((identity): identity is string => identity !== null))];
        if (surfaces.length > 1) throw new Error("native dialog has ambiguous surface identities");
        surface = surfaces[0];
      }
      const candidates = [...document.querySelectorAll<HTMLElement>('[id^="omp-view-"][data-omp-surface]')]
        .filter(candidate => candidate.getAttribute("data-omp-surface") === surface && candidate.getBoundingClientRect().width > 0);
      if (!explicitRoot && candidates.length > 1) throw new Error("native observation has ambiguous surface positioning roots");
      const pane = explicitRoot ?? candidates[0] ?? node.closest<HTMLElement>("[data-omp-ui]");
      if (!pane) throw new Error("pane-relative geometry requires the native OMP positioning root");
      const root = pane.getBoundingClientRect();
      const viewport = window.visualViewport;
      const left = Math.max(root.left, viewport?.offsetLeft ?? 0);
      const top = Math.max(root.top, viewport?.offsetTop ?? 0);
      const right = Math.min(root.right, (viewport?.offsetLeft ?? 0) + (viewport?.width ?? window.innerWidth));
      const bottom = Math.min(root.bottom, (viewport?.offsetTop ?? 0) + (viewport?.height ?? window.innerHeight));
      switch (request.read) {
        case "leftHalf": return rect.left + rect.width / 2 < left + (right - left) / 2;
        case "rightHalf": return rect.left + rect.width / 2 >= left + (right - left) / 2;
        case "topHalf": return rect.top + rect.height / 2 < top + (bottom - top) / 2;
        case "bottomHalf": return rect.top + rect.height / 2 >= top + (bottom - top) / 2;
        case "insideViewport": return rect.width > 0 && rect.height > 0 && rect.left >= left - 0.5 && rect.right <= right + 0.5 && rect.top >= top - 0.5 && rect.bottom <= bottom + 0.5;
      }
      const horizontal = request.read === "rectX" || request.read === "rectWidth" || request.read === "rightGap";
      let divisor = 1;
      if (request.unit === "fraction") divisor = horizontal ? right - left : bottom - top;
      if (request.unit === "cells") {
        const panel = node.closest("[data-omp-overlay]") ?? node.closest('[role="dialog"]');
        const measurements = panel
          ? [panel.querySelector("[data-omp-overlay-metrics]")].filter((metric): metric is Element => metric !== null)
          : [...pane.querySelectorAll("[data-omp-overlay-metrics]")];
        if (measurements.length !== 1) throw new Error("cell geometry requires exactly one actual native overlay measurement element");
        const metrics = measurements[0];
        if (!metrics) throw new Error("cell geometry measurement disappeared");
        const cell = metrics.getBoundingClientRect();
        divisor = horizontal ? cell.width : cell.height;
      }
      if (!(divisor > 0)) throw new Error("geometry reference has no measurable extent");
      switch (request.read) {
        case "rectX": return (rect.left - left) / divisor;
        case "rectY": return (rect.top - top) / divisor;
        case "rectWidth": return rect.width / divisor;
        case "rectHeight": return rect.height / divisor;
        case "rightGap": return (right - rect.right) / divisor;
        case "bottomGap": return (bottom - rect.bottom) / divisor;
        case "scrollTop": case "scrollHeight": case "clientHeight": return node[request.read] / divisor;
        default: throw new Error(`Unsupported DOM geometry read: ${request.read}`);
      }
    }, { request: observation, explicitRoot });
  } finally {
    await explicitRoot?.dispose();
  }
}

export function createDOMMatcher(observation: DOMObservation): (value: unknown) => unknown | undefined {
  if (observation.read === "attribute" && !observation.attribute) throw new Error("attribute observation requires an attribute name");
  if (observation.pattern === undefined) {
    if (observation.flags !== undefined || observation.group !== undefined) throw new Error("DOM flags/group require a pattern");
    return value => value;
  }
  const pattern = new RegExp(observation.pattern, observation.flags);
  const group = observation.group ?? 0;
  if (!Number.isSafeInteger(group) || group < 0) throw new Error("DOM capture group must be a nonnegative integer");
  const shape = new RegExp(`(?:${pattern.source})|`, pattern.flags).exec("");
  if (!shape || group >= shape.length) throw new Error(`DOM pattern has no capture group ${group}`);
  return value => {
    if (typeof value !== "string") throw new Error("DOM pattern extraction requires a string observation");
    pattern.lastIndex = 0;
    const match = pattern.exec(value);
    return match?.[group];
  };
}
