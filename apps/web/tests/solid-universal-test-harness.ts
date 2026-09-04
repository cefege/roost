// Focus-sensitive component tests need Solid's compiler rather than Bun's snapshot JSX transform.
// This harness compiles app TSX to Solid's universal renderer and maintains a small live node tree.
// Tests query only connected descendants, so branch replacement exposes stale-focus defects.

import { mock } from "bun:test";
import type * as SolidApi from "solid-js";
import type { Renderer as UniversalRenderer } from "solid-js/universal";
import solidPlugin from "vite-plugin-solid";

export class TestElement {
  readonly children: TestElement[] = [];
  readonly properties: Record<string, unknown> = {};
  parent: TestElement | undefined;
  textContent = "";
  tabIndex = 0;
  clientWidth = 0;
  rootConnected = false;
  dataset: Record<string, string> = {};

  constructor(
    readonly tag: string,
    private readonly ownerDocument: TestDocument,
  ) {}

  get isConnected(): boolean {
    return this.rootConnected || this.parent?.isConnected === true;
  }

  get testId(): string | undefined {
    return this.properties["data-testid"] as string | undefined;
  }
  focus(): void {
    if (this.isConnected) this.ownerDocument.focus(this);
  }

  contains(candidate: TestElement | null): boolean {
    if (candidate === this) return true;
    return this.children.some((child) => child.contains(candidate));
  }

  querySelector<T extends TestElement = TestElement>(selector: string): T | null {
    return findElement(this, (element) => matchesSelector(element, selector)) as T | null;
  }

  addEventListener(): void {}
  removeEventListener(): void {}

  remove(): void {
    if (this.parent) this.ownerDocument.removeChild(this.parent, this);
  }
}

export class TestDocument {
  activeElement: TestElement | null = null;
  readonly head = new TestElement("head", this);
  private readonly focusListeners = new Set<(event: { target: TestElement }) => void>();
  private readonly roots: TestElement[] = [];

  constructor() {
    this.head.rootConnected = true;
  }

  createElement(tag: string): TestElement {
    return new TestElement(tag, this);
  }

  querySelector<T extends TestElement = TestElement>(selector: string): T | null {
    if (matchesSelector(this.head, selector)) return this.head as T;
    const headMatch = findElement(this.head, (element) => matchesSelector(element, selector));
    if (headMatch) return headMatch as T;
    for (const root of this.roots) {
      const match = findElement(root, (element) => matchesSelector(element, selector));
      if (match) return match as T;
    }
    return null;
  }
  addEventListener(type: string, listener: (event: { target: TestElement }) => void): void {
    if (type === "focusin") this.focusListeners.add(listener);
  }

  removeEventListener(type: string, listener: (event: { target: TestElement }) => void): void {
    if (type === "focusin") this.focusListeners.delete(listener);
  }

  focus(element: TestElement): void {
    this.activeElement = element;
    for (const listener of this.focusListeners) listener({ target: element });
  }


  appendRoot(root: TestElement): void {
    root.rootConnected = true;
    this.roots.push(root);
  }

  removeRoot(root: TestElement): void {
    if (this.activeElement && root.contains(this.activeElement)) this.activeElement = null;
    root.rootConnected = false;
    const index = this.roots.indexOf(root);
    if (index >= 0) this.roots.splice(index, 1);
  }

  removeChild(parent: TestElement, child: TestElement): void {
    if (this.activeElement && child.contains(this.activeElement)) this.activeElement = null;
    const index = parent.children.indexOf(child);
    if (index >= 0) parent.children.splice(index, 1);
    child.parent = undefined;
  }
}

type Solid = typeof SolidApi;
type Renderer = UniversalRenderer<TestElement>;

export interface SolidTestHarness {
  document: TestDocument;
  renderer: Renderer;
  element: (tag: string, props?: Record<string, unknown>) => TestElement;
  mount: (component: () => unknown) => TestElement;
  settle: () => Promise<void>;
  cleanup: () => void;
}

export async function createSolidTestHarness(SolidClient: Solid): Promise<SolidTestHarness> {
  mock.module("solid-js", () => ({ ...SolidClient }));
  const { createRenderer } = await import("solid-js/universal");
  const document = new TestDocument();
  const renderer = createRenderer<TestElement>({
    createElement: (tag) => document.createElement(tag),
    createTextNode: (value) => {
      const text = document.createElement("#text");
      text.textContent = value;
      return text;
    },
    replaceText: (text, value) => { text.textContent = value; },
    isTextNode: (node) => node.tag === "#text",
    setProperty: (node, name, value) => {
      node.properties[name] = value;
      if (name === "tabIndex" && typeof value === "number") node.tabIndex = value;
      if (name === "src" && typeof value === "string") node.properties.src = value;
    },
    insertNode: (parent, node, anchor) => {
      if (node.parent) document.removeChild(node.parent, node);
      const index = anchor ? parent.children.indexOf(anchor) : -1;
      if (index >= 0) parent.children.splice(index, 0, node);
      else parent.children.push(node);
      node.parent = parent;
    },
    removeNode: (parent, node) => document.removeChild(parent, node),
    getParentNode: (node) => node.parent,
    getFirstChild: (node) => node.children[0],
    getNextSibling: (node) => {
      if (!node.parent) return undefined;
      const index = node.parent.children.indexOf(node);
      return index >= 0 ? node.parent.children[index + 1] : undefined;
    },
  });

  mock.module("react/jsx-dev-runtime", () => ({ ...renderer }));
  const compiler = solidPlugin({
    hot: false,
    solid: { generate: "universal", moduleName: "react/jsx-dev-runtime" },
  });
  const transform = compiler.transform as (
    source: string,
    id: string,
    options: { ssr: boolean },
  ) => Promise<{ code: string } | null>;
  Bun.plugin({
    name: "solid-universal-focus-tests",
    setup(builder) {
      builder.onLoad({ filter: /\/apps\/web\/src\/.*\.tsx$/ }, async ({ path }) => {
        const transformed = await transform(await Bun.file(path).text(), path, { ssr: false });
        return transformed ? { contents: transformed.code, loader: "ts" } : undefined;
      });
    },
  });

  const disposers: Array<() => void> = [];
  const element = (tag: string, props: Record<string, unknown> = {}): TestElement => {
    const node = renderer.createElement(tag);
    renderer.spread(node, props);
    return node;
  };
  const mount = (component: () => unknown): TestElement => {
    const root = document.createElement("test-root");
    document.appendRoot(root);
    const dispose = renderer.render(component as () => TestElement, root);
    disposers.push(() => {
      dispose();
      document.removeRoot(root);
    });
    return root;
  };

  return {
    document,
    renderer,
    element,
    mount,
    async settle() {
      for (let idx = 0; idx < 4; idx++) await Promise.resolve();
    },
    cleanup() {
      while (disposers.length > 0) disposers.pop()?.();
      document.activeElement = null;
    },
  };
}

export function findByTestId(root: TestElement, testId: string): TestElement | undefined {
  return findElement(root, (element) => element.testId === testId);
}

export function findByTag(root: TestElement, tag: string): TestElement | undefined {
  return findElement(root, (element) => element.tag === tag);
}

export function textOf(element: TestElement): string {
  return element.tag === "#text"
    ? element.textContent
    : element.children.map(textOf).join("");
}

export function findButton(root: TestElement, text: string): TestElement | undefined {
  return findElement(root, (element) => element.tag === "button" && textOf(element).includes(text));
}

export function click(element: TestElement): void {
  const onClick = element.properties.onClick;
  if (typeof onClick !== "function") throw new Error("node has no click action");
  onClick();
}

function findElement(
  root: TestElement,
  predicate: (element: TestElement) => boolean,
): TestElement | undefined {
  for (const child of root.children) {
    if (predicate(child)) return child;
    const descendant = findElement(child, predicate);
    if (descendant) return descendant;
  }
  return undefined;
}

function matchesSelector(element: TestElement, selector: string): boolean {
  if (selector === element.tag) return true;
  const scriptSource = selector.match(/^script\[src="(.+)"\]$/)?.[1];
  return element.tag === "script" && element.properties.src === scriptSource;
}
