import { createHmac, randomBytes, timingSafeEqual } from "node:crypto";
import type { SkillPackument, SkillSearchHit, SkillSearchResponse, SkillVersionManifest, SkillVersionSummary } from "@omp-source/packages/wire/src/skillshare";
import type { BuiltinSeedContext } from "./builtin-seed";
import { startHttpService, type LocalHttpService, type ServiceTrace } from "./fixture-service-http";

export interface SkillRegistryInput {
  scope: string;
  name: string;
  version: string;
  body: string;
}

export async function startSkillRegistry(input: SkillRegistryInput, context: BuiltinSeedContext, trace: ServiceTrace): Promise<{ service: LocalHttpService; settings: Record<string, unknown> }> {
  const wire = await context.load<typeof import("@omp-source/packages/wire/src/skillshare")>("packages/wire/src/skillshare.ts");
  const tar = await context.load<typeof import("@omp-source/packages/coding-agent/src/skillshare/tar")>("packages/coding-agent/src/skillshare/tar.ts");
  const configuration = await context.load<typeof import("@omp-source/packages/coding-agent/src/extensibility/settings")>("packages/coding-agent/src/extensibility/settings.ts");
  if (!configuration.cfgSkillsRegistryUrl?.id) throw new Error("Production has no configurable skill registry route");
  if (!wire.SKILL_SCOPE_RE.test(input.scope) || !wire.SKILL_NAME_RE.test(input.name)) throw new Error("Invalid parity skill registry package identifier");
  const description = `Parity ${input.name} skill`;
  const content = new TextEncoder().encode(`---\nname: ${input.name}\ndescription: ${description}\nmetadata:\n  version: ${JSON.stringify(input.version)}\n---\n\n${input.body}`);
  const archive = Bun.gzipSync(tar.writeTar([{ path: "SKILL.md", content, executable: false }]));
  const integrity = `sha512-${new Bun.CryptoHasher("sha512").update(archive).digest("base64")}`;
  const publisher = { username: input.scope, avatar: new Bun.CryptoHasher("sha256").update(input.scope).digest("hex") };
  const summary: SkillVersionSummary = { version: input.version, publishedAt: 0, publisher, integrity, size: archive.byteLength, unpackedSize: content.byteLength, fileCount: 1, hasScripts: false, yanked: false };
  const manifest: SkillVersionManifest = {
    ...summary, scope: input.scope, name: input.name, description, metadata: { version: input.version }, keywords: [],
    files: [{ path: "SKILL.md", size: content.byteLength, sha256: new Bun.CryptoHasher("sha256").update(content).digest("hex"), executable: false }],
    readmePath: "SKILL.md", provenance: { publisher, viaToken: false },
  };
  const packument: SkillPackument = {
    scope: input.scope, name: input.name, description, keywords: [], owners: [publisher], distTags: { latest: input.version },
    versions: { [input.version]: summary }, createdAt: 0, updatedAt: 0, downloads: { weekly: 0, total: 0, daily: [] }, canManage: false,
  };
  const hit: SkillSearchHit = { scope: input.scope, name: input.name, description, keywords: [], version: input.version, publisher, updatedAt: 0, weeklyDownloads: 0 };
  const signedPath = `/storage/${input.scope}/${input.name}/${input.version}.tgz`;
  const secret = randomBytes(32);
  const signature = createHmac("sha256", secret).update(signedPath).digest("hex");
  const versions = [input.version, "latest"];
  const versionPaths = versions.map(version => wire.SKILLS_ROUTES.version(input.scope, input.name, encodeURIComponent(version)));
  const tarballPaths = versions.map(version => wire.SKILLS_ROUTES.tarball(input.scope, input.name, encodeURIComponent(version)));
  const service = startHttpService("skill-registry", trace, request => {
    const url = new URL(request.url);
    if (request.method !== "GET") return Response.json({ error: "Method not allowed" }, { status: 405 });
    if (url.pathname === wire.SKILLS_ROUTES.search) {
      const query = (url.searchParams.get("q") ?? "").toLowerCase();
      const hits = `@${input.scope}/${input.name} ${description}`.toLowerCase().includes(query) ? [hit] : [];
      const response: SkillSearchResponse = { total: hits.length, page: Number(url.searchParams.get("page") ?? 1), perPage: 20, hits };
      return Response.json(response);
    }
    if (url.pathname === wire.SKILLS_ROUTES.packument(input.scope, input.name)) return Response.json(packument);
    if (versionPaths.includes(url.pathname)) return Response.json(manifest);
    if (tarballPaths.includes(url.pathname)) return Response.redirect(new URL(`${signedPath}?signature=${signature}`, url.origin).href, 302);
    if (url.pathname === signedPath) {
      const supplied = Buffer.from(url.searchParams.get("signature") ?? "", "hex");
      const expected = Buffer.from(signature, "hex");
      if (supplied.length !== expected.length || !timingSafeEqual(supplied, expected)) return new Response("Invalid storage signature", { status: 403 });
      return new Response(archive, { headers: { "Content-Type": "application/gzip", [wire.SKILL_HEADERS.integrity]: integrity } });
    }
    return Response.json({ error: "Package not found" }, { status: 404 });
  });
  return { service, settings: { [configuration.cfgSkillsRegistryUrl.id]: service.url } };
}
