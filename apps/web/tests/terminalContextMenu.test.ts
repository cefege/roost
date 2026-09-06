// Terminal context-menu mobile sheet regression coverage. The extracted
// sibling launch sequence is exercised through sessionSiblingAction.test.ts;
// this suite keeps the TSX module's viewport contract isolated.

import { expect, mock, test } from "bun:test";

const jsxFragment = Symbol("terminal-context-test-fragment");
mock.module("react/jsx-dev-runtime", () => ({
  Fragment: jsxFragment,
  jsxDEV: (tag: unknown, props: Record<string, unknown> | null) => ({ tag, props }),
}));
mock.module("@solidjs/router", () => ({
  useNavigate: () => () => {},
  useLocation: () => ({ pathname: "/" }),
}));

// The TSX runtime and router mocks must install before this known module binds
// them, so a static import cannot isolate the style test.
const { _terminalActionSheetStyle } = await import(
  "../src/components/TerminalContextMenu.tsx"
);

test("mobile action sheet stays inside the keyboard-safe viewport", () => {
  const style = _terminalActionSheetStyle();

  expect(style["box-sizing"]).toBe("border-box");
  expect(style.bottom).toBe("max(var(--kb-offset), 0px)");
  expect(style["max-height"]).toBe(
    "calc(100dvh - max(var(--kb-offset), 0px) - var(--md-space-4))",
  );
  expect(style["overflow-y"]).toBe("auto");
  expect(String(style.padding)).toContain("env(safe-area-inset-bottom, 0px)");
});
