import { appendFile, mkdir, realpath, stat, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { chromium, type Browser, type BrowserContext, type ConsoleMessage, type Locator, type Page } from "playwright";
import { createDOMMatcher, readDOM, uniqueElement, type DOMObservation } from "./browser-observation";
import { BrowserTransport, hostDefaultDoorOrigin } from "./browser-transport";
import type { BrowserAction } from "./scenario";

export interface BrowserLeg {
  page: Page;
  action(action: BrowserAction): Promise<void>;
  observe(observation: DOMObservation): Promise<unknown | undefined>;
  snapshot(directory: string): Promise<void>;
  stop(): Promise<void>;
}

export interface BrowserLegOptions {
  width?: number;
  height?: number;
  mobile?: boolean;
  allowedOrigins?: string[];
  virtualAudioFile?: string;
  locale?: string;
}

/**
 * A person cannot act on a control behind a modal. Playwright's fill and press
 * do not check `inert`, so an action waits for its target to leave an inert
 * subtree; otherwise its keys reach whichever control still holds focus.
 */
async function awaitInteractive(element: Locator): Promise<void> {
  const handle = await element.elementHandle({ timeout: 15_000 });
  await element.page().waitForFunction(node => !node?.isConnected || !node.closest("[inert]"), handle, { timeout: 15_000 });
}

interface ConsoleFailure { text: string; location: string }
interface HostEffect { channel: "builtin.clipboard"; value: string }
type ClipboardWindow = Window & typeof globalThis & { __ompParityHostEffect(effect: HostEffect): Promise<void> };

class RealBrowserLeg implements BrowserLeg {
  readonly #writes = new Map<string, Promise<void>>();
  readonly #eventTasks = new Set<Promise<void>>();
  readonly #failures: Error[] = [];
  readonly #consoleFailures: ConsoleFailure[] = [];
  readonly #observedErrors = new Set<string>();
  readonly #visibleErrors = new Set<string>();
  readonly #transport: BrowserTransport;
  #failureNumber = 0;
  #stopPromise: Promise<void> | undefined;
  page!: Page;

  constructor(
    readonly browser: Browser,
    readonly context: BrowserContext,
    readonly artifactsRoot: string,
    readonly hostTracePath: string,
    readonly origins: string[],
  ) {
    this.#transport = new BrowserTransport(origins,
      (channel, value) => this.#record("transport.jsonl", { channel, value }),
      (channel, value) => this.#append(hostTracePath, { channel, value }),
      error => this.#failures.push(error));
  }

  async initialize(): Promise<void> {
    await this.#transport.install(this.context);
    await this.context.exposeBinding("__ompParityHostEffect", async (_source, effect: unknown) => {
      if (!effect || typeof effect !== "object" || !("channel" in effect) || !("value" in effect) || effect.channel !== "builtin.clipboard" || typeof effect.value !== "string") {
        throw new Error("Malformed observational clipboard event");
      }
      await this.#append(this.hostTracePath, { channel: effect.channel, value: effect.value });
    });
    await this.context.addInitScript(({ origins }) => {
      if (!origins.includes(location.origin)) return;
      const clipboard = navigator.clipboard;
      if (!clipboard) throw new Error("The real browser clipboard API is unavailable");
      const nativeWrite = clipboard.writeText.bind(clipboard);
      clipboard.writeText = async text => {
        const value = String(text);
        await nativeWrite(value);
        await (window as ClipboardWindow).__ompParityHostEffect({ channel: "builtin.clipboard", value });
      };
    }, { origins: this.origins });
    this.context.on("page", page => this.#observePage(page));
    this.page = await this.context.newPage();
  }

  async action(action: BrowserAction): Promise<void> {
    await this.#guard("action", async () => {
      await this.#record("actions.jsonl", { stage: "start", action });
      switch (action.type) {
        case "click": case "doubleClick": case "fill": case "check": case "hover": case "select": case "upload": {
          const element = await uniqueElement(this.page, action.selector, true);
          if (!element) throw new Error(`Missing browser action target: ${action.selector}`);
          await awaitInteractive(element);
          switch (action.type) {
            case "click": await element.click(); break;
            case "doubleClick": await element.dblclick(); break;
            case "fill": await element.fill(action.value); break;
            case "check": await element.setChecked(action.checked); break;
            case "hover": await element.hover(); break;
            case "select": await element.selectOption(action.value); break;
            case "upload":
              for (const file of action.files) {
                if (!(await stat(file)).isFile()) throw new Error(`Browser upload is not a file: ${file}`);
              }
              await element.setInputFiles(action.files);
              break;
          }
          break;
        }
        case "press":
          if (action.selector) {
            const element = await uniqueElement(this.page, action.selector, true);
            if (!element) throw new Error(`Missing browser key target: ${action.selector}`);
            await awaitInteractive(element);
            await element.press(action.key);
          } else await this.page.keyboard.press(action.key);
          break;
        case "resize": await this.page.setViewportSize({ width: action.width, height: action.height }); break;
        case "reload": await this.page.reload({ waitUntil: "domcontentloaded" }); break;
        case "emulateMedia": await this.page.emulateMedia({ ...(action.reducedMotion ? { reducedMotion: action.reducedMotion } : {}), ...(action.colorScheme ? { colorScheme: action.colorScheme } : {}) }); break;
        case "transportDelay": await this.#transport.setDelay(action.milliseconds, action.direction); break;
        case "nativeEvent": {
          const element = await uniqueElement(this.page, action.selector, true);
          if (!element) throw new Error(`Missing native event target: ${action.selector}`);
          const target = await element.evaluate(node => ({
            surface: node.closest("[data-omp-surface]")?.getAttribute("data-omp-surface") ?? "",
            node: node.closest("[data-omp-node]")?.getAttribute("data-omp-node") ?? "",
          }));
          if (!target.surface || !target.node) throw new Error(`Native event target has no rendered OMP node: ${action.selector}`);
          await this.#transport.ompUi.sendEvent(this.page, target, action.event);
          break;
        }
        case "nativeOpen": await this.#transport.ompUi.refreshOpen(this.page, action.features); break;
      }
      await this.#record("actions.jsonl", { stage: "complete", action });
    });
  }

  async observe(observation: DOMObservation): Promise<unknown | undefined> {
    return this.#guard("observation", async () => {
      const match = createDOMMatcher(observation);
      const locator = this.page.locator(observation.selector);
      const element = observation.read === "count" ? undefined : await uniqueElement(this.page, observation.selector, false);
      const reference = observation.relativeTo ? await uniqueElement(this.page, observation.relativeTo, false) : undefined;
      if (observation.relativeTo && !reference) return undefined;
      const raw = observation.read === "count"
        ? await locator.count()
        : element
          ? await readDOM(element, observation, reference)
          : observation.read === "visible" ? false : undefined;
      if (raw === undefined) return undefined;
      const value = match(raw);
      if (element && typeof value === "string" && value.trim()) {
        const alerts = await element.evaluate(node => {
          const closest = node.closest('[role="alert"]');
          return [closest, ...node.querySelectorAll('[role="alert"]')]
            .filter((alert): alert is Element => alert !== null)
            .map(alert => alert instanceof HTMLElement ? alert.innerText : alert.textContent ?? "");
        });
        for (const text of alerts) {
          if (normalized(text).includes(normalized(value)) || normalized(value).includes(normalized(text))) this.#observedErrors.add(normalized(text));
        }
      }
      return value;
    });
  }

  async snapshot(directory: string): Promise<void> {
    await this.#capture(directory);
    this.#assertCritical();
  }

  stop(): Promise<void> {
    this.#stopPromise ??= this.#stop();
    return this.#stopPromise;
  }

  async #stop(): Promise<void> {
    const errors: Error[] = [];
    try {
      await this.#transport.drain();
      await this.#capture(join(this.artifactsRoot, "final"));
      this.#assertCritical();
      await this.#assertReportedErrors();
    } catch (error) { errors.push(asError(error)); }
    try { await this.#transport.drain(); } catch (error) { errors.push(asError(error)); }
    this.#transport.beginClose();
    try { await this.context.close(); } catch (error) { errors.push(asError(error)); }
    try { await this.#transport.drain(); } catch (error) { errors.push(asError(error)); }
    try { await this.browser.close(); } catch (error) { errors.push(asError(error)); }
    try { await this.#flushLogs(); } catch (error) { errors.push(asError(error)); }
    if (errors.length) throw new AggregateError(errors, "Real parity browser failed");
  }

  #observePage(page: Page): void {
    page.setDefaultTimeout(15_000);
    page.setDefaultNavigationTimeout(30_000);
    page.on("console", message => this.#trackLog(this.#console(message)));
    page.on("pageerror", error => {
      this.#failures.push(error);
      this.#trackLog(this.#record("pageerrors.jsonl", { message: error.message, stack: error.stack, url: page.url() }));
    });
    page.on("crash", () => this.#failures.push(new Error(`Browser page crashed: ${page.url()}`)));
    page.on("dialog", dialog => {
      this.#failures.push(new Error(`Unexpected browser-native ${dialog.type()} dialog: ${dialog.message()}`));
      this.#trackLog(this.#record("dialogs.jsonl", { type: dialog.type(), message: dialog.message(), url: page.url() }));
    });
  }

  async #console(message: ConsoleMessage): Promise<void> {
    const location = message.location();
    await this.#record("console.jsonl", { type: message.type(), text: message.text(), location });
    if (message.type() !== "error") return;
    // Connect refusals are intentionally exercised through their actual visible UI errors.
    const requestRefusal = /^Failed to load resource: the server responded with a status of [45]\d\d\b/.test(message.text())
      && this.origins.some(origin => location.url.startsWith(`${origin}/roost.v1.CoordinatorService/`));
    // Local-door discovery asks the serving coordinator (no door: 404) and the refused host default door;
    // the client treats both as "no local door" and Chromium logs each failed fetch.
    const doorDiscovery = location.url.endsWith("/api/local-bootstrap")
      && ((location.url === `${hostDefaultDoorOrigin}/api/local-bootstrap` && message.text() === "Failed to load resource: net::ERR_CONNECTION_REFUSED")
        || (/^Failed to load resource: the server responded with a status of 404\b/.test(message.text()) && this.origins.some(origin => location.url === `${origin}/api/local-bootstrap`)));
    if (!requestRefusal && !doorDiscovery) this.#consoleFailures.push({ text: message.text(), location: location.url });
  }

  async #assertReportedErrors(): Promise<void> {
    await this.#rememberVisibleErrors();
    const acknowledged = (text: string) => [...this.#observedErrors].some(observed => observed.length > 0 && normalized(text).includes(observed));
    const unexpected = this.#consoleFailures.filter(failure => !acknowledged(failure.text));
    for (const text of this.#visibleErrors) {
      if (!acknowledged(text)) unexpected.push({ text, location: this.page.url() });
    }
    if (unexpected.length) throw new Error(`Unexpected browser/projection errors: ${JSON.stringify(unexpected)}`);
  }

  async #rememberVisibleErrors(): Promise<void> {
    for (const text of await this.page.locator('[role="alert"]:visible').allTextContents()) {
      if (text.trim()) this.#visibleErrors.add(normalized(text));
    }
  }

  #assertCritical(): void {
    if (this.#failures.length) throw new AggregateError([...this.#failures], "Browser JavaScript or real transport failed");
  }

  async #guard<Value>(stage: string, operation: () => Promise<Value>): Promise<Value> {
    try {
      await this.#rememberVisibleErrors();
      this.#assertCritical();
      const result = await operation();
      await this.#rememberVisibleErrors();
      this.#assertCritical();
      return result;
    } catch (error) {
      await this.#record("failures.jsonl", { stage, message: asError(error).message, stack: asError(error).stack });
      try { await this.#capture(join(this.artifactsRoot, `failure-${++this.#failureNumber}`)); }
      catch (captureError) { await this.#record("failures.jsonl", { stage: "failure-snapshot", message: asError(captureError).message }); }
      throw error;
    }
  }

  async #capture(directory: string): Promise<void> {
    await mkdir(directory, { recursive: true });
    const errors: Error[] = [];
    const pages = [this.page, ...this.context.pages().filter(page => page !== this.page)];
    for (const [index, page] of pages.entries()) {
      if (page.isClosed()) continue;
      const prefix = index === 0 ? "page" : `popup-${index}`;
      for (const capture of [
        () => page.screenshot({ path: join(directory, `${prefix}.png`), fullPage: true }).then(() => undefined),
        async () => writeFile(join(directory, `${prefix}.html`), await page.content()),
        async () => writeFile(join(directory, `${prefix}.aria.yaml`), await page.locator("body").ariaSnapshot()),
        async () => writeFile(join(directory, `${prefix}.txt`), await page.locator("body").innerText()),
        () => writeFile(join(directory, `${prefix}.json`), JSON.stringify({ url: page.url(), viewport: page.viewportSize() }, null, 2)),
      ]) {
        try { await capture(); } catch (error) { errors.push(asError(error)); }
      }
    }
    await this.#flushLogs();
    if (errors.length) throw new AggregateError(errors, "Capturing real browser artifacts failed");
  }

  #record(file: string, value: unknown): Promise<void> {
    return this.#append(join(this.artifactsRoot, file), { time: new Date().toISOString(), ...objectRecord(value) });
  }

  #append(path: string, value: unknown): Promise<void> {
    const previous = this.#writes.get(path) ?? Promise.resolve();
    const next = previous.then(() => appendFile(path, `${JSON.stringify(value)}\n`));
    this.#writes.set(path, next);
    return next;
  }

  #trackLog(task: Promise<void>): void {
    const retained = task.catch(error => { this.#failures.push(asError(error)); });
    this.#eventTasks.add(retained);
    void retained.then(() => this.#eventTasks.delete(retained));
  }

  async #flushLogs(): Promise<void> {
    while (this.#eventTasks.size > 0) await Promise.all([...this.#eventTasks]);
    await Promise.all(this.#writes.values());
  }
}

export async function startBrowserLeg(origin: string, artifactsRoot: string, hostTracePath: string, options: BrowserLegOptions = {}): Promise<BrowserLeg> {
  await mkdir(artifactsRoot, { recursive: true });
  await mkdir(dirname(hostTracePath), { recursive: true });
  const origins = [...new Set([origin, ...(options.allowedOrigins ?? [])].map(value => new URL(value).origin))];
  const args: string[] = [];
  if (options.virtualAudioFile) {
    const audioFile = await realpath(options.virtualAudioFile);
    const traceDirectory = await realpath(dirname(hostTracePath));
    if (audioFile !== resolve(options.virtualAudioFile) || dirname(audioFile) !== traceDirectory || !(await stat(audioFile)).isFile()) {
      throw new Error("Virtual browser audio must be a real file in the owned profile trace directory");
    }
    const header = Buffer.from(await Bun.file(audioFile).slice(0, 12).arrayBuffer());
    if (header.subarray(0, 4).toString() !== "RIFF" || header.subarray(8, 12).toString() !== "WAVE") {
      throw new Error("Virtual browser audio requires an actual owned WAV capture file");
    }
    args.push("--use-fake-device-for-media-stream", "--use-fake-ui-for-media-stream", `--use-file-for-fake-audio-capture=${audioFile}`, "--mute-audio");
  }
  const browser = await chromium.launch({ headless: true, args });
  try {
    const context = await browser.newContext({
      viewport: { width: options.width ?? (options.mobile ? 390 : 1280), height: options.height ?? (options.mobile ? 844 : 900) },
      isMobile: options.mobile ?? false,
      hasTouch: options.mobile ?? false,
      deviceScaleFactor: 1,
      serviceWorkers: "block",
      ...(options.locale ? { locale: options.locale } : {}),
    });
    for (const allowedOrigin of origins) {
      await context.grantPermissions(["clipboard-read", "clipboard-write", ...(options.virtualAudioFile ? ["microphone"] : [])], { origin: allowedOrigin });
    }
    const leg = new RealBrowserLeg(browser, context, artifactsRoot, hostTracePath, origins);
    await leg.initialize();
    return leg;
  } catch (error) {
    await browser.close();
    throw error;
  }
}

function normalized(value: string): string { return value.replace(/\s+/g, " ").trim(); }
function asError(value: unknown): Error { return value instanceof Error ? value : new Error(String(value)); }
function objectRecord(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Browser log records must be objects");
  return { ...value };
}
