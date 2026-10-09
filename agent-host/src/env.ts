import { Connection, RemoteExecutionEnv } from "@earendil-works/pi-env";
import type { ExecutionEnv } from "@earendil-works/pi-durable/env";
import type { Context } from "@earendil-works/chord";
import type { EnvTarget } from "@earendil-works/pi-durable";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { RoostTarget } from "./target-doc.ts";

export const ENV_PIPE_PATH = resolve(dirname(fileURLToPath(import.meta.url)), "env-pipe.ts");
const connections = new Map<string, Connection>();
const environments = new Map<string, RemoteExecutionEnv>();

function connectionFor(workerFp: string): Connection {
  let connection = connections.get(workerFp);
  if (!connection) {
    connection = new Connection({
      command: [process.execPath, ENV_PIPE_PATH, workerFp],
      onLog: text => process.stdout.write(`${JSON.stringify({ level: "info", component: "pi-env", worker_fp: workerFp, message: text.trim() })}\n`),
    });
    connections.set(workerFp, connection);
  }
  return connection;
}

export function envFor(): (target: EnvTarget, context: Context) => Promise<ExecutionEnv | undefined> {
  return async ({ conversationId, read }, context) => {
    const target = await read.snapshot(RoostTarget, conversationId, context);
    if (!target?.worker_fp || !target.cwd) return undefined;
    const key = `${target.worker_fp}\0${target.cwd}`;
    let env = environments.get(key);
    if (!env) {
      env = new RemoteExecutionEnv({ connection: connectionFor(target.worker_fp), id: `roost-worker:${target.worker_fp}`, cwd: target.cwd });
      environments.set(key, env);
    }
    return env;
  };
}
