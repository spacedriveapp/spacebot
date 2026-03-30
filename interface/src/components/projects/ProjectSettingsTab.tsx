export function ProjectSettingsTab({ projectId: _projectId }: { projectId: string }) {
	return (
		<div className="flex flex-col gap-6">
			<div className="rounded-xl border border-app-line bg-app-darkBox p-5">
				<h3 className="mb-3 font-plex text-sm font-semibold text-ink">
					Code Graph Settings
				</h3>
				<p className="text-sm text-ink-faint">
					Per-project configuration overrides will be available here.
					Settings include language filters, indexing thresholds,
					LLM enrichment options, and file watcher configuration.
				</p>
			</div>
		</div>
	);
}
