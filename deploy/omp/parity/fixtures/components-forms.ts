import type * as FormModule from "@omp-source/packages/tui/src/components/form";
import type * as FormTheme from "@omp-source/packages/tui/src/chrome/form-theme";
import type * as InputModule from "@omp-source/packages/tui/src/components/input";
import type * as TextModule from "@omp-source/packages/tui/src/components/text";
import type * as WizardModule from "@omp-source/packages/tui/src/components/wizard-step";
import type * as OverlayBox from "@omp-source/packages/tui/src/chrome/overlay-box";
import type * as SettingsListModule from "@omp-source/packages/tui/src/components/settings-list";
import type * as TuiAdapters from "@omp-source/packages/tui/src/theme/tui-adapters";
import type { FixtureCase } from "../scenario";
import { callbackValue } from "../scenario";
import { componentId, shown } from "./components-support";

const form = (name: string) => componentId("components/form", name);
const settings = (name: string) => componentId("components/settings-list", name);
const wizardInput = 'input[aria-label="> "]';
const fieldInput = '[data-omp-role="omp.form.field"] input';

function settingItems() {
  return [
    { id: "fixture-alpha", label: "Fixture toggle alpha", description: "Fixture alpha description", currentValue: "fixture-off", values: ["fixture-off", "fixture-on"] },
    { id: "fixture-beta", label: "Fixture toggle beta", description: "Fixture beta description", currentValue: "fixture-off", values: ["fixture-off", "fixture-on"] },
  ];
}

export const fixtureCases: FixtureCase[] = [
  {
    id: "comp-form-text-field-reject",
    covers: [form("Form"), form("FormField"), form("OptionalText"), form("StyledText"), form("TextFormField")],
    async create(context) {
      const { Form, TextFormField } = await context.load<typeof FormModule>("packages/tui/src/components/form.ts");
      const { formTheme } = await context.load<typeof FormTheme>("packages/tui/src/chrome/form-theme.ts");
      const field = new TextFormField({
        theme: formTheme,
        label: "Fixture form label",
        description: "Fixture form description",
        hint: "Fixture form hint",
        empty: "reject",
        emptyError: "Fixture value is required",
        onSubmit: value => { context.record("value", value); context.done(); },
        onCancel: () => { context.record("cancel", true); context.done(); },
      });
      return new Form({ fields: [field], onCancel: () => { context.record("cancel", true); context.done(); } });
    },
    checkpoints: [
      { name: "form field chrome", comparisons: [shown("label", "Fixture form label"), shown("description", "Fixture form description"), shown("hint", "Fixture form hint")] },
      { name: "empty submit is rejected", terminalInput: "\r", browserActions: [{ type: "press", selector: fieldInput, key: "Enter" }], comparisons: [shown("rejection", "Fixture value is required")] },
      {
        name: "typed value submits",
        terminalInput: "fixture-typed-value\r",
        browserActions: [{ type: "fill", selector: fieldInput, value: "fixture-typed-value" }, { type: "press", selector: fieldInput, key: "Enter" }],
        comparisons: [callbackValue("submitted value", "value", "fixture-typed-value")],
      },
    ],
  },
  {
    id: "comp-form-select-field",
    covers: [form("Form"), form("FormField"), form("SelectFormField")],
    async create(context) {
      const { Form, SelectFormField } = await context.load<typeof FormModule>("packages/tui/src/components/form.ts");
      const { formTheme } = await context.load<typeof FormTheme>("packages/tui/src/chrome/form-theme.ts");
      const { getSelectListTheme } = await context.load<typeof TuiAdapters>("packages/tui/src/theme/tui-adapters.ts");
      const items = Array.from({ length: 4 }, (_, index) => ({ value: `fixture-choice-${index + 1}`, label: `Fixture choice ${index + 1}` }));
      const field = new SelectFormField({
        theme: formTheme,
        label: "Fixture select label",
        items,
        selectTheme: getSelectListTheme(),
        onSubmit: value => { context.record("value", value); context.done(); },
        onCancel: () => { context.record("cancel", true); context.done(); },
      });
      return new Form({ fields: [field], onCancel: () => { context.record("cancel", true); context.done(); } });
    },
    checkpoints: [
      { name: "select field choices", comparisons: [shown("label", "Fixture select label"), shown("first choice", "Fixture choice 1"), shown("last choice", "Fixture choice 4")] },
      {
        name: "second choice submits",
        terminalInput: "\x1b[B\r",
        browserActions: [{ type: "click", selector: 'text="Fixture choice 2"' }],
        comparisons: [callbackValue("submitted choice", "value", "fixture-choice-2")],
      },
    ],
  },
  {
    id: "comp-form-select-picker",
    covers: [form("Form"), form("FormField"), form("SelectFormField")],
    async create(context) {
      const { Form, SelectFormField } = await context.load<typeof FormModule>("packages/tui/src/components/form.ts");
      const { formTheme } = await context.load<typeof FormTheme>("packages/tui/src/chrome/form-theme.ts");
      const { getSelectListTheme } = await context.load<typeof TuiAdapters>("packages/tui/src/theme/tui-adapters.ts");
      const items = Array.from({ length: 14 }, (_, index) => ({ value: `fixture-choice-${index + 1}`, label: `Fixture choice ${index + 1}` }));
      const field = new SelectFormField({
        theme: formTheme,
        label: "Fixture select label",
        items,
        selectTheme: getSelectListTheme(),
        onSubmit: value => { context.record("value", value); context.done(); },
        onCancel: () => { context.record("cancel", true); context.done(); },
      });
      return new Form({ fields: [field], onCancel: () => { context.record("cancel", true); context.done(); } });
    },
    checkpoints: [
      { name: "select field choices", comparisons: [shown("label", "Fixture select label")] },
      {
        name: "second choice submits",
        terminalInput: "\x1b[B\r",
        browserActions: [{ type: "click", selector: '[data-omp-picker-item] :text-is("Fixture choice 2")' }, { type: "click", selector: 'button:has-text("Select")' }],
        comparisons: [callbackValue("submitted choice", "value", "fixture-choice-2")],
      },
    ],
  },
  {
    id: "comp-settings-list",
    covers: [settings("SettingsList")],
    async create(context) {
      const { SettingsList } = await context.load<typeof SettingsListModule>("packages/tui/src/components/settings-list.ts");
      const { getSettingsListTheme } = await context.load<typeof TuiAdapters>("packages/tui/src/theme/tui-adapters.ts");
      return new SettingsList(settingItems(), 6, getSettingsListTheme(), (id, value) => { context.record("change", `${id}=${value}`); }, () => { context.record("cancel", true); context.done(); });
    },
    checkpoints: [
      { name: "settings rows", comparisons: [shown("alpha row", "Fixture toggle alpha"), shown("beta row", "Fixture toggle beta"), shown("alpha description", "Fixture alpha description")] },
      {
        name: "beta cycles",
        terminalInput: "\x1b[B\r",
        browserActions: [{ type: "click", selector: 'text="Fixture toggle beta"' }],
        comparisons: [callbackValue("changed value", "change", "fixture-beta=fixture-on")],
      },
    ],
  },
  {
    id: "comp-settings-form-field",
    covers: [form("Form"), settings("SettingsFormField")],
    async create(context) {
      const { Form } = await context.load<typeof FormModule>("packages/tui/src/components/form.ts");
      const { SettingsFormField } = await context.load<typeof SettingsListModule>("packages/tui/src/components/settings-list.ts");
      const { formTheme } = await context.load<typeof FormTheme>("packages/tui/src/chrome/form-theme.ts");
      const { getSettingsListTheme } = await context.load<typeof TuiAdapters>("packages/tui/src/theme/tui-adapters.ts");
      const field = new SettingsFormField({
        label: "Fixture settings field",
        items: settingItems(),
        maxVisible: 6,
        settingsTheme: getSettingsListTheme(),
        fieldTheme: formTheme,
        onChange: (id, value) => { context.record("change", `${id}=${value}`); },
        onCancel: () => { context.record("cancel", true); context.done(); },
      });
      return new Form({ fields: [field], onCancel: () => { context.record("cancel", true); context.done(); } });
    },
    checkpoints: [
      { name: "settings field rows", comparisons: [shown("label", "Fixture settings field"), shown("alpha row", "Fixture toggle alpha")] },
      {
        name: "beta cycles",
        terminalInput: "\x1b[B\r",
        browserActions: [{ type: "click", selector: 'text="Fixture toggle beta"' }],
        comparisons: [callbackValue("changed value", "change", "fixture-beta=fixture-on")],
      },
    ],
  },
  {
    id: "comp-wizard-step",
    covers: [componentId("components/wizard-step", "WizardStep")],
    async create(context) {
      const { WizardStep } = await context.load<typeof WizardModule>("packages/tui/src/components/wizard-step.ts");
      const { Input } = await context.load<typeof InputModule>("packages/tui/src/components/input.ts");
      const { Text } = await context.load<typeof TextModule>("packages/tui/src/components/text.ts");
      const input = new Input();
      input.onSubmit = value => { context.record("value", value); context.done(); };
      input.onEscape = () => { context.record("cancel", true); context.done(); };
      return new WizardStep({
        kind: "input",
        heading: new Text("Fixture wizard heading", 0, 0),
        intro: new Text("Fixture wizard intro", 0, 0),
        content: input,
        status: new Text("Fixture wizard status", 0, 0),
        footer: new Text("Fixture wizard footer", 0, 0),
      });
    },
    checkpoints: [
      { name: "wizard slots", comparisons: [shown("heading", "Fixture wizard heading"), shown("intro", "Fixture wizard intro"), shown("status", "Fixture wizard status"), shown("footer", "Fixture wizard footer")] },
      {
        name: "typed step value",
        terminalInput: "fixture-step\r",
        browserActions: [{ type: "fill", selector: wizardInput, value: "fixture-step" }, { type: "press", selector: wizardInput, key: "Enter" }],
        comparisons: [callbackValue("step value", "value", "fixture-step")],
      },
    ],
  },
  {
    id: "comp-chrome-overlay-panel",
    covers: [componentId("chrome/overlay-box", "OverlayPanel")],
    async create(context) {
      const { OverlayPanel, PanelDivider } = await context.load<typeof OverlayBox>("packages/tui/src/chrome/overlay-box.ts");
      const { Text } = await context.load<typeof TextModule>("packages/tui/src/components/text.ts");
      const panel = new OverlayPanel("Fixture overlay panel", "omp.overlay.fixture");
      panel.children.push(new Text("Fixture panel first row", 0, 0), new PanelDivider(), new Text("Fixture panel second row", 0, 0));
      context.record("mounted", true);
      return panel;
    },
    checkpoints: [
      { name: "panel rows", comparisons: [shown("title", "Fixture overlay panel"), shown("first row", "Fixture panel first row"), shown("second row", "Fixture panel second row")] },
      { name: "panel narrower than its borders", terminalResize: { columns: 3, rows: 12 }, browserActions: [{ type: "resize", width: 120, height: 420 }], comparisons: [callbackValue("panel mounted before narrowing", "mounted", true)] },
      { name: "panel restored", terminalResize: { columns: 120, rows: 48 }, browserActions: [{ type: "resize", width: 1200, height: 960 }], comparisons: [shown("title after restore", "Fixture overlay panel")] },
    ],
  },
  {
    id: "comp-form-field-direct",
    covers: [form("FormField")],
    async create(context) {
      const { FormField } = await context.load<typeof FormModule>("packages/tui/src/components/form.ts");
      const { formTheme } = await context.load<typeof FormTheme>("packages/tui/src/chrome/form-theme.ts");
      const { Input } = await context.load<typeof InputModule>("packages/tui/src/components/input.ts");
      const input = new Input();
      input.onSubmit = value => { context.record("value", value); context.done(); };
      input.onEscape = () => { context.record("cancel", true); context.done(); };
      return new FormField(input, { theme: formTheme, label: "Fixture direct label", hint: "Fixture direct hint" });
    },
    checkpoints: [
      { name: "direct field chrome", comparisons: [shown("label", "Fixture direct label"), shown("hint", "Fixture direct hint")] },
      {
        name: "typed direct value",
        terminalInput: "fixture-direct\r",
        browserActions: [{ type: "fill", selector: fieldInput, value: "fixture-direct" }, { type: "press", selector: fieldInput, key: "Enter" }],
        comparisons: [callbackValue("direct value", "value", "fixture-direct")],
      },
    ],
  },
];
