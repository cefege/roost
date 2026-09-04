// x-roost-* request headers and their sentinel values, exchanged between
// the web SPA interceptor and the coord auth/middleware stack. Header names
// and sentinels are part of the auth-classification contract (device vs
// access vs tailscale-serve vs public-edge); a rename on one side silently
// reclassifies the other. Keep both ends importing from here.

export const X_ROOST_TAB_ID = "x-roost-tab-id";
export const X_ROOST_DASHBOARD_ID = "x-roost-dashboard-id";
export const X_ROOST_TRACE_ID = "x-roost-trace-id";
export const X_ROOST_REMOTE_ADDR = "x-roost-remote-addr";
export const X_ROOST_ON_HOST = "x-roost-on-host";
export const X_ROOST_LISTENER_TRUST = "x-roost-listener-trust";
export const X_ROOST_AUTH_LAYER = "x-roost-auth-layer";

/** Sentinel values carried in x-roost-auth-layer / listener-trust. */
export const AUTH_LAYER_DEVICE = "device";
export const AUTH_LAYER_ACCESS = "access";
export const AUTH_LAYER_TAILSCALE_SERVE = "tailscale-serve";
export const AUTH_LAYER_PUBLIC_EDGE = "public-edge";
export const LISTENER_TRUST_YES = "1";
