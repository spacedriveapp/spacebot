export function EmptyState() {
	return (
		<div className="flex h-full w-full flex-1 flex-col items-center justify-center gap-3 text-ink-faint">
			<div className="flex h-12 w-12 items-center justify-center rounded-full border border-app-line">
				<svg
					width="24"
					height="24"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					strokeWidth="1.5"
					strokeLinecap="round"
					strokeLinejoin="round"
				>
					<rect x="2" y="3" width="20" height="18" rx="2" />
					<line x1="9" y1="3" x2="9" y2="21" />
					<line x1="15" y1="3" x2="15" y2="21" />
				</svg>
			</div>
			<div className="text-center">
				<p className="text-sm font-medium">No active workers</p>
				<p className="mt-1 text-xs">
					OpenCode workers will appear here as columns when they're running.
				</p>
			</div>
		</div>
	);
}
