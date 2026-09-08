// `roost coord` — run the coordinator in THIS process (the compiled binary's
// server mode; the analog of `herdr server`). runCoord auto-detects the SPA +
// migrations baked into the binary by scripts/build-binary.ts, and otherwise
// serves the SPA from disk and reads migrations from apps/coord/migrations.

export async function coord(_args: string[]): Promise<void> {
  // A static import cannot work here: coordinator module initialization
  // resolves data paths and pulls in the SPA embed, and `roost test` builds
  // that bundle — this command is the only caller that may load it.
  const { runCoord } = await import("../../coord/src/main.ts");
  await runCoord();
}
