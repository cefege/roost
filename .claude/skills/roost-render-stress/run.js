// Inject after assigning __stressSid and rendering the session in the active
// tab. The browser implementation is shared with the Playwright terminal
// scenarios in apps/web/src/lib/smokeHarness.ts.
return await window.__smoke.runRenderStress({
  sessionId: window.__stressSid,
  prefix: window.__stressPrefix || "CELLLINE-",
  screen: window.__stressScreen || "alt",
  iterations: window.__stressIter || 80,
});
