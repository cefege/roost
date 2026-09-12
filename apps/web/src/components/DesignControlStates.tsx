// DesignControlStates renders the interactive control reference for /design.
// It composes the shipped settings primitives without reimplementing their semantics.
// DesignGallery supplies the surrounding catalog surface and theme tokens.

import { type Component, createSignal } from "solid-js";
import {
  Button,
  Checkbox,
  Chip,
  IconButton,
  List,
  ListRow,
  SectionTitle,
  Select,
  Switch,
  SwitchRow,
  TextField,
} from "./Settings/md/primitives.tsx";

const SURFACE_OPTIONS = [
  { value: "surface", label: "Surface" },
  { value: "accent", label: "Accent" },
  { value: "status", label: "Status" },
];

export const DesignControlStates: Component = () => {
  const [switchOn, setSwitchOn] = createSignal(true);
  const [checked, setChecked] = createSignal(false);
  const [selectValue, setSelectValue] = createSignal("surface");
  const [textValue, setTextValue] = createSignal("");
  const [textareaValue, setTextareaValue] = createSignal("");
  const [switchRowOn, setSwitchRowOn] = createSignal(true);
  const [selectedChip, setSelectedChip] = createSignal(false);

  return (
    <div style={{ display: "grid", gap: "var(--md-space-6)" }}>
      <section>
        <SectionTitle>Buttons</SectionTitle>
        <div style={{ display: "flex", "flex-wrap": "wrap", gap: "var(--md-space-3)", "align-items": "center" }}>
          <Button variant="default">Default</Button>
          <Button variant="secondary">Secondary</Button>
          <Button variant="outline">Outline</Button>
          <Button variant="ghost">Ghost</Button>
          <Button variant="destructive">Destructive</Button>
          <Button variant="link">Link</Button>
        </div>
        <div style={{ display: "flex", "flex-wrap": "wrap", gap: "var(--md-space-3)", "align-items": "center", "margin-top": "var(--md-space-3)" }}>
          <Button size="xs">Extra small</Button>
          <Button size="sm">Small</Button>
          <Button size="default">Default</Button>
          <Button size="lg">Large</Button>
          <Button size="icon" variant="secondary" icon="close" aria-label="Icon-sized secondary button" />
          <IconButton size="icon-xs" icon="close" label="Extra small icon button" />
          <IconButton size="icon-sm" icon="close" label="Small icon button" />
          <IconButton size="icon" icon="close" label="Default icon button" />
          <IconButton size="icon-lg" icon="close" label="Large icon button" />
        </div>
      </section>

      <section>
        <SectionTitle>Fields</SectionTitle>
        <div style={{ display: "grid", "grid-template-columns": "repeat(auto-fit, minmax(calc(var(--md-space-9) * 4), 1fr))", gap: "var(--md-space-4)" }}>
          <TextField
            value={textValue()}
            onInput={setTextValue}
            label="Workspace name"
            placeholder="roost"
            description="A visible field description stays associated with the input."
          />
          <TextField
            value={textareaValue()}
            onInput={setTextareaValue}
            label="Prompt"
            type="textarea"
            rows={4}
            placeholder="Describe the task…"
            description="Textarea fields preserve native multiline editing."
          />
          <TextField
            value="Unavailable"
            onInput={() => {}}
            label="Disabled field"
            description="Disabled controls retain their context."
            disabled
          />
          <TextField
            value="invalid name"
            onInput={() => {}}
            label="Invalid field"
            error="Use lowercase letters, numbers, and hyphens."
          />
          <Select
            value={selectValue()}
            onChange={setSelectValue}
            label="Surface role"
            description="The native select opens its shared option surface."
            options={SURFACE_OPTIONS}
          />
          <Select
            value="surface"
            onChange={() => {}}
            label="Disabled select"
            description="Unavailable controls retain their native disabled semantics."
            options={SURFACE_OPTIONS}
            disabled
          />
          <Select
            value="accent"
            onChange={() => {}}
            label="Invalid select"
            options={SURFACE_OPTIONS}
            error="Choose a supported surface role."
          />
        </div>
      </section>

      <section>
        <SectionTitle>Chips</SectionTitle>
        <div style={{ display: "flex", "flex-wrap": "wrap", gap: "var(--md-space-3)" }}>
          <Chip label="Passive label" icon="folder" />
          <Chip
            label={selectedChip() ? "Interactive selected" : "Interactive action"}
            icon="bolt"
            selected={selectedChip()}
            onClick={() => setSelectedChip((selected) => !selected)}
          />
        </div>
      </section>

      <section>
        <SectionTitle>Boolean controls</SectionTitle>
        <List contained>
          <ListRow
            headline="Switch"
            support="Interactive and controlled by the gallery."
            trailing={<Switch checked={switchOn()} onChange={setSwitchOn} label="Enable switch specimen" />}
          />
          <ListRow
            headline="Checkbox"
            support="Interactive and controlled by the gallery."
            trailing={<Checkbox checked={checked()} onChange={setChecked} label="Enable checkbox specimen" />}
          />
          <ListRow
            headline="Disabled switch"
            support="Unavailable controls preserve their native disabled semantics."
            trailing={<Switch checked={false} onChange={() => {}} label="Disabled switch specimen" disabled />}
          />
          <ListRow
            headline="Disabled checkbox"
            support="Unavailable controls preserve their native disabled semantics."
            trailing={<Checkbox checked={true} onChange={() => {}} label="Disabled checkbox specimen" disabled />}
          />
        </List>
        <div style={{ "margin-top": "var(--md-space-4)" }}>
          <SectionTitle>Switch rows</SectionTitle>
          <div style={{ display: "grid", gap: "var(--md-space-4)" }}>
            <SwitchRow
              headline="Switch row"
              support="A labelled setting keeps its support text associated with the switch."
              checked={switchRowOn()}
              onChange={setSwitchRowOn}
            />
            <SwitchRow
              headline="Disabled switch row"
              support="Unavailable settings retain their native disabled semantics."
              checked={false}
              onChange={() => {}}
              disabled
            />
          </div>
        </div>
      </section>
    </div>
  );
};
