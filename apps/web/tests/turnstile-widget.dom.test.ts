// Turnstile must rerender at the provider's 300px size boundary after layout changes.
// Solid's universal renderer supplies a live tree for ResizeObserver and provider callbacks.
// The tests also prove removed retries and challenge frames cannot retain keyboard focus.

import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import type * as SolidApi from "solid-js";
import {
  click,
  createSolidTestHarness,
  findByTestId,
  type TestElement,
} from "./solid-universal-test-harness.ts";

interface RenderOptions {
  size: "normal" | "compact";
}

const solidClientUrl = new URL("./solid.js", import.meta.resolve("solid-js"));
const Solid = await import(solidClientUrl.href) as typeof SolidApi;
const harness = await createSolidTestHarness(Solid);
const { document: fakeDocument, element, renderer } = harness;

class FakeResizeObserver {
  static instances: FakeResizeObserver[] = [];
  observed: TestElement | undefined;
  disconnected = false;

  constructor(readonly callback: () => void) {
    FakeResizeObserver.instances.push(this);
  }

  observe(observed: TestElement): void {
    this.observed = observed;
  }

  disconnect(): void {
    this.disconnected = true;
  }

  trigger(): void {
    this.callback();
  }
}

let failNextRender = false;
let widgetSequence = 0;
let renderSizes: Array<"normal" | "compact"> = [];
let removedWidgets: string[] = [];
const widgetHosts = new Map<string, TestElement>();

const turnstileApi = {
  render(container: TestElement, options: RenderOptions): string {
    renderSizes.push(options.size);
    if (failNextRender) {
      failNextRender = false;
      throw new Error("provider render failed");
    }
    const widgetId = `widget-${++widgetSequence}`;
    const iframe = renderer.createElement("iframe");
    renderer.setProp(iframe, "data-testid", `turnstile-frame-${widgetSequence}`);
    renderer.insertNode(container, iframe);
    widgetHosts.set(widgetId, container);
    return widgetId;
  },
  reset: () => undefined,
  remove(widgetId: string): void {
    removedWidgets.push(widgetId);
    const container = widgetHosts.get(widgetId);
    const iframe = container?.querySelector<TestElement>("iframe");
    if (container && iframe) iframe.remove();
    widgetHosts.delete(widgetId);
  },
};

Object.defineProperty(globalThis, "document", {
  configurable: true,
  value: fakeDocument,
});
Object.defineProperty(globalThis, "window", {
  configurable: true,
  value: { turnstile: turnstileApi },
});
Object.defineProperty(globalThis, "ResizeObserver", {
  configurable: true,
  value: FakeResizeObserver,
});
mock.module("../src/components/Settings/md/Button.tsx", () => ({
  Button: (props: Record<string, unknown>) => element("button", props),
}));

// The component must bind the fake browser globals and provider API installed above.
const { TurnstileWidget } = await import("../src/components/TurnstileWidget.tsx");

function mountWidget(): TestElement {
  const root = harness.mount(() => TurnstileWidget({
    siteKey: "site-key",
    resetNonce: 0,
    onToken: () => undefined,
  }));
  const host = findByTestId(root, "managed-turnstile");
  if (!host) throw new Error("challenge host did not mount");
  host.clientWidth = 300;
  return root;
}

function challengeFrame(host: TestElement): TestElement {
  const iframe = host.querySelector<TestElement>("iframe");
  if (!iframe) throw new Error("challenge frame did not mount");
  return iframe;
}

beforeEach(() => {
  failNextRender = false;
  widgetSequence = 0;
  renderSizes = [];
  removedWidgets = [];
  widgetHosts.clear();
  FakeResizeObserver.instances = [];
  fakeDocument.activeElement = null;
});

afterEach(() => harness.cleanup());

describe("Turnstile responsive focus", () => {
  test("retry focus survives another failure and transfers into the recovered challenge", async () => {
    failNextRender = true;
    const root = mountWidget();
    await harness.settle();
    let retry = findByTestId(root, "managed-turnstile-retry");
    if (!retry) throw new Error("retry action did not render");

    retry.focus();
    failNextRender = true;
    click(retry);
    expect(fakeDocument.activeElement?.testId).toBe("managed-turnstile");
    await harness.settle();
    retry = findByTestId(root, "managed-turnstile-retry");
    if (!retry) throw new Error("repeated failure did not restore retry");
    expect(fakeDocument.activeElement).toBe(retry);

    click(retry);
    expect(fakeDocument.activeElement?.testId).toBe("managed-turnstile");
    await harness.settle();
    expect(findByTestId(root, "managed-turnstile-retry")).toBeUndefined();
    expect(fakeDocument.activeElement).toBe(challengeFrame(findByTestId(root, "managed-turnstile")!));
  });

  test("settles the latest width across an in-flight threshold reversal", async () => {
    const root = mountWidget();
    await harness.settle();
    const host = findByTestId(root, "managed-turnstile");
    const observer = FakeResizeObserver.instances[0];
    if (!host || !observer) throw new Error("responsive challenge did not mount");
    expect(renderSizes).toEqual(["normal"]);

    challengeFrame(host).focus();
    host.clientWidth = 299;
    observer.trigger();
    host.clientWidth = 300;
    observer.trigger();
    await harness.settle();
    expect(renderSizes).toEqual(["normal", "normal"]);
    expect(fakeDocument.activeElement).toBe(challengeFrame(host));

    host.clientWidth = 299;
    observer.trigger();
    await harness.settle();
    expect(renderSizes).toEqual(["normal", "normal", "compact"]);
    expect(fakeDocument.activeElement).toBe(challengeFrame(host));

    host.clientWidth = 300;
    observer.trigger();
    await harness.settle();
    expect(renderSizes).toEqual(["normal", "normal", "compact", "normal"]);
    expect(removedWidgets).toEqual(["widget-1", "widget-2", "widget-3"]);
    expect(fakeDocument.activeElement).toBe(challengeFrame(host));
    expect(findByTestId(root, "managed-turnstile-retry")).toBeUndefined();
  });
});
