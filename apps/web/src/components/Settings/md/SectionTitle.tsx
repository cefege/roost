import { type JSX, type Component } from "solid-js";

// ─── Section title (caps label between cards) ──────────────────────
export const SectionTitle: Component<{ children: JSX.Element }> = (props) => (
  <div class="md-section-title">{props.children}</div>
);
