import { Show, createMemo, type Component } from "solid-js";
import type { AgentImageEntry } from "@roost/shared/wire/agent-entry";

const RASTER_MEDIA_TYPES: Record<string, true> = {
  "image/png": true,
  "image/jpeg": true,
  "image/gif": true,
  "image/webp": true,
  "image/avif": true,
};

export const EntryImage: Component<{ entry: AgentImageEntry }> = (props) => {
  const mediaType = createMemo(() => {
    const candidate = props.entry.media_type.trim().toLowerCase();
    if (!candidate) return "image/png";
    return RASTER_MEDIA_TYPES[candidate] === true ? candidate : null;
  });

  return (
    <Show when={mediaType()}>
      {(type) => (
        <img
          data-testid="agent-entry-image"
          data-seq={props.entry.seq}
          src={`data:${type()};base64,${props.entry.data_b64}`}
          alt={props.entry.alt}
          loading="lazy"
          decoding="async"
          style={{
            display: "block",
            "max-width": "min(46rem, 88%)",
            height: "auto",
            "border-radius": "var(--md-shape-md)",
          }}
        />
      )}
    </Show>
  );
};
