import { test, expect } from "./fixtures.ts";
import type { RecoverySmokeApi } from "./terminal-smoke-api.ts";
import {
  pressPlatformShortcut,
  spawnSmokeShell,
  navigateToSmokeSession,
  switchToSmokeSession,
  inputSmokeTerminal,
} from "./terminal-helpers.ts";
import { holdNativeTerminalSelection, attemptPaintedMarker } from "./terminal-paint-helpers.ts";
import {
  readTerminalStreamProbe,
  waitForCanonicalAdvance,
  expectCanonicalAdvanceHeld,
} from "./terminal-probe-helpers.ts";

test("desktop passive drafting and dictation preserve a selected reader until pane park", async ({
  smokePage,
  stack,
  browserName,
}) => {
  const interimSpeech = "still speaking\npassive dictated line\nanother dictated line";
  const finalSpeech = "finished speech\nfinal dictated line\nlast finalized line";
  const interimDraft = `typed base ${interimSpeech}`;
  const committedDraft = `typed base ${finalSpeech}`;
  await stack.client.transcriptionSetConfig({ deepgramKey: "", deepgramLanguage: "en" });
  await smokePage.addInitScript(({ interimSpeech, finalSpeech }) => {
    interface FakeResult extends Array<{ transcript: string }> {
      isFinal: boolean;
    }
    interface FakeResultEvent {
      resultIndex: number;
      results: FakeResult[];
    }
    let speechStarts = 0;
    class FakeSpeechRecognition {
      continuous = false;
      interimResults = false;
      lang = "";
      onresult: ((event: FakeResultEvent) => void) | null = null;
      onend: (() => void) | null = null;
      onerror: ((event: { error: string }) => void) | null = null;
      start() {
        speechStarts += 1;
        const result = [{ transcript: interimSpeech }] as FakeResult;
        result.isFinal = false;
        queueMicrotask(() => this.onresult?.({ resultIndex: 0, results: [result] }));
      }
      stop() {
        const result = [{ transcript: finalSpeech }] as FakeResult;
        result.isFinal = true;
        this.onresult?.({ resultIndex: 0, results: [result] });
        queueMicrotask(() => this.onend?.());
      }
      abort() {
        queueMicrotask(() => this.onend?.());
      }
    }
    const speechWindow = window as unknown as Window & {
      SpeechRecognition: typeof FakeSpeechRecognition;
      webkitSpeechRecognition: typeof FakeSpeechRecognition;
      __speechStarts: () => number;
    };
    speechWindow.SpeechRecognition = FakeSpeechRecognition;
    speechWindow.webkitSpeechRecognition = FakeSpeechRecognition;
    speechWindow.__speechStarts = () => speechStarts;
  }, { interimSpeech, finalSpeech });
  await smokePage.reload({ waitUntil: "domcontentloaded" });
  await smokePage.waitForFunction(
    () => typeof (window as unknown as Window & { __smoke?: unknown }).__smoke === "object",
  );

  const firstId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(smokePage, firstId);
  const firstSlot = smokePage.getByTestId(`terminal-slot-${firstId}`);
  const firstDock = firstSlot.getByTestId("mobile-chat-input");
  const firstVoice = firstDock.getByTestId("mobile-voice-input");
  const firstInput = firstDock.getByTestId("chat-input");
  await expect(firstVoice).toHaveAttribute("data-engine", "web-speech");
  await firstDock.getByTestId("chat-box").evaluate(async (element) => {
    await Promise.all(element.getAnimations().map((animation) => animation.finished));
  });

  const readerSuffix = crypto.randomUUID().replaceAll("-", "").slice(0, 10);
  const readerPrefix = `DICTATION-READER-${readerSuffix}-`;
  await inputSmokeTerminal(
    smokePage,
    firstId,
    `for i in $(seq 1 120); do printf '${readerPrefix}%03d\\n' "$i"; done\r`,
  );
  await smokePage.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 20_000);
  }, { id: firstId, marker: `${readerPrefix}120` });
  const readerGridBox = await firstSlot.locator(".cell-grid").boundingBox();
  if (!readerGridBox) throw new Error("dictation reader grid geometry disappeared");
  await smokePage.mouse.move(
    readerGridBox.x + readerGridBox.width / 2,
    readerGridBox.y + readerGridBox.height / 2,
  );
  await smokePage.mouse.wheel(0, -1200);
  await expect.poll(async () =>
    (await readTerminalStreamProbe(smokePage, firstId)).browser.presentation?.at_bottom
  ).toBe(false);
  expect(await holdNativeTerminalSelection(smokePage, firstId)).toBe(true);
  const beforePassiveOutput = await readTerminalStreamProbe(smokePage, firstId);
  const passiveMarker = `DICTATION-PENDING-${readerSuffix}`;
  await inputSmokeTerminal(smokePage, firstId, `printf '%s\\n' ${passiveMarker}\r`);
  const passivePending = await waitForCanonicalAdvance(smokePage, firstId, beforePassiveOutput);
  expectCanonicalAdvanceHeld(beforePassiveOutput, passivePending, {
    readerReason: "selection",
    selectionHold: true,
  });
  expect((await attemptPaintedMarker(smokePage, firstId, passiveMarker)).proof).toBeNull();

  const readReaderGeometry = (rememberRow = false) => smokePage.evaluate(({ id, remember }) => {
    const probeWindow = window as Window & { __roostPassiveReaderRow?: HTMLElement };
    const slot = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const terminal = slot?.querySelector(".wterm");
    const selection = window.getSelection();
    const range = selection?.rangeCount ? selection.getRangeAt(0) : null;
    const endpoint = selection?.anchorNode instanceof Element
      ? selection.anchorNode
      : selection?.anchorNode?.parentElement;
    const row = endpoint?.closest(".cell-row");
    if (
      !(terminal instanceof HTMLElement)
      || !(row instanceof HTMLElement)
      || !range
      || !selection
      || selection.isCollapsed
    ) return null;
    if (remember) probeWindow.__roostPassiveReaderRow = row;
    const rowRect = row.getBoundingClientRect();
    const rangeRect = range.getBoundingClientRect();
    return {
      rangeConnected:
        row.isConnected
        && range.startContainer.isConnected
        && range.endContainer.isConnected,
      rangeOnRow:
        row.contains(range.startContainer)
        && row.contains(range.endContainer),
      sameRow: probeWindow.__roostPassiveReaderRow === row,
      scrollTop: terminal.scrollTop,
      selected: selection.toString(),
      rowText: row.textContent ?? "",
      rowRect: {
        left: rowRect.left, top: rowRect.top, right: rowRect.right, bottom: rowRect.bottom,
      },
      rangeRect: {
        left: rangeRect.left, top: rangeRect.top, right: rangeRect.right, bottom: rangeRect.bottom,
      },
    };
  }, { id: firstId, remember: rememberRow });
  const readerBaseline = await readReaderGeometry(true);
  if (!readerBaseline) throw new Error("selected reader geometry was unavailable");
  expect(readerBaseline.rangeConnected).toBe(true);
  expect(readerBaseline.rangeOnRow).toBe(true);
  expect(readerBaseline.sameRow).toBe(true);
  expect(readerBaseline.rowText).toContain(readerPrefix);
  expect(readerBaseline.rangeRect.bottom).toBeGreaterThan(readerBaseline.rangeRect.top);
  const expectReaderPreserved = async (label: string) => {
    const current = await readReaderGeometry();
    expect(current, `${label}: selected reader geometry`).not.toBeNull();
    expect(current!.rangeConnected, `${label}: connected native range`).toBe(true);
    expect(current!.rangeOnRow, `${label}: native range endpoints`).toBe(true);
    expect(current!.sameRow, `${label}: selected row identity`).toBe(true);
    expect(current!.selected, `${label}: selected text`).toBe(readerBaseline.selected);
    expect(current!.rowText, `${label}: selected history row`).toBe(readerBaseline.rowText);
    expect(Math.abs(current!.scrollTop - readerBaseline.scrollTop), `${label}: scrollTop`).toBeLessThanOrEqual(1);
    for (const surface of ["rowRect", "rangeRect"] as const) {
      for (const edge of ["left", "top", "right", "bottom"] as const) {
        expect(
          Math.abs(current![surface][edge] - readerBaseline[surface][edge]),
          `${label}: ${surface}.${edge}`,
        ).toBeLessThanOrEqual(1);
      }
    }
    const presentation = (await readTerminalStreamProbe(smokePage, firstId)).browser.presentation;
    expect(presentation).toMatchObject({
      reader_intent: "reading",
      reader_reason: "selection",
      hold_mask: { selection: true, link: false },
      reconciled: passivePending.browser.dom_reconciled,
    });
  };

  await firstInput.click();
  await expectReaderPreserved("composer focus");
  await firstInput.press("x");
  await expect(firstInput).toHaveValue("x");
  await expectReaderPreserved("composer trusted key edit");
  await firstInput.press("y");
  await expect(firstInput).toHaveValue("xy");
  await expectReaderPreserved("composer repeated trusted key edit");
  await firstInput.fill("typed base");
  await expectReaderPreserved("composer edit");
  const singleLineComposerHeight = await firstInput.evaluate((element) =>
    element.getBoundingClientRect().height);
  await firstInput.fill("typed base\npassive autogrow line\nanother passive line");
  await expect.poll(() => firstInput.evaluate((element) =>
    element.getBoundingClientRect().height)).toBeGreaterThan(singleLineComposerHeight + 1);
  await expectReaderPreserved("composer autogrow");
  await firstInput.fill("typed base");
  await expectReaderPreserved("composer autogrow shrink");
  await firstInput.press("Space");
  if (browserName === "chromium") {
    const imeSession = await smokePage.context().newCDPSession(smokePage);
    try {
      await imeSession.send("Input.imeSetComposition", {
        text: "中",
        selectionStart: 1,
        selectionEnd: 1,
      });
      await imeSession.send("Input.insertText", { text: "中" });
    } finally {
      await imeSession.detach();
    }
  } else {
    // Non-Chromium engines have no CDP IME endpoint. Keep their branch native
    // and trusted rather than synthesizing DOM events; Chromium above owns the
    // required composition contract regardless of the Playwright project name.
    await smokePage.keyboard.insertText("中");
  }
  await expect(firstInput).toHaveValue("typed base 中");
  await expectReaderPreserved("composer IME");
  await firstInput.fill("typed base");
  await firstDock.getByTestId("voice-mic").click();
  await expect(firstVoice).toHaveAttribute("data-state", "listening");
  await expect(firstInput).toHaveValue(interimDraft);
  await expect.poll(() => firstInput.evaluate((element) =>
    element.getBoundingClientRect().height)).toBeGreaterThan(singleLineComposerHeight + 1);
  await expectReaderPreserved("dictation interim");

  await firstInput.press("Enter");
  await expect(firstVoice).toHaveAttribute("data-state", "listening");
  await expect(firstInput).toHaveValue(interimDraft);
  await expectReaderPreserved("dictation Enter");

  await firstDock.getByTestId("voice-mic").click();
  await expect(firstVoice).toHaveAttribute("data-state", "idle");
  await expect(firstInput).toHaveValue(committedDraft);
  await expectReaderPreserved("dictation final");

  await firstDock.getByTestId("chat-send").click();
  await expect(firstInput).toHaveValue("");
  await expect.poll(readReaderGeometry).toBeNull();
  await firstInput.fill("typed base");

  const secondId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await switchToSmokeSession(smokePage, secondId);
  const secondDock = smokePage
    .getByTestId(`terminal-slot-${secondId}`)
    .getByTestId("mobile-chat-input");
  await expect(secondDock).toBeVisible();
  await expect(secondDock.getByTestId("voice-mic")).toBeVisible();
  await expect(firstVoice).toHaveAttribute("data-state", "idle");

  await switchToSmokeSession(smokePage, firstId);
  await expect(firstDock).toBeVisible();
  await expect(firstInput).toHaveValue("typed base");
  await expect(firstVoice).toHaveAttribute("data-state", "idle");

  // Spotlight keeps covered pane DOM mounted, so it must explicitly deactivate
  // that pane's recorder and expose the spotlit pane's mic.
  await pressPlatformShortcut(smokePage, "splitRight", "D");
  await expect.poll(() => smokePage.locator("[data-pane-slot]").evaluateAll((slots) =>
    slots.filter((slot) => {
      const rect = slot.getBoundingClientRect();
      return rect.right > 0 && rect.left < innerWidth && rect.bottom > 0 && rect.top < innerHeight;
    }).length)).toBe(2);
  const visibleIds = await smokePage.locator("[data-pane-slot]").evaluateAll((slots) =>
    slots.flatMap((slot) => {
      const rect = slot.getBoundingClientRect();
      const testId = slot.getAttribute("data-testid") ?? "";
      return rect.right > 0 && rect.left < innerWidth && testId.startsWith("terminal-slot-")
        ? [testId.slice("terminal-slot-".length)]
        : [];
    }));
  const spotlightId = visibleIds.find((id) => id !== firstId);
  expect(spotlightId).toBeTruthy();
  const spotlightSlot = smokePage.getByTestId(`terminal-slot-${spotlightId!}`);
  const spotlightDock = spotlightSlot.getByTestId("mobile-chat-input");

  // Both controls can already be activation targets when two clicks arrive in
  // one task. The first claim must atomically exclude the retained second target.
  const startsBeforeRace = await smokePage.evaluate(() =>
    (window as unknown as Window & { __speechStarts: () => number }).__speechStarts());
  await smokePage.evaluate(([firstSessionId, secondSessionId]) => {
    const firstMic = document.querySelector(
      `[data-testid="terminal-slot-${firstSessionId}"] [data-testid="voice-mic"]`,
    );
    const secondMic = document.querySelector(
      `[data-testid="terminal-slot-${secondSessionId}"] [data-testid="voice-mic"]`,
    );
    if (!(firstMic instanceof HTMLElement) || !(secondMic instanceof HTMLElement)) {
      throw new Error("both split-pane microphones must exist before the activation race");
    }
    firstMic.click();
    secondMic.click();
  }, [firstId, spotlightId!] as const);
  await expect.poll(() => smokePage.evaluate(() =>
    (window as unknown as Window & { __speechStarts: () => number }).__speechStarts()))
    .toBe(startsBeforeRace + 1);
  await expect(smokePage.locator(
    '[data-testid="mobile-voice-input"][data-state="listening"]',
  )).toHaveCount(1);
  await expect(smokePage.getByTestId("voice-mic")).toHaveCount(1);
  await smokePage.getByTestId("voice-discard").click();
  await expect(firstDock.getByTestId("voice-mic")).toBeVisible();
  await expect(spotlightDock.getByTestId("voice-mic")).toBeVisible();

  await firstInput.click();
  await firstDock.getByTestId("voice-mic").click();
  await expect(firstVoice).toHaveAttribute("data-state", "listening");
  await expect(firstInput).toHaveValue(`typed base ${interimSpeech}`);
  await expect(spotlightDock.getByTestId("voice-mic")).toHaveCount(0);
  await spotlightSlot.getByTestId("terminal-display").click();
  await pressPlatformShortcut(smokePage, "spotlight", "Enter");
  await expect(spotlightSlot).toHaveAttribute("data-spotlit", "true");
  await expect(firstDock).toHaveAttribute("data-active", "false");
  await expect(firstDock).toHaveAttribute("aria-hidden", "true");
  await expect(firstDock).toHaveAttribute("inert", "");
  await expect(spotlightDock).toHaveAttribute("data-active", "true");
  await expect(spotlightDock).not.toHaveAttribute("aria-hidden", "true");
  await expect(firstVoice).toHaveAttribute("data-state", "idle");
  await expect(firstInput).toHaveValue("typed base");
  await expect(spotlightDock.getByTestId("voice-mic")).toBeVisible();
  // Compact suppresses the spotlight surface but retains its stored ID. The
  // visible viewport composer must become active rather than a silent mic shell.
  await smokePage.setViewportSize({ width: 390, height: 844 });
  const compactDock = smokePage.getByTestId("mobile-chat-input");
  await expect(compactDock).toHaveCount(1);
  await expect(compactDock).toHaveAttribute("data-placement", "viewport");
  await compactDock.getByTestId("voice-mic").click();
  await expect(compactDock.getByTestId("mobile-voice-input")).toHaveAttribute(
    "data-state",
    "listening",
  );
  await compactDock.getByTestId("voice-discard").click();
  await smokePage.keyboard.press("Escape");
});
