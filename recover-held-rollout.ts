import { loadCoordinatorDeployJournal } from "./apps/roost-cli/src/coordinator-deploy-journal.ts";
import { prepareCoordinatorDeployLocation, acquireFleetPushTransaction } from "./apps/roost-cli/src/push-coordinator.ts";
import { planFromJournal, _atomicFleetConvergenceProblems } from "./apps/roost-cli/src/push-fleet-plan.ts";
import { fleetRuntime } from "./apps/roost-cli/src/push-rollout-runtime.ts";
import { convergeAtomicFleet } from "./apps/roost-cli/src/push-fleet-rollout.ts";
import { workerInventoryForUpdateAdmission } from "./apps/roost-cli/src/status.ts";

const location = prepareCoordinatorDeployLocation();
const transaction = await acquireFleetPushTransaction(location);
try {
  const journal = loadCoordinatorDeployJournal(location.journalPath, location.context);
  if (!journal || journal.phase !== "fleet-converging") {
    throw new Error("expected held fleet-converging coordinator journal");
  }
  const plan = planFromJournal(journal, workerInventoryForUpdateAdmission());
  await convergeAtomicFleet(plan, fleetRuntime(location, plan, _atomicFleetConvergenceProblems));
} finally {
  await transaction.release();
}
