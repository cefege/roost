// Serializes durable agent-reference reports with coordinator recovery reads.
// A queued reporter is process-revalidated only after recovery releases the
// gate, while ordinary session respawn/lifecycle admission stays independent.

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
