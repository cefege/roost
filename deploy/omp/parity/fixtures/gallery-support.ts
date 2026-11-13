import type { Component } from "@omp-source/packages/tui/src/tui";
import type { DescribeContext, NativeNode, NativeUiEvent } from "@omp-source/packages/tui/src/native/node";
import type { ExtensionUiComponent } from "@omp-source/packages/coding-agent/src/extensibility/extensions/types";
import type { Comparison, ComparisonReference, FixtureContext } from "../scenario";

export function contentComparison(name: string, pattern: string, selector: string, expected?: string, proves?: Comparison["proves"]): Comparison {
  const browser: { type: "dom"; selector: string; read: "text"; pattern: string; group: number } = {
    type: "dom", selector, read: "text", pattern, group: 1,
  };
  return { name, terminal: { type: "screen", pattern, group: 1 }, browser, expected, normalize: "whitespace", ...(proves ? { proves } : {}) };
}

export function linkComparison(name: string, label: string, href: string, selector: string, target: ComparisonReference, entry: string): Comparison {
  const escape = (value: string) => value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return {
    name,
    terminal: { type: "ansi", pattern: `\\x1b\\]8;[^;\\x07\\x1b]*;(${escape(href)})(?:\\x07|\\x1b\\\\)(?:(?!\\x1b\\]8;)[\\s\\S])*?${escape(label)}`, group: 1 },
    browser: { type: "dom", selector, read: "attribute", attribute: "href" },
    expected: href,
    proves: [{ entry, effect: "link", value: href, target }],
  };
}

export interface GalleryMount extends Component {
  describe(context: DescribeContext): NativeNode;
  handleNativeEvent?(event: NativeUiEvent): void;
  dispose?(): void;
}

export async function controlledGallery(
  context: FixtureContext,
  component: GalleryMount,
  actions: Readonly<Record<string, () => void>>,
): Promise<ExtensionUiComponent> {
  const { col, node } = await context.load<typeof import("@omp-source/packages/tui/src/native/describe")>("packages/tui/src/native/describe.ts");
  return {
    render(width) { return [...component.render(width)]; },
    describe() {
      return col([
        component,
        node("row", {}, Object.keys(actions).map(action => node("button", {
          text: action, actions: { click: action },
        }, undefined, action))),
      ]);
    },
    handleInput(data) {
      if (Object.keys(actions).length === 0) {
        component.handleInput?.(data);
        context.tui.requestRender();
        return;
      }
      for (const key of data) {
        const action = Object.keys(actions).find(candidate => candidate[0] === key);
        if (action) actions[action]();
        else component.handleInput?.(key);
      }
      context.tui.requestRender();
    },
    handleNativeEvent(event) {
      if (event.type === "action" && actions[event.act]) actions[event.act]();
      else component.handleNativeEvent?.(event);
      context.tui.requestRender();
    },
    invalidate() { component.invalidate?.(); },
    dispose() { component.dispose?.(); },
  };
}

export function assertRegistry(actual: readonly string[], expected: readonly string[], family: string): void {
  const missing = actual.filter(key => !expected.includes(key));
  const stale = expected.filter(key => !actual.includes(key));
  if (missing.length || stale.length) throw new Error(`${family} fixture inventory mismatch: missing=${missing.join(",")} stale=${stale.join(",")}`);
}
