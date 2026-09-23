// Client-Solid test runtime for DOM suites that must observe a local-state
// transition (a disclosure toggle). Bun compiles JSX to eager props, so a
// <Show when={open()}> branch is fixed when the component body runs. mount()
// re-runs the render under a memo when a signal it read changes; createSignal
// returns each call site's previous signal by call order so state survives.
// The suite's JSX shim must call nested components plainly (runWithOwner untracks).

import type * as SolidApi from "solid-js";

export interface RerenderingMount<T> {
  current: () => T;
  dispose: () => void;
}

export function createRerenderingSolid(Solid: typeof SolidApi) {
  let activeSignals: unknown[] | null = null;
  let signalCursor = 0;

  const createSignal = ((...args: Parameters<typeof Solid.createSignal>) => {
    if (activeSignals === null) return Solid.createSignal(...args);
    const slot = signalCursor++;
    activeSignals[slot] ??= Solid.createSignal(...args);
    return activeSignals[slot];
  }) as typeof Solid.createSignal;

  function mount<T>(render: () => T): RerenderingMount<T> {
    const renderSignals: unknown[] = [];
    let current: (() => T) | undefined;
    let dispose: (() => void) | undefined;
    Solid.createRoot((disposeRoot) => {
      dispose = disposeRoot;
      current = Solid.createMemo(() => {
        const outerSignals = activeSignals;
        const outerCursor = signalCursor;
        activeSignals = renderSignals;
        signalCursor = 0;
        try {
          return render();
        } finally {
          activeSignals = outerSignals;
          signalCursor = outerCursor;
        }
      });
    });
    return { current: current!, dispose: dispose! };
  }

  return { runtime: { ...Solid, createSignal }, mount };
}
