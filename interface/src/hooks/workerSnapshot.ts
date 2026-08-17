export function workerLifecycleKey(scopeId: string, workerId: string): string {
	return `${scopeId}:${workerId}`;
}

export function reconcileWorkerSnapshot<TCurrent, TSnapshot>(
	current: Record<string, TCurrent>,
	snapshot: TSnapshot[],
	requestGeneration: number,
	lifecycleGenerations: ReadonlyMap<string, number>,
	belongsToScope: (worker: TCurrent) => boolean,
	currentKey: (worker: TCurrent) => string,
	snapshotId: (worker: TSnapshot) => string,
	snapshotKey: (worker: TSnapshot) => string,
	merge: (current: TCurrent | undefined, snapshot: TSnapshot) => TCurrent,
): Record<string, TCurrent> {
	const next = {...current};
	for (const [workerId, worker] of Object.entries(current)) {
		if (
			belongsToScope(worker) &&
			(lifecycleGenerations.get(currentKey(worker)) ?? 0) <= requestGeneration
		) {
			delete next[workerId];
		}
	}
	for (const worker of snapshot) {
		const workerId = snapshotId(worker);
		if ((lifecycleGenerations.get(snapshotKey(worker)) ?? 0) > requestGeneration) continue;
		next[workerId] = merge(current[workerId], worker);
	}
	return next;
}
