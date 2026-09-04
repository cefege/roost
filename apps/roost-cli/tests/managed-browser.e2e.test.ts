// This E2E drives the production managed-auth UI against one disposable coordinator.
// The managed profile supplies its immutable image and shared Docker network.
// The scenario owns only its coordinator container, browser, proxy, and temporary files.

import { test, expect } from "bun:test";
import type { Browser, Page } from "@playwright/test";
import { chromium } from "@playwright/test";
import type { Server } from "bun";
import { Database } from "bun:sqlite";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { createEmailOutboxPayloadCipher } from "@roost/shared/email-payload";
import { ManagedInstanceRuntime, type ManagedInstanceSpec } from "../src/saas/docker.ts";
import type { RegistryAccount, RegistryCoordinator } from "../src/saas/registry.ts";
import { instanceLayoutFor } from "../src/saas/layout.ts";
import {
  requiredManagedE2eResources,
  writeEd25519VerificationKeyFixture,
} from "./managed-e2e-fixture.ts";

const enabled = process.env.ROOST_SAAS_E2E === "1";
const ACCOUNT_ID = "77777777-7777-4777-8777-777777777777";
const COORDINATOR_ID = "88888888-8888-4888-8888-888888888888";
const EMAIL = "browser-smoke@example.com";
const PASSWORD = "correct horse battery staple";
const NEW_PASSWORD = "a new correct horse battery staple";
const ROUTE_KEY = "c".repeat(64);

function docker(args: readonly string[]): string {
  const result = Bun.spawnSync(["docker", ...args], { stdout: "pipe", stderr: "pipe" });
  if (result.exitCode !== 0) throw new Error(`docker ${args[0] ?? "command"} failed`);
  return result.stdout.toString().trim();
}

function makeSpec(
  root: string,
  sharedKey: string,
  authVerifyKeyFile: string,
  imageId: string,
): ManagedInstanceSpec {
  const account: RegistryAccount = {
    id: ACCOUNT_ID,
    emailNormalized: EMAIL,
    routeKey: ROUTE_KEY,
    state: "pending",
    createdAtMs: 1,
    activatedAtMs: null,
    disabledAtMs: null,
  };
  const hex = COORDINATOR_ID.replaceAll("-", "");
  const coordinator: RegistryCoordinator = {
    id: COORDINATOR_ID,
    accountId: ACCOUNT_ID,
    ordinal: 1,
    routeKey: ROUTE_KEY,
    hostname: `c-${hex}.dashboard.roosttt.com`,
    containerName: `roost-coord-${hex}`,
    dataDir: join(root, "instances", COORDINATOR_ID, "data"),
    imageDigest: imageId,
    state: "seeded",
    createdAtMs: 1,
    seededAtMs: 1,
    runningAtMs: null,
    routedAtMs: null,
    invitedAtMs: null,
    activatedAtMs: null,
    disabledAtMs: null,
    failedAtMs: null,
    updatedAtMs: 1,
    lastError: null,
  };
  return {
    account,
    coordinator,
    authVerifyKeyFile,
    email: {
      resendEndpoint: "https://api.resend.com/emails",
      emailFrom: "Roost Browser Smoke <noreply@example.com>",
      sharedResendApiKeyPath: sharedKey,
    },
  };
}

function containerIp(name: string, network: string): string {
  return docker(["inspect", name, "--format", `{{(index .NetworkSettings.Networks "${network}").IPAddress}}`]);
}

function outboxToken(spec: ManagedInstanceSpec, kind: "owner_activation" | "password_reset"): string {
  const layout = instanceLayoutFor(spec.coordinator);
  const sqlite = new Database(join(layout.dataDir, "coordinator_v2.db"), { readonly: true });
  try {
    const row = sqlite.query(
      "SELECT id, kind, encrypted_payload FROM email_outbox WHERE kind = ? ORDER BY rowid DESC LIMIT 1",
    ).get(kind) as { id: string; kind: string; encrypted_payload: string } | null;
    if (!row) {
      const kinds = sqlite.query("SELECT kind, state FROM email_outbox ORDER BY rowid").all();
      throw new Error(`${kind} outbox row is missing: ${JSON.stringify(kinds)}`);
    }
    const payload = createEmailOutboxPayloadCipher(readFileSync(layout.outboxKeyPath, "utf8"))
      .decrypt({ outboxId: row.id, kind: row.kind }, row.encrypted_payload);
    const expression = kind === "owner_activation"
      ? new RegExp(`/activate/${ROUTE_KEY}#([A-Za-z0-9_-]+)`)
      : new RegExp(`/reset-password/${ROUTE_KEY}#([A-Za-z0-9_-]+)`);
    const match = expression.exec(payload.text ?? payload.html);
    if (!match?.[1]) throw new Error(`${kind} token missing from encrypted outbox`);
    return match[1];
  } finally {
    sqlite.close(true);
  }
}

async function fillManagedInput(page: Page, testId: string, value: string): Promise<void> {
  await page.getByTestId(testId).evaluate((element, nextValue) => {
    if (!(element instanceof HTMLElement) || !("value" in element)) {
      throw new Error("managed auth control does not expose a value");
    }
    (element as HTMLElement & { value: string }).value = nextValue;
    element.dispatchEvent(new InputEvent("input", {
      bubbles: true,
      composed: true,
      inputType: "insertText",
      data: nextValue,
    }));
  }, value);
}

test.skipIf(!enabled)("activates, signs out, logs in, resets, and rejects every old browser key", async () => {
  if (typeof process.getuid !== "function" || process.getuid() !== 0) {
    throw new Error("ROOST_SAAS_E2E requires Linux root");
  }
  const { imageId, network } = requiredManagedE2eResources();
  const root = mkdtempSync(join(tmpdir(), "roost-managed-browser-"));
  const sharedKey = join(root, "resend-key");
  writeFileSync(sharedKey, "re_browser_smoke_not_delivered", { mode: 0o600 });
  const authVerifyKeyFile = join(root, "saas-auth-verify-key");
  writeEd25519VerificationKeyFixture(authVerifyKeyFile);
  const spec = makeSpec(root, sharedKey, authVerifyKeyFile, imageId);
  const runtime = new ManagedInstanceRuntime({ network });
  let proxy: Server<undefined> | undefined;
  let browser: Browser | undefined;
  try {
    await runtime.seedOwnerActivation(spec);
    const activationToken = outboxToken(spec, "owner_activation");
    await runtime.startAndVerify(spec);
    const upstream = `http://${containerIp(spec.coordinator.containerName, network)}:4104`;
    proxy = Bun.serve({
      hostname: "127.0.0.1",
      port: 0,
      async fetch(request) {
        const target = new URL(request.url);
        if (target.pathname === "/__roost/tenant/resolve") {
          return Response.json({ routeKey: ROUTE_KEY }, {
            headers: { "cache-control": "no-store", "x-content-type-options": "nosniff" },
          });
        }
        const prefix = `/_roost/t/${ROUTE_KEY}`;
        const upstreamPath = target.pathname.startsWith(`${prefix}/`)
          ? target.pathname.slice(prefix.length)
          : target.pathname;
        const upstreamUrl = new URL(`${upstreamPath}${target.search}`, upstream);
        const body = request.method === "GET" || request.method === "HEAD"
          ? undefined
          : await request.arrayBuffer();
        const headers = new Headers(request.headers);
        headers.delete("host");
        headers.delete("connection");
        headers.set("accept-encoding", "identity");
        try {
          const response = await fetch(upstreamUrl, {
            method: request.method,
            headers,
            body,
            redirect: "manual",
          });
          const responseHeaders = new Headers(response.headers);
          responseHeaders.delete("content-length");
          responseHeaders.delete("content-encoding");
          return new Response(response.body, {
            status: response.status,
            statusText: response.statusText,
            headers: responseHeaders,
          });
        } catch {
          return new Response("upstream unavailable", { status: 503 });
        }
      },
    });
    const origin = `http://127.0.0.1:${proxy.port}`;
    browser = await chromium.launch({ headless: true });
    const page = await browser.newPage();
    const observed: Array<{ url: string; referer?: string }> = [];
    const responses: Array<{ url: string; status: number }> = [];
    page.on("response", (response) => responses.push({ url: response.url(), status: response.status() }));
    page.on("request", (request) => {
      observed.push({ url: request.url(), referer: request.headers()["referer"] });
    });

    await page.goto(`${origin}/activate/${ROUTE_KEY}#${activationToken}`, { waitUntil: "domcontentloaded" });
    expect(await page.evaluate(() => ({ secure: isSecureContext, subtle: Boolean(crypto.subtle) })))
      .toEqual({ secure: true, subtle: true });
    await page.waitForFunction(() => location.hash === "");
    expect(new URL(page.url()).hash).toBe("");
    await fillManagedInput(page, "managed-activation-password", PASSWORD);
    await fillManagedInput(page, "managed-activation-confirmation", PASSWORD);
    await page.getByTestId("managed-activation").locator("form")
      .evaluate((form) => form.dispatchEvent(new SubmitEvent("submit", { bubbles: true, cancelable: true })));
    try {
      await page.waitForURL(`${origin}/app`, { timeout: 30_000 });
    } catch {
      throw new Error(
        `activation UI failed: ${await page.getByTestId("managed-activation-error").textContent()} `
          + JSON.stringify(responses.filter((response) => response.url.includes("AuthOwnerActivate"))),
      );
    }

    await page.goto(`${origin}/settings/account`);
    await page.getByTestId("settings-account-pane").waitFor();
    await page.getByTestId("managed-sign-out").evaluate((element: HTMLElement) => element.click());
    await page.waitForURL(`${origin}/login`);
    await page.reload();
    expect(new URL(page.url()).pathname).toBe("/login");

    await fillManagedInput(page, "managed-login-email", EMAIL);
    await fillManagedInput(page, "managed-login-password", PASSWORD);
    await page.getByTestId("managed-login").locator("form")
      .evaluate((form) => form.dispatchEvent(new SubmitEvent("submit", { bubbles: true, cancelable: true })));
    await page.waitForURL(`${origin}/app`);

    const oldJwt = await page.evaluate(async () => {
      const open = indexedDB.open("roost-auth");
      const database = await new Promise<IDBDatabase>((resolve, reject) => {
        open.onsuccess = () => resolve(open.result);
        open.onerror = () => reject(open.error);
      });
      const pair = await new Promise<CryptoKeyPair>((resolve, reject) => {
        const request = database.transaction("keys", "readonly").objectStore("keys").get("ed25519");
        request.onsuccess = () => resolve(request.result as CryptoKeyPair);
        request.onerror = () => reject(request.error);
      });
      const raw = new Uint8Array(await crypto.subtle.exportKey("raw", pair.publicKey));
      const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", raw));
      const kid = Array.from(digest, (byte) => byte.toString(16).padStart(2, "0")).join("");
      const encoded = (value: string | Uint8Array) => {
        const bytes = typeof value === "string" ? new TextEncoder().encode(value) : value;
        return btoa(Array.from(bytes, (byte) => String.fromCharCode(byte)).join(""))
          .replace(/\+/g, "-").replace(/\//g, "_").replace(/=/g, "");
      };
      const now = Math.floor(Date.now() / 1000);
      const header = encoded(JSON.stringify({ alg: "EdDSA", typ: "JWT", kid }));
      const payload = encoded(JSON.stringify({ sub: kid, aud: "roost-coordinator", iat: now, exp: now + 300 }));
      const message = new TextEncoder().encode(`${header}.${payload}`);
      const signature = new Uint8Array(await crypto.subtle.sign("Ed25519", pair.privateKey, message));
      return `${header}.${payload}.${encoded(signature)}`;
    });

    // Reset the per-process public-edge budget after the asset-heavy first boot.
    await runtime.stop(spec.coordinator);
    await runtime.startAndVerify(spec);

    await page.goto(`${origin}/forgot-password`);
    await fillManagedInput(page, "managed-forgot-password-email", EMAIL);
    await page.getByTestId("managed-forgot-password").locator("form")
      .evaluate((form) => form.dispatchEvent(new SubmitEvent("submit", { bubbles: true, cancelable: true })));
    await page.getByTestId("managed-forgot-password-ack").waitFor();
    const resetToken = outboxToken(spec, "password_reset");
    await page.goto(`${origin}/reset-password/${ROUTE_KEY}#${resetToken}`, { waitUntil: "domcontentloaded" });
    await page.waitForFunction(() => location.hash === "");
    expect(new URL(page.url()).hash).toBe("");
    await fillManagedInput(page, "managed-reset-password", NEW_PASSWORD);
    await fillManagedInput(page, "managed-reset-confirmation", NEW_PASSWORD);
    await page.getByTestId("managed-password-reset").locator("form")
      .evaluate((form) => form.dispatchEvent(new SubmitEvent("submit", { bubbles: true, cancelable: true })));
    await page.waitForURL(`${origin}/login`);
    await page.reload();
    expect(new URL(page.url()).pathname).toBe("/login");

    const rejected = await fetch(`${origin}/roost.v1.CoordinatorService/SessionsList`, {
      method: "POST",
      headers: {
        authorization: `Bearer ${oldJwt}`,
        "content-type": "application/proto",
        "connect-protocol-version": "1",
        "x-roost-dashboard-id": COORDINATOR_ID,
      },
      body: new Uint8Array(),
    });
    expect(rejected.status).toBe(401);

    await fillManagedInput(page, "managed-login-email", EMAIL);
    await fillManagedInput(page, "managed-login-password", NEW_PASSWORD);
    await page.getByTestId("managed-login").locator("form")
      .evaluate((form) => form.dispatchEvent(new SubmitEvent("submit", { bubbles: true, cancelable: true })));
    await page.waitForURL(`${origin}/app`);
    expect(observed.some((request) => request.url.includes("token=") || request.referer?.includes("token="))).toBe(false);
  } finally {
    await browser?.close();
    proxy?.stop(true);
    Bun.spawnSync(["docker", "rm", "--force", spec.coordinator.containerName], { stdout: "ignore", stderr: "ignore" });
    rmSync(root, { recursive: true, force: true });
  }
}, 180_000);
