import "./styles/embed.css";
import "./components/shell/shell.css";
import "./components/transcript/transcript.css";
import "./components/agents/agents.css";
import "./tool-render/tool-render.css";

export { SessionSurface } from "./app";
export type { SessionSurfaceProps } from "./app";
export { SessionReplica } from "./lib/session-replica";
export type {
	ActiveTool,
	ConnectionPhase,
	GuestSnapshot,
	Notice,
	SessionActivity,
	SessionActions,
	SessionGoal,
	SessionGoalStatus,
	SessionReplicaEffect,
	SessionRuleSummary,
	SessionTodo,
	SessionTodoStatus,
	TranscriptResult,
} from "./lib/client";
export type {
	ToolRenderHost,
	ToolRenderer,
	ToolRenderProps,
	ToolResultBlock,
	ToolResultImage,
	ToolResultLike,
	ToolResultText,
} from "./tool-render/types";
