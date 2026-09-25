// Raw per-platform host sample. The bps rate, the 60s cache and the
// wire shape all live in heartbeat.ts; a sampler only reads counters.
export type HostSample = {
	cpu_pct: number;
	mem_used_bytes: number;
	mem_total_bytes: number;
	disk_used_bytes: number;
	disk_total_bytes: number;
	net: { rxBytes: number; txBytes: number } | null;
};
