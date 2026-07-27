import type { SmokeApi } from "./smoke.ts";

type Step = { name: string; pass: boolean; detail: unknown };

function nextFrame(): Promise<void> {
  const { promise, resolve } = Promise.withResolvers<void>();
  requestAnimationFrame(() => resolve());
  return promise;
}

async function frames(count: number): Promise<void> {
  for (let index = 0; index < count; index++) await nextFrame();
}

async function waitFor(check: () => boolean, frameLimit: number): Promise<boolean> {
  for (let frame = 0; frame < frameLimit; frame++) {
    if (check()) return true;
    await nextFrame();
  }
  return check();
}

export async function runFlow(api: SmokeApi): Promise<{ steps: Step[]; summary: string }> {
  const steps: Step[] = [];
  const record = (name: string, pass: boolean, detail: unknown) => steps.push({ name, pass, detail });

  try {
    const workers = Object.entries(api.state().workers) as Array<[string, { last_seen_ms?: number }]>;
    workers.sort(([, left], [, right]) => (right.last_seen_ms ?? 0) - (left.last_seen_ms ?? 0));
    const workerFp = workers[0]?.[0];
    record("worker_available", !!workerFp, { workerFp });
    if (!workerFp) return { steps, summary: "0/1 passed" };

    const shell = await api.spawnShell(workerFp, "/tmp");
    history.pushState({}, "", `/s/${shell.session_id}`);
    window.dispatchEvent(new PopStateEvent("popstate"));
    const shellSlot = await waitFor(() => api.renderProbe(shell.session_id).found, 300);
    record("shell_painted", shellSlot, { sessionId: shell.session_id });
    await waitFor(() => api.renderProbe(shell.session_id).nonEmptyRows > 0, 300);

    const workspace = await api.createWorkspace(workerFp, "/", shell.session_id);
    record("workspace_created", !!workspace.id, workspace);

    const marker = `ROOST_SMOKE_${crypto.randomUUID().slice(0, 8)}`;
    await api.input(shell.session_id, `printf '%s\\n' ${marker}\n`);
    const echoed = await waitFor(() => api.viewportText(shell.session_id).includes(marker), 300);
    record("shell_round_trip", echoed, { marker });
  } catch (error) {
    record("flow_exception", false, String(error));
  } finally {
    const cleanup = await api.cleanupCreated();
    record("cleanup", cleanup.errors.length === 0, cleanup);
  }

  const passed = steps.filter((step) => step.pass).length;
  return { steps, summary: `${passed}/${steps.length} passed` };
}

export async function runRenderStress(
  api: SmokeApi,
  options: { sessionId: string; prefix: string; screen: "main" | "alt"; iterations: number },
): Promise<{ verdict: "PASS" | "FAIL"; iterations: number; failCount: number; fails: unknown[] }> {
  const deck = document.querySelector('[data-testid="terminal-deck"]') as HTMLElement | null;
  if (!deck) return { verdict: "FAIL", iterations: 0, failCount: 1, fails: ["terminal deck missing"] };

  const original = deck.getAttribute("style");
  const perturbations = [[-200, 0], [200, 0], [0, -180], [0, 180], [-200, -180], [200, 180]] as const;
  const baseline = api.markerScan(options.sessionId, options.prefix);
  const fails: unknown[] = [];
  try {
    for (let iteration = 0; iteration < options.iterations; iteration++) {
      const [x, y] = perturbations[iteration % perturbations.length]!;
      const rect = deck.getBoundingClientRect();
      deck.style.width = `${Math.max(220, Math.round(rect.width + x))}px`;
      deck.style.height = `${Math.max(180, Math.round(rect.height + y))}px`;
      const beforeFrames = api.cellFrameCount(options.sessionId);
      const mode = api.renderProbe(options.sessionId).mode;
      if (mode === "cell") {
        await waitFor(() => api.cellFrameCount(options.sessionId) > beforeFrames, 120);
      } else {
        await frames(6);
      }
      await frames(2);
      const scan = api.markerScan(options.sessionId, options.prefix);
      const changedRange = scan.max !== baseline.max || scan.min !== baseline.min;
      if (scan.duplicated.length || scan.outOfOrder || (options.screen === "main" && changedRange)) {
        fails.push({ iteration, screen: options.screen, scan, baseline });
      }
    }
  } finally {
    if (original === null) deck.removeAttribute("style");
    else deck.setAttribute("style", original);
  }
  return { verdict: fails.length === 0 ? "PASS" : "FAIL", iterations: options.iterations, failCount: fails.length, fails };
}
