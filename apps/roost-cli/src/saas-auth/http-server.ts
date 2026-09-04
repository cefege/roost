/**
 * Routes the loopback signup gateway's small public HTTP authentication surface.
 * The gateway entry point supplies email, Google, and result protocols to this server.
 * Exact methods, paths, origins, and response shapes keep unsupported requests fail-closed.
 */

import type { EmailSignupProtocol } from "./email-signup.ts";
import { GATEWAY_LISTEN_HOST, GATEWAY_LISTEN_PORT } from "./gateway-config.ts";
import type { GoogleOAuthProtocol } from "./google-oauth.ts";
import type { ResultProtocol } from "./result-protocol.ts";
import {
  gatewayInvalid,
  gatewayJson,
  gatewayNotFound,
  gatewayRedirect,
  gatewayUnavailable,
  InvalidGatewayRequest,
  readBoundedJson,
  requireBrowserPost,
  requireGatewayGet,
} from "./request-security.ts";

export interface SaasAuthGatewayOptions {
  signupEnabled: boolean;
  googleEnabled: boolean;
  turnstileSiteKey: string;
  email: EmailSignupProtocol;
  google: GoogleOAuthProtocol;
  result: ResultProtocol;
}

export class SaasAuthGateway {
  constructor(private readonly options: SaasAuthGatewayOptions) {}

  async fetch(request: Request, peerIp: string | null): Promise<Response> {
    const url = new URL(request.url);
    try {
      if (url.pathname === "/__roost/auth/config" && url.search === "" && request.method === "GET") {
        requireGatewayGet(request, peerIp);
        return gatewayJson({
          signupEnabled: this.options.signupEnabled,
          googleEnabled: this.options.googleEnabled,
          turnstileSiteKey: this.options.turnstileSiteKey,
        });
      }
      if (url.pathname === "/auth/google/callback" && request.method === "GET") {
        requireGatewayGet(request, peerIp);
        return await this.options.google.callback(request, url);
      }
      if (url.pathname === "/__roost/auth/result" && url.search === "" && request.method === "GET") {
        requireGatewayGet(request, peerIp);
        return await this.options.result.get(request);
      }
      const post = request.method === "POST" && url.search === "";
      if (post && url.pathname === "/__roost/signup/email/start") {
        const clientIp = requireBrowserPost(request, peerIp);
        return await this.options.email.start(await readBoundedJson(request), clientIp);
      }
      if (post && url.pathname === "/__roost/signup/email/verify") {
        requireBrowserPost(request, peerIp);
        return await this.options.email.verify(await readBoundedJson(request));
      }
      if (post && url.pathname === "/__roost/auth/google/start") {
        const clientIp = requireBrowserPost(request, peerIp);
        return await this.options.google.start(request, await readBoundedJson(request), clientIp);
      }
      if (post && url.pathname === "/__roost/auth/bind-device") {
        requireBrowserPost(request, peerIp);
        return await this.options.result.bind(request, await readBoundedJson(request));
      }
      if (post && url.pathname === "/__roost/auth/link/complete") {
        requireBrowserPost(request, peerIp);
        return await this.options.result.completeLink(request, await readBoundedJson(request));
      }
      return gatewayNotFound();
    } catch (error) {
      if (error instanceof InvalidGatewayRequest) return gatewayInvalid();
      if (url.pathname === "/auth/google/callback") return gatewayRedirect("/auth/google/complete");
      return gatewayUnavailable();
    }
  }
}

export interface StartSaasAuthServerOptions extends SaasAuthGatewayOptions {
  port?: number;
  hostname?: typeof GATEWAY_LISTEN_HOST;
  onStop?: () => void;
}

export interface SaasAuthServer {
  readonly server: Bun.Server<undefined>;
  stop(): void;
}

export function startSaasAuthServer(options: StartSaasAuthServerOptions): SaasAuthServer {
  const port = options.port ?? GATEWAY_LISTEN_PORT;
  if (!Number.isSafeInteger(port) || port < 0 || port > 65_535) throw new RangeError("invalid SaaS auth port");
  const gateway = new SaasAuthGateway(options);
  const server = Bun.serve({
    hostname: options.hostname ?? GATEWAY_LISTEN_HOST,
    port,
    idleTimeout: 10,
    maxRequestBodySize: 16 * 1024,
    fetch(request, bunServer) {
      return gateway.fetch(request, bunServer.requestIP(request)?.address ?? null);
    },
    error() {
      return gatewayUnavailable();
    },
  });
  let stopped = false;
  return {
    server,
    stop() {
      if (stopped) return;
      stopped = true;
      server.stop(true);
      options.onStop?.();
    },
  };
}
