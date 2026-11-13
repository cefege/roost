import type * as BoxModule from "@omp-source/packages/tui/src/components/box";
import type * as TableModule from "@omp-source/packages/tui/src/components/table";
import type * as KeyValueModule from "@omp-source/packages/tui/src/components/key-value-list";
import type * as TreeModule from "@omp-source/packages/tui/src/components/tree-view";
import type { FixtureCase } from "../scenario";
import { contentComparison } from "./gallery-support";

const claim = (entry: string, value: unknown) => ({ entry, effect: "literal" as const, value: value as never });

export const fixtureCases: FixtureCase[] = [
  {
    id: "primitive-structured-identity-values",
    covers: [
      "kind:table", "kind:kv", "kind:tree",
      "property:TspKvProps.items", "property:TspKvProps.items[].k", "property:TspKvProps.items[].v",
      "property:TspTableColumn.id", "property:TspTableProps.rows[].id", "property:TspTableProps.rows[].cells", "property:TspTableProps.rows[].cells[key]",
      "property:TspTreeNode.id", "property:TspTreeNode.label", "property:TspTreeNode.open", "property:TspTreeNode.children",
    ],
    async create(context) {
      const { Box } = await context.load<typeof BoxModule>("packages/tui/src/components/box.ts");
      const { Table } = await context.load<typeof TableModule>("packages/tui/src/components/table.ts");
      const { KeyValueList } = await context.load<typeof KeyValueModule>("packages/tui/src/components/key-value-list.ts");
      const { TreeView } = await context.load<typeof TreeModule>("packages/tui/src/components/tree-view.ts");
      const root = new Box(0, 0);
      root.addChild(new Table([[{ text: "Alpha" }, { text: "11" }], [{ text: "Bravo" }, { text: "22" }]], [{ width: 10, align: "left", overflow: "truncate" }, { width: 6, align: "right", overflow: "allow" }]));
      root.addChild(new KeyValueList([{ label: "Owner", value: "Platform crew" }, { label: "State", value: "Steady" }], { labelWidth: 8 }));
      interface Entry { id: string; title: string; children: Entry[] }
      const roots: Entry[] = [{ id: "root-node", title: "Source tree", children: [{ id: "leaf-node", title: "Leaf entry", children: [] }] }];
      root.addChild(new TreeView({ roots, theme: context.theme, getKey: entry => entry.id, getChildren: entry => entry.children, renderRow: entry => entry.title }));
      return root;
    },
    checkpoints: [{ name: "structured identity and values", comparisons: [
      contentComparison("kv key", "(Owner)", 'dt:has-text("Owner")', "Owner", [claim("property:TspKvProps.items[].k", "Owner")]),
      contentComparison("kv value", "(Platform crew)", 'dd:has-text("Platform crew")', "Platform crew", [claim("property:TspKvProps.items[].v", "Platform crew")]),
      contentComparison("kv items", "(Steady)", 'dd:has-text("Steady")', "Steady", [claim("property:TspKvProps.items", "Steady")]),
      contentComparison("table cell", "(Bravo)", "tbody > tr:nth-child(2) > td:nth-child(1)", "Bravo", [
        claim("property:TspTableProps.rows[].cells", "Bravo"),
        claim("property:TspTableProps.rows[].cells[key]", "Bravo"),
      ]),
      { name: "table row cells share a row", terminal: { type: "screen", pattern: "Bravo", relativeTo: { pattern: "22" }, read: "sameRow" },
        browser: { type: "dom", selector: "tbody > tr:nth-child(2) > td:nth-child(1)", relativeTo: "tbody > tr:nth-child(2) > td:nth-child(2)", read: "sameRow" }, expected: true,
        proves: [
          { entry: "property:TspTableColumn.id", effect: "layout", value: "c1" },
          { entry: "property:TspTableProps.rows[].id", effect: "layout", value: "r1" },
        ] },
      { name: "tree child renders below its open parent", terminal: { type: "screen", pattern: "Leaf entry", relativeTo: { pattern: "Source tree" }, read: "below" },
        browser: { type: "dom", selector: ':text-is("Leaf entry")', relativeTo: ':text-is("Source tree")', read: "below" }, expected: true,
        proves: [
          { entry: "property:TspTreeNode.id", effect: "layout", value: "leaf-node" },
          { entry: "property:TspTreeNode.label", effect: "layout", value: "Leaf entry" },
          { entry: "property:TspTreeNode.open", effect: "layout", value: true },
          { entry: "property:TspTreeNode.children", effect: "layout", value: "leaf-node" },
        ] },
    ] }],
  },
];
