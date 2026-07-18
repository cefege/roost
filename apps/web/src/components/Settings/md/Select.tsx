import { type Component, For } from "solid-js";
import { Dynamic } from "solid-js/web";
import "@material/web/select/outlined-select.js";
import "@material/web/select/select-option.js";

// ─── Select → real md-outlined-select (themed menu + keyboard nav) ───────────
export const Select: Component<{
  value: string;
  onChange: (v: string) => void;
  label?: string;
  options: { value: string; label: string }[];
  class?: string;
  testId?: string;
}> = (props) => (
  <Dynamic
    component="md-outlined-select"
    prop:value={props.value}
    label={props.label}
    class={props.class}
    attr:data-testid={props.testId}
    on:change={(e: Event) =>
      props.onChange((e.currentTarget as HTMLElement & { value: string }).value)
    }
  >
    <For each={props.options}>
      {(o) => (
        <Dynamic component="md-select-option" prop:value={o.value}>
          <div slot="headline">{o.label}</div>
        </Dynamic>
      )}
    </For>
  </Dynamic>
);
