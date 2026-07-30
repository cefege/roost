/** @jsxImportSource react */
import type { ReactNode } from "react";
import type { SessionActivity, SessionGoal, SessionTodo } from "../../lib/session-replica";

export interface ActivityStripProps {
	todos: readonly SessionTodo[];
	goal: SessionGoal | null;
	activity: readonly SessionActivity[];
}

function activityText(row: SessionActivity): string {
	switch (row.kind) {
		case "retry":
			return row.phase === "started"
				? `Retry ${row.attempt}/${row.maxAttempts} in ${Math.ceil(row.delayMs / 1000)}s: ${row.message}`
				: row.phase === "succeeded"
					? `Retry ${row.attempt} succeeded`
					: `Retry ${row.attempt} failed${row.message ? `: ${row.message}` : ""}`;
		case "fallback":
			return row.phase === "applied"
				? `Fallback ${row.from} → ${row.to} (${row.role})`
				: `Fallback ${row.model} succeeded (${row.role})`;
		case "ttsr":
			return `TTSR: ${row.rules.map(rule => rule.name).join(", ") || "rules applied"}`;
		case "todo":
			return row.phase === "reminder"
				? `Todo reminder ${row.attempt}/${row.maxAttempts} · ${row.todoCount} open`
				: "Todo list cleared";
		case "irc":
			return `${row.from ? `${row.from}: ` : "IRC: "}${row.text}`;
		case "goal":
			return row.goal ? `Goal ${row.goal.status}: ${row.goal.objective}` : "Goal cleared";
	}
}

export function ActivityStrip({ todos, goal, activity }: ActivityStripProps): ReactNode {
	const recent = activity.slice(-8).reverse();
	if (!goal && todos.length === 0 && recent.length === 0) return null;
	return (
		<details className="sh-activity" open={todos.some(todo => todo.status === "in_progress")}>
			<summary>
				<span>Session activity</span>
				<span className="sh-activity-count">{todos.length + recent.length + (goal ? 1 : 0)}</span>
			</summary>
			<div className="sh-activity-body">
				{goal && (
					<div className="sh-activity-goal">
						<strong>Goal · {goal.status}</strong>
						<span>{goal.objective}</span>
					</div>
				)}
				{todos.length > 0 && (
					<ul className="sh-activity-todos">
						{todos.map((todo, index) => (
							<li key={`${index}:${todo.content}`} data-status={todo.status}>
								<span aria-hidden="true">{todo.status === "completed" ? "✓" : todo.status === "in_progress" ? "●" : "○"}</span>
								<span>{todo.content}</span>
							</li>
						))}
					</ul>
				)}
				{recent.length > 0 && (
					<ul className="sh-activity-events">
						{recent.map(row => <li key={row.id}>{activityText(row)}</li>)}
					</ul>
				)}
			</div>
		</details>
	);
}
