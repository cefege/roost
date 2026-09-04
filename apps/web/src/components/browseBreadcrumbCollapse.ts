// This controller owns the DOM measurement required to collapse browse breadcrumbs.
// Keeping width observation beside the collapse calculation prevents the page component
// from mixing layout bookkeeping with directory, history, and terminal behavior.

import { createEffect, createSignal, on, onCleanup, onMount, type Accessor } from "solid-js";

export function createBrowseBreadcrumbCollapse(crumbs: Accessor<unknown>) {
  const [hideMiddle, setHideMiddle] = createSignal(0);
  let strip: HTMLDivElement | undefined;
  let mirror: HTMLDivElement | undefined;

  function availableWidth(): number {
    if (!strip) return Infinity;
    const styles = getComputedStyle(strip);
    const padding = parseFloat(styles.paddingLeft || "0") + parseFloat(styles.paddingRight || "0");
    return strip.clientWidth - padding;
  }

  function measure(): void {
    if (!mirror) return;
    const sampleCrumb = mirror.querySelector<HTMLElement>("[data-mirror-crumb]");
    const separator = mirror.querySelector<HTMLElement>("[data-mirror-sep]");
    const overflow = mirror.querySelector<HTMLElement>("[data-mirror-overflow]");
    if (!sampleCrumb || !separator || !overflow) return;

    const widths = Array.from(
      mirror.querySelectorAll<HTMLElement>("[data-mirror-crumb]"),
      (element) => element.offsetWidth,
    );
    if (widths.length <= 3) {
      setHideMiddle(0);
      return;
    }

    const separatorWidth = separator.offsetWidth;
    const overflowWidth = overflow.offsetWidth;
    const available = availableWidth();
    const middleCount = widths.length - 3;
    const totalWidth = widths.reduce((sum, width) => sum + width, 0) + (widths.length - 1) * separatorWidth;
    if (totalWidth <= available) {
      setHideMiddle(0);
      return;
    }

    let collapsed = middleCount;
    for (let count = 1; count <= middleCount; count++) {
      const keptWidth = widths.slice(1 + count).reduce((sum, width) => sum + width, 0);
      const visibleCount = 2 + widths.length - 1 - count;
      const width = widths[0] + overflowWidth + keptWidth + (visibleCount - 1) * separatorWidth;
      if (width <= available) {
        collapsed = count;
        break;
      }
    }
    setHideMiddle(collapsed);
  }

  const resizeObserver = new ResizeObserver(measure);
  onMount(() => {
    if (strip) resizeObserver.observe(strip);
    measure();
  });
  onCleanup(() => resizeObserver.disconnect());
  createEffect(() => {
    crumbs();
    hideMiddle();
    queueMicrotask(() => {
      if (strip) strip.scrollLeft = strip.scrollWidth;
    });
  });
  createEffect(on(crumbs, () => queueMicrotask(measure)));

  return {
    hideMiddle,
    setStripRef: (element: HTMLDivElement) => { strip = element; },
    setMirrorRef: (element: HTMLDivElement) => { mirror = element; },
  };
}
