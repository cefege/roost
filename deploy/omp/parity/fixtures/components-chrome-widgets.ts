import type * as CancellableLoaderModule from "@omp-source/packages/tui/src/components/cancellable-loader";
import type * as CollabQrModule from "@omp-source/packages/tui/src/chrome/collab-qrcode";
import type * as HotkeysModule from "@omp-source/packages/tui/src/hotkeys-markdown";
import type * as ShimmerModule from "@omp-source/packages/tui/src/theme/shimmer";
import { callbackValue, type FixtureCase, type FixtureContext } from "../scenario";
import { componentId, shown, shownAs } from "./components-support";

const cancellableLoaderId = componentId("components/cancellable-loader", "CancellableLoader");
const workingStop = '[data-omp-role="omp.working.stop"]';

async function cancellableLoader(context: FixtureContext, message: string, animatedLabel: boolean) {
  const { CancellableLoader } = await context.load<typeof CancellableLoaderModule>("packages/tui/src/components/cancellable-loader.ts");
  const { shimmerText } = await context.load<typeof ShimmerModule>("packages/tui/src/theme/shimmer.ts");
  const messageColor = animatedLabel
    ? Object.assign((text: string) => shimmerText(text, context.theme), { animated: true as const })
    : (text: string) => context.theme.fg("muted", text);
  const loader = new CancellableLoader(context.tui, text => context.theme.fg("accent", text), messageColor, message);
  loader.onAbort = () => context.record("aborted", true);
  return loader;
}

export const fixtureCases: FixtureCase[] = [
  {
    id: "comp-chrome-cancellable-loader-plain",
    covers: [cancellableLoaderId],
    create: context => cancellableLoader(context, "Plain cancellable fixture", false),
    checkpoints: [
      { name: "plain loader message", comparisons: [shown("loader message", "Plain cancellable fixture", "body")] },
      { name: "cancel with escape", terminalInput: "\x1b", comparisons: [shown("loader message after cancel", "Plain cancellable fixture", "body")] },
    ],
  },
  {
    id: "comp-chrome-cancellable-loader-shimmer",
    covers: [cancellableLoaderId],
    create: context => cancellableLoader(context, "Shimmer cancellable fixture", true),
    checkpoints: [{ name: "shimmering loader message", comparisons: [shown("shimmer message", "Shimmer cancellable fixture", "body")] }],
  },
  {
    id: "comp-chrome-cancellable-loader-working-row",
    covers: [cancellableLoaderId],
    async create(context) {
      const loader = await cancellableLoader(context, "Working cancellable fixture", false);
      const startedAt = Date.now();
      loader.setWorkingRow(() => ({ label: "Working cancellable fixture", startedAt, interruptKey: "escape" }), () => context.record("interrupted", true));
      return loader;
    },
    checkpoints: [
      { name: "working row label", comparisons: [shown("working label", "Working cancellable fixture", "body")] },
      { name: "click stop control", browserActions: [{ type: "click", selector: workingStop }], comparisons: [shown("working label after stop", "Working cancellable fixture", "body")] },
    ],
  },
  {
    id: "comp-chrome-cancellable-loader-trailer-static",
    covers: [cancellableLoaderId],
    async create(context) {
      const { CancellableLoader } = await context.load<typeof CancellableLoaderModule>("packages/tui/src/components/cancellable-loader.ts");
      const loader = new CancellableLoader(context.tui, text => context.theme.fg("accent", text), text => context.theme.fg("muted", text), "Trailer cancellable fixture", ["•"]);
      loader.setTrailer(() => "elapsed 0:12");
      return loader;
    },
    checkpoints: [{ name: "static loader with trailer", comparisons: [shown("trailer message", "Trailer cancellable fixture", "body"), shown("trailer text", "elapsed 0:12", "body")] }],
  },
  {
    id: "comp-chrome-hotkeys-sheet",
    covers: [componentId("hotkeys-markdown", "HotkeysSheetComponent")],
    async create(context) {
      const { HotkeysSheetComponent } = await context.load<typeof HotkeysModule>("packages/tui/src/hotkeys-markdown.ts");
      return new HotkeysSheetComponent({ keybindings: context.keybindings }, () => { context.record("closed", true); context.done(); });
    },
    checkpoints: [
      { name: "hotkeys reference", comparisons: [shown("slash commands action", "Slash commands", "body")] },
      { name: "escape and Close dismiss the sheet", terminalInput: "\x1b", browserActions: [{ type: "click", selector: 'button:has-text("Close") >> nth=0' }], comparisons: [callbackValue("closed", "closed", true)] },
    ],
  },
  {
    id: "comp-chrome-collab-qrcode",
    covers: [componentId("chrome/collab-qrcode", "CollabQrCodeComponent")],
    async create(context) {
      const { CollabQrCodeComponent } = await context.load<typeof CollabQrModule>("packages/tui/src/chrome/collab-qrcode.ts");
      return new CollabQrCodeComponent("https://Join/collab/room/abcdef0123456789abcdef0123456789");
    },
    checkpoints: [{
      name: "narrow terminal shows the join link",
      terminalResize: { columns: 40, rows: 24 },
      comparisons: [shownAs("join link", "(Join)", "(Join)", "Join", "body")],
    }],
  },
];
