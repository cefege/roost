// roost-smoke harness — inject with chrome_javascript after enabling
// localStorage.roostSmoke and reloading the target tab.
//
// The browser owns the implementation: window.__smoke.runFlow() dynamically
// loads apps/web/src/lib/smokeHarness.ts. Keeping this launcher thin prevents
// the live canary and isolated Playwright flow from drifting apart.
(async () => {
  const smoke = window.__smoke;
  if (!smoke) {
    return {
      steps: [{ name: "smoke_backdoor", pass: false, detail: "window.__smoke is unavailable; enable roostSmoke and reload" }],
      summary: "0/1 passed",
    };
  }
  return await smoke.runFlow();
})();
