// Serializes durable agent-reference reports with complete boot reconciliation.
// A queued reporter is process-revalidated only after adoption/respawn/restore,
// while ordinary session lifecycle admission stays independent.

export class AgentReferenceAdmissionGate {
  private tail: Promise<void> = Promise.resolve();

  async runExclusive<Result>(operation: () => Promise<Result>): Promise<Result> {
    const predecessor = this.tail;
    const turn = Promise.withResolvers<void>();
    this.tail = predecessor.then(() => turn.promise);
    await predecessor;
    try {
      return await operation();
    } finally {
      turn.resolve();
    }
  }
}
