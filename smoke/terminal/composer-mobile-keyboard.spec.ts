// This spec keeps the mobile terminal keyboard flow in one Playwright test lifecycle.
// Control and mouse phases share live locators so focus, paint, and geometry state persists.
// The registered case preserves mobile project discovery while phase modules stay cohesive.

import { test } from "./fixtures.ts";
import { prepareMobileKeyboardControls } from "./composer-mobile-keyboard-controls.ts";
import { exerciseMobileMouseAndGeometry } from "./composer-mobile-mouse-controls.ts";

test("mobile terminal keyboard toggles and dispatches special keys", async ({
  mobileSmokePage,
  stack,
}, testInfo) => {
  const context = await prepareMobileKeyboardControls(mobileSmokePage, stack);
  await exerciseMobileMouseAndGeometry(context, testInfo);
});
