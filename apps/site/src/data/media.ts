// Single import site for the repo screenshots (docs/media lives above the Astro
// project root; keeping the relative paths here means one place to fix them).
// Values are astro:assets ImageMetadata — pass straight to <Image src={…}>.
import type { ImageMetadata } from "astro";

import hero from "../../../../docs/media/hero.png";
import workspaceTabs from "../../../../docs/media/workspace-tabs.png";
import splitPanes from "../../../../docs/media/split-panes.png";
import pairQr from "../../../../docs/media/pair-qr.png";
import tabletDesktop from "../../../../docs/media/tablet-desktop.png";
import mobilePhone from "../../../../docs/media/mobile-phone.png";
import sidebarStatus from "../../../../docs/media/sidebar-status.png";

export type Shot = { src: ImageMetadata; alt: string };

/** Captions reuse the README alt text verbatim where one exists. */
export const SHOTS = {
  hero: {
    src: hero,
    alt: "The Roost control panel: sessions grouped by machine in the sidebar, a live terminal filling the workspace",
  },
  workspaceTabs: {
    src: workspaceTabs,
    alt: "A workspace: a tab bar of live sessions above one real terminal",
  },
  splitPanes: {
    src: splitPanes,
    alt: "Multiple terminals tiled in one workspace, auto-arranged to fill the screen",
  },
  pairQr: {
    src: pairQr,
    alt: "Pair a phone or tablet by scanning a QR; it signs itself in, nothing to type",
  },
  tabletDesktop: {
    src: tabletDesktop,
    alt: "Desktop-grade on a tablet, the same real terminal and layout as a laptop",
  },
  mobilePhone: {
    src: mobilePhone,
    alt: "A real terminal on a phone, with full ANSI, touch selection, and an on-screen key row",
  },
  sidebarStatus: {
    src: sidebarStatus,
    alt: "The sidebar groups live sessions by machine, with per-machine CPU, memory, disk, and network tiles",
  },
} satisfies Record<string, Shot>;
