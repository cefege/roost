import { writeFileSync } from "node:fs";
import { PNG_BASE64 } from "./native-contract-hosts";
import type * as Ui from "@omp-source/packages/tui/src/index";
import type * as Describe from "@omp-source/packages/tui/src/native/describe";
import type { NativeAssertion, NativeContract } from "../coverage-native";
import type { FixtureCase, FixtureContext } from "../scenario";

export const galleryProps: ReadonlyArray<readonly [string, Record<string, unknown>]> = [
  ["col", { gap: "sm" }], ["row", { gap: "sm" }], ["card", { head: "Card", inset: true }], ["section", { head: "Section" }], ["rule", { label: "Rule" }],
  ["spacer", { size: "sm" }], ["text", { text: "Gallery text", wrap: "word" }], ["md", { text: "**bold** markdown" }], ["code", { text: "const x = 1;", lang: "ts" }],
  ["diff", { text: "@@ -1 +1 @@\n-a\n+b", path: "a.txt" }], ["ansi", { text: "red" }], ["image", { builtin: "omp", alt: "Gallery image" }],
  ["media", { path: "/tmp/gallery.png" }], ["kv", { items: [{ k: "Key", v: "Value" }] }], ["table", { cols: [{ id: "a", head: "A" }], rows: [{ id: "r", cells: { a: "cell" } }] }],
  ["tree", { nodes: [{ id: "t", label: "Tree" }] }], ["json", { value: { a: 1 } }], ["badge", { text: "Badge" }], ["kbd", { keys: ["Ctrl", "K"] }], ["icon", { name: "check" }],
  ["spinner", { style: "dots", label: "Spin" }], ["shimmer", { text: "Shimmer" }], ["elapsed", { age: 5000 }], ["progress", { value: 0.5, label: "Half" }],
  ["rate", { value: 12, unit: "tok/s" }], ["list", { empty: "Empty list" }], ["item", { label: "Item" }], ["tabs", { items: [{ id: "a", label: "A" }], active: "a" }],
  ["editor", { text: "gallery editor" }], ["input", { text: "gallery input" }], ["button", { text: "Gallery" }], ["status", { separator: " | " }], ["seg", { spans: [{ t: "seg" }] }],
  ["overlay", { anchor: "center", size: "sm", modal: false }], ["toast", { text: "Toast" }], ["rows", { cols: 10, lines: ["row line"] }],
  ["prefs", { title: "Prefs", pages: [{ id: "a", label: "A" }], page: "a", sections: [] }], ["tool", { name: "read", title: "Tool", status: "done" }], ["checklist", { phases: [] }],
  ["agent", { name: "scout", status: "idle" }], ["chart", { kind: "bars" }], ["meter", { value: 0.5 }], ["effort", { level: "high" }],
];

async function createGalleryHost(context: FixtureContext) {
  writeFileSync("/tmp/gallery.png", Buffer.from(PNG_BASE64, "base64"));
  const { Text } = await context.load<typeof Ui>("packages/tui/src/index.ts");
  const { node } = await context.load<typeof Describe>("packages/tui/src/native/describe.ts");
  class KindGallery extends Text {
    describe() {
      const children = galleryProps.map(([kind, props]) => node(kind as "col", props as never, ["overlay", "col", "row"].includes(kind) ? [node("text", { text: `${kind} body` })] : undefined, `gallery-${kind}`));
      return node("col", { gap: "sm" }, children, "gallery");
    }
  }
  return new KindGallery("native kind gallery", 0, 0);
}

async function createPickerHost(context: FixtureContext) {
  const { Text } = await context.load<typeof Ui>("packages/tui/src/index.ts");
  const { node } = await context.load<typeof Describe>("packages/tui/src/native/describe.ts");
  class PickerGallery extends Text {
    describe() {
      return node("col", { gap: "sm" }, [node("picker", { title: "Gallery picker" }, undefined, "gallery-picker")], "gallery");
    }
  }
  return new PickerGallery("native picker gallery", 0, 0);
}

const layeredKinds = ["overlay"];

export interface GalleryLayout { gallery: string; framePointer: string; nodeId: (kind: string) => string }

export const galleryCases = (layout: GalleryLayout): FixtureCase[] => {
  const nodePointer = (kind: string, index: number) => {
    const layer = layeredKinds.indexOf(kind);
    return layer >= 0 ? `/ops/${layer + 1}/4` : `${layout.framePointer}/c/${index - layeredKinds.filter(layered => galleryProps.findIndex(([name]) => name === layered) < index).length}`;
  };
  const request = (pointer: string, selector: string) => ({ verb: "f", direction: "out", occurrence: "last", pointer, target: { selector } }) as const;
  const contracts: NativeContract[] = [];
  const galleryAssertions: NativeAssertion[] = [];
  const gallerySelector = `[data-omp-node="${layout.gallery}"]`;
  galleryAssertions.push({ name: "actual gallery container is present", browser: { type: "dom", selector: gallerySelector, read: "count" }, expected: 1 });
  contracts.push(
    { entry: "property:TspNode.[K].c", request: request(`${layout.framePointer}/c`, gallerySelector), assertions: galleryAssertions },
    { entry: "property:TspNode.[K].id", request: request(`${layout.framePointer}/id`, gallerySelector), assertions: galleryAssertions },
  );
  galleryProps.forEach(([kind], index) => {
    const pointer = nodePointer(kind, index);
    const selector = `[data-omp-node="${layout.nodeId(kind)}"]`;
    const assertions: NativeAssertion[] = [{ name: `actual ${kind} node is rendered once`, browser: { type: "dom", selector, read: "count" }, expected: 1 }];
    contracts.push({ entry: "property:TspNode.[K].k", request: request(`${pointer}/k`, selector), value: kind, assertions });
    contracts.push({ entry: "property:TspNode.[K].p", request: request(`${pointer}/p`, selector), assertions });
    contracts.push({ entry: `property:TspPropsByKind.${kind}`, request: request(`${pointer}/p`, selector), assertions });
    if (index === 0) contracts.push({ entry: "property:TspNode.[K]", request: request(pointer, selector), assertions });
  });
  // A picker is a modal layer, and the browser occludes every non-modal layer beneath one, so it needs its own surface.
  const pickerAssertions: NativeAssertion[] = [{ name: "actual picker node is rendered once", browser: { type: "dom", selector: `[data-omp-node="${layout.gallery}.^gallery-picker"]`, read: "count" }, expected: 1 }];
  const pickerCase: FixtureCase = {
    id: "native-node-kind-picker-property-contract", covers: [], create: createPickerHost,
    checkpoints: [{ name: "picker kind reaches the actual browser as a modal sheet", comparisons: [], nativeContracts: [
      { entry: "property:TspPropsByKind.picker", request: request("/ops/1/4/p", `[data-omp-node="${layout.gallery}.^gallery-picker"]`), assertions: pickerAssertions },
    ] }],
  };
  return [pickerCase, { id: "native-node-kind-gallery-property-contracts", covers: [], create: createGalleryHost, checkpoints: [{ name: "every vocabulary kind reaches the actual browser", comparisons: [], nativeContracts: contracts }] }];
};
