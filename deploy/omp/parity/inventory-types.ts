import type { RuntimeSchema } from "./inventory-runtime-schema";
import type { RuntimeSettingsInventory } from "./inventory-runtime-settings";
export type { RuntimeSchema, RuntimeSchemaDeclaration, RuntimeSchemaField, RuntimeSchemaNode as SchemaNode } from "./inventory-runtime-schema";

export interface InventoryEntry {
  id: string;
  category: string;
  source: string;
  line: number;
  details: Record<string, unknown>;
}

export interface InterfaceInventory {
  revision: string;
  entries: InventoryEntry[];
  runtimeSchema?: RuntimeSchema;
  runtimeSettings?: RuntimeSettingsInventory;
}
