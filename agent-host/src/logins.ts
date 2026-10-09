import { randomUUID } from "node:crypto";
import type { AuthPrompt, AuthEvent } from "@earendil-works/pi-ai";
import type { ModelsService } from "./models.ts";

type PublicPrompt = { id: string; type: AuthPrompt["type"]; message: string; options: string[] };
type Notice = { type: string; message: string; url: string | null; code: string | null };
type Login = { id: string; provider: string; state: string; prompt: PublicPrompt | null; notices: Notice[]; error: string | null; controller: AbortController; waiting?: (value: string) => void; promptId?: string };

export class Logins {
  private readonly logins = new Map<string, Login>();
  private readonly service: ModelsService;
  constructor(service: ModelsService) { this.service = service; }
  start(provider: string): { login_id: string } {
    const login: Login = { id: randomUUID(), provider, state: "waiting", prompt: null, notices: [], error: null, controller: new AbortController() };
    this.logins.set(login.id, login);
    void this.service.models.login(provider, "oauth", {
      signal: login.controller.signal,
      prompt: prompt => new Promise<string>((resolve, reject) => {
        login.promptId = randomUUID();
        login.prompt = { id: login.promptId, type: prompt.type, message: prompt.message, options: prompt.type === "select" ? prompt.options.map(option => option.id) : [] };
        login.state = "prompt";
        login.waiting = resolve;
        prompt.signal?.addEventListener("abort", () => reject(new Error("prompt cancelled")), { once: true });
      }),
      notify: (event: AuthEvent) => {
        const notice: Notice = event.type === "auth_url" ? { type: event.type, message: event.instructions ?? "Sign in", url: event.url, code: null } : event.type === "device_code" ? { type: event.type, message: event.verificationUri, url: event.verificationUri, code: event.userCode } : { type: event.type, message: event.message, url: null, code: null };
        login.notices.push(notice);
      },
    }).then(() => { login.state = "done"; }).catch(error => { login.state = "failed"; login.error = String(error); }).finally(() => { setTimeout(() => this.logins.delete(login.id), 10 * 60 * 1000).unref(); });
    return { login_id: login.id };
  }
  poll(id: string): Record<string, unknown> { const login = this.get(id); return { state: login.state, prompt: login.prompt, notices: login.notices, error: login.error }; }
  respond(id: string, promptId: string, value: string): void {
    const login = this.get(id);
    if (login.promptId !== promptId || !login.waiting) throw Object.assign(new Error("prompt is no longer waiting"), { status: 400, code: "invalid" });
    login.waiting(value); login.waiting = undefined; login.prompt = null; login.state = "waiting";
  }
  cancel(id: string): void { const login = this.get(id); login.controller.abort(); login.state = "failed"; login.error = "cancelled"; }
  private get(id: string): Login { const login = this.logins.get(id); if (!login) throw Object.assign(new Error("login not found"), { status: 404, code: "not_found" }); return login; }
}
