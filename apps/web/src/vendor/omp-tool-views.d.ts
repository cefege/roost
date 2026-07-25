// Types the custom element the vendored omp bundle registers. The bundle is a
// side-effect import (main.tsx); this only teaches Solid's JSX that the tag
// exists, so ToolCard.tsx can write <omp-tool-view>.

declare module "solid-js" {
  namespace JSX {
    interface IntrinsicElements {
      "omp-tool-view": JSX.HTMLAttributes<HTMLElement>;
    }
  }
}

export {};
