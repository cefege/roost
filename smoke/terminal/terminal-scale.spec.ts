// Serial real-stack terminal scale gate for the ordinary Playwright terminal tier.
// The browser fixture supplies the first enrolled document; the harness adds the
// remaining 15 documents and releases every page, PTY, and session in finally.
// This test deliberately has no 500-session path; that remains opt-in only.

import { test, expect } from "./fixtures.ts";
import {
  PR_SCALE_DOCUMENTS,
  PR_SCALE_SESSIONS,
  PR_SCALE_TIMEOUT_MS,
  runPrScaleQualification,
} from "./terminal-scale-harness.ts";

test("32 fixture PTYs remain bounded across 16 active browser documents @serial", async ({
  browser,
  smokePage,
  stack,
}, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop multi-document scale qualification");
  test.setTimeout(PR_SCALE_TIMEOUT_MS);

  const report = await runPrScaleQualification({ browser, stack, initialPage: smokePage });
  await testInfo.attach("terminal-scale.json", {
    body: JSON.stringify(report, null, 2),
    contentType: "application/json",
  });
  expect(report.sessionCount).toBe(PR_SCALE_SESSIONS);
  expect(report.documentCount).toBe(PR_SCALE_DOCUMENTS);
  expect(report.dropRepairFullFrames).toBe(1);
  expect(report.mountedRendererCount).toBeLessThanOrEqual(report.mountedRendererCeiling);
});
