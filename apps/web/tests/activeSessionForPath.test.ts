// activeSessionForPath — the shared route→session resolver used by TabBar /
// FolderList / MobileTopBar so all three scope to exactly what MainPane renders.
// Asserts every terminal route shape resolves: /s/:id, /t/:workerFp/*folderPath,
// /w/:workspaceId; and non-terminal routes return null. Regression for the
// "top tab bar shows ALL workspaces" bug (only /s/ used to resolve).

import { expect, test, describe, beforeEach } from "bun:test";
import { asWorkerFp, asSessionId, asChannelId } from "@roost/shared/wire";
import type { Session } from "@roost/shared/wire";
import { rootStore, setRootStore } from "../src/store/root.ts";
import { activeSessionForPath, resolveSessionByWorkspace } from "../src/store/selectors.ts";
import { encodeFolderPath } from "../src/lib/terminalHref.ts";

const FP_A = asWorkerFp("aa".repeat(32));
const ID = "00000000-0000-4000-8000-000000000001";
const FOLDER = "/Users/you/roost";

function sess(over: Omit<Partial<Session>, "id"> & { id: string }): Session {
  const { id, ...rest } = over;
  return {
    worker_fp: FP_A,
    channel: asChannelId(1),
    kind: "shell",
    cwd: FOLDER,
    spawn_cwd: FOLDER,
    workspace_id: "ws1",
    status: "open",
    agent: null,
    created_at: 1000,
    closed_at: null,
    custom_title: null,
    ...rest,
    id: asSessionId(id),
  } as Session;
}

describe("activeSessionForPath", () => {
  beforeEach(() => setRootStore("sessions", { [ID]: sess({ id: ID }) } as Record<string, Session>));

  test("/s/:id resolves by id", () => {
    expect(activeSessionForPath(`/s/${ID}`)?.id as string).toBe(ID);
  });

  test("/t/:workerFp/*folderPath resolves by (worker, spawn folder)", () => {
    expect(activeSessionForPath(`/t/${FP_A}/${encodeFolderPath(FOLDER)}`)?.id as string).toBe(ID);
  });

  test("/w/:workspaceId resolves by workspace", () => {
    expect(activeSessionForPath("/w/ws1")?.id as string).toBe(ID);
  });

  test("non-terminal routes return null", () => {
    expect(activeSessionForPath("/settings/machines")).toBeNull();
    expect(activeSessionForPath("/search")).toBeNull();
    expect(activeSessionForPath("/")).toBeNull();
  });

  test("empty/unknown workspace → null (empty-workspace bounce)", () => {
    expect(resolveSessionByWorkspace("nope")).toBeNull();
    expect(activeSessionForPath("/w/nope")).toBeNull();
  });
});

void rootStore;
