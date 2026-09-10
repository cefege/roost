// Settings pane switch. SettingsRoot selects its pane from the URL for desktop and compact layouts.
// This file owns only the stable pane-to-component mapping; navigation stays at the shell boundary.
// Every pane keeps its existing data and mutation owner.

import { Match, Switch } from "solid-js";
import { AgentLauncherPane } from "./AgentLauncherPane.tsx";
import { AttachmentsPane } from "./AttachmentsPane.tsx";
import { AuditLogPane } from "./AuditLogPane.tsx";
import { ConnectionPane } from "./ConnectionPane.tsx";
import { DevicesPane } from "./DevicesPane.tsx";
import { MachinesPane } from "./MachinesPane.tsx";
import { McpPane } from "./McpPane.tsx";
import { MetricsPane } from "./MetricsPane.tsx";
import { NotificationsPane } from "./NotificationsPane.tsx";
import { TerminalPane } from "./TerminalPane.tsx";
import { ThemePane } from "./ThemePane.tsx";
import { TranscriptionPane } from "./TranscriptionPane.tsx";

export function SettingsPane(props: { id: string }) {
  return (
    <Switch>
      <Match when={props.id === "machines"}><MachinesPane /></Match>
      <Match when={props.id === "connection"}><ConnectionPane /></Match>
      <Match when={props.id === "devices"}><DevicesPane /></Match>
      <Match when={props.id === "launcher"}><AgentLauncherPane /></Match>
      <Match when={props.id === "mcp"}><McpPane /></Match>
      <Match when={props.id === "voice"}><TranscriptionPane /></Match>
      <Match when={props.id === "terminal"}><TerminalPane /></Match>
      <Match when={props.id === "notifications"}><NotificationsPane /></Match>
      <Match when={props.id === "attachments"}><AttachmentsPane /></Match>
      <Match when={props.id === "theme"}><ThemePane /></Match>
      <Match when={props.id === "audit"}><AuditLogPane /></Match>
      <Match when={props.id === "metrics"}><MetricsPane /></Match>
    </Switch>
  );
}
