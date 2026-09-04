// Managed signup needs a human-verification token without owning Cloudflare's global script.
// This component loads, retries, sizes, and resets one Turnstile widget per signup attempt.
// Callbacks expose the current token or recoverable error while cleanup removes the widget.

import { createEffect, createSignal, onCleanup, onMount, Show } from "solid-js";
import { Button } from "./Settings/md/Button.tsx";

export const TURNSTILE_SCRIPT_URL =
  "https://challenges.cloudflare.com/turnstile/v0/api.js?render=explicit";

const TURNSTILE_NORMAL_WIDTH = 300;

interface TurnstileApi {
  render(
    container: HTMLElement,
    options: {
      sitekey: string;
      action: "signup";
      size: "normal" | "compact";
      callback: (token: string) => void;
      "expired-callback": () => void;
      "error-callback": () => void;
    },
  ): string;
  reset(widgetId: string): void;
  remove(widgetId: string): void;
}

let scriptPromise: Promise<TurnstileApi> | null = null;

function currentTurnstile(): TurnstileApi | undefined {
  return (window as unknown as { turnstile?: TurnstileApi }).turnstile;
}

function loadTurnstile(): Promise<TurnstileApi> {
  const loaded = currentTurnstile();
  if (loaded) return Promise.resolve(loaded);
  if (scriptPromise) return scriptPromise;

  let candidateScript: HTMLScriptElement | undefined;
  const pending = new Promise<TurnstileApi>((resolve, reject) => {
    const existing = document.querySelector<HTMLScriptElement>(
      `script[src="${TURNSTILE_SCRIPT_URL}"]`,
    );
    const script = existing ?? document.createElement("script");
    candidateScript = script;

    function clearListeners(): void {
      script.removeEventListener("load", ready);
      script.removeEventListener("error", unavailable);
    }

    function ready(): void {
      clearListeners();
      const api = currentTurnstile();
      if (api) resolve(api);
      else reject(new Error("Turnstile unavailable"));
    }

    function unavailable(): void {
      clearListeners();
      reject(new Error("Turnstile unavailable"));
    }

    script.addEventListener("load", ready, { once: true });
    script.addEventListener("error", unavailable, { once: true });
    if (!existing) {
      script.src = TURNSTILE_SCRIPT_URL;
      script.async = true;
      script.defer = true;
      script.dataset.roostTurnstile = "true";
      document.head.append(script);
    }
  });

  scriptPromise = pending;
  void pending.then(undefined, () => {
    if (scriptPromise === pending) scriptPromise = null;
    if (candidateScript?.dataset.roostTurnstile === "true") candidateScript.remove();
  });
  return pending;
}

export function TurnstileWidget(props: {
  siteKey: string;
  disabled?: boolean;
  resetNonce: number;
  onToken: (token: string | null) => void;
}) {
  const [loadFailed, setLoadFailed] = createSignal(false);
  const [loading, setLoading] = createSignal(false);
  let host: HTMLDivElement | undefined;
  let api: TurnstileApi | undefined;
  let widgetId: string | undefined;
  let disposed = false;
  let loadAttempt = 0;
  let observedResetNonce = props.resetNonce;

  function removeRenderedWidget(): void {
    if (api && widgetId) api.remove(widgetId);
    api = undefined;
    widgetId = undefined;
  }

  function resetRenderedWidget(): void {
    props.onToken(null);
    if (!api || !widgetId) return;
    try {
      api.reset(widgetId);
      setLoadFailed(false);
    } catch {
      setLoadFailed(true);
    }
  }

  async function renderChallenge(): Promise<void> {
    const attempt = ++loadAttempt;
    props.onToken(null);
    setLoading(true);
    removeRenderedWidget();

    if (!props.siteKey) {
      if (!disposed && attempt === loadAttempt) {
        setLoadFailed(true);
        setLoading(false);
      }
      return;
    }

    try {
      const loaded = await loadTurnstile();
      if (disposed || attempt !== loadAttempt || !host) return;

      const size = host.clientWidth < TURNSTILE_NORMAL_WIDTH ? "compact" : "normal";
      const renderedWidgetId = loaded.render(host, {
        sitekey: props.siteKey,
        action: "signup",
        size,
        callback: (token) => {
          if (disposed || attempt !== loadAttempt || props.disabled) return;
          setLoadFailed(false);
          props.onToken(token);
        },
        "expired-callback": () => {
          if (disposed || attempt !== loadAttempt) return;
          props.onToken(null);
          queueMicrotask(() => {
            if (!disposed && attempt === loadAttempt) resetRenderedWidget();
          });
        },
        "error-callback": () => {
          if (disposed || attempt !== loadAttempt) return;
          props.onToken(null);
          setLoadFailed(true);
          queueMicrotask(() => {
            if (!disposed && attempt === loadAttempt) resetRenderedWidget();
          });
        },
      });

      if (disposed || attempt !== loadAttempt) {
        loaded.remove(renderedWidgetId);
        return;
      }
      api = loaded;
      widgetId = renderedWidgetId;
      setLoadFailed(false);
    } catch {
      if (!disposed && attempt === loadAttempt) setLoadFailed(true);
    } finally {
      if (!disposed && attempt === loadAttempt) setLoading(false);
    }
  }

  function retryChallenge(): void {
    if (loading() || props.disabled) return;
    void renderChallenge();
  }

  onMount(() => {
    void renderChallenge();
  });

  createEffect(() => {
    const nextResetNonce = props.resetNonce;
    if (nextResetNonce === observedResetNonce) return;
    observedResetNonce = nextResetNonce;
    resetRenderedWidget();
  });

  onCleanup(() => {
    disposed = true;
    loadAttempt++;
    props.onToken(null);
    removeRenderedWidget();
  });

  return (
    <div class="managed-turnstile">
      <div
        ref={host}
        class="managed-turnstile-widget"
        data-testid="managed-turnstile"
        aria-busy={loading() || undefined}
        aria-disabled={props.disabled || undefined}
      />
      <Show when={loadFailed()}>
        <p class="managed-auth-hint managed-turnstile-error" role="alert">
          Human verification couldn’t load. Check your connection and try again.
        </p>
        <Button
          data-testid="managed-turnstile-retry"
          type="button"
          variant="text"
          disabled={loading() || props.disabled}
          onClick={retryChallenge}
        >
          {loading() ? "Retrying…" : "Retry human verification"}
        </Button>
      </Show>
    </div>
  );
}
