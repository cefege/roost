// Owns row/span accounting for current canonical terminal caches and old
// versions retained by active socket snapshot cursors. TerminalScreenHub asks
// this owner before replacing a cache so a slow cursor cannot become an
// uncharged resident full.

import type { TerminalSnapshotLease } from "./terminal-screen-frames.ts";
import type { ResidentCache, SessionScreen } from "./terminal-screen-hub-state.ts";

class TerminalSnapshotVersionLease implements TerminalSnapshotLease {
  constructor(
    private readonly residency: TerminalScreenResidency,
    private readonly cache: ResidentCache,
  ) {}

  acquire(): boolean {
    return this.residency.acquireSourceLease(this.cache);
  }

  release(): void {
    this.residency.releaseSourceLease(this.cache);
  }
}

export class TerminalScreenResidency {
  private residentRows = 0;
  private residentSpans = 0;

  constructor(
    private readonly maxRows: number,
    private readonly maxSpans: number,
  ) {}

  sourceLease(cache: ResidentCache): TerminalSnapshotLease {
    return new TerminalSnapshotVersionLease(this, cache);
  }

  canReplace(state: SessionScreen, rows: number, spans: number): boolean {
    const current = state.cache;
    const currentRows = current?.rows ?? 0;
    const currentSpans = current?.spans ?? 0;
    const retainsCurrent = (current?.sourceLeaseCount ?? 0) > 0;
    if (retainsCurrent && state.pinnedCache && state.pinnedCache !== current) return false;
    const retainedRows = retainsCurrent && state.pinnedCache !== current ? currentRows : 0;
    const retainedSpans = retainsCurrent && state.pinnedCache !== current ? currentSpans : 0;
    return this.residentRows - currentRows + retainedRows + rows <= this.maxRows
      && this.residentSpans - currentSpans + retainedSpans + spans <= this.maxSpans;
  }

  replace(state: SessionScreen, next: ResidentCache): boolean {
    if (!this.canReplace(state, next.rows, next.spans)) return false;
    const current = state.cache;
    if (current && !this.retainCurrentVersion(state, current)) return false;
    if (current) {
      this.residentRows -= current.rows;
      this.residentSpans -= current.spans;
    }
    this.residentRows += next.rows;
    this.residentSpans += next.spans;
    state.cache = next;
    return true;
  }

  drop(state: SessionScreen): boolean {
    const current = state.cache;
    if (!current) return true;
    if (!this.retainCurrentVersion(state, current)) return false;
    this.residentRows -= current.rows;
    this.residentSpans -= current.spans;
    state.cache = null;
    return true;
  }

  acquireSourceLease(cache: ResidentCache): boolean {
    if (cache.sourceLeaseCount === 0 && cache.screen.cache !== cache) return false;
    cache.sourceLeaseCount++;
    return true;
  }

  releaseSourceLease(cache: ResidentCache): void {
    if (cache.sourceLeaseCount === 0) return;
    cache.sourceLeaseCount--;
    if (cache.sourceLeaseCount !== 0 || cache.screen.pinnedCache !== cache) return;
    this.residentRows -= cache.rows;
    this.residentSpans -= cache.spans;
    cache.screen.pinnedCache = null;
  }

  private retainCurrentVersion(state: SessionScreen, cache: ResidentCache): boolean {
    if (cache.sourceLeaseCount === 0 || state.pinnedCache === cache) return true;
    if (state.pinnedCache || this.residentRows + cache.rows > this.maxRows
      || this.residentSpans + cache.spans > this.maxSpans) return false;
    state.pinnedCache = cache;
    this.residentRows += cache.rows;
    this.residentSpans += cache.spans;
    return true;
  }
}
