// These tests own managed-container layout, seeding, sandbox, and adoption guarantees.
// They exercise the production Docker runtime against deterministic fixture records.
// Direct health and identity verification live in the focused verification sibling.
import { afterEach, describe, expect, test } from "bun:test";
import { chmodSync, readFileSync, rmSync, statSync, symlinkSync } from "node:fs";
import { ManagedInstanceRuntime, managedDockerInternals } from "../src/saas/docker.ts";
import { instanceLayoutFor } from "../src/saas/layout.ts";
import {
  COORDINATOR_ID,
  ROUTE_KEY,
  cleanupDockerFixtures,
  commandOk,
  testSpec,
  validInspect,
} from "./saas-docker-fixture.ts";

afterEach(cleanupDockerFixtures);

describe("managed Docker runtime", () => {
  test("creates and exactly adopts mode-locked instance data and secrets", () => {
    const spec = testSpec();
    const uid = process.getuid?.() ?? statSync(spec.root).uid;
    const gid = process.getgid?.() ?? statSync(spec.root).gid;
    const runtime = new ManagedInstanceRuntime({
      uid,
      gid,
      randomKey: () => new Uint8Array(32).fill(7),
    });
    const input = {
      account: spec.account,
      coordinator: spec.coordinator,
      authVerifyKeyFile: spec.authVerifyKeyFile,
      email: {
        resendEndpoint: "https://api.resend.com/emails",
        emailFrom: "Roost <noreply@example.com>",
        sharedResendApiKeyPath: spec.sharedKey,
      },
    };
    const layout = runtime.ensureLayout(input);
    for (const path of [layout.instanceDir, layout.dataDir, layout.secretsDir, layout.verifierDir]) {
      expect(statSync(path).mode & 0o777).toBe(0o700);
    }
    for (const path of [
      layout.authorizedKeysPath,
      layout.manifestPath,
      layout.resendApiKeyPath,
      layout.authVerifyKeyPath,
      layout.outboxKeyPath,
    ]) {
      expect(statSync(path).mode & 0o777).toBe(0o600);
    }
    expect(readFileSync(layout.authorizedKeysPath, "utf8")).toBe("");
    expect(readFileSync(layout.resendApiKeyPath, "utf8")).toBe("re_test_shared_key");
    expect(readFileSync(layout.authVerifyKeyPath, "utf8")).toContain("ssh-ed25519 ");
    expect(readFileSync(layout.outboxKeyPath, "utf8")).toHaveLength(43);
    expect(() => runtime.ensureLayout(input)).not.toThrow();
    chmodSync(layout.outboxKeyPath, 0o644);
    expect(() => runtime.ensureLayout(input)).toThrow("wrong mode");
  });

  test("rejects symlinked secret paths instead of following them", () => {
    const spec = testSpec();
    const uid = process.getuid?.() ?? statSync(spec.root).uid;
    const gid = process.getgid?.() ?? statSync(spec.root).gid;
    const runtime = new ManagedInstanceRuntime({ uid, gid, randomKey: () => new Uint8Array(32) });
    const input = {
      account: spec.account,
      coordinator: spec.coordinator,
      authVerifyKeyFile: spec.authVerifyKeyFile,
      email: {
        resendEndpoint: "https://api.resend.com/emails",
        emailFrom: "Roost <noreply@example.com>",
        sharedResendApiKeyPath: spec.sharedKey,
      },
    };
    const layout = runtime.ensureLayout(input);
    rmSync(layout.resendApiKeyPath);
    symlinkSync(spec.sharedKey, layout.resendApiKeyPath);
    expect(() => runtime.ensureLayout(input)).toThrow(/non-regular|ELOOP/);
  });

  test("seed argv contains exact sandboxing but no token, outbox key, labels, or public ports", async () => {
    const spec = testSpec();
    const uid = process.getuid?.() ?? statSync(spec.root).uid;
    const gid = process.getgid?.() ?? statSync(spec.root).gid;
    const calls: string[][] = [];
    const runtime = new ManagedInstanceRuntime({
      uid,
      gid,
      randomKey: () => new Uint8Array(32).fill(9),
      runner: async (argv) => {
        calls.push([...argv]);
        return commandOk();
      },
    });
    await runtime.seedOwnerActivation({
      account: spec.account,
      coordinator: spec.coordinator,
      authVerifyKeyFile: spec.authVerifyKeyFile,
      email: {
        resendEndpoint: "https://api.resend.com/emails",
        emailFrom: "Roost <noreply@example.com>",
        sharedResendApiKeyPath: spec.sharedKey,
      },
    });
    expect(calls).toHaveLength(1);
    const command = calls[0]!;
    expect(command.slice(0, 7)).toEqual(["docker", "run", "--rm", "--network", "none", "--user", "65532:65532"]);
    expect(command).toContain("--read-only");
    expect(command).toContain("__saas-instance");
    expect(command).toContain("seed-owner-activation");
    expect(command).toContain("--email");
    expect(command).not.toContain("--publish");
    expect(command).not.toContain("--label");
    const mounts = command.flatMap((value, index) =>
      command[index - 1] === "--mount" ? [value] : []
    );
    expect(mounts).toHaveLength(3);
    expect(mounts).toContain(
      `type=bind,src=${instanceLayoutFor(spec.coordinator).verifierDir},dst=/run/auth,readonly`,
    );
    const environment = command.flatMap((value, index) =>
      command[index - 1] === "--env" ? [value] : []
    );
    expect(environment).toContain(
      "ROOST_SAAS_AUTH_VERIFY_KEY_FILE=/run/auth/saas-auth-verify-key",
    );
    expect(environment.some((entry) =>
      /(?:ASSERTION_SIGNING|GOOGLE|TURNSTILE|CLIENT_SECRET|PRIVATE_KEY)/i.test(entry)
    )).toBe(false);
    const flattened = command.join(" ");
    expect(flattened).toContain("ROOST_WEB_PUBLIC_URL=https://dashboard.roosttt.com");
    expect(flattened).toContain(`ROOST_TENANT_ROUTE_KEY=${ROUTE_KEY}`);
    expect(flattened).not.toContain("CQkJCQkJ");
    expect(flattened).not.toMatch(/token=/i);
    expect(flattened).not.toContain("/var/run/docker.sock");
  });

  test("creates a persistent container with exactly the managed mounts", async () => {
    const spec = testSpec();
    const uid = process.getuid?.() ?? statSync(spec.root).uid;
    const gid = process.getgid?.() ?? statSync(spec.root).gid;
    const input = {
      account: spec.account,
      coordinator: spec.coordinator,
      authVerifyKeyFile: spec.authVerifyKeyFile,
      email: {
        resendEndpoint: "https://api.resend.com/emails",
        emailFrom: "Roost <noreply@example.com>",
        sharedResendApiKeyPath: spec.sharedKey,
      },
    };
    let created = false;
    let createCommand: readonly string[] | undefined;
    const runtime = new ManagedInstanceRuntime({
      uid,
      gid,
      randomKey: () => new Uint8Array(32).fill(5),
      runner: async (argv) => {
        if (argv[1] === "inspect") {
          return created
            ? commandOk(JSON.stringify([validInspect(spec)]))
            : { exitCode: 1, stdout: "", stderr: "No such object" };
        }
        if (argv[1] === "create") {
          created = true;
          createCommand = [...argv];
        }
        return commandOk();
      },
    });
    await runtime.ensureContainer(input);
    expect(createCommand).toBeDefined();
    const mounts = createCommand!.flatMap((value, index) =>
      createCommand![index - 1] === "--mount" ? [value] : []
    );
    expect(mounts).toEqual([
      `type=bind,src=${instanceLayoutFor(spec.coordinator).dataDir},dst=/data`,
      `type=bind,src=${instanceLayoutFor(spec.coordinator).secretsDir},dst=/run/secrets,readonly`,
      `type=bind,src=${instanceLayoutFor(spec.coordinator).verifierDir},dst=/run/auth,readonly`,
    ]);
  });

  test("adopts only an exact immutable, isolated container", () => {
    const spec = testSpec();
    const inspect = validInspect(spec);
    const layout = instanceLayoutFor(spec.coordinator);
    const input = {
      account: spec.account,
      coordinator: spec.coordinator,
      authVerifyKeyFile: spec.authVerifyKeyFile,
      email: {
        resendEndpoint: "https://api.resend.com/emails",
        emailFrom: "Roost <noreply@example.com>",
        sharedResendApiKeyPath: spec.sharedKey,
      },
    };
    expect(() => managedDockerInternals.assertExactContainer(inspect, input, layout, "web")).not.toThrow();
    const expectedEnvironment = managedDockerInternals.requiredEnvironment(input, layout);
    expect(expectedEnvironment.ROOST_WEB_PUBLIC_URL).toBe("https://dashboard.roosttt.com");
    expect(expectedEnvironment.ROOST_TENANT_ROUTE_KEY).toBe(ROUTE_KEY);
    expect(expectedEnvironment.ROOST_SAAS_AUTH_VERIFY_KEY_FILE)
      .toBe("/run/auth/saas-auth-verify-key");
    expect(Object.keys(expectedEnvironment).some((name) =>
      /(?:ASSERTION_SIGNING|GOOGLE|TURNSTILE|CLIENT_SECRET|PRIVATE_KEY)/i.test(name)
    )).toBe(false);
    const wrongImage = structuredClone(inspect);
    wrongImage.Image = `sha256:${"b".repeat(64)}`;
    expect(() => managedDockerInternals.assertExactContainer(wrongImage, input, layout, "web")).toThrow();

    const wrongLabel = structuredClone(inspect);
    wrongLabel.Config.Labels["com.roost.account-id"] = COORDINATOR_ID;
    expect(() => managedDockerInternals.assertExactContainer(wrongLabel, input, layout, "web")).toThrow();
    const wrongPublicOrigin = structuredClone(inspect);
    wrongPublicOrigin.Config.Env = wrongPublicOrigin.Config.Env.map((entry: string) =>
      entry.startsWith("ROOST_WEB_PUBLIC_URL=")
        ? "ROOST_WEB_PUBLIC_URL=https://c-legacy.dashboard.roosttt.com"
        : entry
    );
    expect(() =>
      managedDockerInternals.assertExactContainer(wrongPublicOrigin, input, layout, "web")
    ).toThrow("ROOST_WEB_PUBLIC_URL");

    const wrongRouteKey = structuredClone(inspect);
    wrongRouteKey.Config.Env = wrongRouteKey.Config.Env.map((entry: string) =>
      entry.startsWith("ROOST_TENANT_ROUTE_KEY=")
        ? `ROOST_TENANT_ROUTE_KEY=${"cd".repeat(32)}`
        : entry
    );
    expect(() =>
      managedDockerInternals.assertExactContainer(wrongRouteKey, input, layout, "web")
    ).toThrow("ROOST_TENANT_ROUTE_KEY");

    const writableRoot = structuredClone(inspect);
    writableRoot.HostConfig.ReadonlyRootfs = false;
    expect(() => managedDockerInternals.assertExactContainer(writableRoot, input, layout, "web")).toThrow();

    const publishedPort = structuredClone(inspect);
    publishedPort.HostConfig.PortBindings = { "4104/tcp": [{ HostPort: "4104" }] };
    expect(() => managedDockerInternals.assertExactContainer(publishedPort, input, layout, "web")).toThrow();

    const hostMount = structuredClone(inspect);
    hostMount.Mounts.push({ Type: "bind", Source: "/home", Destination: "/host", RW: false });
    expect(() => managedDockerInternals.assertExactContainer(hostMount, input, layout, "web")).toThrow();

    const missingVerifyKey = structuredClone(inspect);
    missingVerifyKey.Mounts = missingVerifyKey.Mounts.filter(
      (mount) => mount.Destination !== "/run/auth",
    );
    expect(() =>
      managedDockerInternals.assertExactContainer(missingVerifyKey, input, layout, "web")
    ).toThrow("mount count");

    const wrongVerifyKey = structuredClone(inspect);
    const wrongVerifyKeyMount = wrongVerifyKey.Mounts.find(
      (mount) => mount.Destination === "/run/auth",
    );
    expect(wrongVerifyKeyMount).toBeDefined();
    if (wrongVerifyKeyMount === undefined) throw new Error("verify-key mount fixture was absent");
    wrongVerifyKeyMount.Source = "/wrong/auth";
    expect(() =>
      managedDockerInternals.assertExactContainer(wrongVerifyKey, input, layout, "web")
    ).toThrow("SaaS auth verifier mount");

    const writableVerifyKey = structuredClone(inspect);
    const writableVerifyKeyMount = writableVerifyKey.Mounts.find(
      (mount) => mount.Destination === "/run/auth",
    );
    expect(writableVerifyKeyMount).toBeDefined();
    if (writableVerifyKeyMount === undefined) throw new Error("verify-key mount fixture was absent");
    writableVerifyKeyMount.RW = true;
    expect(() =>
      managedDockerInternals.assertExactContainer(writableVerifyKey, input, layout, "web")
    ).toThrow("SaaS auth verifier mount");

    const noMemoryLimit = structuredClone(inspect);
    noMemoryLimit.HostConfig.Memory = 0;
    expect(() => managedDockerInternals.assertExactContainer(noMemoryLimit, input, layout, "web")).toThrow();

    const siblingNetwork = structuredClone(inspect);
    Object.assign(siblingNetwork.NetworkSettings.Networks, {
      sibling: { IPAddress: "172.20.0.11" },
    });
    expect(() => managedDockerInternals.assertExactContainer(siblingNetwork, input, layout, "web")).toThrow();
  });

});
