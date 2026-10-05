// roost-bench in-page probe, installed on every new document before the app's
// own scripts. It reads only DOM both renderers share (`.wterm` root,
// `.cell-row` rows), so one probe measures the Bun and the Rust client alike.
// Timestamps are epoch ms on the page clock (timeOrigin + now). A waiter is
// ARMED before the driver dispatches input, so the hit time is taken inside
// the MutationObserver callback that saw the text, never at the moment the
// driver happened to ask.
(() => {
  if (window.__bench) return;
  const nowEpochMs = () => performance.timeOrigin + performance.now();
  const rowSelector = ".wterm .cell-row";
  const gridText = () =>
    Array.from(document.querySelectorAll(rowSelector), (row) => row.textContent).join("\n");
  const countChar = (ch) => {
    const text = gridText();
    let count = 0;
    for (const symbol of text) if (symbol === ch) count += 1;
    return count;
  };
  const longTasks = { count: 0, ms: 0 };
  let armed = null;

  const armedSatisfied = () => {
    if (armed.kind === "text") return gridText().includes(armed.needle);
    return countChar(armed.needle) === armed.target;
  };
  const checkArmed = () => {
    if (armed === null || armed.hitMs !== null) return;
    if (!armedSatisfied()) return;
    armed.hitMs = nowEpochMs();
    for (const resolve of armed.waiters.splice(0)) resolve(armed.hitMs);
  };

  const bench = {
    firstCellsEpochMs: null,
    longTasks,
    gridText,
    countChar,
    nowEpochMs,
    resetLongTasks() {
      longTasks.count = 0;
      longTasks.ms = 0;
    },
    // kind "text": hit when the grid contains `needle`.
    // kind "count": hit when `needle` (one character) occurs exactly `target` times.
    arm(kind, needle, target) {
      armed = { kind, needle, target, hitMs: null, waiters: [] };
      const armedAtMs = nowEpochMs();
      checkArmed();
      return armedAtMs;
    },
    awaitArmed(timeoutMs) {
      if (armed === null) return Promise.reject(new Error("nothing armed"));
      const current = armed;
      if (current.hitMs !== null) return Promise.resolve(current.hitMs);
      return new Promise((resolve, reject) => {
        current.waiters.push(resolve);
        setTimeout(() => {
          if (current.hitMs === null) {
            const tail = gridText().slice(-400);
            reject(new Error(`timeout: ${current.kind} ${current.needle} ${current.target}; grid tail: ${tail}`));
          }
        }, timeoutMs);
      });
    },
    carrier() {
      const indicator = document.querySelector(".terminal-transport-indicator");
      if (!indicator) return "";
      return JSON.stringify({ text: indicator.textContent.trim(), data: { ...indicator.dataset } });
    },
    phase() {
      return typeof window.__roostPhaseTimeline === "function" ? window.__roostPhaseTimeline() : null;
    },
    rowCount() {
      return document.querySelectorAll(rowSelector).length;
    },
    // Navigation and wasm resource timing, ms since navigation start.
    bootTiming() {
      const nav = performance.getEntriesByType("navigation")[0];
      const wasm = performance
        .getEntriesByType("resource")
        .filter((entry) => entry.name.split("?")[0].endsWith(".wasm"));
      const first = wasm[0];
      return {
        nav: nav ? { ttfb: nav.responseStart, dcl: nav.domContentLoadedEventEnd } : null,
        wasm: {
          start: first ? first.startTime : null,
          end: first ? first.responseEnd : null,
          fetches: wasm.length,
        },
      };
    },
  };
  window.__bench = bench;

  const onMutation = () => {
    if (bench.firstCellsEpochMs === null
      && document.querySelector(rowSelector) !== null
      && gridText().trim().length > 0) {
      bench.firstCellsEpochMs = nowEpochMs();
    }
    checkArmed();
  };
  new MutationObserver(onMutation).observe(document, {
    subtree: true,
    childList: true,
    characterData: true,
  });
  try {
    new PerformanceObserver((list) => {
      for (const entry of list.getEntries()) {
        longTasks.count += 1;
        longTasks.ms += entry.duration;
      }
    }).observe({ entryTypes: ["longtask"] });
  } catch (_) {
    // A browser without the longtask entry type reports zero long tasks.
  }
})();
