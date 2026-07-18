// Ambient JSX typing for @material/web custom elements used as raw <md-*> tags.
// Consolidates the per-file `declare module "solid-js"` augmentations that were
// drifting (md-ripple alone was declared in 3 files). Primitives that route
// through <Dynamic component="md-..."> don't need this (string tag); raw-tag
// callsites (md-ripple, md-dialog, md-circular-progress, …) do.
export {};

declare module "solid-js" {
  // eslint-disable-next-line @typescript-eslint/no-namespace
  namespace JSX {
    interface IntrinsicElements {
      "md-ripple": Record<string, unknown>;
      "md-circular-progress": Record<string, unknown>;
      "md-dialog": Record<string, unknown>;
      "md-filled-button": Record<string, unknown>;
      "md-filled-tonal-button": Record<string, unknown>;
      "md-text-button": Record<string, unknown>;
      "md-icon-button": Record<string, unknown>;
      "md-switch": Record<string, unknown>;
      "md-checkbox": Record<string, unknown>;
      "md-outlined-text-field": Record<string, unknown>;
      "md-outlined-select": Record<string, unknown>;
      "md-select-option": Record<string, unknown>;
      "md-assist-chip": Record<string, unknown>;
      "md-input-chip": Record<string, unknown>;
      "md-linear-progress": Record<string, unknown>;
    }
  }
}
