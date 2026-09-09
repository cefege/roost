// Chrome mode tests cover browser-local persistence and document presentation.
// A small fake browser profile proves storage failures and cross-tab events stay local.
// The engine is imported only after the fake globals exist because it subscribes at module load.

import { beforeEach, describe, expect, test } from "bun:test";

const CHROME_MODE_STORAGE_KEY = "roost.chromeMode.v1";

class BrowserStorage implements Storage {
  private readonly values = new Map<string, string>();
  readsBlocked = false;
  writesBlocked = false;
  writeCount = 0;

  get length(): number {
    return this.values.size;
  }

  clear(): void {
    this.values.clear();
  }

  getItem(key: string): string | null {
    if (this.readsBlocked) throw new Error("storage unavailable");
    return this.values.get(key) ?? null;
  }

  key(index: number): string | null {
    return [...this.values.keys()][index] ?? null;
  }

  removeItem(key: string): void {
    if (this.writesBlocked) throw new Error("storage unavailable");
    this.values.delete(key);
  }

  setItem(key: string, value: string): void {
    if (this.writesBlocked) throw new Error("storage unavailable");
    this.writeCount++;
    this.values.set(key, value);
  }

  reset(): void {
    this.values.clear();
    this.readsBlocked = false;
    this.writesBlocked = false;
    this.writeCount = 0;
  }

  seed(key: string, value: string): void {
    this.values.set(key, value);
  }
}

class BrowserRoot {
  private readonly attributes = new Map<string, string>();

  getAttribute(name: string): string | null {
    return this.attributes.get(name) ?? null;
  }

  setAttribute(name: string, value: string): void {
    this.attributes.set(name, value);
  }
}

class BrowserWindow {
  private readonly storageListeners = new Set<(event: StorageEvent) => void>();

  addEventListener(type: string, listener: (event: StorageEvent) => void): void {
    if (type === "storage") this.storageListeners.add(listener);
  }

  emitStorage(key: string | null, newValue: string | null): void {
    for (const listener of this.storageListeners) {
      listener({ key, newValue } as StorageEvent);
    }
  }
}

const storage = new BrowserStorage();
const root = new BrowserRoot();
const browserWindow = new BrowserWindow();

Object.defineProperty(globalThis, "localStorage", { configurable: true, value: storage });
Object.defineProperty(globalThis, "document", {
  configurable: true,
  value: { documentElement: root } as unknown as Document,
});
Object.defineProperty(globalThis, "window", { configurable: true, value: browserWindow as unknown as Window });

const {
  applyChromeMode,
  currentChromeMode,
  loadChromeMode,
  setChromeMode,
} = await import("../src/lib/chromeMode.ts");

describe("chromeMode", () => {
  beforeEach(() => {
    storage.reset();
    applyChromeMode("roost");
  });

  test("loads only valid persisted choices without repairing storage", () => {
    expect(loadChromeMode()).toBe("roost");
    storage.seed(CHROME_MODE_STORAGE_KEY, "workbench");
    expect(loadChromeMode()).toBe("workbench");
    storage.seed(CHROME_MODE_STORAGE_KEY, "unexpected");
    expect(loadChromeMode()).toBe("roost");
    expect(storage.writeCount).toBe(0);
  });

  test("falls back to roost when browser storage is blocked", () => {
    storage.seed(CHROME_MODE_STORAGE_KEY, "workbench");
    storage.readsBlocked = true;
    expect(loadChromeMode()).toBe("roost");

    storage.writesBlocked = true;
    setChromeMode("workbench");
    expect(currentChromeMode()).toBe("workbench");
    expect(root.getAttribute("data-chrome-mode")).toBe("workbench");
  });

  test("applies document chrome state without persisting or changing the theme", () => {
    root.setAttribute("data-theme", "night");
    applyChromeMode("workbench");

    expect(currentChromeMode()).toBe("workbench");
    expect(root.getAttribute("data-chrome-mode")).toBe("workbench");
    expect(root.getAttribute("data-theme")).toBe("night");
    expect(storage.writeCount).toBe(0);
  });

  test("persists an explicit valid choice before applying it", () => {
    setChromeMode("workbench");

    expect(storage.getItem(CHROME_MODE_STORAGE_KEY)).toBe("workbench");
    expect(root.getAttribute("data-chrome-mode")).toBe("workbench");
    expect(currentChromeMode()).toBe("workbench");
  });

  test("adopts valid cross-tab storage changes and resets after clearing", () => {
    browserWindow.emitStorage(CHROME_MODE_STORAGE_KEY, "workbench");
    expect(currentChromeMode()).toBe("workbench");
    expect(root.getAttribute("data-chrome-mode")).toBe("workbench");

    browserWindow.emitStorage(null, null);
    expect(currentChromeMode()).toBe("roost");
    expect(root.getAttribute("data-chrome-mode")).toBe("roost");
  });
});
