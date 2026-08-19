import { defineCollection, z } from "astro:content";
import { glob } from "astro/loaders";

/** Docs pages. `order` drives both the sidebar and the section index. */
const docs = defineCollection({
  loader: glob({ pattern: "**/*.md", base: "./src/content/docs" }),
  schema: z.object({
    title: z.string(),
    description: z.string(),
    order: z.number(),
    section: z.enum(["Start", "Concepts", "Reference"]),
  }),
});

/**
 * Comparison pages. `matrix` is the single source of truth for the columns
 * rendered on both /alternatives/ and each /alternatives/<competitor>-vs-roost/
 * page, so the hub and the detail pages cannot drift.
 */
const compare = defineCollection({
  loader: glob({ pattern: "**/*.md", base: "./src/content/compare" }),
  schema: z.object({
    competitor: z.string(),
    vendor: z.string(),
    license: z.string(),
    url: z.string().url(),
    order: z.number(),
    category: z.enum([
      "agent-terminal",
      "browser-terminal",
      "worktree-gui",
      "cloud-agent",
      "classic-multiplexer",
    ]),
    matrix: z.object({
      hostPlatforms: z.string(),
      clientDevices: z.string(),
      multiMachine: z.enum(["yes", "partial", "no"]),
      zeroInstallClient: z.enum(["yes", "no"]),
      persistentSessions: z.enum(["yes", "partial", "no"]),
      anyCli: z.enum(["yes", "partial", "no"]),
      mobileUx: z.string(),
      voiceInput: z.enum(["yes", "no"]),
      pushAgentState: z.string(),
      selfHostedNoAccount: z.enum(["yes", "partial", "no"]),
    }),
    /**
     * Skim-first fields. The detail page answers the question above the fold:
     * `verdict` is the one-sentence bottom line, then the two `pick*` lines are
     * rendered side by side, then the matrix. Prose comes after all of it.
     */
    verdict: z.string(),
    pickRoostIf: z.string(),
    useInsteadIf: z.string(),
  }),
});

export const collections = { docs, compare };
