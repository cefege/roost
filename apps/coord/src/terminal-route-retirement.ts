// Notifies narrow coordinator-local owners when a volatile terminal route ends.
// byte-hub publishes only after removing an exact route key; compatibility
// adapters subscribe so state tied to that channel cannot outlive its route.
// Listener failures never weaken durable route publication.
import { diag } from "@roost/shared/diag";

type TerminalRouteRetirementListener = (routeKey: string) => void;

const listeners = new Set<TerminalRouteRetirementListener>();

export function subscribeTerminalRouteRetirement(
  listener: TerminalRouteRetirementListener,
): () => void {
  listeners.add(listener);
  return () => { listeners.delete(listener); };
}

export function publishTerminalRouteRetirement(routeKey: string): void {
  for (const listener of listeners) {
    try {
      listener(routeKey);
    } catch (error) {
      diag("terminal_route.retirement_listener_failed", {
        error: String(error),
      });
    }
  }
}
