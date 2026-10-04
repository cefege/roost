// The wire rows `window.__smoke.rootSnapshot()` exposes — session,
// workspace and worker — and the scrollback history floor a page reports.
// Type-only; mirrors roost-protocol's `wire` shapes as the page serializes them.

/** Why a get-scrollback-cells page came back short of the requested range —
 *  which history floor the caller hit. Mirrors roost.v1.ScrollbackHistoryFloor
 *  and is the wire form the worker stamps on its rpc-ok data.
 *
 *  "none"          the full requested range was served; no floor was hit.
 *  "evicted"       gone forever: the core's own line ring rolled past those
 *                  rows as newer output arrived.
 *  "resize_replay" lost to the bounded keeper history available during worker
 *                  adoption, where no in-memory core survived. Ordinary live
 *                  resize is in place and never creates this floor. */
export type ScrollbackHistoryFloor = "none" | "evicted" | "resize_replay";

export type Session = {
  status: "open" | "closed";
  id: string;
  worker_fp: string;
  channel: number;
  kind: "shell";
  cwd: string;
  workspace_id: string | null;
  created_at: number;
  closed_at: number | null;
  custom_title: string | null;
  spawn_cwd?: string | null | undefined;
  git_branch?: string | null | undefined;
  git_remote?: string | null | undefined;
  pr_number?: number | null | undefined;
  pr_state?: "open" | "closed" | "merged" | "draft" | null | undefined;
  pr_checks?: "none" | "pending" | "passing" | "failing" | null | undefined;
  pr_url?: string | null | undefined;
  ports?: number[] | undefined;
};

export type Workspace = {
  id: string;
  worker_fp: string;
  name: string;
  folder_path: string;
  color: string | null;
  position: number;
  version: number;
  created_at_ms: number;
  updated_at_ms: number;
  session_ids: string[];
};

export type Worker = {
  fp: string;
  label: string;
  os: "darwin" | "linux" | "win32";
  host_identity: {
    hardware_model: string | null;
    chip: string | null;
    linux_distribution: string | null;
  } | null;
  git_sha: string | null;
  host_metrics: {
    cpu_pct: number;
    mem_used_bytes: number;
    mem_total_bytes: number;
    disk_used_bytes: number;
    disk_total_bytes: number;
    net_rx_bps: number;
    net_tx_bps: number;
    sampled_at_ms: number;
  } | null;
  registered_at_ms: number;
  last_seen_ms: number;
  reachable_addr: string | null;
  keeper_runtime: Readonly<{
    schema_version: 1;
    running_contract: Readonly<{
      protocol_version: number;
      supported_features: readonly string[];
      required_features: readonly string[];
      implementation_digest: string | null;
      bun_abi: string;
      platform: "darwin" | "linux" | "win32";
      arch: string;
      build_sha: string;
    }>;
    keeper_pid: number;
    keeper_epoch: string;
    channel_count: number;
    binding_digest: string;
    reconciled_at_ms: number;
  }> | null;
  terminal_core_capacity: Readonly<{
    pending: number;
    used: number;
    capacity: number;
    estimated_reserved_bytes: number;
    effective_memory_ceiling_bytes: number;
    boot_rss_bytes: number;
    overcommit_count: number;
    refusal_count: number;
  }> | null;
};
