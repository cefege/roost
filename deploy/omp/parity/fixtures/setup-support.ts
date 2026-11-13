import type { FixtureContext } from "../scenario";
import type { SetupHost, SetupScene } from "@omp-source/packages/tui/src/setup/scenes/types";
import type { Model } from "@omp-source/packages/ai/src/types";

export async function createSetup(context: FixtureContext, sceneId: string, failModelSave = false) {
  const { Settings } = await context.load<typeof import("@omp-source/packages/coding-agent/src/config/settings")>("packages/coding-agent/src/config/settings.ts");
  const { createModelBrowserSource } = await context.load<typeof import("@omp-source/packages/coding-agent/src/modes/model-browser-source")>("packages/coding-agent/src/modes/model-browser-source.ts");
  const { AuthStorage } = await context.load<typeof import("@omp-source/packages/ai/src/auth-storage")>("packages/ai/src/auth-storage.ts");
  const { registerOAuthProvider, getOAuthProviders } = await context.load<typeof import("@omp-source/packages/ai/src/registry/oauth/index")>("packages/ai/src/registry/oauth/index.ts");
  const { buildModel } = await context.load<typeof import("@omp-source/packages/catalog/src/build")>("packages/catalog/src/build.ts");
  const { ALL_SCENES, CURRENT_SETUP_VERSION } = await context.load<typeof import("@omp-source/packages/tui/src/setup/wizard")>("packages/tui/src/setup/wizard.ts");
  const { SetupWizardComponent } = await context.load<typeof import("@omp-source/packages/tui/src/setup/wizard-overlay")>("packages/tui/src/setup/wizard-overlay.ts");
  const { markSetupWizardComplete } = await context.load<typeof import("@omp-source/packages/coding-agent/src/modes/setup")>("packages/coding-agent/src/modes/setup.ts");
  const { initTheme } = await context.load<typeof import("@omp-source/packages/tui/src/theme/theme")>("packages/tui/src/theme/theme.ts");
  const { lookup } = await context.load<typeof import("@omp-source/packages/coding-agent/src/config/registry")>("packages/coding-agent/src/config/registry.ts");
  await initTheme(false, "unicode", false, "titanium", "light");
  const settings = Settings.instance;
  for (const [path, value] of Object.entries({ "composer.shape": "band", symbolPreset: "unicode", colorBlindMode: false, "theme.dark": "titanium", "theme.light": "light" })) {
    const setting = lookup(path);
    if (!setting) throw new Error(`Missing setup preference ${path}`);
    setting.set(settings, value);
  }
  await settings.flush();
  const auth = await AuthStorage.create(`${context.workspace}/setup-auth.sqlite`);
  registerOAuthProvider({ id: "parity-local", name: "Parity Local", sourceId: "parity-fixture", async login(callbacks) {
    const code = await callbacks.onPrompt({ message: "Local fixture code", placeholder: "fixture-code", secret: true });
    if (code !== "fixture-code") throw new Error("Fixture code rejected");
    return { access: "local-unused", refresh: "local-unused", expires: 4102444800000 };
  } });
  const models: Model[] = ["alpha", "beta"].map(id => buildModel({ id, name: `Parity ${id}`, provider: "parity-local", api: "openai-completions", baseUrl: "http://127.0.0.1:1/v1", reasoning: true, input: ["text"], contextWindow: 32768, maxTokens: 4096, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 } }));
  const save = (path: string, value: unknown): void => {
    const setting = lookup(path);
    if (!setting) throw new Error(`Unknown setup preference ${path}`);
    setting.set(settings, value);
    context.record(path, value);
  };
  const host: SetupHost = {
    ui: context.tui, statusLine: undefined, composerShape: "band", symbolPreset: "unicode", colorBlindMode: false,
    disabledProviders: getOAuthProviders().filter(provider => provider.id !== "parity-local").map(provider => provider.id),
    authStorage: auth, modelSource: createModelBrowserSource(settings),
    getModels: () => ({ available: models, all: models, current: models[0] }),
    refreshModels: async () => { context.record("models-refreshed", models.map(model => `${model.provider}/${model.id}`)); },
    selectModel: async (_model, selector) => {
      if (failModelSave) throw new Error("Parity persistence refused");
      settings.setModelRole("default", selector); await settings.flush(); context.record("model", selector);
    },
    refreshProvider: async provider => {
      const reopened = await AuthStorage.create(`${context.workspace}/setup-auth.sqlite`);
      context.record("saved-provider", reopened.credentials.hasOAuth(provider));
      reopened.close();
      context.record("provider", provider);
    },
    saveComposerShape: async shape => { save("composer.shape", shape); await settings.flush(); },
    saveSymbolPreset: preset => save("symbolPreset", preset),
    saveColorBlindMode: enabled => save("colorBlindMode", enabled),
    saveTheme: (mode, name) => save(`theme.${mode}`, name),
    captureBrowserSession: async () => { throw new Error("No external browser authentication in parity fixtures"); },
    openInBrowser: () => { throw new Error("No external provider requests in parity fixtures"); },
    markComplete: async version => { await markSetupWizardComplete(settings, version); context.record("setup-version", version); },
    playWelcomeIntro: () => { context.record("welcome", true); },
    showError: message => { context.record("error", message); },
  };
  const scene: SetupScene | undefined = ALL_SCENES.find(candidate => candidate.id === sceneId);
  if (!scene) throw new Error(`Missing canonical setup scene ${sceneId}`);
  const readSavedSetup = async (): Promise<Record<string, unknown>> => {
    await settings.flush();
    const restored = await Settings.loadIsolated({ cwd: context.workspace });
    const restoredAuth = await AuthStorage.create(`${context.workspace}/setup-auth.sqlite`);
    const providerAuthorized = restoredAuth.credentials.hasOAuth("parity-local");
    restoredAuth.close();
    return {
      version: lookup("setupVersion")?.get(restored),
      composer: lookup("composer.shape")?.get(restored),
      symbolPreset: lookup("symbolPreset")?.get(restored),
      colorBlindMode: lookup("colorBlindMode")?.get(restored),
      dark: lookup("theme.dark")?.get(restored),
      light: lookup("theme.light")?.get(restored),
      model: restored.getModelRole("default"),
      providerAuthorized,
    };
  };
  const observedScene: SetupScene = { ...scene, mount: sceneHost => scene.mount({
    ...sceneHost, finish: result => {
      context.record("scene-outcome", { scene: scene.id, result });
      context.record("scene-finish", result);
      void readSavedSetup().then(saved => context.record("scene-settings", { scene: scene.id, result, ...saved }));
      sceneHost.finish(result);
    },
  }) };
  const wizard = new SetupWizardComponent(host, [observedScene]);
  void wizard.run().then(async () => {
    await host.markComplete(CURRENT_SETUP_VERSION);
    context.record("saved-setup", await readSavedSetup());
    auth.close();
    wizard.dispose();
  });
  wizard.handleInput("\r");
  return wizard;
}
