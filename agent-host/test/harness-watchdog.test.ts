import { BACKGROUND_CONTEXT } from "@earendil-works/chord/context";
import { Harness, MemoryStorage } from "@earendil-works/pi-durable";
import { createModels, fauxProvider } from "@earendil-works/pi-ai";
import { expect, test } from "vitest";
import { watchHarness } from "../src/harness-watchdog.ts";
import { createAgentRegistry } from "../src/registry.ts";

type Fatal = { reason: string; cause: unknown };

// Storage whose next commit fails after admission, the way a full or vanished disk does.
async function openHarness(): Promise<{ harness: Harness; failNextCommit: () => void }> {
  const storage = new MemoryStorage();
  let failing = false;
  const flaky = new Proxy(storage, {
    get(target, property) {
      if (property === "commit" && failing) {
        failing = false;
        return async () => { throw new Error("disk went away"); };
      }
      const value = Reflect.get(target, property, target);
      return typeof value === "function" ? value.bind(target) : value;
    },
  });
  const models = createModels({});
  models.setProvider(fauxProvider({ provider: "faux", models: [{ id: "faux-model", reasoning: false }] }).provider);
  const harness = await Harness.open(flaky, { models, registry: createAgentRegistry() }, BACKGROUND_CONTEXT);
  return { harness, failNextCommit: () => { failing = true; } };
}

test("a commit that fails after storage admission is reported as fatal", async () => {
  const { harness, failNextCommit } = await openHarness();
  const fatal: Fatal[] = [];
  const watchdog = watchHarness(harness, (reason, cause) => fatal.push({ reason, cause }), 60_000);
  await watchdog.check("startup");
  expect(fatal).toEqual([]);
  failNextCommit();
  await expect(harness.createConversation({ ownership: { kind: "ownerless" } }, BACKGROUND_CONTEXT)).rejects.toThrow("disk went away");
  await watchdog.check("internal error");
  await watchdog.check("periodic probe");
  expect(fatal).toHaveLength(1);
  expect(fatal[0]?.reason).toBe("harness unusable after internal error");
  expect(String((fatal[0]?.cause as Error).cause)).toContain("disk went away");
  watchdog.stop();
});

test("only a close the host did not ask for is fatal", async () => {
  const unexpected = await openHarness();
  const fatal: Fatal[] = [];
  watchHarness(unexpected.harness, (reason, cause) => fatal.push({ reason, cause }), 60_000);
  await unexpected.harness.close(BACKGROUND_CONTEXT);
  expect(fatal.map(entry => entry.reason)).toEqual(["harness closed unexpectedly"]);

  const planned = await openHarness();
  const quiet: Fatal[] = [];
  const watchdog = watchHarness(planned.harness, (reason, cause) => quiet.push({ reason, cause }), 60_000);
  watchdog.markShuttingDown();
  await planned.harness.close(BACKGROUND_CONTEXT);
  await watchdog.check("after shutdown");
  expect(quiet).toEqual([]);
});
