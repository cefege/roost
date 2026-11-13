import type { BehaviorClaim, Comparison, FixtureCase } from "../scenario";

const sceneConstructors: Readonly<Record<string, string>> = {
  glyphSetupScene: "glyph-mode", composerSetupScene: "composer-shape", modelSetupScene: "model",
  themeSetupScene: "theme", providersSetupScene: "providers",
};
const sceneIds = new Set(Object.values(sceneConstructors));

export function proveSetupFixture(fixture: FixtureCase): FixtureCase {
  const covers: string[] = [];
  const behaviors = [...(fixture.behaviors ?? [])];
  for (const entry of fixture.covers) {
    if (entry.startsWith("setup:") || entry.startsWith("composer:")) { behaviors.push(entry); continue; }
    const constructor = entry.startsWith("scene:") ? sceneConstructors[entry.slice("scene:".length)] : undefined;
    const canonical = constructor ? `scene:${constructor}` : entry;
    if (!covers.includes(canonical)) covers.push(canonical);
  }
  const scene = covers.find(entry => entry.startsWith("scene:"))?.slice("scene:".length);
  if (!scene || !sceneIds.has(scene)) throw new Error(`Unknown source setup scene in ${fixture.id}`);
  const completion = fixture.checkpoints.find(checkpoint => checkpoint.name === "finish and persist setup version");
  if (!completion) throw new Error(`Setup case lacks actual completion persistence: ${fixture.id}`);
  const savedFields = completion.comparisons.filter(comparison => comparison.name.startsWith("saved setup ") && comparison.name !== "saved setup version");
  const previous: { checkpoint: string; comparison: string }[] = [];
  const checkpoints = fixture.checkpoints.map(checkpoint => {
    const cancellation = checkpoint.comparisons.some(comparison => comparison.terminal.type === "effect" && comparison.terminal.channel === "scene-finish" && comparison.expected === "skipped");
    const finishes = cancellation || checkpoint.comparisons.some(comparison => comparison.terminal.type === "effect" && ["symbolPreset", "composer.shape", "model", "theme.dark", "theme.light", "colorBlindMode", "scene-finish"].includes(comparison.terminal.channel));
    if (checkpoint.name === completion.name || !finishes) {
      for (const comparison of checkpoint.comparisons) if (comparison.terminal.type === "effect" || comparison.name === "canonical preview selection" || comparison.name === "selected model preview") previous.push({ checkpoint: checkpoint.name, comparison: comparison.name });
      return checkpoint;
    }
    const result = cancellation ? "skipped" : "done";
    const outcome: Comparison = { name: "actual scene finish identity", terminal: { type: "effect", channel: "scene-outcome" }, browser: { type: "effect", channel: "scene-outcome" }, expected: { scene, result } };
    const saved: Comparison[] = savedFields.map(comparison => {
      if (comparison.terminal.type !== "effect" || !comparison.terminal.pointer) throw new Error(`Missing actual setup saved-field pointer in ${fixture.id}`);
      return { name: `scene reload ${comparison.name}`, terminal: { type: "effect", channel: "scene-settings", pointer: comparison.terminal.pointer },
        browser: { type: "effect", channel: "scene-settings", pointer: comparison.terminal.pointer }, expected: comparison.expected };
    });
    const target: Comparison = { name: "actual completed wizard remains visible", terminal: { type: "screen", pattern: "Setup saved", read: "visible" },
      browser: { type: "dom", selector: "[data-omp-role='omp.setup.outro.title'] :text-is(\"You're all set\")", read: "visible" }, expected: true };
    const comparisons = [...checkpoint.comparisons, outcome, ...saved, target];
    const assertions = comparisons.filter(comparison => comparison.terminal.type === "effect").map(comparison => ({ checkpoint: checkpoint.name, comparison: comparison.name }));
    const claim: BehaviorClaim = { entry: `scene:${scene}`, effect: "interaction", value: { scene, result }, target: { checkpoint: checkpoint.name, comparison: target.name },
      assertionGroups: [{ comparisons: [...assertions, ...previous.filter(reference => ["credentials survived reopening actual store", "canonical preview selection", "selected model preview"].includes(reference.comparison))] }] };
    outcome.proves = [claim];
    return { ...checkpoint, comparisons };
  });
  return { ...fixture, covers, behaviors, checkpoints };
}
