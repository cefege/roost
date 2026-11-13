import type { FixtureCase } from "../scenario";
import { fixtureCases as accountCases } from "./account-apps";
import { fixtureCases as agentCases } from "./agent-apps";
import { fixtureCases as appCases } from "./apps";
import { fixtureCases as extensionCases } from "./extensions";
import { fixtureCases as modelCases } from "./model-apps";
import { fixtureCases as reportCases } from "./report-apps";
import { fixtureCases as workspaceCases } from "./workspace-apps";
import { fixtureCases as timelineCases } from "./timeline";
import { fixtureCases as chatComponentCases } from "./components-chat";
import { fixtureCases as chatMessageComponentCases } from "./components-chat-messages";
import { fixtureCases as chromeBlockComponentCases } from "./components-chrome-blocks";
import { fixtureCases as chromeComponentCases } from "./components-chrome";
import { fixtureCases as chromeWidgetComponentCases } from "./components-chrome-widgets";
import { fixtureCases as cliAppComponentCases } from "./components-cli-apps";
import { fixtureCases as shellComponentCases } from "./components-shell";
import { fixtureCases as chatExecComponentCases } from "./components-chat-exec";
import { fixtureCases as formComponentCases } from "./components-forms";
import { fixtureCases as renderComponentCases } from "./components-render";
import { fixtureCases as overlayComponentCases } from "./components-overlays";

export const fixtureCases: FixtureCase[] = [
  ...accountCases, ...agentCases, ...appCases, ...extensionCases, ...modelCases,
  ...reportCases, ...workspaceCases, ...timelineCases,
  ...chatComponentCases, ...chatMessageComponentCases, ...chromeComponentCases, ...chromeBlockComponentCases, ...chromeWidgetComponentCases,
  ...cliAppComponentCases, ...shellComponentCases, ...chatExecComponentCases, ...formComponentCases, ...renderComponentCases,
  ...overlayComponentCases,
];
