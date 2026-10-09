import type { Harness } from "@earendil-works/pi-durable";
import type { DatabaseSync } from "node:sqlite";
import type { HostConfig } from "./config.ts";
import { Conversations } from "./conversations.ts";
import { EventHub } from "./event-hub.ts";
import { createAgentHost } from "./http.ts";
import { Logins } from "./logins.ts";
import type { ModelsService } from "./models.ts";

export function createAgentHostServices(input: { harness: Harness; db: DatabaseSync; config: HostConfig; models: ModelsService }) {
  const hub = new EventHub(input.harness, input.db);
  const conversations = new Conversations(input.harness, input.db, input.models, hub);
  hub.setConversations(conversations);
  const logins = new Logins(input.models);
  const http = createAgentHost({ config: input.config, conversations, hub, models: input.models, logins });
  return { hub, conversations, logins, http };
}
